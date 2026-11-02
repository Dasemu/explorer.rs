// SPDX-License-Identifier: GPL-3.0-or-later
//! App catalog: the list of launchable entries shown by the launcher.
//!
//! Sources, in priority order when two entries share a display name:
//! 1. Start Menu shortcut folders (user + common) — scanned here with
//!    `std::fs`, so this part is testable on Linux.
//! 2. `shell:AppsFolder` — enumerated by `explorer-win` via the Shell COM API;
//!    adds packaged (UWP/MSIX) apps that have no `.lnk`.
//! 3. Extra folders from the config.
//!
//! Launch history (frecency) is persisted to a small TOML file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AppSource {
    StartMenu,
    AppsFolder,
    Extra,
}

/// How an entry is launched.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LaunchTarget {
    /// A file opened with `ShellExecuteW` (`.lnk`, `.url`, `.exe`, ...).
    Path(PathBuf),
    /// A `shell:AppsFolder` parsing name (AUMID for packaged apps); launched
    /// as `shell:AppsFolder\<id>`.
    AppsFolder(String),
}

impl LaunchTarget {
    /// Stable key used for launch history.
    pub fn key(&self) -> String {
        match self {
            LaunchTarget::Path(p) => format!("path:{}", p.display()).to_lowercase(),
            LaunchTarget::AppsFolder(id) => format!("appsfolder:{id}").to_lowercase(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    pub name: String,
    pub target: LaunchTarget,
    pub source: AppSource,
    /// Extra searchable words (e.g. the containing Start Menu folder).
    pub keywords: Vec<String>,
}

/// File extensions treated as launchable in scanned folders.
pub const LAUNCHABLE_EXTENSIONS: &[&str] = &["lnk", "url", "exe", "appref-ms"];

/// Returns true for shortcuts that are noise in a launcher.
pub fn is_noise(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("uninstall")
        || n.contains("readme")
        || n == "desktop"
        || n.ends_with(" website")
        || n.ends_with(" on the web")
}

/// Document-like extensions that `shell:AppsFolder` lists (manuals, web
/// links) but that are not apps.
const DOC_EXTENSIONS: &[&str] = &[
    ".txt", ".pdf", ".chm", ".htm", ".html", ".rtf", ".md", ".hlp", ".ini", ".log",
];

/// Build an entry from a `shell:AppsFolder` item, filtering noise.
/// `id` is the item's parent-relative parsing name: an AUMID such as
/// `Microsoft.WindowsCalculator_8wekyb3d8bbwe!App` for packaged apps, or a
/// (known-folder-relative) path for classic ones.
pub fn apps_folder_entry(name: &str, id: &str) -> Option<AppEntry> {
    let name = name.trim();
    let id = id.trim();
    if name.is_empty() || id.is_empty() || is_noise(name) {
        return None;
    }
    let lower = id.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return None;
    }
    if DOC_EXTENSIONS.iter().any(|e| lower.ends_with(e)) {
        return None;
    }
    Some(AppEntry {
        name: name.to_string(),
        target: LaunchTarget::AppsFolder(id.to_string()),
        source: AppSource::AppsFolder,
        keywords: Vec::new(),
    })
}

/// True if an AppsFolder id is a packaged-app AUMID (`PackageFamily!AppId`)
/// that can be started with `IApplicationActivationManager`.
pub fn is_aumid(id: &str) -> bool {
    match id.split_once('!') {
        Some((fam, app)) => {
            !fam.is_empty() && !app.is_empty() && !id.contains('\\') && fam.contains('_')
        }
        None => false,
    }
}

/// Recursively scan `root` for launchable files. Unreadable directories are
/// skipped (logged at debug level); recursion depth is bounded.
pub fn scan_folder(root: &Path, source: AppSource) -> Vec<AppEntry> {
    let mut out = Vec::new();
    scan_inner(root, root, source, 0, &mut out);
    out
}

const MAX_DEPTH: usize = 6;

fn scan_inner(root: &Path, dir: &Path, source: AppSource, depth: usize, out: &mut Vec<AppEntry>) {
    if depth > MAX_DEPTH {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            tracing::debug!(dir = %dir.display(), error = %e, "skipping unreadable folder");
            return;
        }
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            scan_inner(root, &path, source, depth + 1, out);
            continue;
        }
        let Some(ext) = path.extension().and_then(|x| x.to_str()) else {
            continue;
        };
        if !LAUNCHABLE_EXTENSIONS
            .iter()
            .any(|l| l.eq_ignore_ascii_case(ext))
        {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if is_noise(stem) {
            continue;
        }
        let keywords = path
            .parent()
            .filter(|p| *p != root)
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default();
        out.push(AppEntry {
            name: stem.to_string(),
            target: LaunchTarget::Path(path.clone()),
            source,
            keywords,
        });
    }
}

/// The merged, deduplicated catalog.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    entries: Vec<AppEntry>,
}

