// SPDX-License-Identifier: GPL-3.0-or-later
//! `explorer-rs-settings.exe`: target of the "explorer.rs settings" Start
//! menu shortcut. Runs unelevated as the signed-in user, creates
//! `%APPDATA%\explorer-rs\config.toml` from the documented defaults if it
//! does not exist yet, and opens it in Notepad.
//!
//! A separate small program (instead of a mode of the setup program)
//! because the setup program's manifest requires administrator rights: an
//! elevated Notepad would edit the wrong user's file after over-the-shoulder
//! elevation, and every click would show a UAC prompt.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::Path;

use explorer_core::paths::AppPaths;

/// Create `path` with the default configuration if it is missing.
/// Returns true if it was created.
fn ensure_config(path: &Path) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // create_new: never overwrite a file created concurrently.
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

fn main() {
    let paths = AppPaths::from_env();
    let config = paths.config_file();
    if let Err(e) = ensure_config(&config) {
        report(&format!(
            "Could not create {}: {e}\n\nNo se pudo crear {}: {e}",
            config.display(),
            config.display()
        ));
        return;
    }
    let notepad = std::env::var_os("SystemRoot")
        .map(|r| Path::new(&r).join("System32").join("notepad.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| "notepad.exe".into());
    if let Err(e) = std::process::Command::new(&notepad).arg(&config).spawn() {
        report(&format!(
            "Could not open {} in Notepad: {e}\n\nNo se pudo abrir {} en el Bloc de notas: {e}",
            config.display(),
            config.display()
        ));
    }
}

#[cfg(windows)]
fn report(text: &str) {
    use explorer_win::util::WideString;
    use explorer_win::windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let t = WideString::new(text);
    let c = WideString::new("explorer.rs");
    // SAFETY: valid strings for the call.
    unsafe { MessageBoxW(None, t.pcwstr(), c.pcwstr(), MB_OK | MB_ICONERROR) };
}

#[cfg(not(windows))]
fn report(text: &str) {
    eprintln!("{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_once_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("config.toml");
        assert!(ensure_config(&p).unwrap());
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            explorer_core::config::DEFAULT_CONFIG_TOML
        );
        std::fs::write(&p, "# mine").unwrap();
        assert!(!ensure_config(&p).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# mine");
    }
}
