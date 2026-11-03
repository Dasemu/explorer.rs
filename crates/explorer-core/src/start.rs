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
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let s = toml::to_string(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, s)?;
        std::fs::rename(&tmp, path)
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
        if self.initialized || catalog.is_empty() {
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
        std::fs::write(&path, "nonsense = [").unwrap();
        assert_eq!(Pins::load(&path), Pins::default());
    }

    #[test]
    fn uninstalled_pins_are_skipped() {
        let mut p = Pins::default();
        p.keys.push("appsfolder:gone".into());
        assert!(p.resolve(&catalog()).is_empty());
    }

    #[test]
    fn letters() {
        assert_eq!(letter_of("árbol"), 'Á');
        assert_eq!(letter_of("7-Zip"), '#');
        assert_eq!(letter_of(""), '#');
    }
}
