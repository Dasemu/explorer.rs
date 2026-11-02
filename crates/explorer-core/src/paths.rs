// SPDX-License-Identifier: GPL-3.0-or-later
//! Resolution of per-user directories (config, logs, state).
//!
//! On Windows these live under `%APPDATA%\explorer-rs` (config) and
//! `%LOCALAPPDATA%\explorer-rs` (logs, state). On other platforms (tests,
//! development) we fall back to XDG-ish locations so the code paths stay
//! exercisable. `EXPLORER_RS_HOME` overrides everything (see `.env.example`).

use std::path::PathBuf;

use crate::APP_DIR_NAME;

/// Environment variable that overrides the base directory for config, logs
/// and state. Useful for testing several configurations side by side.
pub const HOME_OVERRIDE_ENV: &str = "EXPLORER_RS_HOME";

/// Directories used by all explorer.rs processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub log_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl AppPaths {
    /// Resolve paths from the process environment.
    pub fn from_env() -> Self {
        Self::resolve(|k| std::env::var_os(k).map(PathBuf::from))
    }

    /// Resolve paths using a custom environment lookup (testable).
    pub fn resolve(get: impl Fn(&str) -> Option<PathBuf>) -> Self {
        if let Some(base) = get(HOME_OVERRIDE_ENV) {
            return Self::under(base);
        }
        let roaming = get("APPDATA")
            .or_else(|| get("XDG_CONFIG_HOME"))
            .or_else(|| get("HOME").map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        let local = get("LOCALAPPDATA")
            .or_else(|| get("XDG_STATE_HOME"))
            .or_else(|| get("HOME").map(|h| h.join(".local").join("state")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            config_dir: roaming.join(APP_DIR_NAME),
            log_dir: local.join(APP_DIR_NAME).join("logs"),
            state_dir: local.join(APP_DIR_NAME),
        }
    }

    /// All directories under a single base directory.
    pub fn under(base: PathBuf) -> Self {
        Self {
            config_dir: base.clone(),
            log_dir: base.join("logs"),
            state_dir: base.join("state"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn pins_file(&self) -> PathBuf {
        self.state_dir.join("pins.toml")
    }

    pub fn history_file(&self) -> PathBuf {
        self.state_dir.join("launch-history.toml")
    }

    /// Create all directories if missing.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.log_dir)?;
        std::fs::create_dir_all(&self.state_dir)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> {
        let m: HashMap<String, PathBuf> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), PathBuf::from(v)))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn windows_layout() {
        let p = AppPaths::resolve(env(&[
            ("APPDATA", "C:/Users/u/AppData/Roaming"),
            ("LOCALAPPDATA", "C:/Users/u/AppData/Local"),
        ]));
        assert_eq!(
            p.config_file(),
            PathBuf::from("C:/Users/u/AppData/Roaming/explorer-rs/config.toml")
        );
        assert_eq!(
            p.log_dir,
            PathBuf::from("C:/Users/u/AppData/Local/explorer-rs/logs")
        );
    }

    #[test]
    fn override_wins() {
        let p = AppPaths::resolve(env(&[("EXPLORER_RS_HOME", "/tmp/x"), ("APPDATA", "/a")]));
        assert_eq!(p.config_dir, PathBuf::from("/tmp/x"));
        assert_eq!(p.log_dir, PathBuf::from("/tmp/x/logs"));
    }
}
