// SPDX-License-Identifier: GPL-3.0-or-later
//! Placement and inner layout of the taskbar flyouts: window picker
//! (thumbnails), "…" overflow list, tray overflow grid, Quick settings and
//! calendar. Pure geometry in physical pixels; `s` is the DPI scale.

use crate::layout::Rect;

/// Gap between the taskbar and a flyout, and between a flyout and the
/// screen edge (logical px).
pub const FLYOUT_MARGIN: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HAnchor {
    /// Centre horizontally on this screen x (clamped to the monitor).
    Center(f32),
    /// Align to the monitor's right edge (Quick settings, calendar).
    Right,
}

/// Screen position of a flyout of `size` next to the taskbar `bar` (screen
/// rect) on a monitor `monitor` (screen rect). `bottom` is true for a
/// bottom taskbar (flyout opens upwards).
pub fn place(
    size: (f32, f32),
    anchor: HAnchor,
    bar: Rect,
    monitor: Rect,
    bottom: bool,
    margin: f32,
) -> (f32, f32) {
    let (w, h) = size;
    let min_x = monitor.x + margin;
    let max_x = (monitor.right() - w - margin).max(min_x);
    let x = match anchor {
        HAnchor::Center(cx) => (cx - w / 2.0).clamp(min_x, max_x),
        HAnchor::Right => max_x,
    };
    let y = if bottom {
        (bar.y - h - margin).max(monitor.y)
    } else {
        (bar.bottom() + margin).min((monitor.bottom() - h).max(monitor.y))
    };
    (x, y)
}

/// Fit a `src_w` x `src_h` image into `area`, preserving aspect ratio and
/// never upscaling; centred.
pub fn fit(src_w: f32, src_h: f32, area: Rect) -> Rect {
    if src_w <= 0.0 || src_h <= 0.0 || area.w <= 0.0 || area.h <= 0.0 {
        return Rect::new(area.x + area.w / 2.0, area.y + area.h / 2.0, 0.0, 0.0);
    }
    let k = (area.w / src_w).min(area.h / src_h).min(1.0);
    let (w, h) = (src_w * k, src_h * k);
    Rect::new(
        area.x + (area.w - w) / 2.0,
        area.y + (area.h - h) / 2.0,
        w,
        h,
    )
}

// ---------------------------------------------------------------------------
// Window picker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickerCard {
    pub card: Rect,
    pub icon: Rect,
    pub title: Rect,
    pub close: Rect,
    pub thumb: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickerLayout {
    pub size: (f32, f32),
    pub cards: Vec<PickerCard>,
}

/// Cards in rows; at most as many columns as fit in `max_w`.
pub fn picker(n: usize, s: f32, max_w: f32) -> PickerLayout {
    let pad = 8.0 * s;
    let gap = 4.0 * s;
    let card_w = 216.0 * s;
    let title_h = 32.0 * s;
    let thumb_h = 120.0 * s;
    let card_h = title_h + thumb_h + 8.0 * s;
    let n = n.max(1);
    let fit_cols = (((max_w - 2.0 * pad + gap) / (card_w + gap)).floor() as usize).max(1);
    let cols = n.min(fit_cols);
    let rows = n.div_ceil(cols);
    let w = 2.0 * pad + cols as f32 * card_w + (cols - 1) as f32 * gap;
    let h = 2.0 * pad + rows as f32 * card_h + (rows - 1) as f32 * gap;
    let cards = (0..n)
        .map(|i| {
            let (r, c) = (i / cols, i % cols);
            let x = pad + c as f32 * (card_w + gap);
            let y = pad + r as f32 * (card_h + gap);
            let card = Rect::new(x, y, card_w, card_h);
            let icon_sz = 16.0 * s;
            let icon = Rect::new(x + 8.0 * s, y + (title_h - icon_sz) / 2.0, icon_sz, icon_sz);
            let close_sz = 28.0 * s;
            let close = Rect::new(
                card.right() - close_sz - 2.0 * s,
                y + 2.0 * s,
                close_sz,
                close_sz,
            );
            let title = Rect::new(
                icon.right() + 8.0 * s,
                y,
                (close.x - icon.right() - 12.0 * s).max(0.0),
                title_h,
            );
            let thumb = Rect::new(x + 8.0 * s, y + title_h, card_w - 16.0 * s, thumb_h);
            PickerCard {
                card,
                icon,
                title,
                close,
                thumb,
            }
        })
        .collect();
    PickerLayout {
        size: (w, h),
        cards,
    }
}

