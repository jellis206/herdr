use std::io::{self, Write as _};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalNotificationBackend {
    Ghostty,
    Iterm2,
    Kitty,
    WezTerm,
}

pub fn detect_backend() -> Option<TerminalNotificationBackend> {
    let term_program = std::env::var("TERM_PROGRAM").ok();
    let term = std::env::var("TERM").ok();

    match term_program.as_deref() {
        Some("ghostty") => return Some(TerminalNotificationBackend::Ghostty),
        Some("iTerm.app") => return Some(TerminalNotificationBackend::Iterm2),
        Some("WezTerm") => return Some(TerminalNotificationBackend::WezTerm),
        _ => {}
    }

    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        return Some(TerminalNotificationBackend::Kitty);
    }

    match term.as_deref() {
        Some("xterm-ghostty") => Some(TerminalNotificationBackend::Ghostty),
        Some("xterm-kitty") => Some(TerminalNotificationBackend::Kitty),
        Some(term) if term.contains("wezterm") => Some(TerminalNotificationBackend::WezTerm),
        _ => None,
    }
}

pub fn show_notification(
    title: &str,
    body: Option<&str>,
    show_native_notification: impl FnOnce(&str, Option<&str>) -> io::Result<bool>,
) -> io::Result<bool> {
    show_notification_for_backend(detect_backend(), title, body, show_native_notification)
}

fn show_notification_for_backend(
    backend: Option<TerminalNotificationBackend>,
    title: &str,
    body: Option<&str>,
    show_native_notification: impl FnOnce(&str, Option<&str>) -> io::Result<bool>,
) -> io::Result<bool> {
    let Some(backend) = backend else {
        return Ok(false);
    };

    let sequence = match backend {
        TerminalNotificationBackend::Ghostty
        | TerminalNotificationBackend::Iterm2
        | TerminalNotificationBackend::WezTerm => {
            // OSC 9 cannot request silence; keep all audio in Herdr's sound player.
            return show_native_notification(title, body);
        }
        TerminalNotificationBackend::Kitty => build_osc99_notification(title, body),
    };

    let sequence = if std::env::var_os("TMUX").is_some() {
        wrap_tmux_passthrough(&sequence)
    } else {
        sequence
    };

    let mut stdout = io::stdout();
    stdout.write_all(&sequence)?;
    stdout.flush()?;
    Ok(true)
}

pub fn split_message(message: &str) -> (&str, Option<&str>) {
    match message.split_once(": ") {
        Some((title, body)) if !title.is_empty() && !body.is_empty() => (title, Some(body)),
        _ => (message, None),
    }
}

fn build_osc99_notification(title: &str, body: Option<&str>) -> Vec<u8> {
    let title = sanitize_text(title);
    match body {
        Some(body) if !body.is_empty() => {
            let body = sanitize_text(body);
            format!("\x1b]99;i=1:d=0:s=c2lsZW50;{title}\x1b\\\x1b]99;i=1:p=body;{body}\x1b\\")
                .into_bytes()
        }
        _ => format!("\x1b]99;s=c2lsZW50;{title}\x1b\\").into_bytes(),
    }
}

fn sanitize_text(text: impl AsRef<str>) -> String {
    text.as_ref()
        .chars()
        .filter(|ch| *ch != '\u{1b}' && *ch != '\u{7}' && *ch != '\u{9c}')
        .map(|ch| match ch {
            '\n' | '\r' | '\t' => ' ',
            _ => ch,
        })
        .collect()
}

fn wrap_tmux_passthrough(sequence: &[u8]) -> Vec<u8> {
    let mut wrapped = Vec::with_capacity(sequence.len() + 16);
    wrapped.extend_from_slice(b"\x1bPtmux;");
    for &byte in sequence {
        if byte == 0x1b {
            wrapped.push(0x1b);
        }
        wrapped.push(byte);
    }
    wrapped.extend_from_slice(b"\x1b\\");
    wrapped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_message_splits_title_and_body() {
        assert_eq!(
            split_message("agent done: ws · 1"),
            ("agent done", Some("ws · 1"))
        );
    }

    #[test]
    fn split_message_leaves_plain_message_alone() {
        assert_eq!(split_message("agent done"), ("agent done", None));
    }

    #[test]
    fn sanitize_text_strips_control_bytes() {
        assert_eq!(sanitize_text("a\n\tb\u{1b}c\u{7}"), "a  bc");
    }

    #[test]
    fn kitty_notification_uses_structured_title_and_body() {
        let sequence = String::from_utf8(build_osc99_notification("pi finished", Some("ws · 1")))
            .expect("utf8");
        assert!(sequence.contains("]99;i=1:d=0:s=c2lsZW50;pi finished"));
        assert!(sequence.contains("]99;i=1:p=body;ws · 1"));
    }

    #[test]
    fn osc9_backends_use_native_notifications_instead_of_uncontrolled_audio() {
        for backend in [
            TerminalNotificationBackend::Ghostty,
            TerminalNotificationBackend::Iterm2,
            TerminalNotificationBackend::WezTerm,
        ] {
            let mut called = false;
            let shown = show_notification_for_backend(
                Some(backend),
                "done",
                Some("workspace"),
                |title, body| {
                    called = true;
                    assert_eq!((title, body), ("done", Some("workspace")));
                    Ok(true)
                },
            )
            .unwrap();
            assert!(shown && called);
        }
        assert!(!show_notification_for_backend(None, "done", None, |_, _| {
            panic!("unsupported terminals must not emit popups")
        })
        .unwrap());
    }

    #[test]
    fn kitty_title_only_notification_is_silent() {
        assert_eq!(
            build_osc99_notification("done", None),
            b"\x1b]99;s=c2lsZW50;done\x1b\\"
        );
    }

    #[test]
    fn tmux_passthrough_wraps_and_escapes() {
        let wrapped = wrap_tmux_passthrough(b"\x1b]9;hi\x1b\\");
        assert_eq!(wrapped, b"\x1bPtmux;\x1b\x1b]9;hi\x1b\x1b\\\x1b\\");
    }
}
