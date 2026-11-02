// SPDX-License-Identifier: GPL-3.0-or-later
//! Activating, minimising and closing other applications' windows.
//!
//! We use `WM_SYSCOMMAND` like the Windows taskbar does, so applications
//! that customise minimise/restore (e.g. minimise-to-tray) behave normally.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, PostMessageW, SetForegroundWindow, ShowWindow, SC_CLOSE, SC_MINIMIZE, SC_RESTORE,
    SW_SHOW, WM_SYSCOMMAND,
};

fn syscommand(hwnd: HWND, cmd: u32) {
    // SAFETY: posting to a foreign window is safe; failure is ignored
    // because the window may have vanished meanwhile.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0)) };
}

/// Bring `hwnd` to the foreground, restoring it if minimised.
pub fn activate(hwnd: HWND) {
    // SAFETY: plain FFI calls on a window handle.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            syscommand(hwnd, SC_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            tracing::debug!(hwnd = ?hwnd, "SetForegroundWindow refused");
        }
    }
}

pub fn minimize(hwnd: HWND) {
    syscommand(hwnd, SC_MINIMIZE);
}

pub fn close(hwnd: HWND) {
    syscommand(hwnd, SC_CLOSE);
}
