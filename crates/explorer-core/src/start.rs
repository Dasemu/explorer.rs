// SPDX-License-Identifier: GPL-3.0-or-later
//! Start menu state: pinned apps (persisted) and default pins.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::catalog::{AppEntry, Catalog, LaunchTarget};

/// Pinned entries, stored by [`LaunchTarget::key`] in display order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pins {
    /// False until the user (or defaults) set pins; lets us seed defaults
    /// exactly once instead of re-adding pins the user removed.
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub keys: Vec<String>,
    /// The file exists but could not be read (sharing violation, access
    /// denied, ...). While set, the in-memory pins are not authoritative:
    /// defaults are not seeded and nothing is written, so a transient read
    /// failure never overwrites the user's pins. Cleared by a successful
    /// [`Pins::reload`].
    #[serde(skip)]
    pub unreadable: bool,
}

/// Result of reading a pins file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinsRead {
    Loaded(Pins),
    /// No file yet (first run, or the user deleted it).
    NotFound,
    /// The file was read but is not valid TOML / not a pins file.
    Corrupt(String),
    /// The file could not be read (e.g. locked); try again later.
    Unreadable(String),
}

/// What [`Pins::reload`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    /// The in-memory pins now match the file (or the defaults when there
    /// is no file).
    Replaced,
    /// The file was corrupt: it was moved to `*.corrupt`, the in-memory
    /// pins were kept and written back.
    RecoveredCorrupt,
    /// The file could not be read; the in-memory pins were kept. Call
    /// `reload` again later (do not record the file's modification time).
    Retry,
}

/// Default taskbar pins (Windows 11 defaults), same matching rules as
/// [`DEFAULT_PIN_IDS`]. Used with `Pins::seed_with` for
/// `AppPaths::taskbar_pins_file`, written by both the taskbar and the Start
/// menu ("Pin to taskbar"). Readers should re-load the file when its
/// modification time changes.
pub const DEFAULT_TASKBAR_PIN_IDS: &[&str] = &[
    "Microsoft.Windows.Explorer",
    "MSEdge",
    "Microsoft.WindowsStore_",
];

/// Packaged-app AUMID prefixes (and exact ids) pinned by default, in order,
/// when present on the machine.
pub const DEFAULT_PIN_IDS: &[&str] = &[
    "MSEdge",
    "Microsoft.Windows.Explorer",
    "windows.immersivecontrolpanel_",
    "Microsoft.WindowsStore_",
    "Microsoft.WindowsTerminal_",
    "Microsoft.WindowsNotepad_",
    "Microsoft.WindowsCalculator_",
    "Microsoft.Windows.Photos_",
    "Microsoft.Paint_",
    "Microsoft.ScreenSketch_",
];

