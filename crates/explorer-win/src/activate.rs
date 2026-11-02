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

/// Bring one of *our own* windows to the foreground even when another
/// process currently owns it. Tries the plain call first; if Windows'
/// foreground lock refuses, temporarily attaches our input queue to the
/// foreground thread (documented `AttachThreadInput` technique).
pub fn force_foreground(hwnd: HWND) {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId,
    };
    // SAFETY: plain FFI calls; the attach is always undone.
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return;
        }
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        if fg_thread != 0 && fg_thread != me {
            use windows::Win32::System::Threading::AttachThreadInput;
            let attached = AttachThreadInput(me, fg_thread, true).as_bool();
            let _ = BringWindowToTop(hwnd);
            let ok = SetForegroundWindow(hwnd).as_bool();
            let _ = SetFocus(Some(hwnd));
            if attached {
                let _ = AttachThreadInput(me, fg_thread, false);
            }
            if !ok {
                tracing::warn!("could not take the foreground (foreground lock)");
            }
        }
    }
}
