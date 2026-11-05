// SPDX-License-Identifier: GPL-3.0-or-later
//! Taskbar buttons as Windows 11 shows them: pinned apps first (in pin
//! order), then running apps; windows of the same app combined into one
//! button.
//!
//! Inputs are plain data gathered by the Win32 layer:
//! - [`WindowIdent`]: a window that passed the taskbar filter, with its
//!   AppUserModelID (if any) and process image path;
//! - [`PinnedApp`]: a pinned entry with whatever identity could be resolved
//!   (AppsFolder id / AUMID, executable path, display name).
//!
//! Grouping key: AUMID if the window has one, otherwise the process image
//! path (case-insensitive), otherwise the window itself.
//!
//! Pin matching (pure, see [`match_pin`]): AUMID equality first, then
//! executable path equality, then case-insensitive display-name equality as
//! a last resort.

use crate::catalog::LaunchTarget;
use crate::config::Combine;
use crate::shellpath::{file_stem, normalize, same_path};

/// A window that gets a taskbar presence.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowIdent {
    pub hwnd: u64,
    pub title: String,
    /// AppUserModelID from the window's property store.
    pub app_id: Option<String>,
    /// Full process image path.
    pub exe_path: Option<String>,
    /// Display name of the app if known (catalog lookup), for tooltips and
    /// menus of unpinned apps.
    pub app_name: Option<String>,
    pub flashing: bool,
}

impl WindowIdent {
    /// Grouping key; windows with equal keys share one button.
    pub fn group_key(&self) -> String {
        if let Some(id) = self.app_id.as_deref().filter(|s| !s.is_empty()) {
            format!("aumid:{}", id.to_lowercase())
        } else if let Some(p) = self.exe_path.as_deref().filter(|s| !s.is_empty()) {
            format!("exe:{}", normalize(p))
        } else {
            format!("hwnd:{}", self.hwnd)
        }
    }
}

/// A pinned app with the identity information available for matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedApp {
    /// Key in the pins file (`LaunchTarget::key`).
    pub key: String,
    pub name: String,
    pub target: LaunchTarget,
    /// AppUserModelID / AppsFolder id when it is not a path.
    pub app_id: Option<String>,
    /// Executable this pin starts (expanded known-folder path, `.lnk`
    /// target, or the path itself).
    pub exe_path: Option<String>,
}

/// How a window matched a pin; higher is stronger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchKind {
    Name = 1,
    Exe = 2,
    AppId = 3,
}

fn eq_ci(a: &str, b: &str) -> bool {
    !a.is_empty() && a.to_lowercase() == b.to_lowercase()
}

/// Does window `w` belong to pinned app `pin`?
///
/// 1. Both have an AUMID: equal -> match, different -> never a match (an
///    Edge web app has its own AUMID but runs `msedge.exe`).
/// 2. Executable paths equal.
/// 3. Last resort, only when neither side has a conflicting AUMID: the
///    pin's display name equals the executable's file stem or the window
///    title (case-insensitive).
pub fn match_pin(pin: &PinnedApp, w: &WindowIdent) -> Option<MatchKind> {
    match (pin.app_id.as_deref(), w.app_id.as_deref()) {
        (Some(a), Some(b)) if eq_ci(a, b) => return Some(MatchKind::AppId),
        (Some(_), Some(_)) => return None,
        _ => {}
    }
    if let (Some(a), Some(b)) = (pin.exe_path.as_deref(), w.exe_path.as_deref()) {
        if same_path(a, b) {
            return Some(MatchKind::Exe);
        }
    }
    if pin.app_id.is_some() && w.app_id.is_none() && pin.exe_path.is_none() {
        // A packaged/AUMID pin vs. a classic window: only the AUMID could
        // have proved it; names are too weak here ("Terminal" vs. a
        // window titled "Terminal").
        return None;
    }
    let name = pin.name.trim();
    let by_stem = w
        .exe_path
        .as_deref()
        .is_some_and(|p| eq_ci(name, file_stem(p)));
    let by_title = eq_ci(name, w.title.trim());
    (by_stem || by_title).then_some(MatchKind::Name)
}