// ---------------------------------------------------------------------------
// Lists and grids
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ListLayout {
    pub size: (f32, f32),
    pub rows: Vec<Rect>,
}

/// Vertical list of `n` rows (icon + name), wrapping into more columns
/// after `max_rows` rows.
pub fn list(n: usize, s: f32, max_rows: usize) -> ListLayout {
    let pad = 6.0 * s;
    let row_h = 36.0 * s;
    let row_w = 260.0 * s;
    let max_rows = max_rows.max(1);
    let n_eff = n.max(1);
    let cols = n_eff.div_ceil(max_rows);
    let rows_per_col = n_eff.div_ceil(cols);
    let rows = (0..n)
        .map(|i| {
            let (c, r) = (i / rows_per_col, i % rows_per_col);
            Rect::new(pad + c as f32 * row_w, pad + r as f32 * row_h, row_w, row_h)
        })
        .collect();
    ListLayout {
        size: (
            2.0 * pad + cols as f32 * row_w,
            2.0 * pad + rows_per_col as f32 * row_h,
        ),
        rows,
    }
}

/// Square-cell grid (tray overflow): up to 5 columns, like Windows 11.
pub fn icon_grid(n: usize, s: f32) -> ListLayout {
    let pad = 8.0 * s;
    let cell = 40.0 * s;
    let n_eff = n.max(1);
    let cols = n_eff.min(5);
    let rows_n = n_eff.div_ceil(cols);
    let rows = (0..n)
        .map(|i| {
            Rect::new(
                pad + (i % cols) as f32 * cell,
                pad + (i / cols) as f32 * cell,
                cell,
                cell,
            )
        })
        .collect();
    ListLayout {
        size: (
            2.0 * pad + cols as f32 * cell,
            2.0 * pad + rows_n as f32 * cell,
        ),
        rows,
    }
}

// ---------------------------------------------------------------------------
// Quick settings
// ---------------------------------------------------------------------------

/// A row with an icon (button), a slider and a value.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SliderRow {
    pub icon: Rect,
    pub track: Rect,
    pub value: Rect,
}

/// A status row (icon + two lines) with a settings link underneath.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StatusRow {
    pub row: Rect,
    pub icon: Rect,
    pub title: Rect,
    pub detail: Rect,
    pub link: Rect,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct QuickSettingsLayout {
    pub size: (f32, f32),
    pub volume: SliderRow,
    /// Output device name (click: sound settings).
    pub device: Rect,
    pub brightness: Option<SliderRow>,
    pub network: StatusRow,
    pub battery: Option<StatusRow>,
    pub footer: Rect,
    pub settings: Rect,
}

pub fn quick_settings(s: f32, has_brightness: bool, has_battery: bool) -> QuickSettingsLayout {
    let w = 360.0 * s;
    let pad = 16.0 * s;
    let inner = w - 2.0 * pad;
    let mut y = pad;
    let slider = |y: f32| SliderRow {
        icon: Rect::new(pad, y, 36.0 * s, 36.0 * s),
        track: Rect::new(pad + 44.0 * s, y, inner - 44.0 * s - 44.0 * s, 36.0 * s),
        value: Rect::new(w - pad - 40.0 * s, y, 40.0 * s, 36.0 * s),
    };
    let volume = slider(y);
    y += 36.0 * s;
    let device = Rect::new(pad + 44.0 * s, y, inner - 44.0 * s, 22.0 * s);
    y += 22.0 * s + 10.0 * s;
    let brightness = has_brightness.then(|| {
        let r = slider(y);
        y += 36.0 * s + 10.0 * s;
        r
    });
    y += 6.0 * s; // separator space
    let status = |y: f32| {
        let row = Rect::new(pad, y, inner, 44.0 * s);
        StatusRow {
            row,
            icon: Rect::new(pad, y + 4.0 * s, 36.0 * s, 36.0 * s),
            title: Rect::new(pad + 44.0 * s, y + 2.0 * s, inner - 44.0 * s, 20.0 * s),
            detail: Rect::new(pad + 44.0 * s, y + 22.0 * s, inner - 44.0 * s, 20.0 * s),
            link: Rect::new(pad + 44.0 * s, y + 44.0 * s, inner - 44.0 * s, 24.0 * s),
        }
    };
    let network = status(y);
    y += 68.0 * s + 8.0 * s;
    let battery = has_battery.then(|| {
        let r = status(y);
        y += 68.0 * s + 8.0 * s;
        r
    });
    y += 4.0 * s;
    let footer = Rect::new(0.0, y, w, 48.0 * s);
    let settings = Rect::new(w - pad - 40.0 * s, y + 6.0 * s, 36.0 * s, 36.0 * s);
    y += 48.0 * s;
    QuickSettingsLayout {
        size: (w, y),
        volume,
        device,
        brightness,
        network,
        battery,
        footer,
        settings,
    }
}

