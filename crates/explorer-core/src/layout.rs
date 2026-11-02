// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure geometry for the taskbar and launcher: layout and hit-testing.
//!
//! All values are physical pixels; `scale` is `dpi / 96`.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// Logical (96 DPI) sizes of fixed taskbar parts.
pub const START_BUTTON_WIDTH: f32 = 48.0;
pub const CLOCK_WIDTH: f32 = 88.0;
pub const MIN_BUTTON_WIDTH: f32 = 48.0;
pub const BUTTON_GAP: f32 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct BarLayout {
    pub start: Rect,
    pub buttons: Vec<Rect>,
    /// Number of buttons that did not fit.
    pub overflow: usize,
    pub clock: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarHit {
    Start,
    Button(usize),
    Clock,
    Empty,
}

/// Compute the layout of one taskbar.
pub fn bar_layout(
    width: f32,
    height: f32,
    scale: f32,
    n_buttons: usize,
    button_width: f32,
    show_clock: bool,
) -> BarLayout {
    let start = Rect::new(0.0, 0.0, START_BUTTON_WIDTH * scale, height);
    let clock = show_clock.then(|| {
        let w = CLOCK_WIDTH * scale;
        Rect::new((width - w).max(start.right()), 0.0, w, height)
    });
    let area_x = start.right() + BUTTON_GAP * scale;
    let area_end = clock.map(|c| c.x).unwrap_or(width) - BUTTON_GAP * scale;
    let area_w = (area_end - area_x).max(0.0);
    let gap = BUTTON_GAP * scale;
    let ideal = button_width * scale;
    let min_w = MIN_BUTTON_WIDTH * scale;

    let mut buttons = Vec::new();
    let overflow;
    if n_buttons > 0 && area_w > 0.0 {
        let fit_w = (area_w + gap) / n_buttons as f32 - gap;
        let (bw, count) = if fit_w >= ideal {
            (ideal, n_buttons)
        } else if fit_w >= min_w {
            (fit_w, n_buttons)
        } else {
            let count = ((area_w + gap) / (min_w + gap)).floor().max(0.0) as usize;
            (min_w, count.min(n_buttons))
        };
        overflow = n_buttons - count;
        for i in 0..count {
            let x = area_x + i as f32 * (bw + gap);
            buttons.push(Rect::new(x, 0.0, bw, height));
        }
    } else {
        overflow = n_buttons;
    }
    BarLayout {
        start,
        buttons,
        overflow,
        clock,
    }
}

impl BarLayout {
    pub fn hit(&self, x: f32, y: f32) -> BarHit {
        if self.start.contains(x, y) {
            return BarHit::Start;
        }
        if let Some(c) = self.clock {
            if c.contains(x, y) {
                return BarHit::Clock;
            }
        }
        self.buttons
            .iter()
            .position(|r| r.contains(x, y))
            .map(BarHit::Button)
            .unwrap_or(BarHit::Empty)
    }
}

/// Keep `selected` inside the visible window of `rows` rows starting at
/// `scroll`; returns the new scroll offset.
pub fn scroll_to_visible(selected: usize, scroll: usize, rows: usize, total: usize) -> usize {
    if rows == 0 || total == 0 {
        return 0;
    }
    let max_scroll = total.saturating_sub(rows);
    let s = if selected < scroll {
        selected
    } else if selected >= scroll + rows {
        selected + 1 - rows
    } else {
        scroll
    };
    s.min(max_scroll)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ideal_width_when_room() {
        let l = bar_layout(1920.0, 40.0, 1.0, 3, 180.0, true);
        assert_eq!(l.buttons.len(), 3);
        assert_eq!(l.buttons[0].w, 180.0);
        assert_eq!(l.overflow, 0);
        assert_eq!(l.clock.unwrap().right(), 1920.0);
    }

    #[test]
    fn shrinks_then_overflows() {
        let l = bar_layout(800.0, 40.0, 1.0, 6, 180.0, true);
        assert_eq!(l.buttons.len(), 6);
        assert!(l.buttons[0].w < 180.0 && l.buttons[0].w >= MIN_BUTTON_WIDTH);
        let l = bar_layout(400.0, 40.0, 1.0, 30, 180.0, true);
        assert!(l.overflow > 0);
        assert_eq!(l.buttons.len() + l.overflow, 30);
        let clock_x = l.clock.unwrap().x;
        assert!(l.buttons.iter().all(|b| b.right() <= clock_x));
    }

    #[test]
    fn scaling_applies() {
        let l = bar_layout(3840.0, 80.0, 2.0, 1, 180.0, false);
        assert_eq!(l.start.w, 96.0);
        assert_eq!(l.buttons[0].w, 360.0);
        assert!(l.clock.is_none());
    }

    #[test]
    fn hit_testing() {
        let l = bar_layout(1000.0, 40.0, 1.0, 2, 100.0, true);
        assert_eq!(l.hit(5.0, 5.0), BarHit::Start);
        assert_eq!(l.hit(l.buttons[1].x + 1.0, 5.0), BarHit::Button(1));
        assert_eq!(l.hit(995.0, 5.0), BarHit::Clock);
        assert_eq!(l.hit(700.0, 5.0), BarHit::Empty);
    }

    #[test]
    fn tiny_bar_does_not_panic() {
        let l = bar_layout(10.0, 40.0, 1.0, 5, 180.0, true);
        assert_eq!(l.buttons.len() + l.overflow, 5);
    }

    #[test]
    fn scrolling() {
        assert_eq!(scroll_to_visible(0, 0, 5, 20), 0);
        assert_eq!(scroll_to_visible(7, 0, 5, 20), 3);
        assert_eq!(scroll_to_visible(2, 3, 5, 20), 2);
        assert_eq!(scroll_to_visible(19, 0, 5, 20), 15);
        assert_eq!(scroll_to_visible(3, 0, 0, 20), 0);
    }
}