/// One taskbar button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppButton {
    /// Stable identity (hover/drag tracking across rebuilds).
    pub id: String,
    /// Index into the pins slice.
    pub pin: Option<usize>,
    /// App name (pin name, catalog name, or the first window's title).
    pub name: String,
    /// Windows in taskbar order (empty: pinned, not running).
    pub windows: Vec<u64>,
    pub active: bool,
    pub flashing: bool,
}

impl AppButton {
    pub fn is_running(&self) -> bool {
        !self.windows.is_empty()
    }
}

/// Best pin for a group of windows: strongest match, then lowest index.
fn best_pin(pins: &[PinnedApp], group: &[&WindowIdent]) -> Option<usize> {
    let mut best: Option<(MatchKind, usize)> = None;
    for (i, p) in pins.iter().enumerate() {
        let m = group.iter().filter_map(|w| match_pin(p, w)).max();
        if let Some(m) = m {
            if best.is_none_or(|(bm, _)| m > bm) {
                best = Some((m, i));
            }
        }
    }
    best.map(|(_, i)| i)
}

/// Build the button list. `windows` is in taskbar order (first appearance).
pub fn build_buttons(
    pins: &[PinnedApp],
    windows: &[WindowIdent],
    active: Option<u64>,
    combine: Combine,
) -> Vec<AppButton> {
    // Group windows by key, keeping first-appearance order.
    let mut groups: Vec<(String, Vec<&WindowIdent>)> = Vec::new();
    for w in windows {
        let k = w.group_key();
        match groups.iter_mut().find(|(gk, _)| *gk == k) {
            Some((_, v)) => v.push(w),
            None => groups.push((k, vec![w])),
        }
    }
    let assigned: Vec<Option<usize>> = groups.iter().map(|(_, g)| best_pin(pins, g)).collect();

    let make = |id: String, pin: Option<usize>, name: String, ws: &[&WindowIdent]| AppButton {
        id,
        pin,
        name,
        windows: ws.iter().map(|w| w.hwnd).collect(),
        active: ws.iter().any(|w| Some(w.hwnd) == active),
        flashing: ws.iter().any(|w| w.flashing && Some(w.hwnd) != active),
    };
    let window_name = |w: &WindowIdent| w.app_name.clone().unwrap_or_else(|| w.title.clone());

    let mut out = Vec::new();
    for (pi, pin) in pins.iter().enumerate() {
        let ws: Vec<&WindowIdent> = groups
            .iter()
            .zip(assigned.iter())
            .filter(|(_, a)| **a == Some(pi))
            .flat_map(|((_, g), _)| g.iter().copied())
            .collect();
        match combine {
            Combine::Always => out.push(make(
                format!("pin:{}", pin.key),
                Some(pi),
                pin.name.clone(),
                &ws,
            )),
            Combine::Never if ws.is_empty() => out.push(make(
                format!("pin:{}", pin.key),
                Some(pi),
                pin.name.clone(),
                &[],
            )),
            Combine::Never => {
                for w in ws {
                    out.push(make(
                        format!("win:{}", w.hwnd),
                        Some(pi),
                        pin.name.clone(),
                        &[w],
                    ));
                }
            }
        }
    }
    for ((key, g), a) in groups.iter().zip(assigned.iter()) {
        if a.is_some() {
            continue;
        }
        match combine {
            Combine::Always => out.push(make(key.clone(), None, window_name(g[0]), g)),
            Combine::Never => {
                for w in g {
                    out.push(make(format!("win:{}", w.hwnd), None, window_name(w), &[w]));
                }
            }
        }
    }
    out
}

/// Label shown next to the icon when labels are enabled: the window title
/// for single-window buttons, the app name otherwise.
pub fn button_label(b: &AppButton, title_of: impl Fn(u64) -> Option<String>) -> String {
    match b.windows.as_slice() {
        [one] => title_of(*one).unwrap_or_else(|| b.name.clone()),
        _ => b.name.clone(),
    }
}

