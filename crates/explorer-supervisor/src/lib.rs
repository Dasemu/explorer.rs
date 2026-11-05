// SPDX-License-Identifier: GPL-3.0-or-later
//! Portable parts of `explorer-rs.exe`: the guardian restart loop, the
//! `--status` report and `--edit-config` file creation. Everything here
//! runs on Linux too, so it is covered by tests (including an integration
//! test that drives the guardian with a fake supervisor).

pub mod guardian;
pub mod status;

use std::path::Path;

/// Create `path` from the embedded default config if it does not exist.
/// Returns true if the file was created.
pub fn ensure_config_file(path: &Path) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // `create_new` so a file created concurrently is never overwritten.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(explorer_core::config::DEFAULT_CONFIG_TOML.as_bytes())?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_file_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("config.toml");
        assert!(ensure_config_file(&p).unwrap());
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(explorer_core::config::Config::from_toml(&text).is_ok());
        std::fs::write(&p, "# mine\n").unwrap();
        assert!(!ensure_config_file(&p).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# mine\n");
    }
}
