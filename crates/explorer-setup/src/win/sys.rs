// SPDX-License-Identifier: GPL-3.0-or-later
//! Known folders, processes and files.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use explorer_win::util::WideString;
use explorer_win::windows::core::{GUID, HSTRING, PCWSTR};
use explorer_win::windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LUID, WPARAM};
use explorer_win::windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, SE_DEBUG_NAME, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use explorer_win::windows::Win32::Storage::FileSystem::{
    DeleteFileW, MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH,
};
use explorer_win::windows::Win32::System::Com::CoTaskMemFree;
use explorer_win::windows::Win32::System::ProcessStatus::EnumProcesses;
use explorer_win::windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, TerminateProcess, WaitForSingleObject,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};
use explorer_win::windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetShellWindow, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};

/// Path of a known folder (e.g. `FOLDERID_ProgramFiles`).
pub fn known_folder(id: &GUID) -> Result<String> {
    // SAFETY: the returned buffer is freed with CoTaskMemFree.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).context("SHGetKnownFolderPath")?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        if s.is_empty() {
            anyhow::bail!("empty known folder path");
        }
        Ok(s.trim_end_matches('\\').to_string())
    }
}

/// Enable SeDebugPrivilege (administrators hold it) so processes of other
/// users or integrity levels can be stopped. Failure is logged only.
pub fn enable_debug_privilege() {
    // SAFETY: token handle closed below; structures are valid.
    unsafe {
        let mut tok = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut tok,
        )
        .is_err()
        {
            return;
        }
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(PCWSTR::null(), SE_DEBUG_NAME, &mut luid).is_ok() {
            let mut tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                ..Default::default()
            };
            tp.Privileges[0].Luid = luid;
            tp.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
            if let Err(e) = AdjustTokenPrivileges(tok, false, Some(&tp), 0, None, None) {
                tracing::warn!(error = %e, "could not enable SeDebugPrivilege");
            }
        }
        let _ = CloseHandle(tok);
    }
}

/// All processes with their image path (processes we cannot query are
/// skipped).
pub fn processes() -> Vec<(u32, String)> {
    let mut pids = vec![0u32; 4096];
    let mut needed = 0u32;
    // SAFETY: buffer and byte size match.
    if unsafe { EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut needed) }.is_err() {
        return Vec::new();
    }
    pids.truncate(needed as usize / 4);
    pids.into_iter()
        .filter(|&p| p != 0)
        .filter_map(|pid| {
            explorer_win::security::process_image_path(pid)
                .ok()
                .map(|p| (pid, p.to_string_lossy().into_owned()))
        })
        .collect()
}

struct Proc(HANDLE);
impl Drop for Proc {
    fn drop(&mut self) {
        // SAFETY: handle opened by us.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn open_for_stop(pid: u32) -> Option<Proc> {
    // SAFETY: plain FFI call; handle closed by Drop.
    unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) }
        .ok()
        .map(Proc)
}

/// Wait until process `pid` exits (true) or `timeout` passes (false). A
/// process that cannot be opened counts as exited.
pub fn wait_exit(pid: u32, timeout: Duration) -> bool {
    let Some(p) = open_for_stop(pid) else {
        return true;
    };
    // SAFETY: valid handle.
    unsafe { WaitForSingleObject(p.0, timeout.as_millis().min(u32::MAX as u128) as u32) }.0 == 0
}

/// Ask the processes to close (WM_CLOSE to their top-level windows), wait
/// up to `grace`, then terminate whatever is left, in the given order.
/// Returns the processes that could not be stopped.
pub fn stop_processes(procs: &[(u32, String)], grace: Duration) -> Vec<(u32, String)> {
    if procs.is_empty() {
        return Vec::new();
    }
    struct Ctx<'a> {
        pids: &'a [u32],
        posted: usize,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> explorer_win::windows::core::BOOL {
        // SAFETY: lp is the &mut Ctx passed below, valid during EnumWindows.
        let ctx = unsafe { &mut *(lp.0 as *mut Ctx) };
        let mut pid = 0u32;
        // SAFETY: valid out-pointer.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if ctx.pids.contains(&pid) {
            // SAFETY: posting to a window; no pointers.
            if unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok() {
                ctx.posted += 1;
            }
        }
        true.into()
    }
    let pids: Vec<u32> = procs.iter().map(|p| p.0).collect();
    let mut ctx = Ctx {
        pids: &pids,
        posted: 0,
    };
    // SAFETY: callback only uses ctx during the call.
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize)) };
    tracing::info!(posted = ctx.posted, "asked explorer.rs windows to close");

    let deadline = Instant::now() + grace;
    for (pid, _) in procs {
        let left = deadline.saturating_duration_since(Instant::now());
        wait_exit(*pid, left);
    }
    let mut failed = Vec::new();
    for (pid, path) in procs {
        let Some(p) = open_for_stop(*pid) else {
            continue; // already gone
        };
        // SAFETY: valid handle.
        if unsafe { WaitForSingleObject(p.0, 0) }.0 == 0 {
            continue;
        }
        tracing::info!(pid, path, "terminating");
        // SAFETY: valid handle with PROCESS_TERMINATE.
        if let Err(e) = unsafe { TerminateProcess(p.0, 1) } {
            tracing::warn!(pid, path, error = %e, "TerminateProcess failed");
        }
        // SAFETY: valid handle.
        if unsafe { WaitForSingleObject(p.0, 5000) }.0 != 0 {
            failed.push((*pid, path.clone()));
        }
    }
    failed
}