/// What a left click on a button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupClick {
    /// Not running: start the app.
    Launch,
    /// One window: activate it.
    Activate(u64),
    /// One window that is already active: minimize it.
    Minimize(u64),
    /// Several windows: show the window picker.
    Picker,
}

pub fn click(b: &AppButton, active: Option<u64>, is_minimized: impl Fn(u64) -> bool) -> GroupClick {
    match b.windows.as_slice() {
        [] => GroupClick::Launch,
        [w] if active == Some(*w) && !is_minimized(*w) => GroupClick::Minimize(*w),
        [w] => GroupClick::Activate(*w),
        _ => GroupClick::Picker,
    }
}

// ---------------------------------------------------------------------------
// Drag & drop of pinned buttons
// ---------------------------------------------------------------------------

/// Insertion slot (0..=n) for a drop at `x` among buttons with the given
/// horizontal centres.
pub fn drop_slot(centers: &[f32], x: f32) -> usize {
    centers.iter().take_while(|&&c| x > c).count()
}

/// Move `keys[from]` so that it ends up at insertion slot `slot` (computed
/// on the list *before* removal). Returns true if the order changed.
pub fn move_to_slot<T>(keys: &mut Vec<T>, from: usize, slot: usize) -> bool {
    if from >= keys.len() {
        return false;
    }
    let slot = slot.min(keys.len());
    let to = if slot > from { slot - 1 } else { slot };
    if to == from {
        return false;
    }
    let k = keys.remove(from);
    keys.insert(to, k);
    true
}

