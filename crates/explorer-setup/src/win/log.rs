// SPDX-License-Identifier: GPL-3.0-or-later
//! Setup log: `%ProgramData%\explorer-rs\setup.log` (appended), and the
//! protected folder itself (see `explorer_win::secdir`).
//!
//! `%ProgramData%` lets standard users create folders, so a user could
//! pre-create `explorer-rs` (as a junction, or as a folder they own with
//! files in it) and make the elevated installer write elsewhere or run a
//! planted DLL next to the uninstaller copy. Before anything is written
//! there the folder (and its `logs` subfolder, used by the elevated index
//! helper) is made a protected folder owned by Administrators; a folder
//! that was owned by someone else has its contents deleted.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use explorer_win::secdir;

/// Secure `%ProgramData%\explorer-rs` and its `logs` subfolder once per
/// run and return the folder. Errors are remembered (never write to an
/// unprotected folder).
pub fn secure_data_dir() -> Result<PathBuf> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| {
        let secure = || -> Result<PathBuf> {
            let dir = secdir::program_data_dir()?;
            if secdir::secure_dir(&dir)? {
                secdir::wipe_contents(&dir);
            }
            let logs = dir.join("logs");
            if secdir::secure_dir(&logs)? {
                secdir::wipe_contents(&logs);
            }
            Ok(dir)
        };
        secure().map_err(|e| format!("{e:#}"))
    })
    .clone()
    .map_err(anyhow::Error::msg)
}

pub fn log_dir() -> Result<PathBuf> {
    secure_data_dir()
}

/// Create the protected log folder and start logging to `setup.log`.
/// Logging failures never stop setup. Returns the log file path.
pub fn init() -> Option<PathBuf> {
    if !explorer_win::security::is_elevated() {
        // Never create the folder unelevated: it would be owned by the
        // user (and wiped by the next elevated run).
        return None;
    }
    let dir = match secure_data_dir() {
        Ok(d) => d,
        Err(e) => {
            // Without a protected folder do not write an elevated log there.
            eprintln!("setup log disabled: {e:#}");
            return None;
        }
    };
    let path = dir.join("setup.log");
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        let _ = std::fs::remove_file(&path);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let _ = tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_target(false)
        .with_max_level(tracing::Level::DEBUG)
        .try_init();
    Some(path)
}
