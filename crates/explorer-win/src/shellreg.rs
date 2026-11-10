// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-user shell registration I/O (see `explorer_core::shellreg` for the
//! rules). Works on the current user's hive or, from an elevated installer,
//! on another loaded user hive (`HKEY_USERS\<SID>`), because an elevated
//! process's HKCU can belong to a different (administrator) account.

use anyhow::{Context, Result};
use explorer_core::shellreg::{
    backup_on_register, restore, shell_command, BackupAction, RestoreAction, BACKUP_EXISTED_VALUE,
    BACKUP_SUBKEY, BACKUP_VALUE, WINLOGON_SUBKEY,
};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegOpenKeyExW,
    RegSetKeyValueW, HKEY, HKEY_CURRENT_USER, HKEY_USERS, KEY_READ, KEY_WRITE, REG_DWORD,
    REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};

use crate::util::WideString;

/// Which user hive to operate on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hive {
    CurrentUser,
    /// `HKEY_USERS\<sid>` (the user must be signed in, i.e. hive loaded).
    User(String),
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: key opened by us.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

impl Hive {
    /// Open (`create` = create if missing) `subkey` under this hive.
    fn open(&self, subkey: &str, write: bool, create: bool) -> Result<Option<Key>> {
        let (root, path) = match self {
            Hive::CurrentUser => (HKEY_CURRENT_USER, subkey.to_string()),
            Hive::User(sid) => (HKEY_USERS, format!(r"{sid}\{subkey}")),
        };
        let p = WideString::new(&path);
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
                    root,
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
                RegOpenKeyExW(root, p.pcwstr(), None, access, &mut k)
            }
        };
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        r.ok()
            .with_context(|| format!("open registry key {path}"))?;
        Ok(Some(Key(k)))
    }

    fn get_string(&self, subkey: &str, name: &str) -> Option<String> {
        let key = self.open(subkey, false, false).ok()??;
        let n = WideString::new(name);
        let mut len = 0u32;
        // SAFETY: size query then read into a buffer of that size.
        unsafe {
            RegGetValueW(
                key.0,
                None,
                n.pcwstr(),
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut len),
            )
            .ok()
            .ok()?;
            let mut buf = vec![0u16; (len as usize / 2).max(1)];
            RegGetValueW(
                key.0,
                None,
                n.pcwstr(),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            )
            .ok()
            .ok()?;
            Some(crate::util::from_wide(&buf))
        }
    }

    fn get_dword(&self, subkey: &str, name: &str) -> Option<u32> {
        let key = self.open(subkey, false, false).ok()??;
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

    fn set_string(&self, subkey: &str, name: &str, value: &str) -> Result<()> {
        let key = self.open(subkey, true, true)?.context("create key")?;
        let n = WideString::new(name);
        let v: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        // SAFETY: buffer and byte length match.
        unsafe {
            RegSetKeyValueW(
                key.0,
                None,
                n.pcwstr(),
                REG_SZ.0,
                Some(v.as_ptr().cast()),
                (v.len() * 2) as u32,
            )
        }
        .ok()
        .with_context(|| format!("set {name}"))
    }

    fn set_dword(&self, subkey: &str, name: &str, value: u32) -> Result<()> {
        let key = self.open(subkey, true, true)?.context("create key")?;
        let n = WideString::new(name);
        // SAFETY: pointer to a u32.
        unsafe {
            RegSetKeyValueW(
                key.0,
                None,
                n.pcwstr(),
                REG_DWORD.0,
                Some((&value as *const u32).cast()),
                4,
            )
        }
        .ok()
        .with_context(|| format!("set {name}"))
    }

    fn delete_value(&self, subkey: &str, name: &str) -> Result<()> {
        let Some(key) = self.open(subkey, true, false)? else {
            return Ok(());
        };
        let n = WideString::new(name);
        // SAFETY: valid key and name.
        let r = unsafe { RegDeleteKeyValueW(key.0, None, n.pcwstr()) };
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        r.ok().with_context(|| format!("delete {name}"))
    }

    fn delete_tree(&self, subkey: &str) -> Result<()> {
        let Some(key) = self.open(subkey, true, false)? else {
            return Ok(());
        };
        // SAFETY: valid key.
        let r = unsafe { RegDeleteTreeW(key.0, None) };
        drop(key);
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        r.ok().context("delete backup tree")
    }

    /// Current per-user Shell value.
    pub fn shell(&self) -> Option<String> {
        self.get_string(WINLOGON_SUBKEY, "Shell")
    }

    /// `Some(prev)` if a usable backup exists.
    fn backup(&self) -> Option<Option<String>> {
        let existed = self.get_dword(BACKUP_SUBKEY, BACKUP_EXISTED_VALUE)?;
        Some(if existed != 0 {
            self.get_string(BACKUP_SUBKEY, BACKUP_VALUE)
        } else {
            None
        })
    }

    /// Register `exe` as this user's shell, backing up the previous value.
    pub fn register_shell(&self, exe: &str) -> Result<()> {
        let current = self.shell();
        if let BackupAction::Record(prev) = backup_on_register(current.as_deref(), self.backup()) {
            self.set_string(BACKUP_SUBKEY, BACKUP_VALUE, prev.as_deref().unwrap_or(""))?;
            self.set_dword(BACKUP_SUBKEY, BACKUP_EXISTED_VALUE, prev.is_some() as u32)?;
        }
        self.set_string(WINLOGON_SUBKEY, "Shell", &shell_command(exe))
    }

    /// Undo [`Self::register_shell`] if the Shell value is ours. Returns
    /// true if anything changed.
    pub fn restore_shell(&self) -> Result<bool> {
        let action = restore(self.shell().as_deref(), self.backup());
        let changed = match &action {
            RestoreAction::Nothing => false,
            RestoreAction::DeleteValue => {
                self.delete_value(WINLOGON_SUBKEY, "Shell")?;
                true
            }
            RestoreAction::SetValue(v) => {
                self.set_string(WINLOGON_SUBKEY, "Shell", v)?;
                true
            }
        };
        if changed {
            self.delete_tree(BACKUP_SUBKEY)?;
        }
        tracing::info!(?action, hive = ?self, "restore shell");
        Ok(changed)
    }
}