/// Apply a drag of the visible pinned button `from` to insertion `slot`
/// (both relative to `visible`, the keys of the pinned buttons on the bar)
/// to the full pin list. Keys that are not visible (uninstalled apps) keep
/// their positions. Returns true if the order changed.
pub fn reorder_pins(keys: &mut [String], visible: &[String], from: usize, slot: usize) -> bool {
    let mut order = visible.to_vec();
    if !move_to_slot(&mut order, from, slot) {
        return false;
    }
    let mut next = order.into_iter();
    for k in keys.iter_mut() {
        if visible.contains(k) {
            if let Some(n) = next.next() {
                *k = n;
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Show desktop
// ---------------------------------------------------------------------------

/// Toggle state of the "Show desktop" corner button.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ShowDesktop {
    minimized: Option<Vec<u64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShowDesktopAction {
    /// Minimize these windows (top-most first).
    Minimize(Vec<u64>),
    /// Restore these windows (bottom-most first, so the original top window
    /// ends on top).
    Restore(Vec<u64>),
}

impl ShowDesktop {
    pub fn is_showing(&self) -> bool {
        self.minimized.is_some()
    }

    /// `visible` = taskbar windows that are not minimized, in Z order (top
    /// first).
    pub fn toggle(&mut self, visible: &[u64]) -> ShowDesktopAction {
        match self.minimized.take() {
            Some(mut list) if !list.is_empty() => {
                list.reverse();
                ShowDesktopAction::Restore(list)
            }
            _ => {
                let list = visible.to_vec();
                self.minimized = Some(list.clone());
                ShowDesktopAction::Minimize(list)
            }
        }
    }

    /// The user brought a window back (activated a taskbar window after the
    /// minimize sequence settled): forget the toggle, as Windows does.
    pub fn reset(&mut self) {
        self.minimized = None;
    }

    pub fn forget(&mut self, hwnd: u64) {
        if let Some(l) = self.minimized.as_mut() {
            l.retain(|&h| h != hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(h: u64, aumid: Option<&str>, exe: Option<&str>) -> WindowIdent {
        WindowIdent {
            hwnd: h,
            title: format!("w{h}"),
            app_id: aumid.map(Into::into),
            exe_path: exe.map(Into::into),
            ..Default::default()
        }
    }

    fn pin(name: &str, app_id: Option<&str>, exe: Option<&str>) -> PinnedApp {
        PinnedApp {
            key: format!("k:{name}"),
            name: name.into(),
            target: LaunchTarget::AppsFolder(name.into()),
            app_id: app_id.map(Into::into),
            exe_path: exe.map(Into::into),
        }
    }

    #[test]
    fn group_keys() {
        assert_eq!(
            win(1, Some("MSEdge"), Some("x")).group_key(),
            "aumid:msedge"
        );
        assert_eq!(
            win(1, None, Some(r"C:\A\B.exe")).group_key(),
            r"exe:c:\a\b.exe"
        );
        assert_eq!(win(7, Some(""), None).group_key(), "hwnd:7");
    }

    #[test]
    fn matching_priorities() {
        let edge = pin("Microsoft Edge", Some("MSEdge"), None);
        assert_eq!(
            match_pin(&edge, &win(1, Some("msedge"), None)),
            Some(MatchKind::AppId)
        );
        // An Edge web app: different AUMID, same exe -> not Edge.
        let edge_exe = pin(
            "Microsoft Edge",
            Some("MSEdge"),
            Some(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
        );
        assert_eq!(
            match_pin(
                &edge_exe,
                &win(
                    2,
                    Some("MSEdge._crx_abc"),
                    Some(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe")
                )
            ),
            None
        );
        let ff = pin(
            "Firefox",
            None,
            Some(r"C:\Program Files\Mozilla Firefox\firefox.exe"),
        );
        assert_eq!(
            match_pin(
                &ff,
                &win(
                    3,
                    None,
                    Some(r"c:\program files\mozilla firefox\FIREFOX.EXE")
                )
            ),
            Some(MatchKind::Exe)
        );
        // Name fallback: pin with unknown exe vs. exe stem.
        let np = pin("Notepad++", None, None);
        assert_eq!(
            match_pin(&np, &win(4, None, Some(r"C:\NPP\notepad++.exe"))),
            Some(MatchKind::Name)
        );
        assert_eq!(match_pin(&np, &win(5, None, Some(r"C:\x\other.exe"))), None);
        // AUMID-only pin never name-matches a classic window.
        let term = pin(
            "w6",
            Some("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"),
            None,
        );
        assert_eq!(match_pin(&term, &win(6, None, Some(r"C:\x\w6.exe"))), None);
    }

    #[test]
    fn pinned_first_and_merged() {
        let pins = vec![
            pin("Explorer", Some("Microsoft.Windows.Explorer"), None),
            pin("Edge", Some("MSEdge"), None),
        ];
        let ws = vec![
            win(10, None, Some(r"C:\Tools\a.exe")),
            win(11, Some("MSEdge"), None),
            win(12, None, Some(r"C:\Tools\A.EXE")),
            win(13, Some("MSEdge"), None),
        ];
        let b = build_buttons(&pins, &ws, Some(13), Combine::Always);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].name, "Explorer");
        assert!(b[0].windows.is_empty() && !b[0].active);
        assert_eq!(b[1].windows, vec![11, 13]);
        assert!(b[1].active);
        assert_eq!(b[2].windows, vec![10, 12]);
        assert_eq!(b[2].pin, None);
        assert_eq!(b[2].name, "w10");
    }

    #[test]
    fn never_combine_gives_one_button_per_window() {
        let pins = vec![
            pin("Edge", Some("MSEdge"), None),
            pin("Gone", Some("Gone"), None),
        ];
        let ws = vec![
            win(11, Some("MSEdge"), None),
            win(12, None, None),
            win(13, Some("MSEdge"), None),
        ];
        let b = build_buttons(&pins, &ws, None, Combine::Never);
        let ids: Vec<_> = b.iter().map(|b| (b.pin, b.windows.clone())).collect();
        assert_eq!(
            ids,
            vec![
                (Some(0), vec![11]),
                (Some(0), vec![13]),
                (Some(1), vec![]),
                (None, vec![12]),
            ]
        );
    }

    #[test]
    fn strongest_pin_wins() {
        // Window matches pin 0 by name and pin 1 by AUMID: pin 1 gets it.
        let pins = vec![pin("w1", None, None), pin("App", Some("X.App"), None)];
        let ws = vec![win(1, Some("X.App"), None)];
        let b = build_buttons(&pins, &ws, None, Combine::Always);
        assert!(b[0].windows.is_empty());
        assert_eq!(b[1].windows, vec![1]);
    }

    #[test]
    fn flashing_and_click() {
        let mut w = win(1, None, Some("a"));
        w.flashing = true;
        let ws = vec![w, win(2, None, Some("a")), win(3, None, Some("b"))];
        let b = build_buttons(&[], &ws, Some(3), Combine::Always);
        assert!(b[0].flashing && !b[0].active);
        assert!(b[1].active);
        assert_eq!(click(&b[0], Some(3), |_| false), GroupClick::Picker);
        assert_eq!(click(&b[1], Some(3), |_| false), GroupClick::Minimize(3));
        assert_eq!(click(&b[1], Some(3), |_| true), GroupClick::Activate(3));
        assert_eq!(click(&b[1], None, |_| false), GroupClick::Activate(3));
        let pinned = build_buttons(&[pin("P", Some("P"), None)], &[], None, Combine::Always);
        assert_eq!(click(&pinned[0], None, |_| false), GroupClick::Launch);
    }

    #[test]
    fn labels() {
        let b = build_buttons(&[], &[win(1, None, Some("a"))], None, Combine::Always);
        assert_eq!(button_label(&b[0], |h| Some(format!("T{h}"))), "T1");
        let b = build_buttons(
            &[],
            &[win(1, None, Some("a")), win(2, None, Some("a"))],
            None,
            Combine::Always,
        );
        assert_eq!(button_label(&b[0], |_| None), "w1");
    }

    #[test]
    fn drag_reorder() {
        let centers = [10.0, 30.0, 50.0];
        assert_eq!(drop_slot(&centers, 0.0), 0);
        assert_eq!(drop_slot(&centers, 35.0), 2);
        assert_eq!(drop_slot(&centers, 99.0), 3);
        let mut k = vec!["a", "b", "c"];
        assert!(!move_to_slot(&mut k, 0, 0));
        assert!(!move_to_slot(&mut k, 0, 1));
        assert!(move_to_slot(&mut k, 0, 3));
        assert_eq!(k, vec!["b", "c", "a"]);
        assert!(move_to_slot(&mut k, 2, 0));
        assert_eq!(k, vec!["a", "b", "c"]);
        assert!(!move_to_slot(&mut k, 9, 0));
        assert!(move_to_slot(&mut k, 1, 99));
        assert_eq!(k, vec!["a", "c", "b"]);
    }

    #[test]
    fn reorder_keeps_hidden_pins_in_place() {
        let mut keys: Vec<String> = ["a", "gone", "b", "c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let visible: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert!(reorder_pins(&mut keys, &visible, 2, 0));
        assert_eq!(keys, vec!["c", "gone", "a", "b"]);
        assert!(!reorder_pins(&mut keys, &visible, 0, 1));
    }

    #[test]
    fn show_desktop_toggles() {
        let mut s = ShowDesktop::default();
        assert_eq!(
            s.toggle(&[1, 2, 3]),
            ShowDesktopAction::Minimize(vec![1, 2, 3])
        );
        assert!(s.is_showing());
        s.forget(2);
        assert_eq!(s.toggle(&[]), ShowDesktopAction::Restore(vec![3, 1]));
        assert!(!s.is_showing());
        s.toggle(&[1]);
        s.reset();
        assert_eq!(s.toggle(&[4]), ShowDesktopAction::Minimize(vec![4]));
        // Nothing left to restore: minimize again.
        let mut s = ShowDesktop::default();
        s.toggle(&[1]);
        s.forget(1);
        assert_eq!(s.toggle(&[5]), ShowDesktopAction::Minimize(vec![5]));
    }
}
