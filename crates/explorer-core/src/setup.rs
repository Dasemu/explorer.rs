// SPDX-License-Identifier: GPL-3.0-or-later
//! Names and small decisions shared between the installer
//! (`explorer-rs-setup.exe`, crate `explorer-setup`) and the programs it
//! installs. The installer's full planning logic lives in
//! `explorer_setup::plan`; only what the other binaries need is here.

/// Machine-wide installer state: `HKLM\Software\explorer-rs` (64-bit view).
pub const MACHINE_KEY: &str = r"Software\explorer-rs";
/// `REG_SZ`: install folder, e.g. `C:\Program Files\explorer-rs`.
pub const VALUE_INSTALL_DIR: &str = "InstallDir";
/// `REG_SZ`: installed version.
pub const VALUE_VERSION: &str = "Version";
/// `REG_SZ`: start mode chosen in the installer (`alongside`, `shell`, `none`).
pub const VALUE_MODE: &str = "Mode";
/// `REG_DWORD`: 1 if "Fast file search" was chosen (index helper task
/// registered), 0 otherwise.
pub const VALUE_FILE_SEARCH: &str = "FileSearch";

/// Per-user `Run` value name used for "run alongside Windows Explorer".
pub const RUN_VALUE_NAME: &str = "explorer-rs";
/// `Run` key, relative to a user hive.
pub const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Supervisor components when the installer enabled fast file search.
///
/// The installer cannot edit every user's `config.toml`, so it records
/// `FileSearch = 1` under [`MACHINE_KEY`] instead. The supervisor should call
/// this with the configured component list, whether the user's config file
/// overrides `supervisor.components` ([`config_overrides_components`]) and
/// the registry hint: `indexd` is appended only when the user did not choose
/// the components themselves.
pub fn components_with_file_search_hint(
    configured: &[String],
    user_sets_components: bool,
    file_search_hint: bool,
) -> Vec<String> {
    let mut out = configured.to_vec();
    if file_search_hint && !user_sets_components && !out.iter().any(|c| c == "indexd") {
        out.push("indexd".into());
    }
    out
}

/// True if the TOML text of a user config sets `[supervisor] components` to
/// something other than the default list, so the user's choice wins over
/// installer defaults. A list equal to the default does not count: config
/// files created from `config.example.toml` contain it verbatim. Invalid
/// TOML counts as "not set" (the config loader reports the error).
pub fn config_overrides_components(toml_text: &str) -> bool {
    let Some(list) = toml_text
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("supervisor").cloned())
        .and_then(|s| s.as_table().and_then(|s| s.get("components").cloned()))
    else {
        return false;
    };
    let default = crate::config::SupervisorConfig::default().components;
    let given: Option<Vec<String>> = list.as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    });
    given.as_ref() != Some(&default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn file_search_hint_adds_indexd_only_when_user_did_not_choose() {
        let base = v(&["taskbar", "launcher"]);
        assert_eq!(
            components_with_file_search_hint(&base, false, true),
            v(&["taskbar", "launcher", "indexd"])
        );
        assert_eq!(components_with_file_search_hint(&base, true, true), base);
        assert_eq!(components_with_file_search_hint(&base, false, false), base);
        let with = v(&["taskbar", "indexd"]);
        assert_eq!(components_with_file_search_hint(&with, false, true), with);
    }

    #[test]
    fn detects_explicit_components() {
        assert!(config_overrides_components(
            "[supervisor]\ncomponents = [\"taskbar\"]\n"
        ));
        assert!(!config_overrides_components(
            "[supervisor]\ncomponents = [\"taskbar\", \"launcher\"]\n"
        ));
        assert!(!config_overrides_components(
            "[general]\nlog_level = \"info\"\n"
        ));
        assert!(!config_overrides_components("not toml ["));
        // The shipped example (a copy of the defaults) is not an override.
        assert!(!config_overrides_components(
            crate::config::DEFAULT_CONFIG_TOML
        ));
    }
}
