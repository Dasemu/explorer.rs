// SPDX-License-Identifier: GPL-3.0-or-later
//! Names shared between processes.
//!
//! Inter-process communication is deliberately minimal: components find each
//! other by window class name and talk with registered window messages. There
//! is no shared memory and no long-lived pipe, so a crashed peer can never
//! wedge another component; a message to a missing window is simply dropped.

/// Window class of the launcher's (hidden while idle) top-level window.
pub const LAUNCHER_WINDOW_CLASS: &str = "ExplorerRs.Launcher";
/// Window class of each taskbar AppBar window (one per monitor).
pub const TASKBAR_WINDOW_CLASS: &str = "ExplorerRs.Taskbar";

/// Registered message (RegisterWindowMessageW) posted to the launcher window
/// to toggle its visibility. `wParam` = 0 toggle, 1 show, 2 hide.
pub const MSG_LAUNCHER_COMMAND: &str = "ExplorerRs.LauncherCommand";

/// Registered message asking an explorer.rs process to stop gracefully.
///
/// - Components (taskbar, launcher, desktop) handle it in the shared window
///   procedure and exit with code 0 (an intentional stop, not restarted).
/// - The supervisor and the guardian handle it on their session window
///   (guardian: its `ExplorerRs.Hotkeys` window): they stop their children
///   gracefully and exit; the guardian records a clean end in the crash log.
///
/// Used instead of `WM_CLOSE`, which components now ignore (any program can
/// send `WM_CLOSE`, and a destroyed main window means a restart).
pub const MSG_SHUTDOWN: &str = "ExplorerRs.Shutdown";

/// Registered message posted to the guardian's `ExplorerRs.Hotkeys` window
/// by `--restore-explorer`: "Windows Explorer was restored as the shell",
/// so the guardian (the only crash-log writer while it runs) records it.
pub const MSG_RESTORED: &str = "ExplorerRs.Restored";

/// Window class of the guardian's message-only hotkey window. Its owner
/// process is the guardian.
pub const GUARDIAN_WINDOW_CLASS: &str = "ExplorerRs.Hotkeys";

/// First private message (`WM_APP`).
pub const WM_APP_FIRST: u32 = 0x8000;
/// First registered (`RegisterWindowMessageW`) message.
pub const REGISTERED_FIRST: u32 = 0xC000;
/// At most this many messages are deferred per window while its handler is
/// busy (see [`deferrable_while_busy`]).
pub const MAX_DEFERRED: usize = 256;

/// A message that reached a window while its handler was already running
/// (re-entrancy, e.g. a modal popup menu inside the handler). Private
/// `WM_APP` messages and registered messages that were *posted* (not sent:
/// a sender waits for a result) are kept and handled once the handler
/// returns, instead of being dropped. System messages keep falling back to
/// `DefWindowProcW`.
pub fn deferrable_while_busy(msg: u32, sent: bool) -> bool {
    !sent && msg >= WM_APP_FIRST
}

/// `wParam` values for [`MSG_LAUNCHER_COMMAND`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum LauncherCommand {
    Toggle = 0,
    Show = 1,
    Hide = 2,
}

impl LauncherCommand {
    pub fn from_wparam(w: usize) -> Option<Self> {
        match w {
            0 => Some(Self::Toggle),
            1 => Some(Self::Show),
            2 => Some(Self::Hide),
            _ => None,
        }
    }
}

/// Environment variable the supervisor sets on children so they know they run
/// supervised and in which mode ("dev" or "shell").
pub const MODE_ENV: &str = "EXPLORER_RS_MODE";

/// Run mode of the whole shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Runs alongside explorer.exe as normal windows / AppBar. Safe default.
    Dev,
    /// Runs as the user's shell (registered via scripts/install-shell.ps1).
    Shell,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Shell => "shell",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "dev" => Some(Self::Dev),
            "shell" => Some(Self::Shell),
            _ => None,
        }
    }

    /// Read the mode from the environment, defaulting to [`RunMode::Dev`].
    /// Defaulting to dev is a safety property: a component started by hand
    /// never tries to take over shell duties.
    pub fn from_env() -> Self {
        std::env::var(MODE_ENV)
            .ok()
            .and_then(|s| Self::parse(&s))
            .unwrap_or(Self::Dev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_roundtrip() {
        for c in [
            LauncherCommand::Toggle,
            LauncherCommand::Show,
            LauncherCommand::Hide,
        ] {
            assert_eq!(LauncherCommand::from_wparam(c as usize), Some(c));
        }
        assert_eq!(LauncherCommand::from_wparam(9), None);
    }

    #[test]
    fn deferral_rules() {
        // WM_APP range and registered messages, when posted.
        assert!(deferrable_while_busy(WM_APP_FIRST, false));
        assert!(deferrable_while_busy(0xBFFF, false));
        assert!(deferrable_while_busy(REGISTERED_FIRST, false));
        assert!(deferrable_while_busy(0xFFFF, false));
        // Sent messages and system messages are never deferred.
        assert!(!deferrable_while_busy(WM_APP_FIRST + 1, true));
        assert!(!deferrable_while_busy(0x0010 /* WM_CLOSE */, false));
        assert!(!deferrable_while_busy(0x0400 /* WM_USER */, false));
        assert!(!deferrable_while_busy(0x7FFF, false));
    }

    #[test]
    fn mode_parse() {
        assert_eq!(RunMode::parse("dev"), Some(RunMode::Dev));
        assert_eq!(RunMode::parse("shell"), Some(RunMode::Shell));
        assert_eq!(RunMode::parse("SHELL"), None);
    }
}
