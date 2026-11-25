//! Writes terminal desktop notifications, wrapping the OSC sequence for tmux when needed.

use std::fmt;
use std::io;
use std::io::stdout;

use codex_terminal_detection::Multiplexer;
use codex_terminal_detection::TerminalName;
use codex_terminal_detection::terminal_info;
use crossterm::Command;
use ratatui::crossterm::execute;

// Ghostty copies the parsed notification title and body into 63-byte and 255-byte buffers.
const GHOSTTY_TITLE_BYTES: usize = 63;
const GHOSTTY_BODY_BYTES: usize = 255;
const APP_NAME: &str = "Codex";

#[derive(Debug, Clone, Copy)]
pub(super) enum OscProtocol {
    Osc9,
    Osc777,
}

#[derive(Debug)]
pub struct OscBackend {
    pub(super) protocol: OscProtocol,
    terminal_name: TerminalName,
    dcs_passthrough: bool,
}

impl OscBackend {
    pub(super) fn new(protocol: OscProtocol) -> Self {
        let terminal = terminal_info();
        Self {
            protocol,
            terminal_name: terminal.name,
            dcs_passthrough: matches!(terminal.multiplexer, Some(Multiplexer::Tmux { .. })),
        }
    }

    pub fn notify(&mut self, session_title: Option<&str>, message: &str) -> io::Result<()> {
        execute!(
            stdout(),
            PostNotification {
                protocol: self.protocol,
                session_title,
                message,
                terminal_name: self.terminal_name,
                dcs_passthrough: self.dcs_passthrough,
            }
        )
    }
}

struct PostNotification<'a> {
    protocol: OscProtocol,
    session_title: Option<&'a str>,
    message: &'a str,
    terminal_name: TerminalName,
    dcs_passthrough: bool,
}

