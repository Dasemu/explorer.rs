// SPDX-License-Identifier: GPL-3.0-or-later
//! Setup log: `%ProgramData%\explorer-rs\setup.log` (appended).
//!
//! `%ProgramData%` lets standard users create folders, so a user could
//! pre-create `explorer-rs` as a junction and make the elevated installer
//! write elsewhere. The folder is therefore recreated if it is a reparse
//! point and gets a protected ACL (SYSTEM and Administrators full control,
//! Users read) before the log file is opened.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use explorer_win::util::WideString;
use explorer_win::windows::Win32::Foundation::{LocalFree, HLOCAL};
use explorer_win::windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW, SDDL_REVISION_1,
    SE_FILE_OBJECT,
};
use explorer_win::windows::Win32::Security::{
    GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};
use explorer_win::windows::Win32::UI::Shell::FOLDERID_ProgramData;

/// SYSTEM + Administrators: full control; Users: read & execute.
const DIR_SDDL: &str = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)";

pub fn log_dir() -> Result<PathBuf> {
    let base = crate::win::sys::known_folder(&FOLDERID_ProgramData)?;
    Ok(PathBuf::from(base).join(explorer_core::APP_DIR_NAME))
}

fn protect_dir(dir: &Path) -> Result<()> {
    let sddl = WideString::new(DIR_SDDL);
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: valid SDDL string; `sd` freed with LocalFree below; the DACL
    // pointer lives inside `sd`.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.pcwstr(),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .context("ConvertStringSecurityDescriptorToSecurityDescriptorW")?;
        let mut present = explorer_win::windows::core::BOOL(0);
        let mut defaulted = explorer_win::windows::core::BOOL(0);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let r = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted)
            .context("GetSecurityDescriptorDacl")
            .and_then(|_| {
                let path = WideString::new(&dir.to_string_lossy());
                SetNamedSecurityInfoW(
                    path.pcwstr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(dacl),
                    None,
                )
                .ok()
                .context("SetNamedSecurityInfoW")
            });
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        r
    }
}

/// Create the protected log folder and start logging to `setup.log`.
/// Logging failures never stop setup. Returns the log file path.
pub fn init() -> Option<PathBuf> {
    let dir = log_dir().ok()?;
    if let Ok(md) = std::fs::symlink_metadata(&dir) {
        if md.file_type().is_symlink() || !md.is_dir() {
            // Not a plain folder we created: remove the link (not its target).
            let _ = std::fs::remove_dir(&dir).or_else(|_| std::fs::remove_file(&dir));
        }
    }
    std::fs::create_dir_all(&dir).ok()?;
    if let Err(e) = protect_dir(&dir) {
        // Without a protected folder do not write an elevated log there.
        eprintln!("setup log disabled: {e:#}");
        return None;
    }
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