/// True if Windows Explorer provides the desktop (owns the shell window).
pub fn explorer_shell_running() -> bool {
    // SAFETY: plain queries.
    let shell = unsafe { GetShellWindow() };
    if shell.is_invalid() {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: valid out-pointer.
    unsafe { GetWindowThreadProcessId(shell, Some(&mut pid)) };
    explorer_win::security::process_image_path(pid)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .is_some_and(|n| n == "explorer.exe")
}

fn move_file(from: &Path, to: Option<&Path>, flags: u32) -> Result<()> {
    let f = HSTRING::from(from.as_os_str());
    let t = to.map(|t| HSTRING::from(t.as_os_str()));
    // SAFETY: valid strings for the call.
    unsafe {
        MoveFileExW(
            &f,
            t.as_ref()
                .map(|t| PCWSTR(t.as_ptr()))
                .unwrap_or(PCWSTR::null()),
            explorer_win::windows::Win32::Storage::FileSystem::MOVE_FILE_FLAGS(flags),
        )
    }
    .with_context(|| format!("MoveFileExW {} -> {:?}", from.display(), to))
}

/// Delete `path` now, or at the next reboot if it is in use. Returns true if
/// a reboot is needed. A missing file is fine.
pub fn delete_file(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let p = WideString::new(&path.to_string_lossy());
    // SAFETY: valid string.
    if unsafe { DeleteFileW(p.pcwstr()) }.is_ok() {
        return Ok(false);
    }
    // In use (e.g. a running exe): rename out of the way first so the
    // folder can still be removed, then delete at reboot.
    let old = unique_old_name(path);
    let target = if move_file(path, Some(&old), 0).is_ok() {
        old
    } else {
        path.to_path_buf()
    };
    move_file(&target, None, MOVEFILE_DELAY_UNTIL_REBOOT.0)?;
    tracing::info!(path = %target.display(), "scheduled for deletion at reboot");
    Ok(true)
}

fn unique_old_name(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (0..1000)
        .map(|i| path.with_file_name(format!("{name}.{i}.old")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.with_file_name(format!("{name}.{}.old", std::process::id())))
}

/// Write `data` to `dest` atomically: temp file + rename. If `dest` is
/// locked (a process we could not stop), it is renamed to `*.old` and
/// deleted at reboot. Returns true if a reboot is needed for cleanup.
pub fn write_file(dest: &Path, data: &[u8]) -> Result<bool> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dest.with_file_name(format!("{name}.new-{}", std::process::id()));
    {
        use std::io::Write;
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(data)
            .and_then(|_| f.sync_all())
            .with_context(|| format!("write {}", tmp.display()))?;
    }
    let flags = MOVEFILE_REPLACE_EXISTING.0 | MOVEFILE_WRITE_THROUGH.0;
    if move_file(&tmp, Some(dest), flags).is_ok() {
        return Ok(false);
    }
    // Locked: move the old file aside (allowed for running executables).
    let old = unique_old_name(dest);
    let result = move_file(dest, Some(&old), 0)
        .and_then(|_| move_file(&tmp, Some(dest), flags))
        .with_context(|| format!("replace {}", dest.display()));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    delete_file(&old)
}

/// Copy `src` to `dest` through [`write_file`].
pub fn copy_file(src: &Path, dest: &Path) -> Result<bool> {
    let data = std::fs::read(src).with_context(|| format!("read {}", src.display()))?;
    write_file(dest, &data)
}
