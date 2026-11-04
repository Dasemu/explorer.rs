// SPDX-License-Identifier: GPL-3.0-or-later
//! Rules deciding which top-level windows get a taskbar button.
//!
//! The Win32 layer collects a [`WindowInfo`] snapshot per window; this module
//! decides. Each decision carries a [`Reason`] so `--dump-windows` can explain
//! why a window is (not) shown — the main tool for debugging filter reports
//! from real machines.
//!
//! The rules follow the documented behaviour of the Windows taskbar
//! ("Managing Taskbar Buttons" on MSDN) plus the well-known UWP quirks:
//! - a visible, unowned window without `WS_EX_TOOLWINDOW` gets a button;
//! - `WS_EX_APPWINDOW` forces a button even for owned windows;
//! - cloaked windows (DWM) are hidden: this covers suspended UWP apps, windows
//!   on other virtual desktops and "ghost" ApplicationFrameWindows.

/// Window style bits (subset of WinUser.h) used by the filter.
pub mod style {
    pub const WS_CHILD: u32 = 0x4000_0000;
    pub const WS_VISIBLE: u32 = 0x1000_0000;
    pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    pub const WS_EX_APPWINDOW: u32 = 0x0004_0000;
    pub const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
}

/// Class of the frame window hosting UWP / WinUI apps.
pub const UWP_FRAME_CLASS: &str = "ApplicationFrameWindow";
/// Class of the actual UWP content window (child of the frame when running).
pub const UWP_CORE_CLASS: &str = "Windows.UI.Core.CoreWindow";

/// Shell-owned classes that must never be shown, regardless of style.
pub const ALWAYS_HIDDEN_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "Windows.UI.Core.CoreWindow",
    crate::ipc::TASKBAR_WINDOW_CLASS,
    crate::ipc::LAUNCHER_WINDOW_CLASS,
];

/// Snapshot of the properties of a top-level window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowInfo {
    /// HWND as an integer (opaque here).
    pub hwnd: u64,
    pub title: String,
    pub class_name: String,
    pub style: u32,
    pub ex_style: u32,
    /// `GetWindow(GW_OWNER)` is non-null.
    pub has_owner: bool,
    /// Value of `DWMWA_CLOAKED` (0 = not cloaked).
    pub cloaked: u32,
    /// Executable file name of the owning process, e.g. `notepad.exe`.
    pub process_name: Option<String>,
    /// For [`UWP_FRAME_CLASS`] windows: whether a [`UWP_CORE_CLASS`] child
    /// exists. `None` when not checked.
    pub uwp_has_core_child: Option<bool>,
}

/// Outcome of filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Shown,
    ShownAppWindow,
    NotVisible,
    Child,
    Cloaked,
    ToolWindow,
    NoActivate,
    Owned,
    EmptyTitle,
    ShellClass,
    UwpGhostFrame,
    ExcludedByConfig,
}

impl Reason {
    pub fn is_shown(self) -> bool {
        matches!(self, Reason::Shown | Reason::ShownAppWindow)
    }
}

/// User-configurable exclusions on top of the built-in rules.
#[derive(Debug, Clone, Default)]
pub struct FilterRules {
    pub exclude_classes: Vec<String>,
    /// Compared case-insensitively against [`WindowInfo::process_name`].
    pub exclude_processes: Vec<String>,
}

impl FilterRules {
    pub fn from_config(c: &crate::config::TaskbarConfig) -> Self {
        Self {
            exclude_classes: c.exclude_classes.clone(),
            exclude_processes: c.exclude_processes.clone(),
        }
    }

