//! Composites a pane's visible Kitty placements into one RGBA image.
//!
//! Geometry comes from `clipped_placement`, the same math that positions
//! placements on the host terminal, so a snapshot matches what users see.
//! Text cells are not rendered.

use ratatui::layout::Rect;

use super::{clipped_placement, HostCellSize, HostPlacement, HostSourceKey};
use crate::ghostty::{KittyImageFormat, KittyImagePlacement};
use crate::layout::PaneId;

pub(crate) struct PaneSnapshot {
    pub(crate) width_px: u32,
    pub(crate) height_px: u32,
    /// Placements that contributed pixels to the canvas.
    pub(crate) placements: u32,
    /// Straight-alpha RGBA8, row-major, `width_px * height_px * 4` bytes.
    pub(crate) rgba: Vec<u8>,
}

/// Draws `placements` in ascending `z` over a transparent `cols` x `rows` cell
/// canvas. Placements without decoded RGB/RGBA pixels are skipped.
// ponytail: canvas is cols*rows*cell pixels in memory; tile it if huge panes matter.
pub(crate) fn composite(
    mut placements: Vec<KittyImagePlacement>,
    cols: u16,
    rows: u16,
    cell: HostCellSize,
) -> PaneSnapshot {
    let width_px = u32::from(cols) * cell.width_px;
    let height_px = u32::from(rows) * cell.height_px;
    let mut rgba = vec![0u8; width_px as usize * height_px as usize * 4];
    let mut composited = 0;
    placements.sort_by_key(|placement| placement.z);
    for placement in placements {
        let bpp = match placement.format {
            KittyImageFormat::Rgb => 3,
            KittyImageFormat::Rgba => 4,
            // Encoded PNG only arrives with test-only PNG forwarding.
            KittyImageFormat::Png => continue,
        };
        let image_width = placement.image_width as usize;
        let image_height = placement.image_height as usize;
        if placement.data.len() < image_width * image_height * bpp {
            continue;
        }
        let host = HostPlacement {
            raw_data: None,
            pane_id: PaneId::from_raw(0),
            host_image_id: None,
            area: Rect::new(0, 0, cols, rows),
            cell_size: cell,
            source_key: HostSourceKey::Terminal {
                pane_id: PaneId::from_raw(0),
                image_id: placement.image_id,
            },
            placement,
            scrollback_offset: 0,
        };
        let Some((clip, _)) = clipped_placement(&host) else {
            continue;
        };
        let data = &host.placement.data;
        // Same rectangle `encode_display_placement` asks the host to fill.
        let dest_x = u64::from(clip.x) * u64::from(cell.width_px) + u64::from(clip.x_offset);
        let dest_y = u64::from(clip.y) * u64::from(cell.height_px) + u64::from(clip.y_offset);
        let dest_w = u64::from(clip.cols) * u64::from(cell.width_px);
        let dest_h = u64::from(clip.rows) * u64::from(cell.height_px);
        for dy in 0..dest_h {
            let y = dest_y + dy;
            if y >= u64::from(height_px) {
                break;
            }
            let sy = u64::from(clip.source_y) + dy * u64::from(clip.source_height) / dest_h;
            if sy >= image_height as u64 {
                break;
            }
            for dx in 0..dest_w {
                let x = dest_x + dx;
                if x >= u64::from(width_px) {
                    break;
                }
                let sx = u64::from(clip.source_x) + dx * u64::from(clip.source_width) / dest_w;
                if sx >= image_width as u64 {
                    break;
                }
                let src = (sy as usize * image_width + sx as usize) * bpp;
                let alpha = if bpp == 4 { data[src + 3] } else { 255 };
                let dst = (y as usize * width_px as usize + x as usize) * 4;
                blend(&mut rgba[dst..dst + 4], &data[src..src + 3], alpha);
            }
        }
        composited += 1;
    }
    PaneSnapshot {
        width_px,
        height_px,
        placements: composited,
        rgba,
    }
}

/// Straight-alpha source-over.
fn blend(dst: &mut [u8], src: &[u8], alpha: u8) {
    match alpha {
        0 => {}
        255 => {
            dst[..3].copy_from_slice(src);
            dst[3] = 255;
        }
        _ => {
            let sa = u32::from(alpha);
            let da = u32::from(dst[3]) * (255 - sa) / 255;
            let out_a = sa + da;
            for channel in 0..3 {
                dst[channel] =
                    ((u32::from(src[channel]) * sa + u32::from(dst[channel]) * da) / out_a) as u8;
            }
            dst[3] = out_a as u8;
        }
    }
}

