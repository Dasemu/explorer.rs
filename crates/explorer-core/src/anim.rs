// SPDX-License-Identifier: GPL-3.0-or-later
//! Time-based animation helpers, independent of any window system.
//!
//! Everything is driven by the caller's clock (`Instant`), so the values
//! only depend on elapsed time, never on how often frames are drawn: a
//! slow frame skips ahead instead of slowing the animation down.
//!
//! - [`Tween`]: one 0 → 1 progression (open slide, view transition);
//! - [`Fades`]: per-key levels that ease towards on/off (hover highlights).

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// Frame interval requested from the timer while something animates.
/// (Windows rounds timers up to the ~15.6 ms tick unless a higher
/// resolution is active; values only depend on elapsed time anyway.)
pub const FRAME: Duration = Duration::from_millis(8);

/// Popup opening (Start menu, flyouts): slide + content fade.
pub const OPEN: Duration = Duration::from_millis(220);
/// Distance of the opening slide, logical pixels.
pub const OPEN_SLIDE: f32 = 28.0;
/// Content change inside an open popup (view switch).
pub const VIEW: Duration = Duration::from_millis(140);
pub const VIEW_SLIDE: f32 = 12.0;
/// Hover highlight fade in / out.
pub const HOVER_IN: Duration = Duration::from_millis(90);
pub const HOVER_OUT: Duration = Duration::from_millis(180);

/// Fluent "decelerate": fast start, gentle stop.
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Stronger deceleration for slides (close to Fluent's `0.1, 0.9, 0.2, 1`).
pub fn ease_out_quint(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(5)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// A single 0 → 1 progression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tween {
    start: Instant,
    dur: Duration,
}

impl Tween {
    pub fn new(start: Instant, dur: Duration) -> Self {
        Self { start, dur }
    }

    /// Linear progress in [0, 1].
    pub fn progress(&self, now: Instant) -> f32 {
        if self.dur.is_zero() {
            return 1.0;
        }
        let e = now.saturating_duration_since(self.start).as_secs_f32();
        (e / self.dur.as_secs_f32()).clamp(0.0, 1.0)
    }

    pub fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }
}

/// Opening of a popup: how far it still is from its final place and how
/// opaque its content is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpenFrame {
    /// Remaining slide distance (same unit as `distance`), towards the
    /// final position; 0 when done.
    pub offset: f32,
    /// Content opacity, 0..=1.
    pub opacity: f32,
}

pub fn open_frame(t: &Tween, now: Instant, distance: f32) -> OpenFrame {
    let p = t.progress(now);
    OpenFrame {
        offset: distance * (1.0 - ease_out_quint(p)),
        // The content fades in faster than the window settles.
        opacity: ease_out_cubic((p * 1.6).min(1.0)),
    }
}

/// Per-key animated levels in [0, 1] (e.g. hover highlight of each button).
#[derive(Debug, Clone)]
pub struct Fades<K: Eq + Hash + Copy> {
    levels: HashMap<K, (f32, bool)>,
    last: Option<Instant>,
    fade_in: Duration,
    fade_out: Duration,
}

impl<K: Eq + Hash + Copy> Default for Fades<K> {
    fn default() -> Self {
        Self::new(HOVER_IN, HOVER_OUT)
    }
}

impl<K: Eq + Hash + Copy> Fades<K> {
    pub fn new(fade_in: Duration, fade_out: Duration) -> Self {
        Self {
            levels: HashMap::new(),
            last: None,
            fade_in,
            fade_out,
        }
    }

    /// Make `key` the only one heading to 1 (all others head to 0).
    /// `animate == false` jumps straight to the targets.
    pub fn set_only(&mut self, key: Option<K>, now: Instant, animate: bool) {
        self.advance(now);
        for (k, (lvl, on)) in self.levels.iter_mut() {
            *on = Some(*k) == key;
            if !animate {
                *lvl = if *on { 1.0 } else { 0.0 };
            }
        }
        if let Some(k) = key {
            let e = self.levels.entry(k).or_insert((0.0, true));
            e.1 = true;
            if !animate {
                e.0 = 1.0;
            }
        }
        self.levels.retain(|_, (l, on)| *on || *l > 0.0);
    }

