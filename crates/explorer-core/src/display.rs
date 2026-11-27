// SPDX-License-Identifier: GPL-3.0-or-later
//! What a display change (`WM_DISPLAYCHANGE`) means for the taskbars.
//!
//! The taskbar used to destroy and recreate every bar on each display
//! change. A game that switches the resolution for exclusive full screen
//! fires one too; the new bars came up topmost over the game, which
//! dropped out of exclusive full screen, which changed the resolution back,
//! which fired another display change: a loop. Bars are now only rebuilt
//! when the set of monitors really changed; a new resolution, position or
//! scale only moves the existing bars ([`classify`]).

use crate::fullscreen::ScreenRect;

/// One monitor as the taskbar sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorKey {
    /// GDI device name (`\\.\DISPLAY2`): stable while the monitor stays
    /// connected, unlike the `HMONITOR` handle, which Windows may replace.
    pub device: String,
    /// `HMONITOR` as an integer.
    pub handle: isize,
    pub primary: bool,
    pub rect: ScreenRect,
    pub dpi: u32,
}

/// What the bars must do after a display change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayChange {
    /// Same monitors, same geometry: nothing to move (the work area is
    /// still checked by the regular reservation check).
    Unchanged,
    /// Same monitors, but a resolution, position, scale or handle changed:
    /// keep the bars and move them.
    Reposition,
    /// Monitors were added or removed, or another one became the primary
    /// (only the primary bar has the tray): destroy and recreate the bars.
    Rebuild,
}

fn identity(set: &[MonitorKey]) -> (Vec<&str>, Option<&str>) {
    let mut devices: Vec<&str> = set.iter().map(|m| m.device.as_str()).collect();
    devices.sort_unstable();
    let primary = set.iter().find(|m| m.primary).map(|m| m.device.as_str());
    (devices, primary)
}

/// Compare the monitors the bars were built for (`old`) with the current
/// ones (`new`). An empty `new` list (all displays off for a moment, e.g.
/// while waking up) changes nothing: the next display change will tell.
pub fn classify(old: &[MonitorKey], new: &[MonitorKey]) -> DisplayChange {
    if new.is_empty() {
        return DisplayChange::Unchanged;
    }
    if identity(old) != identity(new) {
        return DisplayChange::Rebuild;
    }
    let moved = new.iter().any(|n| {
        old.iter()
            .find(|o| o.device == n.device)
            .is_none_or(|o| o.rect != n.rect || o.dpi != n.dpi || o.handle != n.handle)
    });
    if moved {
        DisplayChange::Reposition
    } else {
        DisplayChange::Unchanged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(device: &str, primary: bool, x: i32, w: i32, h: i32, dpi: u32) -> MonitorKey {
        MonitorKey {
            device: device.into(),
            handle: x as isize + 1,
            primary,
            rect: ScreenRect {
                left: x,
                top: 0,
                right: x + w,
                bottom: h,
            },
            dpi,
        }
    }

    fn three() -> Vec<MonitorKey> {
        vec![
            mon(r"\\.\DISPLAY1", true, 0, 2560, 1440, 120),
            mon(r"\\.\DISPLAY2", false, 2560, 1920, 1080, 96),
            mon(r"\\.\DISPLAY3", false, 4480, 3840, 2160, 144),
        ]
    }

    #[test]
    fn identical_sets_change_nothing() {
        assert_eq!(classify(&three(), &three()), DisplayChange::Unchanged);
        // Order does not matter.
        let mut rev = three();
        rev.reverse();
        assert_eq!(classify(&three(), &rev), DisplayChange::Unchanged);
    }

    #[test]
    fn a_game_changing_the_resolution_only_moves_the_bars() {
        let mut game = three();
        // DISPLAY2 switched to 1280x720 for an exclusive full-screen game.
        game[1].rect.right = game[1].rect.left + 1280;
        game[1].rect.bottom = 720;
        assert_eq!(classify(&three(), &game), DisplayChange::Reposition);
        // And back.
        assert_eq!(classify(&game, &three()), DisplayChange::Reposition);
    }

    #[test]
    fn scale_position_or_handle_changes_move_the_bars() {
        let mut dpi = three();
        dpi[2].dpi = 192;
        assert_eq!(classify(&three(), &dpi), DisplayChange::Reposition);
        let mut pos = three();
        pos[1].rect.top = -200;
        assert_eq!(classify(&three(), &pos), DisplayChange::Reposition);
        let mut handle = three();
        handle[0].handle = 999;
        assert_eq!(classify(&three(), &handle), DisplayChange::Reposition);
    }

    #[test]
    fn added_removed_or_new_primary_rebuilds() {
        let two = three()[..2].to_vec();
        assert_eq!(classify(&three(), &two), DisplayChange::Rebuild);
        assert_eq!(classify(&two, &three()), DisplayChange::Rebuild);
        let mut swapped = three();
        swapped[0].primary = false;
        swapped[1].primary = true;
        assert_eq!(classify(&three(), &swapped), DisplayChange::Rebuild);
        let mut renamed = three();
        renamed[2].device = r"\\.\DISPLAY7".into();
        assert_eq!(classify(&three(), &renamed), DisplayChange::Rebuild);
        // No bars yet (first build).
        assert_eq!(classify(&[], &three()), DisplayChange::Rebuild);
    }

    #[test]
    fn no_monitors_for_a_moment_keeps_the_bars() {
        assert_eq!(classify(&three(), &[]), DisplayChange::Unchanged);
    }
}
