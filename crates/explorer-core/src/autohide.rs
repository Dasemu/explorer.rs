// SPDX-License-Identifier: GPL-3.0-or-later
//! Taskbar auto-hide rules ("Automatically hide the taskbar").
//!
//! A hidden bar is slid past its screen edge, leaving a thin strip on the
//! monitor that catches the pointer. It comes back when the pointer touches
//! that strip or the Start menu opens, and stays while anything it opened
//! (flyout, menu, the Start menu) is up or the pointer is on it. It slides
//! away once none of that holds for [`HIDE_DELAY`].

use std::time::{Duration, Instant};

/// How long the bar lingers after the pointer leaves it.
pub const HIDE_DELAY: Duration = Duration::from_millis(500);
/// Slide in / out duration.
pub const SLIDE: Duration = Duration::from_millis(160);
/// Logical pixels of a hidden bar left on screen to catch the pointer.
pub const PEEK: f32 = 2.0;

/// What could keep the bar on screen right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reasons {
    /// The pointer is over the bar.
    pub pointer: bool,
    /// One of the bar's flyouts or menus is open.
    pub popup: bool,
    /// The Start menu (launcher) is open.
    pub launcher: bool,
    /// A button is being dragged.
    pub dragging: bool,
    /// A full-screen app is shown on the bar's monitor: touching the edge
    /// (common in games) must not bring the bar up.
    pub fullscreen: bool,
}

/// Whether the bar should be on screen.
pub fn wants_shown(r: &Reasons) -> bool {
    r.popup || r.launcher || r.dragging || (r.pointer && !r.fullscreen)
}

/// Physical pixels the bar is pushed past its edge when `hidden` of the
/// way out (0 = fully shown, 1 = hidden), keeping `peek` pixels on screen.
pub fn offset(thickness: i32, peek: i32, hidden: f32) -> i32 {
    ((thickness - peek).max(0) as f32 * hidden.clamp(0.0, 1.0)).round() as i32
}

/// Delays hiding until the bar has not been wanted for [`HIDE_DELAY`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Linger {
    idle_since: Option<Instant>,
}

impl Linger {
    /// Feed the current [`wants_shown`]; true once the bar should hide.
    pub fn update(&mut self, wanted: bool, now: Instant) -> bool {
        if wanted {
            self.idle_since = None;
            return false;
        }
        let since = *self.idle_since.get_or_insert(now);
        now.duration_since(since) >= HIDE_DELAY
    }

    pub fn reset(&mut self) {
        self.idle_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_shows_the_bar_except_over_full_screen_apps() {
        let pointer = Reasons {
            pointer: true,
            ..Default::default()
        };
        assert!(wants_shown(&pointer));
        assert!(!wants_shown(&Reasons {
            fullscreen: true,
            ..pointer
        }));
        assert!(!wants_shown(&Reasons::default()));
    }

    #[test]
    fn open_popups_and_the_start_menu_keep_it() {
        for r in [
            Reasons {
                popup: true,
                ..Default::default()
            },
            Reasons {
                launcher: true,
                fullscreen: true,
                ..Default::default()
            },
            Reasons {
                dragging: true,
                ..Default::default()
            },
        ] {
            assert!(wants_shown(&r), "{r:?}");
        }
    }

    #[test]
    fn offset_leaves_the_peek_strip() {
        assert_eq!(offset(48, 2, 0.0), 0);
        assert_eq!(offset(48, 2, 1.0), 46);
        assert_eq!(offset(48, 2, 0.5), 23);
        assert_eq!(offset(48, 2, 3.0), 46);
        assert_eq!(offset(1, 2, 1.0), 0);
    }

    #[test]
    fn linger_waits_before_hiding() {
        let t0 = Instant::now();
        let mut l = Linger::default();
        assert!(!l.update(false, t0));
        assert!(!l.update(false, t0 + Duration::from_millis(300)));
        // Wanted again: the delay starts over.
        assert!(!l.update(true, t0 + Duration::from_millis(400)));
        assert!(!l.update(false, t0 + Duration::from_millis(600)));
        assert!(!l.update(false, t0 + Duration::from_millis(1000)));
        assert!(l.update(false, t0 + Duration::from_millis(1100)));
    }
}
