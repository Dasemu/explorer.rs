// SPDX-License-Identifier: GPL-3.0-or-later
//! Which components the supervisor runs, and which ones a config change
//! affects (config hot reload).

use crate::config::{Config, FileSearch, SupervisorConfig};

/// Name of the file-name index daemon component.
pub const INDEXD: &str = "indexd";

/// Registry value written by the installer (`HKLM\Software\explorer-rs`,
/// REG_DWORD 1 = file search chosen, 0 = not chosen).
pub const FILE_SEARCH_SUBKEY: &str = crate::setup::MACHINE_KEY;
pub const FILE_SEARCH_VALUE: &str = crate::setup::VALUE_FILE_SEARCH;

/// True if the TOML text sets `supervisor.components` to something other
/// than the default list. A list equal to the default does not count: config
/// files created from the documented default (`--edit-config`,
/// config.example.toml) contain it verbatim, and must still pick up the
/// installer's file-search choice. Broken TOML counts as "not explicit" (the
/// config falls back to defaults anyway).
pub fn components_explicit(toml_text: &str) -> bool {
    let Some(list) = toml_text
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("supervisor").cloned())
        .and_then(|s| s.as_table().and_then(|s| s.get("components").cloned()))
    else {
        return false;
    };
    let default = SupervisorConfig::default().components;
    let given: Option<Vec<String>> = list.as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    });
    given.as_ref() != Some(&default)
}

/// Final list of components to run.
///
/// - `file_search = "on"`: `indexd` is added if missing;
/// - `file_search = "off"`: `indexd` is removed;
/// - `file_search = "auto"`: if the user listed `components` explicitly the
///   list is used as written; otherwise `indexd` is added when the
///   installer's registry value (`installer_choice`) is 1.
pub fn resolve(
    cfg: &SupervisorConfig,
    explicit: bool,
    installer_choice: Option<u32>,
) -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    for c in &cfg.components {
        if !list.contains(c) {
            list.push(c.clone());
        }
    }
    let want_indexd = match cfg.file_search {
        FileSearch::On => Some(true),
        FileSearch::Off => Some(false),
        FileSearch::Auto if explicit => None,
        FileSearch::Auto => Some(installer_choice == Some(1)),
    };
    match want_indexd {
        Some(true) if !list.iter().any(|c| c == INDEXD) => list.push(INDEXD.into()),
        Some(false) => list.retain(|c| c != INDEXD),
        _ => {}
    }
    list
}

/// Components that must be restarted because the parts of the config they
/// read changed. `[general]` affects everyone (language, theme, logging).
pub fn affected_by_change(old: &Config, new: &Config) -> Vec<&'static str> {
    let general = old.general != new.general;
    let mut out = Vec::new();
    if general || old.taskbar != new.taskbar || old.tray != new.tray {
        out.push("taskbar");
    }
    if general || old.launcher != new.launcher || old.index != new.index {
        out.push("launcher");
    }
    if general || old.index != new.index {
        out.push(INDEXD);
    }
    if general || old.desktop != new.desktop {
        out.push("desktop");
    }
    out
}

/// `(to_start, to_stop)` when the component list changes from `old` to
/// `new`, preserving `new`'s order.
pub fn list_diff(old: &[String], new: &[String]) -> (Vec<String>, Vec<String>) {
    let start = new.iter().filter(|c| !old.contains(c)).cloned().collect();
    let stop = old.iter().filter(|c| !new.contains(c)).cloned().collect();
    (start, stop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_list_in_file_is_not_explicit() {
        let d = SupervisorConfig::default().components;
        let toml_default = format!("[supervisor]\ncomponents = {:?}\n", d);
        assert!(!components_explicit(&toml_default));
        assert!(!components_explicit(crate::config::DEFAULT_CONFIG_TOML));
        assert!(components_explicit(
            "[supervisor]\ncomponents = [\"taskbar\"]\n"
        ));
        assert!(!components_explicit("[general]\n"));
        assert!(!components_explicit("not toml ["));
    }

    fn sup(components: &[&str], fs: FileSearch) -> SupervisorConfig {
        SupervisorConfig {
            components: components.iter().map(|s| s.to_string()).collect(),
            file_search: fs,
            ..SupervisorConfig::default()
        }
    }

    #[test]
    fn explicit_detection() {
        assert!(!components_explicit(""));
        assert!(!components_explicit("[supervisor]\ncrash_loop_max = 3\n"));
        assert!(components_explicit("[supervisor]\ncomponents = []\n"));
        assert!(!components_explicit("[taskbar]\ncomponents = 1\n"));
        assert!(!components_explicit("not toml = = ="));
    }

    #[test]
    fn auto_follows_installer_unless_explicit() {
        let c = sup(&["taskbar", "launcher"], FileSearch::Auto);
        assert_eq!(
            resolve(&c, false, Some(1)),
            ["taskbar", "launcher", "indexd"]
        );
        assert_eq!(resolve(&c, false, Some(0)), ["taskbar", "launcher"]);
        assert_eq!(resolve(&c, false, None), ["taskbar", "launcher"]);
        assert_eq!(resolve(&c, true, Some(1)), ["taskbar", "launcher"]);
        let with = sup(&["taskbar", "indexd"], FileSearch::Auto);
        assert_eq!(resolve(&with, true, Some(0)), ["taskbar", "indexd"]);
    }

    #[test]
    fn on_off_force() {
        let c = sup(&["taskbar", "indexd", "taskbar"], FileSearch::Off);
        assert_eq!(resolve(&c, true, Some(1)), ["taskbar"]);
        let c = sup(&["taskbar"], FileSearch::On);
        assert_eq!(resolve(&c, true, Some(0)), ["taskbar", "indexd"]);
        let c = sup(&["indexd"], FileSearch::On);
        assert_eq!(resolve(&c, false, None), ["indexd"]);
    }

    #[test]
    fn change_impact() {
        let a = Config::default();
        assert!(affected_by_change(&a, &a).is_empty());
        let mut b = a.clone();
        b.taskbar.height = 40;
        assert_eq!(affected_by_change(&a, &b), ["taskbar"]);
        let mut b = a.clone();
        b.index.launcher_results = 3;
        assert_eq!(affected_by_change(&a, &b), ["launcher", "indexd"]);
        let mut b = a.clone();
        b.general.log_level = "debug".into();
        assert_eq!(affected_by_change(&a, &b).len(), 4);
        let mut b = a.clone();
        b.supervisor.crash_loop_max = 9;
        assert!(affected_by_change(&a, &b).is_empty());
    }

    #[test]
    fn diff() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let (start, stop) = list_diff(&s(&["taskbar", "launcher"]), &s(&["taskbar", "indexd"]));
        assert_eq!(start, ["indexd"]);
        assert_eq!(stop, ["launcher"]);
    }
}