    /// Advance all levels to `now`. Returns true while any is moving.
    pub fn advance(&mut self, now: Instant) -> bool {
        let dt = self
            .last
            .map(|l| now.saturating_duration_since(l).as_secs_f32())
            .unwrap_or(0.0);
        self.last = Some(now);
        let step_in = step(dt, self.fade_in);
        let step_out = step(dt, self.fade_out);
        for (lvl, on) in self.levels.values_mut() {
            if *on {
                *lvl = (*lvl + step_in).min(1.0);
            } else {
                *lvl = (*lvl - step_out).max(0.0);
            }
        }
        self.levels.retain(|_, (l, on)| *on || *l > 0.0);
        self.animating()
    }

    pub fn animating(&self) -> bool {
        self.levels
            .values()
            .any(|(l, on)| if *on { *l < 1.0 } else { *l > 0.0 })
    }

    /// Eased level of `key` (0 when unknown).
    pub fn level(&self, key: K) -> f32 {
        self.levels
            .get(&key)
            .map(|(l, _)| ease_out_cubic(*l))
            .unwrap_or(0.0)
    }

    pub fn clear(&mut self) {
        self.levels.clear();
    }
}

fn step(dt: f32, d: Duration) -> f32 {
    if d.is_zero() {
        1.0
    } else {
        dt / d.as_secs_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_endpoints() {
        for f in [ease_out_cubic, ease_out_quint] {
            assert_eq!(f(0.0), 0.0);
            assert_eq!(f(1.0), 1.0);
            assert_eq!(f(-1.0), 0.0);
            assert_eq!(f(2.0), 1.0);
            assert!(f(0.5) > 0.5, "decelerating curves are ahead at half time");
        }
    }

    #[test]
    fn tween_is_time_based() {
        let t0 = Instant::now();
        let t = Tween::new(t0, Duration::from_millis(200));
        assert_eq!(t.progress(t0), 0.0);
        assert!((t.progress(t0 + Duration::from_millis(100)) - 0.5).abs() < 1e-3);
        assert!(t.done(t0 + Duration::from_millis(200)));
        assert!(t.done(t0 + Duration::from_secs(5)));
        // A clock before the start is clamped.
        assert_eq!(t.progress(t0 - Duration::from_millis(5)), 0.0);
        assert!(Tween::new(t0, Duration::ZERO).done(t0));
    }

    #[test]
    fn open_frame_settles() {
        let t0 = Instant::now();
        let t = Tween::new(t0, OPEN);
        let f = open_frame(&t, t0, 30.0);
        assert_eq!(f.offset, 30.0);
        assert_eq!(f.opacity, 0.0);
        let f = open_frame(&t, t0 + OPEN, 30.0);
        assert_eq!(f.offset, 0.0);
        assert_eq!(f.opacity, 1.0);
        let mid = open_frame(&t, t0 + OPEN / 2, 30.0);
        assert!(mid.offset > 0.0 && mid.offset < 15.0);
        assert!(mid.opacity > 0.9);
    }

    #[test]
    fn fades_move_towards_targets() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut f: Fades<u32> = Fades::new(ms(100), ms(200));
        f.set_only(Some(1), t0, true);
        assert_eq!(f.level(1), 0.0);
        assert!(f.animating());
        f.advance(t0 + ms(50));
        assert!(f.level(1) > 0.0 && f.level(1) < 1.0);
        f.advance(t0 + ms(100));
        assert_eq!(f.level(1), 1.0);
        assert!(!f.animating());
        // Switch to 2: 1 fades out over 200 ms, 2 fades in.
        f.set_only(Some(2), t0 + ms(100), true);
        f.advance(t0 + ms(200));
        assert_eq!(f.level(2), 1.0);
        assert!(f.level(1) > 0.0);
        f.advance(t0 + ms(300));
        assert_eq!(f.level(1), 0.0);
        assert!(!f.animating());
        // Finished fade-outs are dropped.
        assert_eq!(f.levels.len(), 1);
        f.set_only(None, t0 + ms(300), false);
        assert_eq!(f.level(2), 0.0);
        assert!(f.levels.is_empty());
    }

    #[test]
    fn fades_with_unknown_key_and_zero_durations() {
        let t0 = Instant::now();
        let mut f: Fades<u8> = Fades::new(Duration::ZERO, Duration::ZERO);
        assert_eq!(f.level(9), 0.0);
        f.set_only(Some(9), t0, true);
        f.advance(t0);
        assert_eq!(f.level(9), 1.0);
    }
}
