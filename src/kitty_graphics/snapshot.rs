//! Composites a pane's visible Kitty placements into one RGBA image.
//!
//! Geometry comes from `clip_geometry`, the same math that positions
//! placements on the host terminal, so a snapshot matches what users see.
//! Text cells are not rendered.

use std::collections::{HashMap, HashSet};

use ratatui::layout::Rect;

use super::{clip_geometry, ClippedPlacement, HostCellSize, PlacementGeometry};
use crate::ghostty::{KittyImageDescriptor, KittyImageFormat, KittyImagePlacement};

/// Largest canvas a snapshot allocates (256 MiB of RGBA).
const MAX_CANVAS_PIXELS: u64 = 64 * 1024 * 1024;
/// Total destination area drawn, in multiples of the canvas, before giving up.
const MAX_CANVAS_COVERAGE: u64 = 8;

pub(crate) struct PaneSnapshot {
    pub(crate) width_px: u32,
    pub(crate) height_px: u32,
    /// Placements that contributed pixels to the canvas.
    pub(crate) placements: u32,
    /// Straight-alpha RGBA8, row-major, `width_px * height_px * 4` bytes.
    pub(crate) rgba: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SnapshotError {
    CanvasTooLarge,
    TooMuchCoverage,
}

impl SnapshotError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::CanvasTooLarge => "snapshot_too_large",
            Self::TooMuchCoverage => "snapshot_too_complex",
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::CanvasTooLarge => {
                format!("snapshot canvas exceeds {MAX_CANVAS_PIXELS} pixels")
            }
            Self::TooMuchCoverage => {
                format!("visible placements cover more than {MAX_CANVAS_COVERAGE} times the canvas")
            }
        }
    }
}

/// Data filter for the placement reader: copies each inline image once, however
/// many placements show it. File-backed images are read later by
/// `image_pixels`, outside the terminal lock.
pub(crate) fn needs_pixels() -> impl FnMut(KittyImageDescriptor) -> bool {
    let mut seen = HashSet::new();
    move |descriptor| !descriptor.source_file && seen.insert(descriptor.image_id)
}

/// Pixels per image id: inline data moved out of the placement that carries it,
/// or the retained file read once.
pub(crate) fn image_pixels(
    placements: &mut [KittyImagePlacement],
) -> std::io::Result<HashMap<u32, Vec<u8>>> {
    let mut pixels = HashMap::new();
    for placement in placements {
        if pixels.contains_key(&placement.image_id) {
            continue;
        }
        let data = if !placement.data.is_empty() {
            std::mem::take(&mut placement.data)
        } else if let Some(source) = &placement.source_file {
            source.copy_rgba()?
        } else {
            continue;
        };
        pixels.insert(placement.image_id, data);
    }
    Ok(pixels)
}

struct Draw<'a> {
    clip: ClippedPlacement,
    data: &'a [u8],
    bpp: usize,
    image_width: usize,
    image_height: usize,
    dest_x: u64,
    dest_y: u64,
    dest_w: u64,
    dest_h: u64,
    x_end: u64,
    y_end: u64,
}

