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
pub const MIN_BUTTON_WIDTH: f32 = 44.0;
pub const ICON_BUTTON_WIDTH: f32 = 44.0;
pub const BUTTON_GAP: f32 = 2.0;
pub const TRAY_ICON_WIDTH: f32 = 26.0;
pub const EDGE_PAD: f32 = 6.0;

/// Inputs for [`bar_layout`]. Sizes are logical (96 DPI) pixels except
/// `width`/`height`, which are physical.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarSpec {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub n_buttons: usize,
    /// Width of a labelled button; ignored when `labels` is false.
    pub button_width: f32,
    /// Show window titles next to icons (false = icon-only, Windows 11 style).
    pub labels: bool,
    /// Centre the start button + window buttons (Windows 11 default).
    pub centered: bool,
    pub show_clock: bool,
    pub tray_icons: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BarLayout {
    pub start: Rect,
    pub buttons: Vec<Rect>,
    /// Number of buttons that did not fit.
    pub overflow: usize,
    pub tray: Vec<Rect>,
    pub clock: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarHit {
    Start,
    Button(usize),
    Tray(usize),
    Clock,
    Empty,
}

/// Compute the layout of one taskbar.
pub fn bar_layout(spec: &BarSpec) -> BarLayout {
    let s = spec.scale;
    let h = spec.height;
    let gap = BUTTON_GAP * s;

    // Right side: tray icons then clock, flush right.
    let mut right = spec.width - EDGE_PAD * s;
    let clock = spec.show_clock.then(|| {
        let w = CLOCK_WIDTH * s;
        right -= w;
        Rect::new(right, 0.0, w, h)
    });
    let tray_w = TRAY_ICON_WIDTH * s;
    let mut tray: Vec<Rect> = (0..spec.tray_icons)
        .map(|_| {
            right -= tray_w;
            Rect::new(right, 0.0, tray_w, h)
        })
        .collect();
    tray.reverse();
    let right_limit = (right - gap).max(0.0);

    let start_w = START_BUTTON_WIDTH * s;
    let ideal = if spec.labels {
        spec.button_width * s
    } else {
        ICON_BUTTON_WIDTH * s
    };
    let min_w = MIN_BUTTON_WIDTH * s;

    // Space for buttons if left-aligned (the most available).
    let left_x = EDGE_PAD * s;
    let area_w = (right_limit - (left_x + start_w + gap)).max(0.0);

    let n = spec.n_buttons;
    let (bw, count) = if n == 0 || area_w <= 0.0 {
        (ideal, 0)
    } else {
        let fit_w = (area_w + gap) / n as f32 - gap;
        if fit_w >= ideal {
            (ideal, n)
        } else if fit_w >= min_w {
            (fit_w, n)
        } else {
            let c = ((area_w + gap) / (min_w + gap)).floor().max(0.0) as usize;
            (min_w, c.min(n))
        }
    };
    let group_w = start_w + count as f32 * (bw + gap);
    let x0 = if spec.centered {
        let centered = (spec.width - group_w) / 2.0;
        // Never overlap the tray/clock; fall back towards the left.
        centered.min(right_limit - group_w).max(left_x)
    } else {
        left_x
    };
    let start = Rect::new(x0, 0.0, start_w, h);
    let buttons = (0..count)
        .map(|i| Rect::new(start.right() + gap + i as f32 * (bw + gap), 0.0, bw, h))
        .collect();
    BarLayout {
        start,
        buttons,
        overflow: n - count,
        tray,
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
        if let Some(i) = self.tray.iter().position(|r| r.contains(x, y)) {
            return BarHit::Tray(i);
        }
        self.buttons
            .iter()
            .position(|r| r.contains(x, y))
            .map(BarHit::Button)
            .unwrap_or(BarHit::Empty)
    }
}

/// Grid of `count` cells, `cols` per row, inside `area`; returns cell rects
/// for rows `[first_row, first_row + visible_rows)`.
pub fn grid(area: Rect, cols: usize, cell_h: f32, count: usize) -> Vec<Rect> {
    if cols == 0 {
        return Vec::new();
    }
    let cw = area.w / cols as f32;
    let rows_fit = (area.h / cell_h).floor().max(0.0) as usize;
    (0..count.min(rows_fit * cols))
        .map(|i| {
            let (r, c) = (i / cols, i % cols);
            Rect::new(
                area.x + c as f32 * cw,
                area.y + r as f32 * cell_h,
                cw,
                cell_h,
            )
        })
        .collect()
}

/// Direction for [`nav`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// Spatial keyboard navigation: the nearest item from `cur` in direction
/// `dir`, preferring items aligned on the other axis. `None` at the edge.
pub fn nav(rects: &[Rect], cur: usize, dir: Dir) -> Option<usize> {
    let c = rects.get(cur)?;
    let (cx, cy) = (c.x + c.w / 2.0, c.y + c.h / 2.0);
    rects
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != cur)
        .filter_map(|(i, r)| {
            let (x, y) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
            let (dx, dy) = (x - cx, y - cy);
            let (primary, secondary) = match dir {
                Dir::Up => (-dy, dx.abs()),
                Dir::Down => (dy, dx.abs()),
                Dir::Left => (-dx, dy.abs()),
                Dir::Right => (dx, dy.abs()),
            };
            (primary > 1.0).then_some((i, primary + secondary * 2.0))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
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

    fn spec(width: f32, n: usize) -> BarSpec {
        BarSpec {
            width,
            height: 40.0,
            scale: 1.0,
            n_buttons: n,
            button_width: 180.0,
            labels: true,
            centered: false,
            show_clock: true,
            tray_icons: 0,
        }
    }

    #[test]
    fn ideal_width_when_room() {
        let l = bar_layout(&spec(1920.0, 3));
        assert_eq!(l.buttons.len(), 3);
        assert_eq!(l.buttons[0].w, 180.0);
        assert_eq!(l.overflow, 0);
        assert_eq!(l.clock.unwrap().right(), 1920.0 - EDGE_PAD);
    }

    #[test]
    fn shrinks_then_overflows() {
        let l = bar_layout(&spec(800.0, 6));
        assert_eq!(l.buttons.len(), 6);
        assert!(l.buttons[0].w < 180.0 && l.buttons[0].w >= MIN_BUTTON_WIDTH);
        let l = bar_layout(&spec(400.0, 30));
        assert!(l.overflow > 0);
        assert_eq!(l.buttons.len() + l.overflow, 30);
        let clock_x = l.clock.unwrap().x;
        assert!(l.buttons.iter().all(|b| b.right() <= clock_x));
    }

    #[test]
    fn scaling_applies() {
        let l = bar_layout(&BarSpec {
            width: 3840.0,
            height: 80.0,
            scale: 2.0,
            show_clock: false,
            ..spec(3840.0, 1)
        });
        assert_eq!(l.start.w, 96.0);
        assert_eq!(l.buttons[0].w, 360.0);
        assert!(l.clock.is_none());
    }

    #[test]
    fn centered_icon_only_group() {
        let l = bar_layout(&BarSpec {
            labels: false,
            centered: true,
            ..spec(1920.0, 4)
        });
        let group_l = l.start.x;
        let group_r = l.buttons.last().unwrap().right();
        let mid = (group_l + group_r) / 2.0;
        assert!((mid - 960.0).abs() < 3.0, "group centred, mid={mid}");
        assert_eq!(l.buttons[0].w, ICON_BUTTON_WIDTH);
    }

    #[test]
    fn tray_sits_left_of_clock() {
        let l = bar_layout(&BarSpec {
            tray_icons: 3,
            ..spec(1920.0, 2)
        });
        assert_eq!(l.tray.len(), 3);
        let c = l.clock.unwrap();
        assert!((l.tray[2].right() - c.x).abs() < 0.01);
        assert!(l.tray[0].x < l.tray[1].x);
        assert_eq!(l.hit(l.tray[1].x + 1.0, 5.0), BarHit::Tray(1));
    }

    #[test]
    fn hit_testing() {
        let l = bar_layout(&BarSpec {
            button_width: 100.0,
            ..spec(1000.0, 2)
        });
        assert_eq!(l.hit(l.start.x + 5.0, 5.0), BarHit::Start);
        assert_eq!(l.hit(l.buttons[1].x + 1.0, 5.0), BarHit::Button(1));
        assert_eq!(l.hit(990.0, 5.0), BarHit::Clock);
        assert_eq!(l.hit(700.0, 5.0), BarHit::Empty);
    }

    #[test]
    fn tiny_bar_does_not_panic() {
        let l = bar_layout(&spec(10.0, 5));
        assert_eq!(l.buttons.len() + l.overflow, 5);
        let l = bar_layout(&BarSpec {
            centered: true,
            tray_icons: 9,
            ..spec(10.0, 5)
        });
        assert_eq!(l.buttons.len() + l.overflow, 5);
    }

    #[test]
    fn grid_cells() {
        let g = grid(Rect::new(0.0, 0.0, 600.0, 200.0), 6, 90.0, 20);
        assert_eq!(g.len(), 12); // 2 rows fit
        assert_eq!(g[7], Rect::new(100.0, 90.0, 100.0, 90.0));
    }

    #[test]
    fn spatial_navigation() {
        // 3x2 grid then a single wide row below.
        let mut r = grid(Rect::new(0.0, 0.0, 300.0, 200.0), 3, 100.0, 6);
        r.push(Rect::new(0.0, 220.0, 150.0, 50.0));
        assert_eq!(nav(&r, 0, Dir::Right), Some(1));
        assert_eq!(nav(&r, 0, Dir::Down), Some(3));
        assert_eq!(nav(&r, 4, Dir::Up), Some(1));
        assert_eq!(nav(&r, 3, Dir::Down), Some(6));
        assert_eq!(nav(&r, 0, Dir::Up), None);
        assert_eq!(nav(&r, 2, Dir::Right), None);
        assert_eq!(nav(&r, 99, Dir::Right), None);
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