impl Catalog {
    /// Merge entries from all sources. Entries are deduplicated by
    /// case-insensitive display name, keeping the highest-priority source
    /// (see module docs); then sorted alphabetically.
    pub fn build(sources: impl IntoIterator<Item = AppEntry>) -> Self {
        let mut all: Vec<AppEntry> = sources.into_iter().collect();
        all.sort_by_key(|a| a.source);
        let mut seen = HashSet::new();
        let mut entries: Vec<AppEntry> = all
            .into_iter()
            .filter(|e| !e.name.trim().is_empty())
            .filter(|e| seen.insert(e.name.trim().to_lowercase()))
            .collect();
        entries.sort_by_key(|a| a.name.to_lowercase());
        Self { entries }
    }

    pub fn entries(&self) -> &[AppEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Per-entry launch statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchStats {
    pub count: u32,
    /// Unix seconds of the last launch.
    pub last: u64,
}

/// Launch history keyed by [`LaunchTarget::key`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaunchHistory {
    #[serde(default)]
    pub entries: HashMap<String, LaunchStats>,
}

impl LaunchHistory {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Write atomically (temp file + rename) so a crash never leaves a
    /// truncated file behind.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let s = toml::to_string(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, s)?;
        std::fs::rename(&tmp, path)
    }

    pub fn record(&mut self, target: &LaunchTarget, now: u64) {
        let s = self.entries.entry(target.key()).or_default();
        s.count = s.count.saturating_add(1);
        s.last = now;
    }

    pub fn record_now(&mut self, target: &LaunchTarget) {
        self.record(target, unix_now());
    }

    /// Frecency boost in [0, 100]: frequent and recent launches rank higher.
    pub fn boost(&self, target: &LaunchTarget, now: u64) -> u32 {
        let Some(s) = self.entries.get(&target.key()) else {
            return 0;
        };
        let age_days = now.saturating_sub(s.last) / 86_400;
        let recency = match age_days {
            0 => 4,
            1..=6 => 3,
            7..=29 => 2,
            _ => 1,
        };
        let freq = (s.count.min(50) as f32).sqrt(); // 0..~7
        ((freq * recency as f32 * 3.5) as u32).min(100)
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, src: AppSource) -> AppEntry {
        AppEntry {
            name: name.into(),
            target: LaunchTarget::AppsFolder(name.into()),
            source: src,
            keywords: vec![],
        }
    }

    #[test]
    fn scan_finds_shortcuts_and_skips_noise() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("Accessories")).unwrap();
        std::fs::write(root.join("Accessories/Notepad.lnk"), b"").unwrap();
        std::fs::write(root.join("Firefox.LNK"), b"").unwrap();
        std::fs::write(root.join("Uninstall Foo.lnk"), b"").unwrap();
        std::fs::write(root.join("desktop.ini"), b"").unwrap();
        let found = scan_folder(root, AppSource::StartMenu);
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Notepad", "Firefox"]);
        assert_eq!(found[0].keywords, vec!["Accessories".to_string()]);
        assert!(found[1].keywords.is_empty());
    }

    #[test]
    fn scan_missing_folder_is_empty() {
        assert!(scan_folder(Path::new("/definitely/not/here"), AppSource::Extra).is_empty());
    }

    #[test]
    fn dedupe_prefers_start_menu() {
        let c = Catalog::build([
            entry("Calculator", AppSource::AppsFolder),
            entry("calculator", AppSource::StartMenu),
            entry("Zed", AppSource::Extra),
            entry("  ", AppSource::Extra),
        ]);
        assert_eq!(c.len(), 2);
        assert_eq!(c.entries()[0].source, AppSource::StartMenu);
        assert_eq!(c.entries()[1].name, "Zed");
    }

    #[test]
    fn history_roundtrip_and_boost() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("h.toml");
        let t = LaunchTarget::Path(PathBuf::from("C:/x.lnk"));
        let mut h = LaunchHistory::default();
        let now = 1_000_000;
        assert_eq!(h.boost(&t, now), 0);
        h.record(&t, now);
        h.record(&t, now);
        let fresh = h.boost(&t, now);
        assert!(fresh > 0);
        assert!(h.boost(&t, now + 90 * 86_400) < fresh);
        h.save(&p).unwrap();
        assert_eq!(LaunchHistory::load(&p), h);
        // Corrupt file -> empty history, never a crash.
        std::fs::write(&p, "garbage = [").unwrap();
        assert_eq!(LaunchHistory::load(&p), LaunchHistory::default());
    }

    #[test]
    fn apps_folder_filtering() {
        assert!(apps_folder_entry(
            "Calculator",
            "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
        )
        .is_some());
        assert!(apps_folder_entry("Git Release Notes", "C:\\Git\\ReleaseNotes.html").is_none());
        assert!(apps_folder_entry("Site", "https://example.com").is_none());
        assert!(apps_folder_entry("", "x").is_none());
        assert!(apps_folder_entry("Uninstall Foo", "x").is_none());
    }

    #[test]
    fn aumid_detection() {
        assert!(is_aumid("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"));
        assert!(!is_aumid(
            "{6D809377-6AF0-444B-8957-A3773F02200E}\\Git\\git-bash.exe"
        ));
        assert!(!is_aumid("Chrome"));
        assert!(!is_aumid("!App"));
    }

    #[test]
    fn noise_rules() {
        assert!(is_noise("Uninstall Git"));
        assert!(is_noise("Git ReadMe"));
        assert!(!is_noise("Git Bash"));
    }
}