/// SID of the interactive (signed-in) user. From an elevated installer
/// this is the signed-in user even when elevation used another account.
/// Fallback chain:
/// 1. owner of the shell window (`GetShellWindow`);
/// 2. owner of a `Shell_TrayWnd` window (Explorer's or our taskbar's);
/// 3. user of an explorer.exe / explorer-rs.exe process in the active
///    console session;
/// 4. this process's own user (a warning is logged when elevated: it may
///    be the administrator who approved elevation, not the signed-in user).
pub fn interactive_user_sid() -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetShellWindow};
    let sid_of_window = |h: windows::Win32::Foundation::HWND| -> Option<String> {
        let pid = crate::process::window_pid(h);
        (pid != 0)
            .then(|| crate::security::process_user_sid(pid).ok())
            .flatten()
    };
    // SAFETY: plain query.
    let shell = unsafe { GetShellWindow() };
    if !shell.is_invalid() {
        if let Some(sid) = sid_of_window(shell) {
            return Some(sid);
        }
    }
    let class = crate::util::WideString::new(crate::tray::TRAY_WINDOW_CLASS);
    // SAFETY: valid class string.
    if let Ok(tray) = unsafe { FindWindowExW(None, None, class.pcwstr(), None) } {
        if !tray.is_invalid() {
            if let Some(sid) = sid_of_window(tray) {
                tracing::info!("interactive user taken from the tray window owner");
                return Some(sid);
            }
        }
    }
    if let Some(sid) = console_session_user() {
        tracing::info!("interactive user taken from a shell process of the console session");
        return Some(sid);
    }
    let own = crate::security::current_user_sid().ok();
    if crate::security::is_elevated() {
        tracing::warn!(
            sid = ?own,
            "could not find the signed-in user; using this (elevated) process's own user"
        );
    }
    own
}

/// User of an explorer.exe or explorer-rs.exe process in the active console
/// session.
fn console_session_user() -> Option<String> {
    use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
    // SAFETY: plain query.
    let console = unsafe { WTSGetActiveConsoleSessionId() };
    if console == u32::MAX {
        return None;
    }
    let mut candidates: Vec<(u8, u32)> = crate::process::all_processes()
        .into_iter()
        .filter(|p| crate::process::session_of(p.pid) == Some(console))
        .filter_map(|p| Some((explorer_core::shellreg::shell_process_rank(&p.exe)?, p.pid)))
        .collect();
    candidates.sort();
    candidates
        .into_iter()
        .find_map(|(_, pid)| crate::security::process_user_sid(pid).ok())
}
