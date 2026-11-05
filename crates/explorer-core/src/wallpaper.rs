// SPDX-License-Identifier: GPL-3.0-or-later
//! Wallpaper geometry for the desktop window (`explorer-rs-desktop`).
//!
//! Inputs are the user's settings (`HKCU\Control Panel\Desktop`
//! `WallpaperStyle` / `TileWallpaper`, the background colour), the image
//! size and the monitor rectangles in physical pixels. Output is, per
//! monitor, which part of the image goes where.

use crate::layout::Rect;

/// "Choose a fit for your desktop image" in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Fill,
    Fit,
    Stretch,
    Tile,
    Center,
    Span,
}

impl Style {
    /// From the registry strings (`WallpaperStyle`, `TileWallpaper`).
    /// Missing or unknown values give the Windows default, Fill.
    pub fn from_registry(wallpaper_style: Option<&str>, tile: Option<&str>) -> Self {
        let tile = tile.map(str::trim) == Some("1");
        match wallpaper_style.map(str::trim) {
            Some("0") if tile => Style::Tile,
            Some("0") => Style::Center,
            Some("2") => Style::Stretch,
            Some("6") => Style::Fit,
            Some("22") => Style::Span,
            Some("10") => Style::Fill,
            None if tile => Style::Tile,
            _ => Style::Fill,
        }
    }
}

/// How to draw the image on one monitor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// Draw image region `src` (image pixels) into `dst` (screen pixels).
    /// `dst` always lies inside the monitor.
    Draw { src: Rect, dst: Rect },
    /// Repeat the image at its natural size, starting at the monitor's
    /// top-left corner.
    Tile,
    /// Nothing of the image is visible on this monitor.
    None,
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.right().min(b.right());
    let y1 = a.bottom().min(b.bottom());
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// Map the whole image onto `full` (screen rect, possibly larger than the
/// monitor), then keep only the part inside `monitor`.
fn clip_mapping(img_w: f32, img_h: f32, full: Rect, monitor: Rect) -> Placement {
    let Some(dst) = intersect(full, monitor) else {
        return Placement::None;
    };
    let sx = img_w / full.w;
    let sy = img_h / full.h;
    let src = Rect::new(
        (dst.x - full.x) * sx,
        (dst.y - full.y) * sy,
        dst.w * sx,
        dst.h * sy,
    );
    Placement::Draw { src, dst }
}

/// Rect of size `w`×`h` centred on `area`.
fn centered(w: f32, h: f32, area: Rect) -> Rect {
    Rect::new(
        area.x + (area.w - w) / 2.0,
        area.y + (area.h - h) / 2.0,
        w,
        h,
    )
}

/// Where the image goes on `monitor`. `virtual_screen` is the bounding box
/// of all monitors (used by [`Style::Span`]).
pub fn place(
    style: Style,
    img_w: u32,
    img_h: u32,
    monitor: Rect,
    virtual_screen: Rect,
) -> Placement {
    if img_w == 0 || img_h == 0 || monitor.w <= 0.0 || monitor.h <= 0.0 {
        return Placement::None;
    }
    let (iw, ih) = (img_w as f32, img_h as f32);
    match style {
        Style::Tile => Placement::Tile,
        Style::Stretch => Placement::Draw {
            src: Rect::new(0.0, 0.0, iw, ih),
            dst: monitor,
        },
        Style::Center => clip_mapping(iw, ih, centered(iw, ih, monitor), monitor),
        Style::Fit => {
            let s = (monitor.w / iw).min(monitor.h / ih);
            clip_mapping(iw, ih, centered(iw * s, ih * s, monitor), monitor)
        }
        Style::Fill => {
            let s = (monitor.w / iw).max(monitor.h / ih);
            clip_mapping(iw, ih, centered(iw * s, ih * s, monitor), monitor)
        }
        Style::Span => {
            let area = if virtual_screen.w > 0.0 && virtual_screen.h > 0.0 {
                virtual_screen
            } else {
                monitor
            };
            let s = (area.w / iw).max(area.h / ih);
            clip_mapping(iw, ih, centered(iw * s, ih * s, area), monitor)
        }
    }
}

/// Bounding box of `monitors`.
pub fn virtual_screen(monitors: &[Rect]) -> Rect {
    let mut it = monitors.iter();
    let Some(first) = it.next() else {
        return Rect::default();
    };
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.right(), first.bottom());
    for m in it {
        x0 = x0.min(m.x);
        y0 = y0.min(m.y);
        x1 = x1.max(m.right());
        y1 = y1.max(m.bottom());
    }
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// Size to decode the image at: no larger than what any monitor shows
/// (saves memory for 8K wallpapers on small screens) and no side above
/// `max_side` (the GPU's maximum bitmap size). Never upscales.
pub fn decode_size(
    style: Style,
    img_w: u32,
    img_h: u32,
    monitors: &[Rect],
    max_side: u32,
) -> (u32, u32) {
    if img_w == 0 || img_h == 0 {
        return (img_w, img_h);
    }
    let vs = virtual_screen(monitors);
    let mut needed: f32 = match style {
        Style::Tile | Style::Center => 1.0,
        _ => 0.0,
    };
    if needed == 0.0 {
        for m in monitors {
            if let Placement::Draw { src, dst } = place(style, img_w, img_h, *m, vs) {
                if src.w > 0.0 && src.h > 0.0 {
                    needed = needed.max(dst.w / src.w).max(dst.h / src.h);
                }
            }
        }
        if needed == 0.0 {
            needed = 1.0;
        }
    }
    let max_side = max_side.max(1) as f32;
    let cap = (max_side / img_w as f32).min(max_side / img_h as f32);
    let s = needed.min(cap).min(1.0);
    let w = ((img_w as f32 * s).round() as u32).max(1);
    let h = ((img_h as f32 * s).round() as u32).max(1);
    (w, h)
}

