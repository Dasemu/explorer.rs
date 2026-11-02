// SPDX-License-Identifier: GPL-3.0-or-later
//! Cross-process signalling via registered window messages.

use explorer_core::ipc::{LauncherCommand, LAUNCHER_WINDOW_CLASS, MSG_LAUNCHER_COMMAND};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, PostMessageW, RegisterWindowMessageW,
};

use crate::util::WideString;

pub fn registered_message(name: &str) -> u32 {
    let w = WideString::new(name);
    // SAFETY: valid string.
    unsafe { RegisterWindowMessageW(w.pcwstr()) }
}

pub fn launcher_message() -> u32 {
    registered_message(MSG_LAUNCHER_COMMAND)
}

pub fn find_window(class: &str) -> Option<HWND> {
    let w = WideString::new(class);
    // SAFETY: valid string.
    unsafe { FindWindowW(w.pcwstr(), None) }.ok()
}

/// Ask the launcher to toggle/show/hide. Grants it the right to take the
/// foreground, which we hold because we just received user input. Returns
/// false when no launcher is running (e.g. it is being restarted).
pub fn send_launcher_command(cmd: LauncherCommand) -> bool {
    let Some(hwnd) = find_window(LAUNCHER_WINDOW_CLASS) else {
        tracing::warn!("launcher window not found; is the launcher running?");
        return false;
    };
    let pid = crate::winlist::process_id(hwnd);
    // SAFETY: plain FFI calls.
    unsafe {
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        PostMessageW(
            Some(hwnd),
            launcher_message(),
            WPARAM(cmd as usize),
            LPARAM(0),
        )
        .is_ok()
    }
}
