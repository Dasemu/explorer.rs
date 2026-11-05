// SPDX-License-Identifier: GPL-3.0-or-later
//! Registry access for the installer: `HKEY_LOCAL_MACHINE` and
//! `HKEY_USERS` (64-bit view; the installer is a 64-bit process).

use anyhow::{Context, Result};
use explorer_win::util::WideString;
use explorer_win::windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS};
use explorer_win::windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW, RegSetKeyValueW, HKEY, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, KEY_WRITE,
    REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_NOEXPAND, RRF_RT_REG_DWORD,
    RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};

use crate::plan::RegValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    Hklm,
    Hku,
}

impl Root {
    fn hkey(self) -> HKEY {
        match self {
            Root::Hklm => HKEY_LOCAL_MACHINE,
            Root::Hku => HKEY_USERS,
        }
    }
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: key opened by us.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn open(root: Root, subkey: &str, write: bool, create: bool) -> Result<Option<Key>> {
    let p = WideString::new(subkey);
    let access = if write {
        KEY_READ | KEY_WRITE
    } else {
        KEY_READ
    };
    let mut k = HKEY::default();
    // SAFETY: valid strings and out-pointer.
    let r = unsafe {
        if create {
            RegCreateKeyExW(
                root.hkey(),
                p.pcwstr(),
                None,
                None,
                REG_OPTION_NON_VOLATILE,
                access,
                None,
                &mut k,
                None,
            )
        } else {
            RegOpenKeyExW(root.hkey(), p.pcwstr(), None, access, &mut k)
        }
    };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    r.ok()
        .with_context(|| format!("open registry key {root:?}\\{subkey}"))?;
    Ok(Some(Key(k)))
}

/// REG_SZ or REG_EXPAND_SZ (not expanded).
pub fn get_string(root: Root, subkey: &str, name: &str) -> Option<String> {
    let key = open(root, subkey, false, false).ok()??;
    let n = WideString::new(name);
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut len = 0u32;
    // SAFETY: size query, then a read into a buffer of that size.
    unsafe {
        RegGetValueW(key.0, None, n.pcwstr(), flags, None, None, Some(&mut len))
            .ok()
            .ok()?;
        let mut buf = vec![0u16; (len as usize / 2).max(1)];
        RegGetValueW(
            key.0,
            None,
            n.pcwstr(),
            flags,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
        Some(explorer_win::util::from_wide(&buf))
    }
}

pub fn get_dword(root: Root, subkey: &str, name: &str) -> Option<u32> {
    let key = open(root, subkey, false, false).ok()??;
    let n = WideString::new(name);
    let mut v = 0u32;
    let mut len = 4u32;
    // SAFETY: out-pointer to a u32.
    unsafe {
        RegGetValueW(
            key.0,
            None,
            n.pcwstr(),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut v as *mut u32).cast()),
            Some(&mut len),
        )
    }
    .ok()
    .ok()?;
    Some(v)
}

/// Create `subkey` if needed and write all values.
pub fn set_values(root: Root, subkey: &str, values: &[(String, RegValue)]) -> Result<()> {
    let key = open(root, subkey, true, true)?.context("create key")?;
    for (name, value) in values {
        let n = WideString::new(name);
        // SAFETY: data pointers and byte lengths match.
        let r = unsafe {
            match value {
                RegValue::Sz(s) => {
                    let w: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
                    RegSetKeyValueW(
                        key.0,
                        None,
                        n.pcwstr(),
                        REG_SZ.0,
                        Some(w.as_ptr().cast()),
                        (w.len() * 2) as u32,
                    )
                }
                RegValue::Dword(d) => RegSetKeyValueW(
                    key.0,
                    None,
                    n.pcwstr(),
                    REG_DWORD.0,
                    Some((d as *const u32).cast()),
                    4,
                ),
            }
        };
        r.ok()
            .with_context(|| format!("set {root:?}\\{subkey}\\{name}"))?;
    }
    Ok(())
}

/// Delete one value; a missing key or value is not an error.
pub fn delete_value(root: Root, subkey: &str, name: &str) -> Result<()> {
    let Some(key) = open(root, subkey, true, false)? else {
        return Ok(());
    };
    let n = WideString::new(name);
    // SAFETY: valid key and name.
    let r = unsafe { RegDeleteKeyValueW(key.0, None, n.pcwstr()) };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    r.ok()
        .with_context(|| format!("delete {root:?}\\{subkey}\\{name}"))
}

/// Delete `subkey` with all values and subkeys; missing is not an error.
pub fn delete_key(root: Root, subkey: &str) -> Result<()> {
    let p = WideString::new(subkey);
    // SAFETY: valid root and string. With a subkey name RegDeleteTreeW
    // removes that key and everything below it.
    let r = unsafe { RegDeleteTreeW(root.hkey(), p.pcwstr()) };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    r.ok()
        .with_context(|| format!("delete key {root:?}\\{subkey}"))?;
    // Older documentation is ambiguous about whether the key itself goes
    // too; make sure it does.
    // SAFETY: as above.
    let r = unsafe {
        explorer_win::windows::Win32::System::Registry::RegDeleteKeyW(root.hkey(), p.pcwstr())
    };
    if r != ERROR_FILE_NOT_FOUND {
        let _ = r.ok();
    }
    Ok(())
}

/// Names of the direct subkeys of `subkey` (`""` = the root itself).
pub fn subkeys(root: Root, subkey: &str) -> Vec<String> {
    let key = if subkey.is_empty() {
        None
    } else {
        match open(root, subkey, false, false) {
            Ok(Some(k)) => Some(k),
            _ => return Vec::new(),
        }
    };
    let h = key.as_ref().map(|k| k.0).unwrap_or(root.hkey());
    let mut out = Vec::new();
    for i in 0.. {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        // SAFETY: buffer and length are valid.
        let r = unsafe {
            RegEnumKeyExW(
                h,
                i,
                Some(explorer_win::windows::core::PWSTR(buf.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if r == ERROR_NO_MORE_ITEMS || r.is_err() {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    out
}
