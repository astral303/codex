mod bel;
mod osc777;
mod osc9;

use std::io;

use bel::BelBackend;
use codex_config::types::NotificationMethod;
use codex_terminal_detection::Multiplexer;
use codex_terminal_detection::TerminalInfo;
use codex_terminal_detection::TerminalName;
use codex_terminal_detection::terminal_info;
use osc9::Osc9Backend;
use osc777::Osc777Backend;

#[derive(Debug)]
pub enum DesktopNotificationBackend {
    Osc9(Osc9Backend),
    Osc777(Osc777Backend),
    Bel(BelBackend),
}

impl DesktopNotificationBackend {
    pub fn for_method(method: NotificationMethod) -> Self {
        match method {
            NotificationMethod::Auto => Self::for_method(auto_method(&terminal_info())),
            NotificationMethod::Osc9 => Self::Osc9(Osc9Backend::new()),
            NotificationMethod::Osc777 => Self::Osc777(Osc777Backend::new()),
            NotificationMethod::Bel => Self::Bel(BelBackend),
        }
    }

    pub fn method(&self) -> NotificationMethod {
        match self {
            DesktopNotificationBackend::Osc9(_) => NotificationMethod::Osc9,
            DesktopNotificationBackend::Osc777(_) => NotificationMethod::Osc777,
            DesktopNotificationBackend::Bel(_) => NotificationMethod::Bel,
        }
    }

    /// OSC 9 and BEL ignore `session_title` because they have no title field.
    pub fn notify(&mut self, session_title: Option<&str>, message: &str) -> io::Result<()> {
        match self {
            DesktopNotificationBackend::Osc9(backend) => backend.notify(message),
            DesktopNotificationBackend::Osc777(backend) => backend.notify(session_title, message),
            DesktopNotificationBackend::Bel(backend) => backend.notify(message),
        }
    }
}

pub fn detect_backend(method: NotificationMethod) -> DesktopNotificationBackend {
    DesktopNotificationBackend::for_method(method)
}

/// OSC 777 has no capability query, and Windows Terminal ignores the sequence unless the profile
/// sets `compatibility.allowOSC777`. Select it only when the user configures it.
fn auto_method(terminal: &TerminalInfo) -> NotificationMethod {
    if supports_osc9(terminal) {
        NotificationMethod::Osc9
    } else {
        NotificationMethod::Bel
    }
}

fn uses_tmux_passthrough() -> bool {
    matches!(terminal_info().multiplexer, Some(Multiplexer::Tmux { .. }))
}

fn supports_osc9(terminal: &TerminalInfo) -> bool {
    matches!(
        terminal.name,
        TerminalName::Ghostty
            | TerminalName::Iterm2
            | TerminalName::Kitty
            | TerminalName::WarpTerminal
            | TerminalName::WezTerm
    )
}

#[cfg(test)]
mod tests {
    use super::auto_method;
    use super::detect_backend;
    use super::supports_osc9;
    use codex_config::types::NotificationMethod;
    use codex_terminal_detection::TerminalInfo;
    use codex_terminal_detection::TerminalName;
    use pretty_assertions::assert_eq;

    fn test_terminal(name: TerminalName) -> TerminalInfo {
        TerminalInfo {
            backend: None,
            name,
            term_program: None,
            version: None,
            term: None,
            multiplexer: None,
        }
    }

    #[test]
    fn selects_osc9_method() {
        assert!(matches!(
            detect_backend(NotificationMethod::Osc9),
            super::DesktopNotificationBackend::Osc9(_)
        ));
    }

    #[test]
    fn selects_osc777_method() {
        assert!(matches!(
            detect_backend(NotificationMethod::Osc777),
            super::DesktopNotificationBackend::Osc777(_)
        ));
    }

    #[test]
    fn auto_does_not_select_osc777_for_terminals_that_implement_it() {
        for (name, method) in [
            (TerminalName::WindowsTerminal, NotificationMethod::Bel),
            (TerminalName::Ghostty, NotificationMethod::Osc9),
            (TerminalName::WezTerm, NotificationMethod::Osc9),
        ] {
            assert_eq!(auto_method(&test_terminal(name)), method, "{name:?}");
        }
    }

    #[test]
    fn selects_bel_method() {
        assert!(matches!(
            detect_backend(NotificationMethod::Bel),
            super::DesktopNotificationBackend::Bel(_)
        ));
    }

    #[test]
    fn supports_osc9_for_supported_terminals() {
        for name in [
            TerminalName::Ghostty,
            TerminalName::Iterm2,
            TerminalName::Kitty,
            TerminalName::WarpTerminal,
            TerminalName::WezTerm,
        ] {
            assert!(
                supports_osc9(&test_terminal(name)),
                "{name:?} should support OSC 9"
            );
        }
    }

    #[test]
    fn supports_osc9_for_unsupported_terminals() {
        for name in [
            TerminalName::AppleTerminal,
            TerminalName::Alacritty,
            TerminalName::Dumb,
            TerminalName::GnomeTerminal,
            TerminalName::Konsole,
            TerminalName::Unknown,
            TerminalName::VsCode,
            TerminalName::Vte,
            TerminalName::WindowsTerminal,
        ] {
            assert_eq!(
                supports_osc9(&test_terminal(name)),
                false,
                "{name:?} should not support OSC 9"
            );
        }
    }
}
