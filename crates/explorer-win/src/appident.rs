// SPDX-License-Identifier: GPL-3.0-or-later
//! Identity information used to match running windows, pinned apps and
//! tray icons (documented Shell / registry APIs):
//!
//! - known-folder GUID -> path (`SHGetKnownFolderPath`);
//! - the executable behind a `shell:AppsFolder` item
//!   (`PKEY_Link_TargetParsingPath` on the shell item) or a `.lnk` file
//!   (`IShellLinkW::GetPath`);
//! - Windows 11's per-icon tray settings under
//!   `HKCU\Control Panel\NotifyIconSettings` (registry layout is not a
//!   documented API; if it changes we fall back to "not promoted", see
//!   docs/decisions/0007-undocumented-apis.md).

use explorer_core::shellpath::{KnownFolders, COMMON_KNOWN_FOLDERS};
use explorer_core::tray_promotion::NotifySetting;
use windows::core::{Interface, GUID, PWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::{
    CoCreateInstance, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::{
    IShellItem2, IShellLinkW, SHCreateItemFromParsingName, SHGetKnownFolderPath, ShellLink,
    KF_FLAG_DEFAULT, SLGP_RAWPATH,
};

use crate::util::WideString;

/// PKEY_Link_TargetParsingPath {B9B4B3FC-2B51-4A42-B5D8-324146AFCF25}, 2.
const PKEY_LINK_TARGET_PARSING_PATH: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xb9b4b3fc_2b51_4a42_b5d8_324146afcf25),
    pid: 2,
};

fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: CoTaskMem string from the callee, freed exactly once.
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        s
    }
}

/// Resolve [`COMMON_KNOWN_FOLDERS`]. Cheap; call once per process.
pub fn known_folders() -> KnownFolders {
    KnownFolders::new(COMMON_KNOWN_FOLDERS.iter().filter_map(|g| {
        let guid = GUID::try_from(g.trim_matches(['{', '}'])).ok()?;
        // SAFETY: valid GUID pointer; result freed by take_pwstr.
        let p = unsafe { SHGetKnownFolderPath(&guid, KF_FLAG_DEFAULT, None) }.ok()?;
        let s = take_pwstr(p);
        (!s.is_empty()).then(|| (g.to_string(), s))
    }))
}

/// Executable a `shell:AppsFolder\<id>` item points to (classic apps with
/// a Start-menu shortcut). Requires COM.
pub fn apps_folder_target(id: &str) -> Option<String> {
    let p = WideString::new(&format!("shell:AppsFolder\\{id}"));
    // SAFETY: documented Shell calls; the string is freed.
    unsafe {
        let item: IShellItem2 = SHCreateItemFromParsingName(p.pcwstr(), None).ok()?;
        let s = take_pwstr(item.GetString(&PKEY_LINK_TARGET_PARSING_PATH).ok()?);
        (!s.is_empty()).then_some(s)
    }
}

/// Target path of a `.lnk` shortcut. Requires COM.
pub fn shortcut_target(lnk: &str) -> Option<String> {
    let p = WideString::new(lnk);
    // SAFETY: documented ShellLink COM calls with a sized buffer.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(p.pcwstr(), STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
            .ok()?;
        let s = crate::util::from_wide(&buf);
        (!s.is_empty()).then_some(s)
    }
}

const NOTIFY_SETTINGS: &str = r"Control Panel\NotifyIconSettings";

fn reg_string(key: HKEY, sub: &WideString, value: &str) -> Option<String> {
    let v = WideString::new(value);
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: buffer and its byte size are consistent.
    let r = unsafe {
        RegGetValueW(
            key,
            sub.pcwstr(),
            v.pcwstr(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    r.is_ok().then(|| crate::util::from_wide(&buf))
}

fn reg_dword(key: HKEY, sub: &WideString, value: &str) -> Option<u32> {
    let v = WideString::new(value);
    let mut data = 0u32;
    let mut size = 4u32;
    // SAFETY: out-pointers to a u32 and its size.
    let r = unsafe {
        RegGetValueW(
            key,
            sub.pcwstr(),
            v.pcwstr(),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(data)
}

/// Read every `NotifyIconSettings` entry, with `ExecutablePath` expanded
/// through `folders`. Missing key (older Windows): empty list.
pub fn notify_settings(folders: &KnownFolders) -> Vec<NotifySetting> {
    let path = WideString::new(NOTIFY_SETTINGS);
    let mut key = HKEY::default();
    // SAFETY: valid out-pointer; the key is closed below.
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.pcwstr(), None, KEY_READ, &mut key) };
    if r.is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for index in 0..2048u32 {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        // SAFETY: name buffer and length are consistent.
        let r = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if r.is_err() {
            break;
        }
        let sub = WideString::new(&String::from_utf16_lossy(&name[..len as usize]));
        let Some(exe) = reg_string(key, &sub, "ExecutablePath") else {
            continue;
        };
        out.push(NotifySetting {
            executable_path: folders.expand(&exe),
            uid: reg_dword(key, &sub, "UID"),
            is_promoted: reg_dword(key, &sub, "IsPromoted").unwrap_or(0) != 0,
        });
    }
    // SAFETY: key opened above.
    let _ = unsafe { RegCloseKey(key) };
    out
}
