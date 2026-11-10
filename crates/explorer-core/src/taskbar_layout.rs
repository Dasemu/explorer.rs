// SPDX-License-Identifier: GPL-3.0-or-later
//! Layout of the full taskbar (Windows 11 arrangement):
//!
//! ```text
//! [Start][app buttons...][…]          [^][tray icons][net vol bat][clock][|]
//! ```
//!
//! - the start button and app buttons are centred or left-aligned;
//! - buttons that do not fit are replaced by a "…" (more) button;
//! - `^` (chevron) opens the tray overflow; it is shown only when some tray
//!   icons are hidden;
//! - the system glyphs (network, volume, battery) open Quick settings;
//! - the thin rectangle at the far right is "Show desktop".
//!
//! Pure geometry in physical pixels; `scale` is `dpi / 96`.

use crate::layout::{
    Rect, BUTTON_GAP, CLOCK_WIDTH, EDGE_PAD, ICON_BUTTON_WIDTH, MIN_BUTTON_WIDTH,
    START_BUTTON_WIDTH, TRAY_ICON_WIDTH,
};

/// Logical width of one system glyph slot.
pub const SYSTEM_GLYPH_WIDTH: f32 = 28.0;
/// Logical width of the tray chevron.
pub const CHEVRON_WIDTH: f32 = 26.0;
/// Logical width of the "Show desktop" strip.
pub const SHOW_DESKTOP_WIDTH: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TaskbarSpec {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub n_buttons: usize,
    pub labels: bool,
    /// Logical width of a labelled button.
    pub button_width: f32,
    pub centered: bool,
    /// Promoted tray icons shown on the bar.
    pub tray_icons: usize,
    /// Show the `^` button (some icons are in the overflow).
    pub tray_chevron: bool,
    /// Number of system glyphs (network, volume, battery...).
    pub system_glyphs: usize,
    pub show_clock: bool,
    pub show_desktop: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TaskbarLayout {
    pub start: Rect,
    /// Rects of the first `buttons.len()` app buttons.
    pub buttons: Vec<Rect>,
    /// Number of app buttons that did not fit (shown in the "…" flyout).
    pub hidden: usize,
    /// The "…" button, present iff `hidden > 0`.
    pub more: Option<Rect>,
    pub chevron: Option<Rect>,
    pub tray: Vec<Rect>,
    pub system: Vec<Rect>,
    pub clock: Option<Rect>,
    pub show_desktop: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Start,
    Button(usize),
    More,
    Chevron,
    Tray(usize),
    System(usize),
    Clock,
    ShowDesktop,
    Empty,
}

pub fn layout(spec: &TaskbarSpec) -> TaskbarLayout {
    let s = spec.scale;
    let h = spec.height;
    let gap = BUTTON_GAP * s;

    // Right side, from the edge inwards.
    let mut right = spec.width;
    let show_desktop = spec.show_desktop.then(|| {
        let w = SHOW_DESKTOP_WIDTH * s;
        right -= w;
        Rect::new(right, 0.0, w, h)
    });
    if !spec.show_desktop {
        right -= EDGE_PAD * s;
    }
    let clock = spec.show_clock.then(|| {
        let w = CLOCK_WIDTH * s;
        right -= w;
        Rect::new(right, 0.0, w, h)
    });
    let sys_w = SYSTEM_GLYPH_WIDTH * s;
    let mut system: Vec<Rect> = (0..spec.system_glyphs)
        .map(|_| {
            right -= sys_w;
            Rect::new(right, 0.0, sys_w, h)
        })
        .collect();
    system.reverse();
    if spec.system_glyphs > 0 {
        right -= 4.0 * s;
    }
    let tray_w = TRAY_ICON_WIDTH * s;
    let mut tray: Vec<Rect> = (0..spec.tray_icons)
        .map(|_| {
            right -= tray_w;
            Rect::new(right, 0.0, tray_w, h)
        })
        .collect();
    tray.reverse();
    let chevron = spec.tray_chevron.then(|| {
        let w = CHEVRON_WIDTH * s;
        right -= w;
        Rect::new(right, 0.0, w, h)
    });
    let right_limit = (right - gap).max(0.0);

    // Left side: start + buttons (+ "…").
    let start_w = START_BUTTON_WIDTH * s;
    let ideal = if spec.labels {
        spec.button_width * s
    } else {
        ICON_BUTTON_WIDTH * s
    };
    let min_w = MIN_BUTTON_WIDTH * s;
    let more_w = ICON_BUTTON_WIDTH * s;
    let left_x = EDGE_PAD * s;
    let area_w = (right_limit - (left_x + start_w + gap)).max(0.0);

    let n = spec.n_buttons;
    let fits = |count: usize, w: f32| count as f32 * (w + gap) <= area_w + 0.01;
    let (bw, count, more) = if n == 0 {
        (ideal, 0, false)
    } else if fits(n, ideal) {
        (ideal, n, false)
    } else {
        let fit_w = (area_w + gap) / n as f32 - gap;
        if fit_w >= min_w {
            (fit_w, n, false)
        } else {
            // Overflow: keep room for the "…" button.
            let avail = (area_w - (more_w + gap)).max(0.0);
            let c = ((avail + gap) / (min_w + gap)).floor().max(0.0) as usize;
            (min_w, c.min(n), true)
        }
    };
    let group_w = start_w + count as f32 * (bw + gap) + if more { more_w + gap } else { 0.0 };
    let x0 = if spec.centered {
        ((spec.width - group_w) / 2.0)
            .min(right_limit - group_w)
            .max(left_x)
    } else {
        left_x
    };
    let start = Rect::new(x0, 0.0, start_w, h);
    let buttons: Vec<Rect> = (0..count)
        .map(|i| Rect::new(start.right() + gap + i as f32 * (bw + gap), 0.0, bw, h))
        .collect();
    let more_rect = more.then(|| {
        let x = buttons.last().map(|r| r.right()).unwrap_or(start.right()) + gap;
        Rect::new(x, 0.0, more_w, h)
    });
    TaskbarLayout {
        start,
        buttons,
        hidden: n - count,
        more: more_rect,
        chevron,
        tray,
        system,
        clock,
        show_desktop,
    }
}

impl TaskbarLayout {
    pub fn hit(&self, x: f32, y: f32) -> Hit {
        let inside = |r: &Rect| r.contains(x, y);
        if inside(&self.start) {
            return Hit::Start;
        }
        if let Some(i) = self.buttons.iter().position(inside) {
            return Hit::Button(i);
        }
        if self.more.as_ref().is_some_and(inside) {
            return Hit::More;
        }
        if self.chevron.as_ref().is_some_and(inside) {
            return Hit::Chevron;
        }
        if let Some(i) = self.tray.iter().position(inside) {
            return Hit::Tray(i);
        }
        if let Some(i) = self.system.iter().position(inside) {
            return Hit::System(i);
        }
        if self.clock.as_ref().is_some_and(inside) {
            return Hit::Clock;
        }
        if self.show_desktop.as_ref().is_some_and(inside) {
            return Hit::ShowDesktop;
        }
        Hit::Empty
    }

    /// The whole system-glyph area (the Quick settings anchor).
    pub fn system_area(&self) -> Option<Rect> {
        let first = self.system.first()?;
        let last = self.system.last()?;
        Some(Rect::new(first.x, first.y, last.right() - first.x, first.h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(width: f32, n: usize) -> TaskbarSpec {
        TaskbarSpec {
            width,
            height: 48.0,
            scale: 1.0,
            n_buttons: n,
            labels: false,
            button_width: 180.0,
            centered: true,
            tray_icons: 2,
            tray_chevron: true,
            system_glyphs: 3,
            show_clock: true,
            show_desktop: true,
        }
    }

    #[test]
    fn right_side_order() {
        let l = layout(&spec(1920.0, 3));
        let sd = l.show_desktop.unwrap();
        let c = l.clock.unwrap();
        assert_eq!(sd.right(), 1920.0);
        assert!((c.right() - sd.x).abs() < 0.01);
        assert!(l.system[2].right() <= c.x + 0.01);
        assert!(l.tray[1].right() <= l.system[0].x);
        assert!(l.chevron.unwrap().right() <= l.tray[0].x + 0.01);
        assert_eq!(l.hit(sd.x + 1.0, 10.0), Hit::ShowDesktop);
        assert_eq!(l.hit(l.system[1].x + 1.0, 10.0), Hit::System(1));
        assert_eq!(l.hit(l.chevron.unwrap().x + 1.0, 10.0), Hit::Chevron);
        let a = l.system_area().unwrap();
        assert_eq!(a.x, l.system[0].x);
        assert_eq!(a.right(), l.system[2].right());
    }

    #[test]
    fn centred_without_overflow() {
        let l = layout(&spec(1920.0, 4));
        assert_eq!(l.hidden, 0);
        assert!(l.more.is_none());
        let mid = (l.start.x + l.buttons[3].right()) / 2.0;
        assert!((mid - 960.0).abs() < 3.0);
    }

    #[test]
    fn overflow_shows_more_button() {
        let l = layout(&spec(800.0, 40));
        assert!(l.hidden > 0);
        assert_eq!(l.buttons.len() + l.hidden, 40);
        let more = l.more.unwrap();
        assert!(more.x >= l.buttons.last().unwrap().right());
        assert!(more.right() <= l.chevron.unwrap().x);
        assert_eq!(l.hit(more.x + 2.0, 5.0), Hit::More);
    }

    #[test]
    fn shrinks_before_overflowing() {
        let l = layout(&TaskbarSpec {
            labels: true,
            ..spec(1200.0, 6)
        });
        assert_eq!(l.hidden, 0);
        assert!(l.buttons[0].w < 180.0 && l.buttons[0].w >= MIN_BUTTON_WIDTH);
    }

    #[test]
    fn optional_parts_absent() {
        let l = layout(&TaskbarSpec {
            tray_icons: 0,
            tray_chevron: false,
            system_glyphs: 0,
            show_clock: false,
            show_desktop: false,
            ..spec(1000.0, 1)
        });
        assert!(l.chevron.is_none() && l.clock.is_none() && l.show_desktop.is_none());
        assert!(l.system_area().is_none());
        assert_eq!(l.hit(999.0, 5.0), Hit::Empty);
    }

    #[test]
    fn tiny_bar_does_not_panic() {
        for w in [0.0, 10.0, 200.0] {
            let l = layout(&spec(w, 7));
            assert_eq!(l.buttons.len() + l.hidden, 7);
        }
    }

    #[test]
    fn scaled() {
        let l = layout(&TaskbarSpec {
            scale: 2.0,
            ..spec(3840.0, 1)
        });
        assert_eq!(l.show_desktop.unwrap().w, 16.0);
        assert_eq!(l.buttons[0].w, 88.0);
    }
}