    /// Decide whether `w` should have a taskbar button, and why.
    pub fn classify(&self, w: &WindowInfo) -> Reason {
        use style::*;
        if w.style & WS_VISIBLE == 0 {
            return Reason::NotVisible;
        }
        if w.style & WS_CHILD != 0 {
            return Reason::Child;
        }
        if ALWAYS_HIDDEN_CLASSES.contains(&w.class_name.as_str()) {
            return Reason::ShellClass;
        }
        if self.exclude_classes.iter().any(|c| c == &w.class_name) {
            return Reason::ExcludedByConfig;
        }
        if let Some(p) = &w.process_name {
            if self
                .exclude_processes
                .iter()
                .any(|e| e.eq_ignore_ascii_case(p))
            {
                return Reason::ExcludedByConfig;
            }
        }
        if w.cloaked != 0 {
            return Reason::Cloaked;
        }
        if w.class_name == UWP_FRAME_CLASS && w.uwp_has_core_child == Some(false) {
            // A frame without content is the classic "ghost" UWP window
            // (e.g. Settings after it was suspended but not yet cloaked).
            return Reason::UwpGhostFrame;
        }
        let app_window = w.ex_style & WS_EX_APPWINDOW != 0;
        if app_window {
            // Explicit opt-in wins over tool/owned rules, but an untitled
            // window is still noise.
            if w.title.trim().is_empty() {
                return Reason::EmptyTitle;
            }
            return Reason::ShownAppWindow;
        }
        if w.ex_style & WS_EX_TOOLWINDOW != 0 {
            return Reason::ToolWindow;
        }
        if w.ex_style & WS_EX_NOACTIVATE != 0 {
            return Reason::NoActivate;
        }
        if w.has_owner {
            return Reason::Owned;
        }
        if w.title.trim().is_empty() {
            return Reason::EmptyTitle;
        }
        Reason::Shown
    }

    pub fn should_show(&self, w: &WindowInfo) -> bool {
        self.classify(w).is_shown()
    }
}

#[cfg(test)]
mod tests {
    use super::style::*;
    use super::*;

    fn normal(title: &str) -> WindowInfo {
        WindowInfo {
            hwnd: 1,
            title: title.into(),
            class_name: "Notepad".into(),
            style: WS_VISIBLE,
            process_name: Some("notepad.exe".into()),
            ..Default::default()
        }
    }

    fn rules() -> FilterRules {
        FilterRules::default()
    }

    #[test]
    fn plain_window_is_shown() {
        assert_eq!(rules().classify(&normal("a.txt - Notepad")), Reason::Shown);
    }

    #[test]
    fn invisible_child_tool_hidden() {
        let mut w = normal("x");
        w.style = 0;
        assert_eq!(rules().classify(&w), Reason::NotVisible);
        let mut w = normal("x");
        w.style |= WS_CHILD;
        assert_eq!(rules().classify(&w), Reason::Child);
        let mut w = normal("x");
        w.ex_style = WS_EX_TOOLWINDOW;
        assert_eq!(rules().classify(&w), Reason::ToolWindow);
        let mut w = normal("x");
        w.ex_style = WS_EX_NOACTIVATE;
        assert_eq!(rules().classify(&w), Reason::NoActivate);
    }

    #[test]
    fn owned_hidden_unless_appwindow() {
        let mut w = normal("Find");
        w.has_owner = true;
        assert_eq!(rules().classify(&w), Reason::Owned);
        w.ex_style = WS_EX_APPWINDOW;
        assert_eq!(rules().classify(&w), Reason::ShownAppWindow);
    }

    #[test]
    fn appwindow_beats_toolwindow() {
        let mut w = normal("x");
        w.ex_style = WS_EX_APPWINDOW | WS_EX_TOOLWINDOW;
        assert!(rules().should_show(&w));
    }

    #[test]
    fn cloaked_hidden_even_with_appwindow() {
        let mut w = normal("Settings");
        w.ex_style = WS_EX_APPWINDOW;
        w.cloaked = 2; // DWM_CLOAKED_SHELL
        assert_eq!(rules().classify(&w), Reason::Cloaked);
    }

    #[test]
    fn uwp_ghost_frames() {
        let mut w = normal("Settings");
        w.class_name = UWP_FRAME_CLASS.into();
        w.uwp_has_core_child = Some(false);
        assert_eq!(rules().classify(&w), Reason::UwpGhostFrame);
        w.uwp_has_core_child = Some(true);
        assert!(rules().should_show(&w));
        w.uwp_has_core_child = None;
        assert!(rules().should_show(&w));
    }

    #[test]
    fn shell_classes_and_empty_titles() {
        let mut w = normal("Program Manager");
        w.class_name = "Progman".into();
        assert_eq!(rules().classify(&w), Reason::ShellClass);
        assert_eq!(rules().classify(&normal("   ")), Reason::EmptyTitle);
    }

    #[test]
    fn config_exclusions() {
        let r = FilterRules {
            exclude_classes: vec!["Notepad".into()],
            exclude_processes: vec![],
        };
        assert_eq!(r.classify(&normal("x")), Reason::ExcludedByConfig);
        let r = FilterRules {
            exclude_classes: vec![],
            exclude_processes: vec!["NOTEPAD.EXE".into()],
        };
        assert_eq!(r.classify(&normal("x")), Reason::ExcludedByConfig);
    }
}
