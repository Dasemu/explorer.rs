// SPDX-License-Identifier: GPL-3.0-or-later
//! Small read-mostly registry helpers (value enumeration, typed reads,
//! value deletion) used by startup apps, the wallpaper and the supervisor.

use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegEnumValueW, RegGetValueW, RegOpenKeyExW, RegQueryInfoKeyW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY,
    REG_SAM_FLAGS, REG_VALUE_TYPE, RRF_NOEXPAND, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD,
    RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};

use crate::util::{from_wide, WideString};

/// Registry root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    CurrentUser,
    /// Always the 64-bit view.
    LocalMachine,
}

impl Root {
    fn hkey(self) -> HKEY {
        match self {
            Root::CurrentUser => HKEY_CURRENT_USER,
            Root::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }

    fn view(self) -> REG_SAM_FLAGS {
        match self {
            Root::CurrentUser => REG_SAM_FLAGS(0),
            Root::LocalMachine => KEY_WOW64_64KEY,
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

fn open(root: Root, subkey: &str, access: REG_SAM_FLAGS) -> Option<Key> {
    let p = WideString::new(subkey);
    let mut k = HKEY::default();
    // SAFETY: valid string and out-pointer.
    unsafe { RegOpenKeyExW(root.hkey(), p.pcwstr(), None, access | root.view(), &mut k) }
        .ok()
        .ok()?;
    Some(Key(k))
}

/// One registry value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    pub name: String,
    pub kind: REG_VALUE_TYPE,
    pub data: Vec<u8>,
}

impl Value {
    /// REG_SZ / REG_EXPAND_SZ payload as a string (unexpanded).
    pub fn as_string(&self) -> String {
        let u16s: Vec<u16> = self
            .data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        from_wide(&u16s)
    }
}

/// All values of a key (empty if the key is missing).
pub fn values(root: Root, subkey: &str) -> Vec<Value> {
    let Some(key) = open(root, subkey, KEY_READ) else {
        return Vec::new();
    };
    let (mut max_name, mut max_data) = (0u32, 0u32);
    // SAFETY: valid key and out-pointers.
    let _ = unsafe {
        RegQueryInfoKeyW(
            key.0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&mut max_name),
            Some(&mut max_data),
            None,
            None,
        )
    };
    let mut out = Vec::new();
    let mut name = vec![0u16; max_name as usize + 2];
    let mut data = vec![0u8; max_data as usize + 2];
    let mut index = 0u32;
    // Bounded: a key changing under us cannot make this loop forever.
    while index < 10_000 {
        let mut name_len = name.len() as u32;
        let mut data_len = data.len() as u32;
        let mut kind = 0u32;
        // SAFETY: buffers and their lengths match.
        let r = unsafe {
            RegEnumValueW(
                key.0,
                index,
                Some(windows::core::PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            )
        };
        if r == ERROR_NO_MORE_ITEMS {
            break;
        }
        if r == ERROR_MORE_DATA {
            // Value grew since RegQueryInfoKeyW: enlarge and retry.
            name.resize(name.len() * 2 + 64, 0);
            data.resize(data.len() * 2 + 256, 0);
            continue;
        }
        if r.is_ok() {
            out.push(Value {
                name: String::from_utf16_lossy(&name[..name_len as usize]),
                kind: REG_VALUE_TYPE(kind),
                data: data[..data_len as usize].to_vec(),
            });
        }
        index += 1;
    }
    out
}

/// A REG_SZ / REG_EXPAND_SZ value (not expanded).
pub fn read_string(root: Root, subkey: &str, name: &str) -> Option<String> {
    let n = WideString::new(name);
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut len = 0u32;
    let hkey = open(root, subkey, KEY_READ)?;
    // SAFETY: size query then read into a buffer of that size.
    unsafe {
        RegGetValueW(hkey.0, None, n.pcwstr(), flags, None, None, Some(&mut len))
            .ok()
            .ok()?;
        let mut buf = vec![0u16; (len as usize / 2).max(1) + 1];
        let mut len = (buf.len() * 2) as u32;
        RegGetValueW(
            hkey.0,
            None,
            n.pcwstr(),
            flags,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
        Some(from_wide(&buf))
    }
}

/// A REG_DWORD value.
pub fn read_dword(root: Root, subkey: &str, name: &str) -> Option<u32> {
    let hkey = open(root, subkey, KEY_READ)?;
    let n = WideString::new(name);
    let mut v = 0u32;
    let mut len = 4u32;
    // SAFETY: out-pointer to a u32.
    unsafe {
        RegGetValueW(
            hkey.0,
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

/// A REG_BINARY value.
pub fn read_binary(root: Root, subkey: &str, name: &str) -> Option<Vec<u8>> {
    let hkey = open(root, subkey, KEY_READ)?;
    let n = WideString::new(name);
    let mut len = 0u32;
    // SAFETY: size query then read into a buffer of that size.
    unsafe {
        RegGetValueW(
            hkey.0,
            None,
            n.pcwstr(),
            RRF_RT_REG_BINARY,
            None,
            None,
            Some(&mut len),
        )
        .ok()
        .ok()?;
        let mut buf = vec![0u8; len as usize];
        RegGetValueW(
            hkey.0,
            None,
            n.pcwstr(),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
        buf.truncate(len as usize);
        Some(buf)
    }
}

/// Delete one value. Returns true on success (or if it did not exist).
pub fn delete_value(root: Root, subkey: &str, name: &str) -> bool {
    let Some(key) = open(root, subkey, KEY_SET_VALUE) else {
        return false;
    };
    let n = WideString::new(name);
    // SAFETY: valid key and name.
    let r = unsafe { RegDeleteValueW(key.0, n.pcwstr()) };
    r.is_ok() || r == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND
}
