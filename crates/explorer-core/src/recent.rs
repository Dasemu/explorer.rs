// SPDX-License-Identifier: GPL-3.0-or-later
//! "Recommended" section of the Start menu: recently opened files.
//!
//! Windows keeps one shortcut per recently opened item in
//! `%APPDATA%\Microsoft\Windows\Recent` and touches it on every open, so the
//! shortcut's modification time is the "last opened" time. The launcher
//! lists those `.lnk` files on a worker thread, resolves their targets with
//! `IShellLinkW` and hands the rest to the pure helpers here: ordering,
//! privacy, de-duplication and the bilingual relative time ("Today 14:32" /
//! "Hoy 14:32").

use std::path::{Path, PathBuf};

use crate::clock::DateTime;
use crate::i18n::Lang;

/// Upper bound of shortcuts whose targets are resolved per refresh.
pub const MAX_RESOLVE: usize = 40;

/// A resolved recent item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentFile {
    /// Display name (the target's file name).
    pub name: String,
    pub path: PathBuf,
    /// Last opened, Unix seconds (UTC).
    pub opened: i64,
}

/// `Start_TrackDocs` (HKCU ...\Explorer\Advanced): Windows' "Show recently
/// opened items in Start" switch. Missing means enabled.
pub fn recent_files_enabled(track_docs: Option<u32>) -> bool {
    track_docs != Some(0)
}

/// Shortcut names from the Recent folder, newest first, limited to
/// [`MAX_RESOLVE`]. Only `.lnk` files are kept.
pub fn newest_shortcuts(mut items: Vec<(PathBuf, i64)>) -> Vec<(PathBuf, i64)> {
    items.retain(|(p, _)| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
    });
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    items.truncate(MAX_RESOLVE);
    items
}

/// Whether the target's existence may be checked from a worker thread.
/// Network paths are not checked: a missing server can stall for tens of
/// seconds.
pub fn may_check_exists(target: &Path) -> bool {
    let s = target.to_string_lossy();
    !(s.starts_with("\\\\") || s.starts_with("//"))
}

/// Keep the first (newest) entry per target path, case-insensitively, and
/// at most `max` entries.
pub fn dedupe(items: Vec<RecentFile>, max: usize) -> Vec<RecentFile> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|f| seen.insert(f.path.to_string_lossy().to_lowercase()))
        .take(max)
        .collect()
}

