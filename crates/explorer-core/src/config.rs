// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration model and TOML parsing.
//!
//! The documented default lives in `config/default.toml` at the repository
//! root and is embedded in the binary. A test guarantees that the embedded
//! file and `Config::default()` stay identical.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The documented default configuration file, embedded verbatim.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("../../../config/default.toml");

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid config value: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: GeneralConfig,
    pub taskbar: TaskbarConfig,
    pub tray: TrayConfig,
    pub launcher: LauncherConfig,
    pub supervisor: SupervisorConfig,
    pub index: IndexConfig,
    pub desktop: DesktopConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeneralConfig {
    pub log_level: String,
    pub log_keep_files: usize,
    pub theme: ThemeMode,
    pub language: crate::i18n::LanguageSetting,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
            log_keep_files: 7,
            theme: ThemeMode::System,
            language: crate::i18n::LanguageSetting::System,
        }
    }
}

/// Colour mode: follow Windows, or force dark/light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    System,
    Dark,
    Light,
}

impl ThemeMode {
    pub fn to_theme_mode(self) -> crate::theme::Mode {
        match self {
            ThemeMode::System => crate::theme::Mode::System,
            ThemeMode::Dark => crate::theme::Mode::Dark,
            ThemeMode::Light => crate::theme::Mode::Light,
        }
    }
}

/// Taskbar alignment: follow Windows' "Taskbar alignment" setting, or force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Alignment {
    System,
    Center,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Bottom,
    Top,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MonitorSelection {
    All,
    Primary,
}

/// Windows 11 "Combine taskbar buttons and hide labels".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Combine {
    /// One button per app; several windows open the window picker.
    Always,
    /// One button per window.
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaskbarConfig {
    pub position: Edge,
    pub alignment: Alignment,
    pub button_labels: bool,
    pub height: u32,
    pub monitors: MonitorSelection,
    pub windows_on_own_monitor_only: bool,
    pub button_width: u32,
    pub show_clock: bool,
    pub clock_format: String,
    pub date_format: String,
    pub exclude_classes: Vec<String>,
    pub exclude_processes: Vec<String>,
    pub combine: Combine,
}

impl Default for TaskbarConfig {
    fn default() -> Self {
        Self {
            position: Edge::Bottom,
            alignment: Alignment::System,
            button_labels: false,
            height: 48,
            monitors: MonitorSelection::All,
            windows_on_own_monitor_only: false,
            button_width: 180,
            show_clock: true,
            clock_format: "%H:%M".into(),
            date_format: "%d/%m/%Y".into(),
            exclude_classes: Vec::new(),
            exclude_processes: Vec::new(),
            combine: Combine::Always,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrayConfig {
    pub enabled: bool,
    pub intercept_in_dev_mode: bool,
    pub show_all: bool,
}

impl Default for TrayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            intercept_in_dev_mode: true,
            show_all: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherConfig {
    pub win_key_opens_launcher: bool,
    pub max_results: usize,
    pub extra_folders: Vec<String>,
    pub width: u32,
    pub height: u32,
    pub everything: EverythingConfig,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            win_key_opens_launcher: true,
            max_results: 12,
            extra_folders: Vec::new(),
            width: 640,
            height: 720,
            everything: EverythingConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EverythingConfig {
    pub enabled: bool,
    pub max_results: usize,
}

impl Default for EverythingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_results: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SupervisorConfig {
    pub restart_initial_delay_ms: u64,
    pub restart_max_delay_ms: u64,
    pub crash_loop_max: u32,
    pub crash_loop_window_s: u64,
    pub components: Vec<String>,
    /// Whether the file-name index daemon (`indexd`) runs. See
    /// [`crate::components::resolve`].
    pub file_search: FileSearch,
}

/// `supervisor.file_search`: run the file-name index daemon?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileSearch {
    /// Follow the installer's choice (`HKLM\Software\explorer-rs\FileSearch`)
    /// unless `components` is set explicitly.
    #[default]
    Auto,
    /// Always run `indexd`.
    On,
    /// Never run `indexd`.
    Off,
}

/// `[desktop]` section: the wallpaper window (`explorer-rs-desktop`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopConfig {
    pub enabled: DesktopMode,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            enabled: DesktopMode::Shell,
        }
    }
}

