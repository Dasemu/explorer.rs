// SPDX-License-Identifier: GPL-3.0-or-later
//! The protected per-machine folder `%ProgramData%\explorer-rs` (setup log,
//! the uninstaller copy, temporary task XML, the elevated index helper's
//! logs).
//!
//! `%ProgramData%` lets standard users create folders. A user could
//! therefore pre-create `explorer-rs` (as a junction, or as a folder they
//! own with files planted in it) before an elevated process writes there.
//! [`secure_dir`] removes a junction, takes ownership for Administrators and
//! replaces the DACL with a protected one (SYSTEM and Administrators full
//! control, Users read & execute); the caller wipes the contents of a
//! folder that was not owned by Administrators/SYSTEM before. Elevated
//! readers that must not change anything (the index helper) only use the
//! folder when [`is_trusted_dir`] holds.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use windows::core::PWSTR;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, ACL, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SE_RESTORE_NAME, SE_TAKE_OWNERSHIP_NAME,
};
use windows::Win32::UI::Shell::FOLDERID_ProgramData;

use crate::util::WideString;

/// Owner Administrators; SYSTEM + Administrators: full control; Users:
/// read & execute; protected (no inheritance from `%ProgramData%`).
const DIR_SDDL: &str = "O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)";

/// `%ProgramData%\explorer-rs` (from the known-folder API, not from the
/// environment, which the user controls).
pub fn program_data_dir() -> Result<PathBuf> {
    let base = crate::util::known_folder(&FOLDERID_ProgramData)?;
    Ok(PathBuf::from(base).join(explorer_core::APP_DIR_NAME))
}

/// True if `path` is a reparse point (symbolic link, junction, ...).
pub fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    std::fs::symlink_metadata(path).is_ok_and(|m| {
        m.file_type().is_symlink() || m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    })
}

/// String SID of the owner of `path`.
pub fn owner_sid(path: &Path) -> Result<String> {
    let p = WideString::new(&path.to_string_lossy());
    let mut owner = PSID::default();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: out-pointers are valid; `sd` is freed with LocalFree and
    // `owner` points into it.
    unsafe {
        GetNamedSecurityInfoW(
            p.pcwstr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            &mut sd,
        )
        .ok()
        .context("GetNamedSecurityInfoW")?;
        let mut s = PWSTR::null();
        let r = ConvertSidToStringSidW(owner, &mut s).context("ConvertSidToStringSidW");
        let out = r.map(|_| {
            let v = s.to_string().unwrap_or_default();
            let _ = LocalFree(Some(HLOCAL(s.0.cast())));
            v
        });
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        out
    }
}

/// A real folder (not a reparse point) owned by Administrators or SYSTEM.
pub fn is_trusted_dir(path: &Path) -> bool {
    path.is_dir()
        && !is_reparse_point(path)
        && owner_sid(path).is_ok_and(|s| explorer_core::setup::trusted_owner_sid(&s))
}

/// Create `dir` if needed and make it a protected folder owned by
/// Administrators (see the module docs). Requires an elevated caller.
/// Returns true if the folder already existed and was NOT owned by
/// Administrators/SYSTEM: its contents are untrusted and the caller should
/// delete them before using the folder.
pub fn secure_dir(dir: &Path) -> Result<bool> {
    if is_reparse_point(dir) {
        tracing::warn!(dir = %dir.display(), "removing a link where a protected folder belongs");
        // Removes the link itself, never its target.
        let _ = std::fs::remove_dir(dir).or_else(|_| std::fs::remove_file(dir));
    }
    let existed = dir.exists();
    if existed && !dir.is_dir() {
        std::fs::remove_file(dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let untrusted =
        existed && !owner_sid(dir).is_ok_and(|s| explorer_core::setup::trusted_owner_sid(&s));
    if untrusted {
        tracing::warn!(dir = %dir.display(), owner = ?owner_sid(dir).ok(), "folder was not created by an administrator; taking ownership");
    }
    // Setting another owner than ourselves needs these (administrators
    // hold them; failures surface in SetNamedSecurityInfoW below).
    crate::security::enable_privilege(SE_TAKE_OWNERSHIP_NAME);
    crate::security::enable_privilege(SE_RESTORE_NAME);
    apply_sddl(dir)?;
    if is_reparse_point(dir) {
        bail!("{} became a link while it was being secured", dir.display());
    }
    Ok(untrusted)
}

fn apply_sddl(dir: &Path) -> Result<()> {
    let sddl = WideString::new(DIR_SDDL);
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: valid SDDL string; `sd` freed with LocalFree below; the owner
    // and DACL pointers live inside `sd`.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.pcwstr(),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .context("ConvertStringSecurityDescriptorToSecurityDescriptorW")?;
        let r = (|| -> Result<()> {
            let mut present = windows::core::BOOL(0);
            let mut defaulted = windows::core::BOOL(0);
            let mut dacl: *mut ACL = std::ptr::null_mut();
            GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted)
                .context("GetSecurityDescriptorDacl")?;
            let mut owner = PSID::default();
            let mut owner_defaulted = windows::core::BOOL(0);
            GetSecurityDescriptorOwner(sd, &mut owner, &mut owner_defaulted)
                .context("GetSecurityDescriptorOwner")?;
            let path = WideString::new(&dir.to_string_lossy());
            SetNamedSecurityInfoW(
                path.pcwstr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                Some(owner),
                None,
                Some(dacl),
                None,
            )
            .ok()
            .context("SetNamedSecurityInfoW")
        })();
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        r
    }
}

/// Delete everything inside `dir` without following links (a link is
/// removed itself, never its target). Errors (files in use) are logged.
pub fn wipe_contents(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let r = if is_reparse_point(&p) {
            std::fs::remove_dir(&p).or_else(|_| std::fs::remove_file(&p))
        } else if p.is_dir() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        if let Err(err) = r {
            tracing::warn!(path = %p.display(), error = %err, "cannot delete untrusted file");
        }
    }
}
