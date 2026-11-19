// SPDX-License-Identifier: GPL-3.0-or-later
//! Is the foreground window a full-screen app (game, video, slideshow)?
//!
//! Windows' own AppBar manager tells appbars with `ABN_FULLSCREENAPP`, but
//! that manager lives in explorer.exe: with explorer.rs as the shell nobody
//! sends it, and the taskbar stayed on top of full-screen games. The
//! taskbar therefore checks the foreground window itself: a window that
//! covers its whole monitor (not just the work area) is full screen, unless
//! it is a desktop or taskbar window, which always do.

/// Screen rectangle in physical pixels (`RECT` layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Window classes that cover a monitor without being a full-screen app.
pub const NEVER_FULLSCREEN_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "ExplorerRs.Desktop",
    "ExplorerRs.Taskbar",
    "ExplorerRs.Launcher",
];

/// `win` covers all of `monitor`.
pub fn covers(win: ScreenRect, monitor: ScreenRect) -> bool {
    monitor.right > monitor.left
        && monitor.bottom > monitor.top
        && win.left <= monitor.left
        && win.top <= monitor.top
        && win.right >= monitor.right
        && win.bottom >= monitor.bottom
}

/// Whether a foreground window of class `class` with rectangle `win` is a
/// full-screen app on `monitor`.
pub fn is_fullscreen(class: &str, win: ScreenRect, monitor: ScreenRect) -> bool {
    !NEVER_FULLSCREEN_CLASSES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(class))
        && covers(win, monitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(left: i32, top: i32, right: i32, bottom: i32) -> ScreenRect {
        ScreenRect {
            left,
            top,
            right,
            bottom,
        }
    }

    const MON: ScreenRect = ScreenRect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

    #[test]
    fn games_and_videos_are_fullscreen() {
        // Borderless / exclusive full screen: exactly the monitor.
        assert!(is_fullscreen("UnityWndClass", MON, MON));
        // Some games overshoot by their (invisible) borders.
        assert!(is_fullscreen("SDL_app", r(-8, -8, 1928, 1088), MON));
        // On a second monitor.
        let right = r(1920, 0, 4480, 1440);
        assert!(is_fullscreen("Chrome_WidgetWin_1", right, right));
        assert!(!is_fullscreen("Chrome_WidgetWin_1", right, MON));
    }

    #[test]
    fn maximized_and_desktop_windows_are_not() {
        // Maximized: the work area leaves the taskbar uncovered.
        assert!(!is_fullscreen("Notepad", r(-8, -8, 1928, 1040), MON));
        assert!(!is_fullscreen("Notepad", r(100, 100, 800, 600), MON));
        for class in ["Progman", "workerw", "ExplorerRs.Desktop", "Shell_TrayWnd"] {
            assert!(!is_fullscreen(class, MON, MON), "{class}");
        }
        // A degenerate monitor never counts.
        assert!(!covers(MON, r(0, 0, 0, 0)));
    }
}