/// Display name for a target path (its last component).
pub fn display_name(target: &Path) -> String {
    target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| target.to_string_lossy().into_owned())
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil date/time for seconds since the epoch (already shifted to local
/// time by the caller).
pub fn civil_from_secs(secs: i64) -> DateTime {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    DateTime {
        year: y.clamp(0, 9999) as u16,
        month: m,
        day: d,
        weekday: ((days + 4).rem_euclid(7)) as u8, // 1970-01-01 was a Thursday
        hour: (rem / 3600) as u8,
        minute: (rem % 3600 / 60) as u8,
        second: (rem % 60) as u8,
    }
}

const MONTHS_EN: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_ES: [&str; 12] = [
    "ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic",
];

/// "Today 14:32" / "Hoy 14:32" style label for when an item was opened.
///
/// Both times are local seconds (Unix seconds plus the UTC offset). Less
/// than a minute ago: "Just now"; under an hour: "12 min ago"; today and
/// yesterday with the time; this year "12 Oct" / "Oct 12"; older with the
/// year. Future times (clock changes) count as "Just now".
pub fn relative_time(lang: Lang, then_local: i64, now_local: i64) -> String {
    let es = lang == Lang::Es;
    let diff = now_local - then_local;
    if diff < 60 {
        return if es { "Ahora mismo" } else { "Just now" }.to_string();
    }
    let t = civil_from_secs(then_local);
    let n = civil_from_secs(now_local);
    let day_t = then_local.div_euclid(86_400);
    let day_n = now_local.div_euclid(86_400);
    if diff < 3600 && day_t == day_n {
        let m = diff / 60;
        return if es {
            format!("Hace {m} min")
        } else {
            format!("{m} min ago")
        };
    }
    let hm = format!("{}:{:02}", t.hour, t.minute);
    if day_t == day_n {
        return if es {
            format!("Hoy {hm}")
        } else {
            format!("Today {hm}")
        };
    }
    if day_t + 1 == day_n {
        return if es {
            format!("Ayer {hm}")
        } else {
            format!("Yesterday {hm}")
        };
    }
    let mi = (t.month as usize).clamp(1, 12) - 1;
    match (es, t.year == n.year) {
        (true, true) => format!("{} {}", t.day, MONTHS_ES[mi]),
        (true, false) => format!("{} {} {}", t.day, MONTHS_ES[mi], t.year),
        (false, true) => format!("{} {}", MONTHS_EN[mi], t.day),
        (false, false) => format!("{} {}, {}", MONTHS_EN[mi], t.day, t.year),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-03 14:32:00 "local".
    fn now() -> i64 {
        days_from_civil(2026, 11, 3) * 86_400 + 14 * 3600 + 32 * 60
    }

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        let t = civil_from_secs(now());
        assert_eq!(
            (t.year, t.month, t.day, t.hour, t.minute),
            (2026, 11, 3, 14, 32)
        );
        assert_eq!(t.weekday, 2); // Tuesday
        let t = civil_from_secs(days_from_civil(2024, 2, 29) * 86_400);
        assert_eq!((t.year, t.month, t.day), (2024, 2, 29));
        let t = civil_from_secs(-1);
        assert_eq!((t.year, t.month, t.day, t.hour), (1969, 12, 31, 23));
    }

    #[test]
    fn relative_labels_bilingual() {
        let n = now();
        assert_eq!(relative_time(Lang::En, n - 10, n), "Just now");
        assert_eq!(relative_time(Lang::Es, n + 500, n), "Ahora mismo");
        assert_eq!(relative_time(Lang::En, n - 12 * 60, n), "12 min ago");
        assert_eq!(relative_time(Lang::Es, n - 12 * 60, n), "Hace 12 min");
        assert_eq!(relative_time(Lang::En, n - 5 * 3600, n), "Today 9:32");
        assert_eq!(relative_time(Lang::Es, n - 5 * 3600, n), "Hoy 9:32");
        assert_eq!(relative_time(Lang::En, n - 20 * 3600, n), "Yesterday 18:32");
        assert_eq!(relative_time(Lang::Es, n - 20 * 3600, n), "Ayer 18:32");
        let oct = days_from_civil(2026, 10, 12) * 86_400;
        assert_eq!(relative_time(Lang::En, oct, n), "Oct 12");
        assert_eq!(relative_time(Lang::Es, oct, n), "12 oct");
        let old = days_from_civil(2025, 9, 1) * 86_400;
        assert_eq!(relative_time(Lang::En, old, n), "Sep 1, 2025");
        assert_eq!(relative_time(Lang::Es, old, n), "1 sept 2025");
    }

    #[test]
    fn minutes_across_midnight_use_yesterday() {
        let midnight = days_from_civil(2026, 11, 3) * 86_400;
        assert_eq!(
            relative_time(Lang::En, midnight - 5 * 60, midnight + 5 * 60),
            "Yesterday 23:55"
        );
    }

    #[test]
    fn ordering_privacy_and_dedupe() {
        assert!(recent_files_enabled(None));
        assert!(recent_files_enabled(Some(1)));
        assert!(!recent_files_enabled(Some(0)));
        let v = newest_shortcuts(vec![
            (PathBuf::from("a.docx.lnk"), 5),
            (PathBuf::from("b.LNK"), 9),
            (PathBuf::from("desktop.ini"), 99),
            (PathBuf::from("c.lnk"), 7),
        ]);
        let names: Vec<_> = v
            .iter()
            .map(|(p, _)| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["b.LNK", "c.lnk", "a.docx.lnk"]);
        let f = |p: &str, t| RecentFile {
            name: display_name(Path::new(p)),
            path: PathBuf::from(p),
            opened: t,
        };
        let d = dedupe(
            vec![f("C:/A.txt", 3), f("c:/a.txt", 2), f("C:/b.txt", 1)],
            5,
        );
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].opened, 3);
        assert_eq!(dedupe(vec![f("x", 1), f("y", 1)], 1).len(), 1);
        assert!(!may_check_exists(Path::new("\\\\srv\\share\\f.txt")));
        assert!(may_check_exists(Path::new("C:\\f.txt")));
        assert_eq!(display_name(Path::new("/tmp/report.pdf")), "report.pdf");
    }
}
