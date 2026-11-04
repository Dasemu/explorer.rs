// SPDX-License-Identifier: GPL-3.0-or-later
//! Process helpers for the guardian, supervisor and `--status`: list
//! processes of this session, ask them to close, start explorer.exe.

use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, TerminateProcess, PROCESS_TERMINATE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};

use crate::util::from_wide;

/// A process in the current session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// Executable file name, e.g. `explorer.exe`.
    pub exe: String,
}

fn session_of(pid: u32) -> Option<u32> {
    let mut s = 0u32;
    // SAFETY: valid out-pointer.
    unsafe { ProcessIdToSessionId(pid, &mut s) }.ok().map(|_| s)
}

/// All processes of the current Windows session.
pub fn session_processes() -> Vec<ProcessInfo> {
    // SAFETY: plain query.
    let own_session = session_of(unsafe { GetCurrentProcessId() });
    let mut out = Vec::new();
    // SAFETY: snapshot handle is closed below; entry struct is sized.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let pid = e.th32ProcessID;
            if own_session.is_none() || session_of(pid) == own_session {
                out.push(ProcessInfo {
                    pid,
                    exe: from_wide(&e.szExeFile),
                });
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Is explorer.exe running in this session?
pub fn explorer_running() -> bool {
    session_processes()
        .iter()
        .any(|p| p.exe.eq_ignore_ascii_case("explorer.exe"))
}

/// Full path of explorer.exe (`%WINDIR%\explorer.exe`).
pub fn explorer_path() -> std::path::PathBuf {
    let windir = std::env::var_os("WINDIR")
        .or_else(|| std::env::var_os("SystemRoot"))
        .unwrap_or_else(|| "C:\\Windows".into());
    std::path::PathBuf::from(windir).join("explorer.exe")
}

/// Start explorer.exe. Never panics; errors are logged and returned.
pub fn start_explorer() -> std::io::Result<()> {
    let path = explorer_path();
    tracing::warn!(path = %path.display(), "starting explorer.exe");
    match std::process::Command::new(&path).spawn() {
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::error!(error = %e, "could not start explorer.exe");
            Err(e)
        }
    }
}

/// Terminate a process (best effort).
pub fn terminate(pid: u32) -> bool {
    // SAFETY: handle closed below.
    unsafe {
        match OpenProcess(PROCESS_TERMINATE, false, pid) {
            Ok(h) => {
                let ok = TerminateProcess(h, 1).is_ok();
                let _ = CloseHandle(h);
                ok
            }
            Err(_) => false,
        }
    }
}

struct CloseCtx {
    pid: u32,
    posted: usize,
}

unsafe extern "system" fn close_proc(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` points to the CloseCtx owned by `post_close` for the
    // duration of EnumWindows.
    let ctx = unsafe { &mut *(data.0 as *mut CloseCtx) };
    let mut pid = 0u32;
    // SAFETY: valid out-pointer.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == ctx.pid {
        // SAFETY: posting never blocks on the target.
        if unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok() {
            ctx.posted += 1;
        }
    }
    BOOL(1)
}

/// Post `WM_CLOSE` to every top-level window of `pid` (non-blocking).
/// Returns how many windows were asked to close.
pub fn post_close(pid: u32) -> usize {
    let mut ctx = CloseCtx { pid, posted: 0 };
    // SAFETY: the callback only uses `ctx` during this call.
    let _ = unsafe { EnumWindows(Some(close_proc), LPARAM(&mut ctx as *mut CloseCtx as isize)) };
    ctx.posted
}
