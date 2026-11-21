// SPDX-License-Identifier: GPL-3.0-or-later
//! Which monitors show a full-screen app (game, video, slideshow)?
//!
//! Windows' own AppBar manager tells appbars with `ABN_FULLSCREENAPP`, but
//! that manager lives in explorer.exe: with explorer.rs as the shell nobody
//! sends it, and the taskbar stayed on top of full-screen games. The
//! taskbar therefore checks itself: a window that covers its whole monitor
//! (not just the work area) is full screen, unless it is a desktop or
//! taskbar window, which always do.
//!
//! It is decided per monitor from the Z order, not from the foreground
//! window: a full-screen video on one monitor stays full screen while the
//! user works in an app on another one ([`fullscreen_per_monitor`]).

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

/// A visible, non-minimized top-level window of another process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: u64,
    /// Monitor the window is on (mostly).
    pub monitor: isize,
    pub rect: ScreenRect,
    pub class: String,
    /// Always-on-top, tool window or click-through (overlays, widgets,
    /// notifications): such windows never end a monitor's full-screen
    /// state, but can be full screen themselves.
    pub overlay: bool,
}

/// For each monitor, the full-screen window shown on it, if any.
/// `windows` are in Z order (top first); `monitors` are (id, rectangle).
///
/// Walking down the Z order, the first window on a monitor that covers it
/// makes that monitor full screen; the first ordinary window on it that
/// does not, makes it not full screen. Overlays and desktop/taskbar windows
/// decide nothing.
pub fn fullscreen_per_monitor(
    windows: &[Candidate],
    monitors: &[(isize, ScreenRect)],
) -> Vec<(isize, u64)> {
    let mut decided: Vec<isize> = Vec::new();
    let mut out = Vec::new();
    for w in windows {
        if decided.len() == monitors.len() {
            break;
        }
        if decided.contains(&w.monitor)
            || NEVER_FULLSCREEN_CLASSES
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&w.class))
        {
            continue;
        }
        let Some(&(_, mon)) = monitors.iter().find(|(id, _)| *id == w.monitor) else {
            continue;
        };
        if covers(w.rect, mon) {
            out.push((w.monitor, w.id));
            decided.push(w.monitor);
        } else if !w.overlay {
            decided.push(w.monitor);
        }
    }
    out
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

    fn cand(id: u64, monitor: isize, rect: ScreenRect, overlay: bool) -> Candidate {
        Candidate {
            id,
            monitor,
            rect,
            class: "App".into(),
            overlay,
        }
    }

    const RIGHT: ScreenRect = ScreenRect {
        left: 1920,
        top: 0,
        right: 3840,
        bottom: 1080,
    };

    #[test]
    fn full_screen_stays_while_working_on_another_monitor() {
        let mons = [(1, MON), (2, RIGHT)];
        // The user clicked an app on monitor 2: it is on top of the Z order,
        // the video below it still covers monitor 1.
        let ws = [
            cand(10, 2, r(2000, 100, 3000, 900), false),
            cand(11, 1, MON, false),
        ];
        assert_eq!(fullscreen_per_monitor(&ws, &mons), vec![(1, 11)]);
        // Back to an ordinary window on monitor 1: no longer full screen.
        let ws = [
            cand(12, 1, r(100, 100, 900, 900), false),
            cand(11, 1, MON, false),
        ];
        assert!(fullscreen_per_monitor(&ws, &mons).is_empty());
    }

    #[test]
    fn overlays_and_desktops_decide_nothing() {
        let mons = [(1, MON)];
        // A small always-on-top widget above the game.
        let ws = [
            cand(20, 1, r(10, 10, 300, 200), true),
            cand(21, 1, MON, false),
        ];
        assert_eq!(fullscreen_per_monitor(&ws, &mons), vec![(1, 21)]);
        // Only the desktop covers the monitor.
        let mut desk = cand(22, 1, MON, false);
        desk.class = "Progman".into();
        assert!(fullscreen_per_monitor(&[desk], &mons).is_empty());
        // A window on an unknown monitor is ignored.
        assert!(fullscreen_per_monitor(&[cand(23, 9, MON, false)], &mons).is_empty());
        // Two monitors, both full screen.
        let mons = [(1, MON), (2, RIGHT)];
        let ws = [cand(30, 1, MON, false), cand(31, 2, RIGHT, false)];
        assert_eq!(fullscreen_per_monitor(&ws, &mons), vec![(1, 30), (2, 31)]);
    }
}
