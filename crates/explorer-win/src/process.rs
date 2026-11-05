// SPDX-License-Identifier: GPL-3.0-or-later
//! Process helpers for the guardian, supervisor, setup and `--status`:
//! list processes of this session, ask explorer.rs processes to stop
//! gracefully, find the guardian, detect the Windows Explorer shell, start
//! explorer.exe.

use windows::core::BOOL;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, TerminateProcess, WaitForSingleObject, INFINITE,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GetShellWindow, GetWindowThreadProcessId, PostMessageW,
    HWND_MESSAGE,
};

use crate::util::from_wide;

/// A process in the current session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// Executable file name, e.g. `explorer.exe`.
    pub exe: String,
}

/// Windows session of process `pid`.
pub fn session_of(pid: u32) -> Option<u32> {
    let mut s = 0u32;
    // SAFETY: valid out-pointer.
    unsafe { ProcessIdToSessionId(pid, &mut s) }.ok().map(|_| s)
}

/// All processes of the current Windows session.
pub fn session_processes() -> Vec<ProcessInfo> {
    // SAFETY: plain query.
    let own_session = session_of(unsafe { GetCurrentProcessId() });
    enumerate(|pid| own_session.is_none() || session_of(pid) == own_session)
}

/// All processes of every session.
pub fn all_processes() -> Vec<ProcessInfo> {
    enumerate(|_| true)
}

fn enumerate(keep: impl Fn(u32) -> bool) -> Vec<ProcessInfo> {
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
            if keep(pid) {
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

/// Owner process id of a window (0 if unknown).
pub fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: valid out-pointer; a stale handle just yields 0.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn image_of_window(hwnd: HWND) -> Option<String> {
    let pid = window_pid(hwnd);
    if pid == 0 {
        return None;
    }
    crate::security::process_image_path(pid)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Is the Windows Explorer **shell** running on this desktop? True when
/// the shell window (`GetShellWindow`) or a `Shell_TrayWnd` window belongs
/// to explorer.exe. A File Explorer window alone (explorer.exe without the
/// shell) does not count, nor does our own tray host (its class is also
/// `Shell_TrayWnd`, but it belongs to explorer-rs-taskbar.exe).
pub fn explorer_shell_running() -> bool {
    // SAFETY: plain query.
    let shell = unsafe { GetShellWindow() };
    let shell_owner = (!shell.is_invalid())
        .then(|| image_of_window(shell))
        .flatten();
    let class = crate::util::WideString::new(crate::tray::TRAY_WINDOW_CLASS);
    let mut trays = Vec::new();
    let mut prev: Option<HWND> = None;
    // SAFETY: valid class string; bounded loop.
    unsafe {
        for _ in 0..16 {
            match FindWindowExW(None, prev, class.pcwstr(), None) {
                Ok(h) if !h.is_invalid() => {
                    if let Some(img) = image_of_window(h) {
                        trays.push(img);
                    }
                    prev = Some(h);
                }
                _ => break,
            }
        }
    }
    explorer_core::guardian::explorer_shell_running(
        shell_owner.as_deref(),
        trays.iter().map(String::as_str),
    )
}

/// The guardian's message-only window (`ExplorerRs.Hotkeys`), if a
/// guardian runs on this desktop.
pub fn guardian_window() -> Option<HWND> {
    let class = crate::util::WideString::new(explorer_core::ipc::GUARDIAN_WINDOW_CLASS);
    // SAFETY: valid class string.
    unsafe {
        FindWindowExW(
            Some(HWND_MESSAGE),
            None,
            class.pcwstr(),
            windows::core::PCWSTR::null(),
        )
    }
    .ok()
    .filter(|h| !h.is_invalid())
}

/// Post a registered message (by name) to the guardian's window. Returns
/// false if no guardian runs.
pub fn post_to_guardian(message: &str) -> bool {
    let Some(h) = guardian_window() else {
        return false;
    };
    let msg = crate::ipc::registered_message(message);
    // SAFETY: posting never blocks; no pointers.
    unsafe { PostMessageW(Some(h), msg, WPARAM(0), LPARAM(0)) }.is_ok()
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

/// Wait (up to `timeout`) for process `pid` to exit. A process that cannot
/// be opened counts as exited.
pub fn wait_exit(pid: u32, timeout: std::time::Duration) -> bool {
    // SAFETY: handle closed below.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return true;
        };
        let ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
        let r = WaitForSingleObject(h, ms);
        let _ = CloseHandle(h);
        r.0 == 0
    }
}

struct SendHandle(HANDLE);
// SAFETY: a process handle may be waited on from any thread.
unsafe impl Send for SendHandle {}

/// Call `on_exit` (on a new thread) when process `pid` exits. Returns false
/// if the process cannot be opened (it may already be gone).
pub fn on_process_exit(pid: u32, on_exit: impl FnOnce() + Send + 'static) -> bool {
    // SAFETY: the handle is closed by the waiting thread.
    let h = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(h) => SendHandle(h),
        Err(_) => return false,
    };
    std::thread::Builder::new()
        .name("process-watch".into())
        .spawn(move || {
            let h = h;
            // SAFETY: valid handle owned by this thread.
            unsafe {
                WaitForSingleObject(h.0, INFINITE);
                let _ = CloseHandle(h.0);
            }
            on_exit();
        })
        .is_ok()
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

struct PostCtx {
    pids: Vec<u32>,
    msg: u32,
    posted: usize,
}

unsafe extern "system" fn post_proc(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` points to the PostCtx owned by `post_shutdown_all`
    // for the duration of EnumWindows.
    let ctx = unsafe { &mut *(data.0 as *mut PostCtx) };
    let pid = window_pid(hwnd);
    if ctx.pids.contains(&pid) {
        // SAFETY: posting never blocks on the target.
        if unsafe { PostMessageW(Some(hwnd), ctx.msg, WPARAM(0), LPARAM(0)) }.is_ok() {
            ctx.posted += 1;
        }
    }
    BOOL(1)
}

/// Post the graceful-stop message (`ExplorerRs.Shutdown`) to every
/// top-level window of the given processes (non-blocking). Components exit
/// with code 0; a supervisor stops its components first; a guardian stops
/// its supervisor and records a clean end. Returns how many windows were
/// asked.
pub fn post_shutdown_all(pids: &[u32]) -> usize {
    let mut ctx = PostCtx {
        pids: pids.to_vec(),
        msg: crate::window::shutdown_message(),
        posted: 0,
    };
    // SAFETY: the callback only uses `ctx` during this call.
    let _ = unsafe { EnumWindows(Some(post_proc), LPARAM(&mut ctx as *mut PostCtx as isize)) };
    ctx.posted
}

/// [`post_shutdown_all`] for one process.
pub fn post_shutdown(pid: u32) -> usize {
    post_shutdown_all(&[pid])
}