/// Slider value (0..=1) for a pointer at screen-space `x` over `track`.
pub fn slider_value(track: Rect, x: f32) -> f32 {
    if track.w <= 0.0 {
        return 0.0;
    }
    ((x - track.x) / track.w).clamp(0.0, 1.0)
}

/// Step a level by `delta` (e.g. +-0.02 per wheel notch), clamped, snapped
/// to whole percents to avoid drift.
pub fn step_level(level: f32, delta: f32) -> f32 {
    (((level + delta) * 100.0).round() / 100.0).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Calendar
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct CalendarLayout {
    pub size: (f32, f32),
    /// Full date ("martes, 3 de noviembre de 2026").
    pub date: Rect,
    pub month: Rect,
    pub prev: Rect,
    pub next: Rect,
    pub weekdays: Vec<Rect>,
    /// 42 day cells (6 weeks).
    pub days: Vec<Rect>,
}

pub fn calendar(s: f32) -> CalendarLayout {
    let pad = 16.0 * s;
    let cell = 44.0 * s;
    let w = 2.0 * pad + 7.0 * cell;
    let mut y = pad;
    let date = Rect::new(pad, y, 7.0 * cell, 28.0 * s);
    y += 28.0 * s + 12.0 * s;
    let nav = 32.0 * s;
    let month = Rect::new(pad + 4.0 * s, y, 7.0 * cell - 2.0 * nav - 8.0 * s, 36.0 * s);
    let prev = Rect::new(w - pad - 2.0 * nav, y + 2.0 * s, nav, nav);
    let next = Rect::new(w - pad - nav, y + 2.0 * s, nav, nav);
    y += 36.0 * s + 6.0 * s;
    let weekdays = (0..7)
        .map(|i| Rect::new(pad + i as f32 * cell, y, cell, 28.0 * s))
        .collect();
    y += 28.0 * s;
    let days = (0..42)
        .map(|i| {
            Rect::new(
                pad + (i % 7) as f32 * cell,
                y + (i / 7) as f32 * cell,
                cell,
                cell,
            )
        })
        .collect();
    y += 6.0 * cell + pad;
    CalendarLayout {
        size: (w, y),
        date,
        month,
        prev,
        next,
        weekdays,
        days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1920.0,
        h: 1080.0,
    };
    const BAR: Rect = Rect {
        x: 0.0,
        y: 1032.0,
        w: 1920.0,
        h: 48.0,
    };

    #[test]
    fn placement_above_bottom_bar() {
        let (x, y) = place((300.0, 200.0), HAnchor::Center(960.0), BAR, MON, true, 12.0);
        assert_eq!((x, y), (810.0, 1032.0 - 200.0 - 12.0));
        // Clamped at the edges.
        let (x, _) = place((300.0, 200.0), HAnchor::Center(10.0), BAR, MON, true, 12.0);
        assert_eq!(x, 12.0);
        let (x, _) = place(
            (300.0, 200.0),
            HAnchor::Center(1915.0),
            BAR,
            MON,
            true,
            12.0,
        );
        assert_eq!(x, 1920.0 - 300.0 - 12.0);
        let (x, _) = place((300.0, 200.0), HAnchor::Right, BAR, MON, true, 12.0);
        assert_eq!(x, 1608.0);
        // Too tall: stays on screen.
        let (_, y) = place((300.0, 5000.0), HAnchor::Right, BAR, MON, true, 12.0);
        assert_eq!(y, 0.0);
    }

    #[test]
    fn placement_below_top_bar_and_second_monitor() {
        let mon = Rect::new(1920.0, -200.0, 1280.0, 1024.0);
        let bar = Rect::new(1920.0, -200.0, 1280.0, 40.0);
        let (x, y) = place(
            (200.0, 100.0),
            HAnchor::Center(2000.0),
            bar,
            mon,
            false,
            12.0,
        );
        assert_eq!(y, -200.0 + 40.0 + 12.0);
        assert_eq!(x, 1932.0);
    }

    #[test]
    fn fitting() {
        let a = Rect::new(0.0, 0.0, 200.0, 100.0);
        let r = fit(1920.0, 1080.0, a);
        assert!((r.w - 177.78).abs() < 0.1 && (r.h - 100.0).abs() < 0.01);
        assert!((r.x - 11.11).abs() < 0.1);
        // Never upscale.
        assert_eq!(fit(50.0, 20.0, a), Rect::new(75.0, 40.0, 50.0, 20.0));
        assert_eq!(fit(0.0, 10.0, a).w, 0.0);
    }

    #[test]
    fn picker_wraps() {
        let p = picker(3, 1.0, 2000.0);
        assert_eq!(p.cards.len(), 3);
        assert!(p.cards[1].card.x > p.cards[0].card.right());
        assert_eq!(p.cards[0].card.y, p.cards[2].card.y);
        let p = picker(5, 1.0, 500.0);
        // 2 columns fit in 500 px.
        assert_eq!(p.cards[2].card.x, p.cards[0].card.x);
        assert!(p.size.0 <= 500.0);
        for c in &p.cards {
            assert!(c.title.right() <= c.close.x);
            assert!(c.thumb.bottom() <= c.card.bottom());
        }
        // Never empty / zero columns.
        assert_eq!(picker(0, 1.0, 10.0).cards.len(), 1);
    }

    #[test]
    fn lists_and_grids() {
        let l = list(3, 1.0, 10);
        assert_eq!(l.rows.len(), 3);
        assert_eq!(l.rows[1].y, l.rows[0].bottom());
        let l = list(25, 1.0, 10);
        // 3 balanced columns of 9 rows.
        assert_eq!(l.rows[8].x, l.rows[0].x);
        assert!(l.rows[9].x > l.rows[0].x);
        assert!(l
            .rows
            .iter()
            .all(|r| r.right() <= l.size.0 && r.bottom() <= l.size.1));
        let g = icon_grid(7, 1.0);
        assert_eq!(g.rows[5].y, g.rows[0].bottom());
        assert_eq!(g.size, (216.0, 96.0));
        assert_eq!(icon_grid(0, 1.0).rows.len(), 0);
    }

    #[test]
    fn quick_settings_rows() {
        let full = quick_settings(1.0, true, true);
        let min = quick_settings(1.0, false, false);
        assert!(full.size.1 > min.size.1);
        assert!(min.brightness.is_none() && min.battery.is_none());
        let b = full.brightness.unwrap();
        assert!(b.track.y > full.volume.track.bottom());
        assert!(full.network.row.y > b.track.bottom());
        assert!(full.battery.unwrap().row.y > full.network.link.y);
        assert!(full.footer.bottom() <= full.size.1 + 0.01);
        assert!(full.volume.track.right() <= full.volume.value.x);
    }

    #[test]
    fn sliders() {
        let t = Rect::new(100.0, 0.0, 200.0, 10.0);
        assert_eq!(slider_value(t, 50.0), 0.0);
        assert_eq!(slider_value(t, 200.0), 0.5);
        assert_eq!(slider_value(t, 900.0), 1.0);
        assert_eq!(slider_value(Rect::default(), 1.0), 0.0);
        assert_eq!(step_level(0.5, 0.02), 0.52);
        assert_eq!(step_level(0.99, 0.02), 1.0);
        assert_eq!(step_level(0.01, -0.02), 0.0);
        assert_eq!(step_level(0.333, 0.02), 0.35);
    }

    #[test]
    fn calendar_grid() {
        let c = calendar(1.0);
        assert_eq!(c.days.len(), 42);
        assert_eq!(c.weekdays.len(), 7);
        assert!(c.days[0].y >= c.weekdays[0].bottom());
        assert_eq!(c.days[7].y, c.days[0].bottom());
        assert!(c.next.right() <= c.size.0);
        assert!(c.days[41].bottom() <= c.size.1);
        assert!(c.prev.right() <= c.next.x);
    }
}
