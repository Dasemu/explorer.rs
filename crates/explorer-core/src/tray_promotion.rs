// SPDX-License-Identifier: GPL-3.0-or-later
//! Which notification-area icons are shown on the taskbar ("promoted") and
//! which go into the overflow flyout behind the chevron.
//!
//! Windows 11 keeps one registry key per icon under
//! `HKCU\Control Panel\NotifyIconSettings\<id>` with (among others) the
//! values `ExecutablePath` (often known-folder relative, see
//! [`crate::shellpath`]), `UID` (the icon's `uID`, absent for GUID icons)
//! and `IsPromoted` (DWORD, 1 = shown on the taskbar; absent = not
//! promoted). Settings > Personalization > Taskbar > "Other system tray
//! icons" toggles `IsPromoted`.
//!
//! We map our icons (owner window -> process image path) to those entries.
//! Icons without an entry follow Windows' default for new icons: hidden in
//! the overflow.

use crate::shellpath::same_path;

/// One `NotifyIconSettings` entry, with its path already expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifySetting {
    pub executable_path: String,
    pub uid: Option<u32>,
    pub is_promoted: bool,
}

/// Promotion state of an icon owned by `exe_path` with `uid`.
///
/// - No entry for the executable: `None` (caller applies the default).
/// - Exactly one entry: its value.
/// - Several entries (an app with several icons): the one whose `UID`
///   matches; if none matches, promoted if any of them is promoted (better
///   to show an icon than to lose it).
pub fn lookup(settings: &[NotifySetting], exe_path: Option<&str>, uid: u32) -> Option<bool> {
    let exe = exe_path?;
    let same: Vec<&NotifySetting> = settings
        .iter()
        .filter(|s| same_path(&s.executable_path, exe))
        .collect();
    match same.as_slice() {
        [] => None,
        [one] => Some(one.is_promoted),
        many => Some(
            many.iter()
                .find(|s| s.uid == Some(uid))
                .map(|s| s.is_promoted)
                .unwrap_or_else(|| many.iter().any(|s| s.is_promoted)),
        ),
    }
}

/// Final decision for one icon.
pub fn is_promoted(
    settings: &[NotifySetting],
    exe_path: Option<&str>,
    uid: u32,
    show_all: bool,
) -> bool {
    show_all || lookup(settings, exe_path, uid).unwrap_or(false)
}

/// Split icon indices into (on the bar, in the overflow flyout), keeping
/// order.
pub fn partition(promoted: &[bool]) -> (Vec<usize>, Vec<usize>) {
    let mut bar = Vec::new();
    let mut overflow = Vec::new();
    for (i, &p) in promoted.iter().enumerate() {
        if p {
            bar.push(i);
        } else {
            overflow.push(i);
        }
    }
    (bar, overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(path: &str, uid: Option<u32>, promoted: bool) -> NotifySetting {
        NotifySetting {
            executable_path: path.into(),
            uid,
            is_promoted: promoted,
        }
    }

    #[test]
    fn single_entry_decides() {
        let v = vec![
            s(r"C:\Program Files\A\a.exe", Some(1), true),
            s(r"C:\B\b.exe", None, false),
        ];
        assert_eq!(lookup(&v, Some(r"c:\program files\a\A.EXE"), 7), Some(true));
        assert_eq!(lookup(&v, Some(r"C:\B\b.exe"), 0), Some(false));
        assert_eq!(lookup(&v, Some(r"C:\C\c.exe"), 0), None);
        assert_eq!(lookup(&v, None, 0), None);
    }

    #[test]
    fn several_entries_use_uid() {
        let v = vec![
            s(r"C:\A\a.exe", Some(1), false),
            s(r"C:\A\a.exe", Some(2), true),
        ];
        assert_eq!(lookup(&v, Some(r"C:\A\a.exe"), 1), Some(false));
        assert_eq!(lookup(&v, Some(r"C:\A\a.exe"), 2), Some(true));
        // Unknown uid: any promoted wins.
        assert_eq!(lookup(&v, Some(r"C:\A\a.exe"), 9), Some(true));
    }

    #[test]
    fn defaults_and_show_all() {
        assert!(!is_promoted(&[], Some(r"C:\x.exe"), 0, false));
        assert!(is_promoted(&[], Some(r"C:\x.exe"), 0, true));
        assert!(is_promoted(&[], None, 0, true));
    }

    #[test]
    fn partition_keeps_order() {
        assert_eq!(
            partition(&[true, false, true, false]),
            (vec![0, 2], vec![1, 3])
        );
        assert_eq!(partition(&[]), (vec![], vec![]));
    }
}
