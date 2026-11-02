use std::fmt;
use std::io;
use std::io::stdout;

use crossterm::Command;
use ratatui::crossterm::execute;

use super::uses_tmux_passthrough;

const APP_NAME: &str = "Codex";

#[derive(Debug)]
pub struct Osc777Backend {
    dcs_passthrough: bool,
}

impl Osc777Backend {
    pub fn new() -> Self {
        Self {
            dcs_passthrough: uses_tmux_passthrough(),
        }
    }

    pub fn notify(&mut self, session_title: Option<&str>, message: &str) -> io::Result<()> {
        execute!(
            stdout(),
            PostNotification {
                session_title: session_title.map(str::to_string),
                message: message.to_string(),
                dcs_passthrough: self.dcs_passthrough,
            }
        )
    }
}

#[derive(Debug, Clone)]
pub struct PostNotification {
    pub session_title: Option<String>,
    pub message: String,
    pub dcs_passthrough: bool,
}

impl Command for PostNotification {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        let title = notification_title(self.session_title.as_deref());
        let body = replace_control_characters(&self.message);
        if self.dcs_passthrough {
            write!(
                f,
                "\x1bPtmux;\x1b\x1b]777;notify;{title};{body}\x1b\x1b\\\x1b\\"
            )
        } else {
            write!(f, "\x1b]777;notify;{title};{body}\x1b\\")
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

/// Replace semicolons in a session title because the first one ends the title field and moves the
/// rest of the title into the body.
fn notification_title(session_title: Option<&str>) -> String {
    match session_title {
        Some(session_title) => {
            let session_title = replace_control_characters(session_title).replace(';', " ");
            format!("{APP_NAME}: {session_title}")
        }
        None => APP_NAME.to_string(),
    }
}

/// Replace control characters because messages quote commands and agent responses, and session
/// titles can hold any text. One of these characters can end the OSC sequence early and print the
/// rest of the sequence.
fn replace_control_characters(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

#[cfg(test)]
mod tests {
    use crossterm::Command;
    use pretty_assertions::assert_eq;

    use super::PostNotification;

    fn ansi_for(message: &str, dcs_passthrough: bool) -> String {
        ansi_with_session_title(/*session_title*/ None, message, dcs_passthrough)
    }

    fn ansi_with_session_title(
        session_title: Option<&str>,
        message: &str,
        dcs_passthrough: bool,
    ) -> String {
        let mut ansi = String::new();
        PostNotification {
            session_title: session_title.map(str::to_string),
            message: message.to_string(),
            dcs_passthrough,
        }
        .write_ansi(&mut ansi)
        .expect("OSC 777 command should format");
        ansi
    }

    #[test]
    fn post_notification_writes_plain_osc777_sequence() {
        assert_eq!(
            ansi_for(
                "Approval requested: cargo test",
                /*dcs_passthrough*/ false
            ),
            "\u{1b}]777;notify;Codex;Approval requested: cargo test\u{1b}\\"
        );
    }

    #[test]
    fn post_notification_writes_tmux_dcs_wrapped_osc777_sequence() {
        assert_eq!(
            ansi_for("done", /*dcs_passthrough*/ true),
            "\u{1b}Ptmux;\u{1b}\u{1b}]777;notify;Codex;done\u{1b}\u{1b}\\\u{1b}\\"
        );
    }

    #[test]
    fn post_notification_keeps_semicolons_in_the_body() {
        assert_eq!(
            ansi_for("cd src; ls", /*dcs_passthrough*/ false),
            "\u{1b}]777;notify;Codex;cd src; ls\u{1b}\\"
        );
    }

    #[test]
    fn post_notification_replaces_control_characters_in_the_body() {
        assert_eq!(
            ansi_for(
                "日本\u{1b}\\b\u{7}c\nd\u{9c}e",
                /*dcs_passthrough*/ false
            ),
            "\u{1b}]777;notify;Codex;日本 \\b c d e\u{1b}\\"
        );
    }

    #[test]
    fn post_notification_adds_the_session_title_to_the_title() {
        assert_eq!(
            ansi_with_session_title(
                Some("Fix undo grouping"),
                "done",
                /*dcs_passthrough*/ false
            ),
            "\u{1b}]777;notify;Codex: Fix undo grouping;done\u{1b}\\"
        );
    }

    #[test]
    fn post_notification_replaces_semicolons_and_control_characters_in_the_session_title() {
        assert_eq!(
            ansi_with_session_title(Some("a;b\u{1b}\\c"), "done", /*dcs_passthrough*/ false),
            "\u{1b}]777;notify;Codex: a b \\c;done\u{1b}\\"
        );
    }
}