pub(crate) fn encode_png(snapshot: &PaneSnapshot) -> Result<Vec<u8>, png::EncodingError> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, snapshot.width_px, snapshot.height_px);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&snapshot.rgba)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;
    use crate::ghostty::Terminal;

    const CELL: HostCellSize = HostCellSize {
        width_px: 4,
        height_px: 8,
    };

    fn terminal(cols: u16, rows: u16) -> Terminal {
        let mut terminal = Terminal::new(cols, rows, 0).unwrap();
        terminal.enable_kitty_graphics().unwrap();
        terminal
            .resize(cols, rows, CELL.width_px, CELL.height_px)
            .unwrap();
        terminal
    }

    /// zlib stream with one stored block, as `o=z` senders like termium emit.
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let len = u16::try_from(data.len()).unwrap();
        let mut out = vec![0x78, 0x01, 0x01];
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(data);
        let (mut a, mut b) = (1u32, 0u32);
        for byte in data {
            a = (a + u32::from(*byte)) % 65521;
            b = (b + a) % 65521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }

    /// Places a solid image with its top-left at 1-based cell (`row`, `col`).
    fn place(
        terminal: &mut Terminal,
        id: u32,
        (row, col): (u16, u16),
        (width, height): (u32, u32),
        pixel: &[u8],
        z: i32,
        zlib: bool,
    ) {
        let format = if pixel.len() == 3 { 24 } else { 32 };
        let mut data = pixel.repeat((width * height) as usize);
        let mut compression = "";
        if zlib {
            data = zlib_stored(&data);
            compression = ",o=z";
        }
        let payload = base64::engine::general_purpose::STANDARD.encode(data);
        terminal.write(
            format!(
                "\x1b[{row};{col}H\x1b_Ga=T,f={format},s={width},v={height},i={id},p=1,z={z},C=1{compression},q=2;{payload}\x1b\\"
            )
            .as_bytes(),
        );
    }

    /// Composites, round-trips through PNG and returns (width, height, rgba).
    fn snapshot(terminal: &Terminal, cols: u16, rows: u16) -> (u32, u32, u32, Vec<u8>) {
        let snapshot = composite(terminal.kitty_image_placements().unwrap(), cols, rows, CELL);
        let png = encode_png(&snapshot).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut rgba = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut rgba).unwrap();
        (info.width, info.height, snapshot.placements, rgba)
    }

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * width + x) * 4) as usize;
        rgba[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn rgb_tiles_land_on_their_cells() {
        let mut terminal = terminal(10, 4);
        place(&mut terminal, 1, (1, 1), (8, 8), &[255, 0, 0], 0, true);
        place(&mut terminal, 2, (3, 6), (8, 8), &[0, 0, 255], 0, true);

        let (width, height, placements, rgba) = snapshot(&terminal, 10, 4);

        assert_eq!((width, height, placements), (40, 32, 2));
        assert_eq!(pixel(&rgba, width, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&rgba, width, 7, 7), [255, 0, 0, 255]);
        assert_eq!(pixel(&rgba, width, 8, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&rgba, width, 20, 16), [0, 0, 255, 255]);
        assert_eq!(pixel(&rgba, width, 27, 23), [0, 0, 255, 255]);
        assert_eq!(pixel(&rgba, width, 19, 16), [0, 0, 0, 0]);
        assert_eq!(pixel(&rgba, width, 20, 24), [0, 0, 0, 0]);
    }

    #[test]
    fn rgba_blends_over_lower_z() {
        let mut terminal = terminal(4, 2);
        // Upper placement is written first so z order, not write order, decides.
        place(
            &mut terminal,
            2,
            (1, 1),
            (4, 8),
            &[0, 255, 0, 128],
            1,
            false,
        );
        place(&mut terminal, 1, (1, 1), (8, 8), &[255, 0, 0], 0, false);

        let (width, _, placements, rgba) = snapshot(&terminal, 4, 2);

        assert_eq!(placements, 2);
        assert_eq!(pixel(&rgba, width, 0, 0), [127, 128, 0, 255]);
        assert_eq!(pixel(&rgba, width, 4, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn translucent_pixel_over_transparent_canvas_keeps_its_color() {
        let mut terminal = terminal(2, 1);
        place(&mut terminal, 1, (1, 1), (4, 8), &[0, 0, 200, 64], 0, false);

        let (width, _, _, rgba) = snapshot(&terminal, 2, 1);

        assert_eq!(pixel(&rgba, width, 0, 0), [0, 0, 200, 64]);
    }

    #[test]
    fn placements_past_the_viewport_edges_are_clipped() {
        let mut terminal = terminal(4, 2);
        // Three cells wide and two rows tall from the last column and row.
        place(&mut terminal, 1, (2, 4), (12, 16), &[9, 9, 9], 0, false);
        let mut placements = terminal.kitty_image_placements().unwrap();
        // Scrolled half off the top-left.
        let mut above = placements[0].clone();
        above.image_id = 2;
        above.render.viewport_col = -2;
        above.render.viewport_row = -1;
        above.data = [1, 2, 3].repeat(12 * 16);
        placements.push(above);

        let snapshot = composite(placements, 4, 2, CELL);

        assert_eq!(snapshot.placements, 2);
        let px = |x: u32, y: u32| pixel(&snapshot.rgba, snapshot.width_px, x, y);
        assert_eq!(px(12, 8), [9, 9, 9, 255]);
        assert_eq!(px(15, 15), [9, 9, 9, 255]);
        assert_eq!(px(0, 0), [1, 2, 3, 255]);
        assert_eq!(px(3, 7), [1, 2, 3, 255]);
        assert_eq!(px(4, 0), [0, 0, 0, 0]);
    }
}