/// Draws `placements` in order (libghostty returns them in ascending `z`) over
/// a transparent `cols` x `rows` cell canvas. Placements whose image has no
/// RGB/RGBA entry in `pixels` are skipped.
pub(crate) fn composite(
    placements: &[KittyImagePlacement],
    pixels: &HashMap<u32, Vec<u8>>,
    cols: u16,
    rows: u16,
    cell: HostCellSize,
) -> Result<PaneSnapshot, SnapshotError> {
    let width_px = u32::from(cols)
        .checked_mul(cell.width_px)
        .ok_or(SnapshotError::CanvasTooLarge)?;
    let height_px = u32::from(rows)
        .checked_mul(cell.height_px)
        .ok_or(SnapshotError::CanvasTooLarge)?;
    let canvas_pixels = u64::from(width_px) * u64::from(height_px);
    if canvas_pixels > MAX_CANVAS_PIXELS {
        return Err(SnapshotError::CanvasTooLarge);
    }
    let (canvas_w, canvas_h) = (u64::from(width_px), u64::from(height_px));
    let area = Rect::new(0, 0, cols, rows);

    let mut draws = Vec::new();
    let mut coverage = 0u64;
    for placement in placements {
        let bpp = match placement.format {
            KittyImageFormat::Rgb => 3,
            KittyImageFormat::Rgba => 4,
            // Encoded PNG only arrives with test-only PNG forwarding.
            KittyImageFormat::Png => continue,
        };
        let Some(data) = pixels.get(&placement.image_id) else {
            continue;
        };
        let image_width = placement.image_width as usize;
        let image_height = placement.image_height as usize;
        let needed = image_width
            .checked_mul(image_height)
            .and_then(|pixels| pixels.checked_mul(bpp));
        if needed.is_none_or(|needed| data.len() < needed) {
            continue;
        }
        let Some((clip, _)) = clip_geometry(&PlacementGeometry {
            area,
            cell_size: cell,
            placement,
        }) else {
            continue;
        };
        // Same rectangle `encode_display_placement` asks the host to fill.
        let dest_x = u64::from(clip.x) * u64::from(cell.width_px) + u64::from(clip.x_offset);
        let dest_y = u64::from(clip.y) * u64::from(cell.height_px) + u64::from(clip.y_offset);
        let dest_w = u64::from(clip.cols) * u64::from(cell.width_px);
        let dest_h = u64::from(clip.rows) * u64::from(cell.height_px);
        let x_end = (dest_x + dest_w).min(canvas_w);
        let y_end = (dest_y + dest_h).min(canvas_h);
        if dest_x >= x_end || dest_y >= y_end {
            continue;
        }
        coverage += (x_end - dest_x) * (y_end - dest_y);
        if coverage > MAX_CANVAS_COVERAGE * canvas_pixels {
            return Err(SnapshotError::TooMuchCoverage);
        }
        draws.push(Draw {
            clip,
            data,
            bpp,
            image_width,
            image_height,
            dest_x,
            dest_y,
            dest_w,
            dest_h,
            x_end,
            y_end,
        });
    }

    let mut rgba = vec![0u8; canvas_pixels as usize * 4];
    for draw in &draws {
        let clip = draw.clip;
        for y in draw.dest_y..draw.y_end {
            let sy = u64::from(clip.source_y)
                + (y - draw.dest_y) * u64::from(clip.source_height) / draw.dest_h;
            if sy >= draw.image_height as u64 {
                break;
            }
            for x in draw.dest_x..draw.x_end {
                let sx = u64::from(clip.source_x)
                    + (x - draw.dest_x) * u64::from(clip.source_width) / draw.dest_w;
                if sx >= draw.image_width as u64 {
                    break;
                }
                let src = (sy as usize * draw.image_width + sx as usize) * draw.bpp;
                let alpha = if draw.bpp == 4 {
                    draw.data[src + 3]
                } else {
                    255
                };
                let dst = (y * canvas_w + x) as usize * 4;
                blend(&mut rgba[dst..dst + 4], &draw.data[src..src + 3], alpha);
            }
        }
    }
    Ok(PaneSnapshot {
        width_px,
        height_px,
        placements: draws.len() as u32,
        rgba,
    })
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
    // Encoding runs on the server's request path; favour speed over size.
    encoder.set_compression(png::Compression::Fast);
    encoder.write_header()?.write_image_data(&snapshot.rgba)?;
    Ok(bytes)
}

#[cfg(test)]
pub(crate) mod tests {
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