/// When the desktop (wallpaper) window runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopMode {
    /// Only in shell mode (explorer.exe draws the desktop in dev mode).
    #[default]
    Shell,
    /// In both modes.
    Always,
    /// Never.
    Never,
}

impl DesktopMode {
    /// Should the desktop window run in `mode`?
    pub fn enabled_in(self, mode: crate::ipc::RunMode) -> bool {
        match self {
            DesktopMode::Shell => mode == crate::ipc::RunMode::Shell,
            DesktopMode::Always => true,
            DesktopMode::Never => false,
        }
    }
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            restart_initial_delay_ms: 500,
            restart_max_delay_ms: 30_000,
            crash_loop_max: 5,
            crash_loop_window_s: 60,
            components: vec!["taskbar".into(), "launcher".into(), "desktop".into()],
            file_search: FileSearch::Auto,
        }
    }
}

/// Component names accepted in `supervisor.components`.
///
/// `indexd` is not in the default list; it is added by
/// `supervisor.file_search` (see [`crate::components::resolve`]) or by
/// listing it explicitly. `desktop` exits immediately unless enabled by
/// `[desktop] enabled`.
pub const KNOWN_COMPONENTS: &[&str] = &["taskbar", "launcher", "indexd", "desktop"];

/// File-name index daemon (`explorer-rs-indexd`) settings. See docs/INDEX.md.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexConfig {
    /// Folders or volumes to index (e.g. `"C:\\"`, `"D:\\Data"`). Empty means
    /// every fixed local volume on Windows and the home directory elsewhere.
    pub roots: Vec<String>,
    /// Use the NTFS master file table and USN change journal when possible
    /// (requires the elevated helper, see docs/decisions/0010). When false or
    /// unavailable, folders are walked and watched instead.
    pub use_ntfs: bool,
    /// File or folder names never indexed (case-insensitive, exact name or
    /// `*` wildcards).
    pub exclude_names: Vec<String>,
    /// Path prefixes never indexed (case-insensitive).
    pub exclude_paths: Vec<String>,
    /// Hide results whose parent folder the current user cannot list.
    pub check_access: bool,
    /// Seconds between periodic snapshot saves (0 disables periodic saves;
    /// a snapshot is still written on clean shutdown).
    pub snapshot_interval_s: u64,
    /// Seconds between full rescans of walked (non-NTFS) roots that cannot be
    /// watched for changes. 0 disables periodic rescans.
    pub rescan_interval_s: u64,
    /// Upper bound for `limit` in a single query.
    pub max_query_results: usize,
    /// Number of "Files" results shown in the launcher (feature `file-index`).
    pub launcher_results: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            use_ntfs: true,
            exclude_names: vec![
                "$Recycle.Bin".into(),
                "System Volume Information".into(),
                "$WinREAgent".into(),
            ],
            exclude_paths: Vec::new(),
            check_access: true,
            snapshot_interval_s: 600,
            rescan_interval_s: 3600,
            max_query_results: 1000,
            launcher_results: 5,
        }
    }
}

