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
    fn mode_parse() {
        assert_eq!(RunMode::parse("dev"), Some(RunMode::Dev));
        assert_eq!(RunMode::parse("shell"), Some(RunMode::Shell));
        assert_eq!(RunMode::parse("SHELL"), None);
    }
}
