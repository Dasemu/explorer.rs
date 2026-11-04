// SPDX-License-Identifier: GPL-3.0-or-later
//! App discovery and launching.
//!
//! - Start Menu folders come from the documented known-folder API.
//! - `shell:AppsFolder` is enumerated with `IShellItem` / `IEnumShellItems`
//!   (documented Shell API). It lists both classic and packaged apps; the
//!   parent-relative parsing name is the AUMID for packaged apps.
//! - Packaged apps are started with `IApplicationActivationManager`
//!   (documented, works without explorer.exe); everything else with
//!   `ShellExecuteW`.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use explorer_core::catalog::{apps_folder_entry, is_aumid, AppEntry, LaunchTarget};
use windows::core::{GUID, PWSTR};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_LOCAL_SERVER};
use windows::Win32::UI::Shell::{
    ApplicationActivationManager, BHID_EnumItems, FOLDERID_CommonPrograms, FOLDERID_Programs,
    IApplicationActivationManager, IEnumShellItems, IShellItem, SHCreateItemFromParsingName,
    SHGetKnownFolderPath, ShellExecuteW, AO_NONE, KF_FLAG_DEFAULT, SIGDN_NORMALDISPLAY,
    SIGDN_PARENTRELATIVEPARSING,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::util::WideString;

/// Take ownership of a CoTaskMem-allocated string.
fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: p is a valid NUL-terminated string allocated with
    // CoTaskMemAlloc by the callee; freed exactly once here.
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        s
    }
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: valid GUID pointer; result freed by take_pwstr.
    let p = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
    let s = take_pwstr(p);
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// Per-user and all-users Start Menu "Programs" folders.
pub fn start_menu_folders() -> Vec<PathBuf> {
    [&FOLDERID_Programs, &FOLDERID_CommonPrograms]
        .into_iter()
        .filter_map(known_folder)
        .collect()
}

/// Enumerate `shell:AppsFolder`. Requires COM on the calling thread.
pub fn apps_folder() -> Result<Vec<AppEntry>> {
    let root = WideString::new("shell:AppsFolder");
    // SAFETY: standard Shell COM calls with valid arguments.
    unsafe {
        let folder: IShellItem =
            SHCreateItemFromParsingName(root.pcwstr(), None).context("open shell:AppsFolder")?;
        let items: IEnumShellItems = folder
            .BindToHandler(None, &BHID_EnumItems)
            .context("enumerate shell:AppsFolder")?;
        let mut out = Vec::new();
        loop {
            let mut batch: [Option<IShellItem>; 1] = [None];
            let mut fetched = 0u32;
            if items.Next(&mut batch, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(item) = batch[0].take() else { break };
            let name = item
                .GetDisplayName(SIGDN_NORMALDISPLAY)
                .map(take_pwstr)
                .unwrap_or_default();
            let id = item
                .GetDisplayName(SIGDN_PARENTRELATIVEPARSING)
                .map(take_pwstr)
                .unwrap_or_default();
            if let Some(e) = apps_folder_entry(&name, &id) {
                out.push(e);
            }
        }
        Ok(out)
    }
}

/// Launch an entry. Requires COM on the calling thread. `elevated` uses
/// the "runas" verb (UAC prompt) and is ignored for packaged apps.
pub fn launch(target: &LaunchTarget, elevated: bool) -> Result<()> {
    match target {
        LaunchTarget::AppsFolder(id) if is_aumid(id) && !elevated => match activate_packaged(id) {
            Ok(pid) => {
                tracing::info!(aumid = %id, pid, "activated packaged app");
                Ok(())
            }
            Err(e) => {
                tracing::warn!(aumid = %id, error = %e, "ActivateApplication failed, falling back to ShellExecute");
                shell_execute(&format!("shell:AppsFolder\\{id}"), false)
            }
        },
        LaunchTarget::AppsFolder(id) => shell_execute(&format!("shell:AppsFolder\\{id}"), elevated),
        LaunchTarget::Path(p) => shell_execute(&p.to_string_lossy(), elevated),
    }
}

fn activate_packaged(aumid: &str) -> Result<u32> {
    let id = WideString::new(aumid);
    // SAFETY: documented COM activation.
    unsafe {
        let mgr: IApplicationActivationManager =
            CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
        Ok(mgr.ActivateApplication(id.pcwstr(), None, AO_NONE)?)
    }
}

/// `ShellExecuteW` with the "open" or "runas" verb.
pub fn shell_execute(file: &str, elevated: bool) -> Result<()> {
    let verb = WideString::new(if elevated { "runas" } else { "open" });
    let f = WideString::new(file);
    // SAFETY: valid strings kept alive for the call.
    let r = unsafe { ShellExecuteW(None, verb.pcwstr(), f.pcwstr(), None, None, SW_SHOWNORMAL) };
    // Values <= 32 are errors per the ShellExecute contract.
    if r.0 as isize <= 32 {
        return Err(anyhow!(
            "ShellExecuteW({file}) failed with code {}",
            r.0 as isize
        ));
    }
    Ok(())
}