impl Config {
    /// Parse and validate a TOML document.
    pub fn from_toml(s: &str) -> Result<Self, ConfigError> {
        let cfg: Config = toml::from_str(s)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load from a file. A missing file yields the default config; a broken
    /// file is an error (the caller decides whether to fall back).
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Io {
                path: path.display().to_string(),
                source: e,
            }),
        }
    }

    /// Load, falling back to defaults on any error. The error is returned
    /// alongside so it can be logged once logging is up. A shell must start
    /// even with a broken config file.
    pub fn load_or_default(path: &Path) -> (Self, Option<ConfigError>) {
        match Self::load(path) {
            Ok(c) => (c, None),
            Err(e) => (Self::default(), Some(e)),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let t = &self.taskbar;
        if !(16..=200).contains(&t.height) {
            return Err(ConfigError::Invalid(format!(
                "taskbar.height must be between 16 and 200, got {}",
                t.height
            )));
        }
        if !(40..=1000).contains(&t.button_width) {
            return Err(ConfigError::Invalid(format!(
                "taskbar.button_width must be between 40 and 1000, got {}",
                t.button_width
            )));
        }
        let l = &self.launcher;
        if l.max_results == 0 || l.max_results > 100 {
            return Err(ConfigError::Invalid(
                "launcher.max_results must be between 1 and 100".into(),
            ));
        }
        if l.width < 400 || l.height < 400 {
            return Err(ConfigError::Invalid(
                "launcher.width and launcher.height must be >= 400".into(),
            ));
        }
        let s = &self.supervisor;
        if s.restart_initial_delay_ms == 0 || s.restart_initial_delay_ms > s.restart_max_delay_ms {
            return Err(ConfigError::Invalid(
                "supervisor.restart_initial_delay_ms must be > 0 and <= restart_max_delay_ms"
                    .into(),
            ));
        }
        for c in &s.components {
            if !KNOWN_COMPONENTS.contains(&c.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "unknown component {c:?} in supervisor.components (known: {KNOWN_COMPONENTS:?})"
                )));
            }
        }
        let i = &self.index;
        if i.max_query_results == 0 || i.max_query_results > 100_000 {
            return Err(ConfigError::Invalid(
                "index.max_query_results must be between 1 and 100000".into(),
            ));
        }
        if i.launcher_results > 50 {
            return Err(ConfigError::Invalid(
                "index.launcher_results must be at most 50".into(),
            ));
        }
        match self.general.log_level.as_str() {
            "error" | "warn" | "info" | "debug" | "trace" => {}
            other => {
                return Err(ConfigError::Invalid(format!(
                    "general.log_level {other:?} is not one of error/warn/info/debug/trace"
                )))
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_default_matches_struct_default() {
        let parsed = Config::from_toml(DEFAULT_CONFIG_TOML).expect("default.toml must parse");
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn empty_file_is_default() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
    }

    #[test]
    fn partial_override() {
        let c = Config::from_toml("[taskbar]\nposition = \"top\"\nheight = 32\n").unwrap();
        assert_eq!(c.taskbar.position, Edge::Top);
        assert_eq!(c.taskbar.height, 32);
        assert!(c.taskbar.show_clock);
    }

    #[test]
    fn unknown_keys_rejected() {
        assert!(Config::from_toml("[taskbar]\nheigth = 32\n").is_err());
    }

    #[test]
    fn invalid_values_rejected() {
        assert!(Config::from_toml("[taskbar]\nheight = 2\n").is_err());
        assert!(Config::from_toml("[supervisor]\ncomponents = [\"tray\"]\n").is_err());
        assert!(Config::from_toml("[general]\nlog_level = \"loud\"\n").is_err());
        assert!(Config::from_toml("[index]\nmax_query_results = 0\n").is_err());
    }

    #[test]
    fn indexd_is_known_but_not_default() {
        assert!(KNOWN_COMPONENTS.contains(&"indexd"));
        assert!(!Config::default()
            .supervisor
            .components
            .contains(&"indexd".to_string()));
        let c =
            Config::from_toml("[supervisor]\ncomponents = [\"taskbar\", \"indexd\"]\n").unwrap();
        assert_eq!(c.supervisor.components.len(), 2);
    }

    #[test]
    fn desktop_and_file_search_keys() {
        let c = Config::default();
        assert!(c.supervisor.components.contains(&"desktop".to_string()));
        assert_eq!(c.supervisor.file_search, FileSearch::Auto);
        assert_eq!(c.desktop.enabled, DesktopMode::Shell);
        let c = Config::from_toml(
            "[supervisor]\nfile_search = \"off\"\n[desktop]\nenabled = \"always\"\n",
        )
        .unwrap();
        assert_eq!(c.supervisor.file_search, FileSearch::Off);
        assert!(c.desktop.enabled.enabled_in(crate::ipc::RunMode::Dev));
        assert!(!DesktopMode::Shell.enabled_in(crate::ipc::RunMode::Dev));
        assert!(DesktopMode::Shell.enabled_in(crate::ipc::RunMode::Shell));
        assert!(!DesktopMode::Never.enabled_in(crate::ipc::RunMode::Shell));
        assert!(Config::from_toml("[desktop]\nenabled = \"yes\"\n").is_err());
    }

    #[test]
    fn missing_file_is_default_broken_file_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        assert_eq!(Config::load(&p).unwrap(), Config::default());
        std::fs::write(&p, "this is not toml = = =").unwrap();
        let (c, err) = Config::load_or_default(&p);
        assert_eq!(c, Config::default());
        assert!(err.is_some());
    }
}