    /// RGB pixels where red is the source x, green the source y, blue the tag.
    pub(crate) fn gradient(width: u32, height: u32, tag: u8) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| [x as u8, y as u8, tag]))
            .collect()
    }

    /// zlib stream with one stored block, as `o=z` senders like termium emit.
    pub(crate) fn zlib_stored(data: &[u8]) -> Vec<u8> {
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

    /// Transmits and places an image at 1-based cell (`row`, `col`) with extra
    /// control keys such as `z=1` or `c=2,r=2`.
    pub(crate) fn transmit(
        id: u32,
        (row, col): (u16, u16),
        (width, height): (u32, u32),
        pixels: &[u8],
        extra: &str,
    ) -> Vec<u8> {
        let format = if pixels.len() == (width * height * 3) as usize {
            24
        } else {
            32
        };
        let payload = base64::engine::general_purpose::STANDARD.encode(pixels);
        format!("\x1b[{row};{col}H\x1b_Ga=T,f={format},s={width},v={height},i={id},p=1,C=1{extra},q=2;{payload}\x1b\\")
            .into_bytes()
    }

    /// termium-style tile: zlib-compressed RGB gradient tagged with its id.
    pub(crate) fn tile(id: u32, cell: (u16, u16), size: (u32, u32)) -> Vec<u8> {
        let pixels = zlib_stored(&gradient(size.0, size.1, id as u8));
        let payload = base64::engine::general_purpose::STANDARD.encode(pixels);
        let (row, col) = cell;
        format!(
            "\x1b[{row};{col}H\x1b_Ga=T,f=24,o=z,s={},v={},i={id},p=1,C=1,q=2;{payload}\x1b\\",
            size.0, size.1
        )
        .into_bytes()
    }

    fn snapshot(terminal: &Terminal, cols: u16, rows: u16) -> Result<PaneSnapshot, SnapshotError> {
        let mut placements = terminal
            .kitty_image_placements_with_data_filter(needs_pixels())
            .unwrap();
        let pixels = image_pixels(&mut placements).unwrap();
        composite(&placements, &pixels, cols, rows, CELL)
    }

    /// Round-trips through PNG and returns a pixel reader.
    fn decoded(snapshot: &PaneSnapshot) -> impl Fn(u32, u32) -> [u8; 4] {
        let png = encode_png(snapshot).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut rgba = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut rgba).unwrap();
        let width = info.width;
        move |x, y| {
            let at = ((y * width + x) * 4) as usize;
            rgba[at..at + 4].try_into().unwrap()
        }
    }

    #[test]
    fn termium_tiles_land_on_their_cells() {
        let mut terminal = terminal(10, 4);
        terminal.write(&tile(1, (1, 1), (8, 8)));
        terminal.write(&tile(2, (3, 6), (8, 8)));

        let snapshot = snapshot(&terminal, 10, 4).unwrap();
        let px = decoded(&snapshot);

        assert_eq!((snapshot.width_px, snapshot.height_px), (40, 32));
        assert_eq!(snapshot.placements, 2);
        assert_eq!(px(0, 0), [0, 0, 1, 255]);
        assert_eq!(px(3, 5), [3, 5, 1, 255]);
        assert_eq!(px(7, 7), [7, 7, 1, 255]);
        assert_eq!(px(8, 0), [0, 0, 0, 0]);
        assert_eq!(px(20, 16), [0, 0, 2, 255]);
        assert_eq!(px(25, 18), [5, 2, 2, 255]);
        assert_eq!(px(19, 16), [0, 0, 0, 0]);
        assert_eq!(px(20, 24), [0, 0, 0, 0]);
    }

    #[test]
    fn scaled_placement_samples_nearest_source_pixel() {
        let mut terminal = terminal(4, 2);
        // A 4x8 image stretched over 2x2 cells (8x16 pixels).
        terminal.write(&transmit(1, (1, 1), (4, 8), &gradient(4, 8, 9), ",c=2,r=2"));

        let snapshot = snapshot(&terminal, 4, 2).unwrap();
        let px = decoded(&snapshot);

        assert_eq!(px(0, 0), [0, 0, 9, 255]);
        assert_eq!(px(2, 3), [1, 1, 9, 255]);
        assert_eq!(px(7, 15), [3, 7, 9, 255]);
        assert_eq!(px(8, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn source_rect_crops_the_image() {
        let mut terminal = terminal(4, 2);
        terminal.write(&transmit(
            1,
            (1, 1),
            (8, 16),
            &gradient(8, 16, 9),
            ",x=4,y=8,w=4,h=8",
        ));

        let snapshot = snapshot(&terminal, 4, 2).unwrap();
        let px = decoded(&snapshot);

        assert_eq!(px(0, 0), [4, 8, 9, 255]);
        assert_eq!(px(3, 7), [7, 15, 9, 255]);
        assert_eq!(px(4, 0), [0, 0, 0, 0]);
        assert_eq!(px(0, 8), [0, 0, 0, 0]);
    }

    #[test]
    fn rgba_blends_over_lower_z() {
        let mut terminal = terminal(4, 2);
        // Upper placement is written first so z order, not write order, decides.
        terminal.write(&transmit(
            2,
            (1, 1),
            (4, 8),
            &[0, 255, 0, 128].repeat(32),
            ",z=1",
        ));
        terminal.write(&transmit(1, (1, 1), (8, 8), &[255, 0, 0].repeat(64), ""));

        let snapshot = snapshot(&terminal, 4, 2).unwrap();
        let px = decoded(&snapshot);

        assert_eq!(snapshot.placements, 2);
        assert_eq!(px(0, 0), [127, 128, 0, 255]);
        assert_eq!(px(4, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn translucent_pixel_over_transparent_canvas_keeps_its_color() {
        let mut terminal = terminal(2, 1);
        terminal.write(&transmit(
            1,
            (1, 1),
            (4, 8),
            &[0, 0, 200, 64].repeat(32),
            "",
        ));

        let snapshot = snapshot(&terminal, 2, 1).unwrap();

        assert_eq!(decoded(&snapshot)(0, 0), [0, 0, 200, 64]);
    }

    #[test]
    fn placements_past_the_viewport_edges_are_clipped() {
        let mut terminal = terminal(4, 2);
        // Three cells wide and two rows tall from the last column and row.
        terminal.write(&transmit(1, (2, 4), (12, 16), &gradient(12, 16, 1), ""));
        let mut placements = terminal
            .kitty_image_placements_with_data_filter(needs_pixels())
            .unwrap();
        // The same geometry scrolled two cells left and one row up.
        let mut above = placements[0].clone();
        above.image_id = 2;
        above.render.viewport_col = -2;
        above.render.viewport_row = -1;
        placements.push(above);
        let mut pixels = image_pixels(&mut placements).unwrap();
        pixels.insert(2, gradient(12, 16, 2));

        let snapshot = composite(&placements, &pixels, 4, 2, CELL).unwrap();
        let px = decoded(&snapshot);

        assert_eq!(snapshot.placements, 2);
        assert_eq!(px(12, 8), [0, 0, 1, 255]);
        assert_eq!(px(15, 15), [3, 7, 1, 255]);
        assert_eq!(px(0, 0), [8, 8, 2, 255]);
        assert_eq!(px(3, 7), [11, 15, 2, 255]);
        assert_eq!(px(4, 0), [0, 0, 0, 0]);
        assert_eq!(px(11, 15), [0, 0, 0, 0]);
    }

    fn repeat_placements(terminal: &mut Terminal, copies: u32) {
        terminal.write(&transmit(1, (1, 1), (8, 8), &gradient(8, 8, 1), ""));
        for placement in 2..=copies {
            terminal
                .write(format!("\x1b[1;1H\x1b_Ga=p,i=1,p={placement},C=1,q=2\x1b\\").as_bytes());
        }
    }

    #[test]
    fn repeated_image_pixels_are_copied_once() {
        let mut terminal = terminal(2, 1);
        repeat_placements(&mut terminal, 5);

        let mut placements = terminal
            .kitty_image_placements_with_data_filter(needs_pixels())
            .unwrap();

        assert_eq!(placements.len(), 5);
        let copied = placements.iter().filter(|p| !p.data.is_empty()).count();
        assert_eq!(copied, 1);
        let pixels = image_pixels(&mut placements).unwrap();
        let snapshot = composite(&placements, &pixels, 2, 1, CELL).unwrap();
        assert_eq!(snapshot.placements, 5);
        assert_eq!(decoded(&snapshot)(7, 7), [7, 7, 1, 255]);
    }

    #[test]
    fn coverage_beyond_the_cap_is_rejected() {
        let mut terminal = self::terminal(2, 1);
        repeat_placements(&mut terminal, 8);
        assert_eq!(snapshot(&terminal, 2, 1).unwrap().placements, 8);

        let mut terminal = self::terminal(2, 1);
        repeat_placements(&mut terminal, 9);
        assert_eq!(
            snapshot(&terminal, 2, 1).err(),
            Some(SnapshotError::TooMuchCoverage)
        );
    }

    #[test]
    fn oversized_canvas_is_rejected_without_overflow() {
        let huge = HostCellSize {
            width_px: u32::MAX,
            height_px: u32::MAX,
        };
        let large = HostCellSize {
            width_px: 4096,
            height_px: 4096,
        };
        for cell in [huge, large] {
            assert_eq!(
                composite(&[], &HashMap::new(), 10, 4, cell).err(),
                Some(SnapshotError::CanvasTooLarge)
            );
        }
    }
}