impl Command for PostNotification<'_> {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        let body = self.format_field(self.message, GHOSTTY_BODY_BYTES);
        let sequence = match self.protocol {
            OscProtocol::Osc9 => format!("\x1b]9;{body}\x07"),
            OscProtocol::Osc777 => {
                let title = match self.session_title {
                    Some(session_title) => format!("{APP_NAME}: {session_title}"),
                    None => APP_NAME.to_string(),
                };
                let title = self.format_field(&title, GHOSTTY_TITLE_BYTES);
                format!("\x1b]777;notify;{title};{body}\x1b\\")
            }
        };
        if self.dcs_passthrough {
            let escaped = sequence.replace('\x1b', "\x1b\x1b");
            write!(f, "\x1bPtmux;{escaped}\x1b\\")
        } else {
            f.write_str(&sequence)
        }
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(std::io::Error::other(
            "tried to execute PostNotification using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

impl PostNotification<'_> {
    fn format_field(&self, text: &str, ghostty_field_bytes: usize) -> String {
        let mut field: String = text
            .chars()
            .map(|c| match c {
                // ASCII semicolons delimit OSC 777 fields.
                ';' if matches!(self.protocol, OscProtocol::Osc777) => '；',
                c if c.is_control() => ' ',
                c => c,
            })
            .collect();
        if self.terminal_name == TerminalName::Ghostty && field.len() > ghostty_field_bytes {
            field.truncate(field.floor_char_boundary(ghostty_field_bytes - '…'.len_utf8()));
            field.push('…');
        }
        field
    }
}

#[cfg(test)]
mod tests {
    use codex_terminal_detection::TerminalName;
    use crossterm::Command;
    use pretty_assertions::assert_eq;

    use super::OscProtocol;
    use super::PostNotification;

    #[test]
    fn post_notification_writes_plain_osc9_sequence() {
        let message = "a".repeat(2048);
        let mut ansi = String::new();
        let command = PostNotification {
            protocol: OscProtocol::Osc9,
            session_title: None,
            message: &message,
            terminal_name: TerminalName::Iterm2,
            dcs_passthrough: false,
        };

        command
            .write_ansi(&mut ansi)
            .expect("OSC 9 command should format");

        assert_eq!(ansi, format!("\u{1b}]9;{message}\u{7}"));
    }

    #[test]
    fn long_approval_in_ghostty_fits_the_notification_body() {
        // The next UTF-8 character crosses the cutoff for the ellipsis.
        let prefix = format!("Approval requested: {}", "a".repeat(231));
        let message = format!("{prefix}🦀{}", "b".repeat(2048));
        for (protocol, expected) in [
            (OscProtocol::Osc9, format!("\x1b]9;{prefix}…\x07")),
            (
                OscProtocol::Osc777,
                format!("\x1b]777;notify;Codex;{prefix}…\x1b\\"),
            ),
        ] {
            let mut ansi = String::new();
            PostNotification {
                protocol,
                session_title: None,
                message: &message,
                terminal_name: TerminalName::Ghostty,
                dcs_passthrough: false,
            }
            .write_ansi(&mut ansi)
            .expect("format Ghostty notification");
            assert_eq!(ansi, expected);
        }
    }

    #[test]
    fn post_notification_writes_tmux_dcs_wrapped_osc9_sequence() {
        let mut ansi = String::new();
        let command = PostNotification {
            protocol: OscProtocol::Osc9,
            session_title: None,
            message: "done",
            terminal_name: TerminalName::Iterm2,
            dcs_passthrough: true,
        };

        command
            .write_ansi(&mut ansi)
            .expect("OSC 9 command should format");

        assert_eq!(ansi, "\u{1b}Ptmux;\u{1b}\u{1b}]9;done\u{7}\u{1b}\\");
    }

    #[test]
    fn post_notification_keeps_control_characters_out_of_osc9_body() {
        let mut ansi = String::new();
        let command = PostNotification {
            protocol: OscProtocol::Osc9,
            session_title: None,
            message: "before\x07\x1bafter",
            terminal_name: TerminalName::Iterm2,
            dcs_passthrough: true,
        };

        command
            .write_ansi(&mut ansi)
            .expect("OSC 9 command should format");

        assert_eq!(
            ansi,
            "\u{1b}Ptmux;\u{1b}\u{1b}]9;before  after\u{7}\u{1b}\\"
        );
    }

    #[test]
    fn osc777_sends_title_and_plain_text_body_directly_and_through_tmux() {
        let mut sequences = Vec::new();
        for dcs_passthrough in [false, true] {
            let command = PostNotification {
                protocol: OscProtocol::Osc777,
                session_title: None,
                message: "Approval requested: cargo test; git clean\x07\x1b\u{009c}-fd",
                terminal_name: TerminalName::WindowsTerminal,
                dcs_passthrough,
            };
            let mut ansi = String::new();
            command.write_ansi(&mut ansi).expect("format OSC 777");
            sequences.push(ansi);
        }
        insta::assert_debug_snapshot!(sequences, @r#"
        [
            "\u{1b}]777;notify;Codex;Approval requested: cargo test； git clean   -fd\u{1b}\\",
            "\u{1b}Ptmux;\u{1b}\u{1b}]777;notify;Codex;Approval requested: cargo test； git clean   -fd\u{1b}\u{1b}\\\u{1b}\\",
        ]
        "#);
    }

    #[test]
    fn osc777_title_names_the_session() {
        let mut ansi = String::new();
        PostNotification {
            protocol: OscProtocol::Osc777,
            session_title: Some("fix; deploy\x07"),
            message: "done",
            terminal_name: TerminalName::WindowsTerminal,
            dcs_passthrough: false,
        }
        .write_ansi(&mut ansi)
        .expect("format OSC 777");

        assert_eq!(ansi, "\x1b]777;notify;Codex: fix； deploy ;done\x1b\\");
    }

    #[test]
    fn long_session_title_in_ghostty_fits_the_notification_title() {
        // The next UTF-8 character crosses the cutoff for the ellipsis.
        let session_title = format!("{}🦀{}", "a".repeat(52), "b".repeat(64));
        let mut ansi = String::new();
        PostNotification {
            protocol: OscProtocol::Osc777,
            session_title: Some(&session_title),
            message: "done",
            terminal_name: TerminalName::Ghostty,
            dcs_passthrough: false,
        }
        .write_ansi(&mut ansi)
        .expect("format Ghostty notification");

        assert_eq!(
            ansi,
            format!("\x1b]777;notify;Codex: {}…;done\x1b\\", "a".repeat(52))
        );
    }
}