impl Pins {
    /// Read and parse `path` without side effects.
    pub fn read(path: &Path) -> PinsRead {
        match std::fs::read_to_string(path) {
            Ok(s) => match toml::from_str::<Pins>(&s) {
                Ok(p) => PinsRead::Loaded(p),
                Err(e) => PinsRead::Corrupt(e.to_string()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => PinsRead::NotFound,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                PinsRead::Corrupt(e.to_string())
            }
            Err(e) => PinsRead::Unreadable(e.to_string()),
        }
    }

    /// Load the pins at start-up (see [`Pins::reload`] for error handling).
    pub fn load(path: &Path) -> Self {
        let mut p = Self::default();
        p.reload(path);
        p
    }

    /// Re-read `path`. A missing file yields the defaults (seeded later);
    /// a corrupt file is moved aside once (`*.corrupt`) and the in-memory
    /// pins are kept and written back; an unreadable file leaves
    /// everything as it is and marks the pins [`Pins::unreadable`].
    pub fn reload(&mut self, path: &Path) -> Reload {
        match Self::read(path) {
            PinsRead::Loaded(p) => {
                *self = p;
                Reload::Replaced
            }
            PinsRead::NotFound => {
                *self = Self::default();
                Reload::Replaced
            }
            PinsRead::Corrupt(e) => {
                let bad = path.with_extension("toml.corrupt");
                tracing::warn!(path = %path.display(), moved_to = %bad.display(), error = %e, "pins file is corrupt; keeping the pins in memory");
                let _ = std::fs::remove_file(&bad);
                if let Err(e) = std::fs::rename(path, &bad) {
                    tracing::warn!(error = %e, "cannot move the corrupt pins file aside");
                }
                // Never seed defaults over what the user had.
                self.initialized = true;
                self.unreadable = false;
                if let Err(e) = self.save(path) {
                    tracing::warn!(error = %e, "cannot rewrite the pins file");
                }
                Reload::RecoveredCorrupt
            }
            PinsRead::Unreadable(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read the pins file; keeping the pins in memory");
                self.unreadable = true;
                Reload::Retry
            }
        }
    }

    /// If the last read failed, try again (before changing the pins).
    pub fn retry_if_unreadable(&mut self, path: &Path) {
        if self.unreadable {
            self.reload(path);
        }
    }

    /// Write atomically (unique temporary file + rename). Refuses while
    /// the file could not be read, so it is never overwritten blindly.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if self.unreadable {
            return Err(std::io::Error::other(
                "the pins file could not be read; not overwriting it",
            ));
        }
        let s = toml::to_string(self).map_err(std::io::Error::other)?;
        crate::fsutil::write_atomic(path, s.as_bytes())
    }

    pub fn contains(&self, t: &LaunchTarget) -> bool {
        self.keys.contains(&t.key())
    }

    pub fn pin(&mut self, t: &LaunchTarget) -> bool {
        self.initialized = true;
        let k = t.key();
        if self.keys.contains(&k) {
            return false;
        }
        self.keys.push(k);
        true
    }

    pub fn unpin(&mut self, t: &LaunchTarget) -> bool {
        self.initialized = true;
        let k = t.key();
        let before = self.keys.len();
        self.keys.retain(|x| x != &k);
        before != self.keys.len()
    }

    /// Move a pin to the front (Windows' "Move to front").
    pub fn move_to_front(&mut self, t: &LaunchTarget) {
        let k = t.key();
        if let Some(i) = self.keys.iter().position(|x| x == &k) {
            let k = self.keys.remove(i);
            self.keys.insert(0, k);
        }
    }

    /// Seed the Start menu defaults once, from the apps in `catalog`.
    pub fn seed_defaults(&mut self, catalog: &Catalog) -> bool {
        self.seed_with(DEFAULT_PIN_IDS, catalog)
    }

    /// Seed `ids` once (see [`DEFAULT_PIN_IDS`] for the matching rules).
    pub fn seed_with(&mut self, ids: &[&str], catalog: &Catalog) -> bool {
        if self.initialized || self.unreadable || catalog.is_empty() {
            return false;
        }
        for id in ids {
            let found = catalog.entries().iter().find(|e| match &e.target {
                LaunchTarget::AppsFolder(a) => {
                    if id.ends_with('_') {
                        a.starts_with(id)
                    } else {
                        a == id
                    }
                }
                LaunchTarget::Path(_) => false,
            });
            if let Some(e) = found {
                let k = e.target.key();
                if !self.keys.contains(&k) {
                    self.keys.push(k);
                }
            }
        }
        self.initialized = true;
        true
    }

    /// Resolve pins to catalog entries, skipping uninstalled apps.
    pub fn resolve<'a>(&self, catalog: &'a Catalog) -> Vec<&'a AppEntry> {
        self.keys
            .iter()
            .filter_map(|k| catalog.entries().iter().find(|e| &e.target.key() == k))
            .collect()
    }
}

/// Number of pages of the pinned grid (at least one, even when empty).
pub fn page_count(total: usize, per_page: usize) -> usize {
    if per_page == 0 {
        return 1;
    }
    total.div_ceil(per_page).max(1)
}

/// Indexes of the pins shown on `page` (clamped to the last page).
pub fn page_range(total: usize, per_page: usize, page: usize) -> std::ops::Range<usize> {
    if per_page == 0 {
        return 0..0;
    }
    let page = page.min(page_count(total, per_page) - 1);
    let start = page * per_page;
    start.min(total)..(start + per_page).min(total)
}

