// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop-window helpers: virtual screen and the undocumented
//! `SetShellWindow` (ADR 0007; failure is harmless).

use windows::core::s;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// Bounding rectangle of all monitors, physical pixels.
pub fn virtual_screen() -> RECT {
    // SAFETY: plain queries.
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
        RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }
}

/// Register `hwnd` as the shell window (what `GetShellWindow` returns).
/// Undocumented user32 export, resolved at run time. It fails (returns
/// false) when another shell window exists, e.g. explorer.exe's.
pub fn set_shell_window(hwnd: HWND) -> bool {
    type SetShellWindowFn = unsafe extern "system" fn(HWND) -> i32;
    // SAFETY: user32 is always loaded in a GUI process; the export, if
    // present, has the signature BOOL SetShellWindow(HWND).
    unsafe {
        let Ok(user32) = GetModuleHandleW(windows::core::w!("user32.dll")) else {
            return false;
        };
        let Some(f) = GetProcAddress(user32, s!("SetShellWindow")) else {
            tracing::info!("SetShellWindow is not exported; skipped");
            return false;
        };
        let f: SetShellWindowFn = std::mem::transmute(f);
        let ok = f(hwnd) != 0;
        tracing::info!(ok, "SetShellWindow");
        ok
    }
}
