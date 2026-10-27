use std::io::{self, Write};

pub(crate) fn terminal_bell_sound(
    count: u16,
    config: &crate::config::SoundConfig,
) -> Option<crate::sound::Sound> {
    // Coalesce a PTY batch into one sound rather than starting a player per BEL byte.
    (count > 0 && config.enabled).then_some(crate::sound::Sound::Request)
}

pub(crate) fn write_window_title<W: Write>(writer: &mut W, title: Option<&str>) -> io::Result<()> {
    let title = title.unwrap_or("herdr");
    let safe_title = title
        .chars()
        .filter(|ch| !matches!(*ch, '\u{1b}' | '\u{7}' | '\u{9c}'))
        .collect::<String>();
    write!(writer, "\x1b]0;{safe_title}\x07")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_bells_use_notification_sound_and_respect_mute() {
        let mut config = crate::config::SoundConfig::default();
        assert_eq!(
            terminal_bell_sound(130, &config),
            Some(crate::sound::Sound::Request)
        );
        assert_eq!(terminal_bell_sound(0, &config), None);
        config.enabled = false;
        assert_eq!(terminal_bell_sound(1, &config), None);
    }

    #[test]
    fn window_title_strips_terminators_and_defaults_to_herdr() {
        let mut output = Vec::new();
        write_window_title(&mut output, Some("herdr\x1b api\u{7}\u{9c}")).unwrap();
        assert_eq!(output, b"\x1b]0;herdr api\x07");

        output.clear();
        write_window_title(&mut output, None).unwrap();
        assert_eq!(output, b"\x1b]0;herdr\x07");
    }
}