/// Group an alphabetical app list under first-letter headers, as Windows'
/// "All apps" view does. Non-letters go under '#'.
pub fn letter_of(name: &str) -> char {
    name.chars()
        .next()
        .filter(|c| c.is_alphabetic())
        .map(|c| c.to_uppercase().next().unwrap_or(c))
        .unwrap_or('#')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::AppSource;

    fn app(name: &str, id: &str) -> AppEntry {
        AppEntry {
            name: name.into(),
            target: LaunchTarget::AppsFolder(id.into()),
            source: AppSource::AppsFolder,
            keywords: vec![],
        }
    }

    fn catalog() -> Catalog {
        Catalog::build([
            app(
                "Calculadora",
                "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
            ),
            app("Microsoft Edge", "MSEdge"),
            app(
                "Bloc de notas",
                "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App",
            ),
            app("Random", "Foo_123!App"),
        ])
    }

    #[test]
    fn defaults_seeded_once_in_order() {
        let c = catalog();
        let mut p = Pins::default();
        assert!(p.seed_defaults(&c));
        let names: Vec<_> = p.resolve(&c).iter().map(|e| e.name.clone()).collect();
        assert_eq!(
            names,
            vec!["Microsoft Edge", "Bloc de notas", "Calculadora"]
        );
        // User removes everything: defaults must not come back.
        for e in c.entries() {
            p.unpin(&e.target);
        }
        assert!(!p.seed_defaults(&c));
        assert!(p.keys.is_empty());
    }

    #[test]
    fn pin_unpin_front_and_persist() {
        let c = catalog();
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("pins.toml");
        let mut p = Pins::default();
        let r = &c.entries()[3].target;
        assert!(p.pin(r));
        assert!(!p.pin(r));
        p.pin(&c.entries()[0].target);
        p.move_to_front(&c.entries()[0].target);
        assert_eq!(p.keys[0], c.entries()[0].target.key());
        p.save(&path).unwrap();
        assert_eq!(Pins::load(&path), p);
        assert!(p.unpin(r));
        assert!(!p.contains(r));
    }

    #[test]
    fn corrupt_file_is_moved_aside_and_pins_kept() {
        let c = catalog();
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("pins.toml");
        let mut p = Pins::default();
        p.pin(&c.entries()[3].target);
        p.save(&path).unwrap();
        std::fs::write(&path, "nonsense = [").unwrap();
        assert_eq!(p.reload(&path), Reload::RecoveredCorrupt);
        assert_eq!(p.keys, vec![c.entries()[3].target.key()]);
        assert!(path.with_extension("toml.corrupt").exists());
        // Written back, valid again.
        assert_eq!(Pins::load(&path), p);
        // A corrupt file at start-up: no defaults are seeded over it.
        std::fs::write(&path, "keys = 5").unwrap();
        let mut fresh = Pins::load(&path);
        assert!(fresh.initialized && fresh.keys.is_empty());
        assert!(!fresh.seed_defaults(&c));
    }

    #[test]
    fn unreadable_file_keeps_pins_and_blocks_writes() {
        let c = catalog();
        let d = tempfile::tempdir().unwrap();
        // A directory where the file should be: exists but cannot be read.
        let path = d.path().join("pins.toml");
        std::fs::create_dir(&path).unwrap();
        let mut p = Pins::default();
        p.pin(&c.entries()[0].target);
        assert_eq!(p.reload(&path), Reload::Retry);
        assert!(p.unreadable);
        assert_eq!(p.keys.len(), 1);
        assert!(p.save(&path).is_err());
        let mut fresh = Pins::load(&path);
        assert!(fresh.unreadable);
        assert!(!fresh.seed_defaults(&c), "no defaults over unreadable pins");
        // Readable again: the file wins.
        std::fs::remove_dir(&path).unwrap();
        Pins::default().save(&path).unwrap();
        fresh.retry_if_unreadable(&path);
        assert!(!fresh.unreadable);
        assert!(!path.with_extension("toml.corrupt").exists());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let d = tempfile::tempdir().unwrap();
        let mut p = Pins {
            initialized: true,
            keys: vec!["x".into()],
            unreadable: false,
        };
        assert_eq!(p.reload(&d.path().join("none.toml")), Reload::Replaced);
        assert_eq!(p, Pins::default());
    }

    #[test]
    fn uninstalled_pins_are_skipped() {
        let mut p = Pins::default();
        p.keys.push("appsfolder:gone".into());
        assert!(p.resolve(&catalog()).is_empty());
    }

    #[test]
    fn paging() {
        assert_eq!(page_count(0, 18), 1);
        assert_eq!(page_count(18, 18), 1);
        assert_eq!(page_count(19, 18), 2);
        assert_eq!(page_range(19, 18, 0), 0..18);
        assert_eq!(page_range(19, 18, 1), 18..19);
        assert_eq!(page_range(19, 18, 7), 18..19);
        assert_eq!(page_range(0, 18, 0), 0..0);
        assert_eq!(page_range(5, 0, 0), 0..0);
    }

    #[test]
    fn letters() {
        assert_eq!(letter_of("árbol"), 'Á');
        assert_eq!(letter_of("7-Zip"), '#');
        assert_eq!(letter_of(""), '#');
    }
}