/// `HKCU\Control Panel\Colors\Background` ("R G B") as 0xRRGGBB.
pub fn parse_rgb(s: &str) -> Option<u32> {
    let parts: Vec<u32> = s
        .split_whitespace()
        .map(|p| p.parse::<u32>().ok().filter(|v| *v <= 255))
        .collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [r, g, b] => Some((r << 16) | (g << 8) | b),
        _ => None,
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

    fn draw(p: Placement) -> (Rect, Rect) {
        match p {
            Placement::Draw { src, dst } => (src, dst),
            other => panic!("expected Draw, got {other:?}"),
        }
    }

    fn approx(a: Rect, b: Rect) {
        let close = |x: f32, y: f32| (x - y).abs() < 0.01;
        assert!(
            close(a.x, b.x) && close(a.y, b.y) && close(a.w, b.w) && close(a.h, b.h),
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn registry_mapping() {
        assert_eq!(Style::from_registry(Some("10"), Some("0")), Style::Fill);
        assert_eq!(Style::from_registry(Some("6"), None), Style::Fit);
        assert_eq!(Style::from_registry(Some("2"), None), Style::Stretch);
        assert_eq!(Style::from_registry(Some("0"), Some("1")), Style::Tile);
        assert_eq!(Style::from_registry(Some("0"), Some("0")), Style::Center);
        assert_eq!(Style::from_registry(Some("22"), None), Style::Span);
        assert_eq!(Style::from_registry(None, None), Style::Fill);
        assert_eq!(Style::from_registry(Some("99"), None), Style::Fill);
    }

    #[test]
    fn fill_crops_the_long_side() {
        // 4:3 image on 16:9 monitor: full width, crop top/bottom.
        let (src, dst) = draw(place(Style::Fill, 1600, 1200, MON, MON));
        approx(dst, MON);
        approx(src, Rect::new(0.0, 150.0, 1600.0, 900.0));
    }

    #[test]
    fn fit_letterboxes() {
        let (src, dst) = draw(place(Style::Fit, 1600, 1200, MON, MON));
        approx(src, Rect::new(0.0, 0.0, 1600.0, 1200.0));
        approx(dst, Rect::new(240.0, 0.0, 1440.0, 1080.0));
    }

    #[test]
    fn stretch_and_tile() {
        let (src, dst) = draw(place(Style::Stretch, 100, 50, MON, MON));
        approx(src, Rect::new(0.0, 0.0, 100.0, 50.0));
        approx(dst, MON);
        assert_eq!(place(Style::Tile, 100, 50, MON, MON), Placement::Tile);
    }

    #[test]
    fn center_small_and_large() {
        let (src, dst) = draw(place(Style::Center, 200, 100, MON, MON));
        approx(src, Rect::new(0.0, 0.0, 200.0, 100.0));
        approx(dst, Rect::new(860.0, 490.0, 200.0, 100.0));
        // Larger than the monitor: the middle part, 1:1.
        let (src, dst) = draw(place(Style::Center, 3840, 2160, MON, MON));
        approx(dst, MON);
        approx(src, Rect::new(960.0, 540.0, 1920.0, 1080.0));
    }

    #[test]
    fn span_across_two_monitors() {
        let left = MON;
        let right = Rect::new(1920.0, 0.0, 1920.0, 1080.0);
        let vs = virtual_screen(&[left, right]);
        approx(vs, Rect::new(0.0, 0.0, 3840.0, 1080.0));
        let (src_l, dst_l) = draw(place(Style::Span, 3840, 1080, left, vs));
        let (src_r, dst_r) = draw(place(Style::Span, 3840, 1080, right, vs));
        approx(dst_l, left);
        approx(dst_r, right);
        approx(src_l, Rect::new(0.0, 0.0, 1920.0, 1080.0));
        approx(src_r, Rect::new(1920.0, 0.0, 1920.0, 1080.0));
    }

    #[test]
    fn monitor_with_negative_origin() {
        let m = Rect::new(-1280.0, 0.0, 1280.0, 1024.0);
        let (_, dst) = draw(place(Style::Fill, 1920, 1080, m, m));
        approx(dst, m);
    }

    #[test]
    fn degenerate_inputs() {
        assert_eq!(place(Style::Fill, 0, 10, MON, MON), Placement::None);
        assert_eq!(
            place(Style::Fill, 10, 10, Rect::new(0.0, 0.0, 0.0, 0.0), MON),
            Placement::None
        );
        assert_eq!(virtual_screen(&[]), Rect::default());
    }

    #[test]
    fn decode_size_rules() {
        // 8K image filling a 1080p monitor is decoded at a quarter.
        assert_eq!(
            decode_size(Style::Fill, 7680, 4320, &[MON], 16384),
            (1920, 1080)
        );
        // Never upscale.
        assert_eq!(
            decode_size(Style::Fill, 800, 600, &[MON], 16384),
            (800, 600)
        );
        // Centre/tile: natural size, but capped by the GPU limit.
        assert_eq!(
            decode_size(Style::Center, 20000, 1000, &[MON], 16384),
            (16384, 819)
        );
        assert_eq!(decode_size(Style::Tile, 64, 64, &[MON], 16384), (64, 64));
    }

    #[test]
    fn rgb_parsing() {
        assert_eq!(parse_rgb("0 0 0"), Some(0));
        assert_eq!(parse_rgb(" 58 110 165 "), Some(0x3A6EA5));
        assert_eq!(parse_rgb("1 2"), None);
        assert_eq!(parse_rgb("256 0 0"), None);
        assert_eq!(parse_rgb("a b c"), None);
    }
}
