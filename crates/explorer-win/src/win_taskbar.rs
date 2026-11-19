// SPDX-License-Identifier: GPL-3.0-or-later
//! Hiding Windows' own taskbar while ours runs alongside Explorer
//! (`[taskbar] hide_windows_taskbar`).
//!
//! Hiding the window alone would leave its screen space reserved, so, as
//! other replacement taskbars do, we also switch it to auto-hide
//! (`ABM_SETSTATE`), which releases the work area; our own AppBar then
//! reserves the space. The previous auto-hide state is written to a state
//! file *before* anything changes, so it can be restored by whoever comes
//! next: the taskbar on exit, the supervisor when the taskbar stops for
//! good, the guardian, `--restore-explorer` or Ctrl+Alt+Shift+E. The file
//! exists exactly while Windows' taskbar is hidden by us.

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::Shell::{SHAppBarMessage, ABM_GETSTATE, ABM_SETSTATE, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, IsWindowVisible, ShowWindow, SW_HIDE, SW_SHOWNA,
};

use crate::util::WideString;

const PRIMARY: &str = "Shell_TrayWnd";
const SECONDARY: &str = "Shell_SecondaryTrayWnd";
/// `ABS_AUTOHIDE` / `ABS_ALWAYSONTOP` (shellapi.h).
const ABS_AUTOHIDE: usize = 0x1;
const ABS_ALWAYSONTOP: usize = 0x2;

/// Windows of `class` that belong to another process (not our own tray
/// host, which also creates a `Shell_TrayWnd` in dev mode).
fn windows_of(class: &str) -> Vec<HWND> {
    let c = WideString::new(class);
    let me = std::process::id();
    let mut out = Vec::new();
    let mut prev: Option<HWND> = None;
    loop {
        // SAFETY: valid class string; `prev` is a window from the last call.
        let next = unsafe { FindWindowExW(None, prev, c.pcwstr(), PCWSTR::null()) };
        let Ok(h) = next else { break };
        if h.is_invalid() {
            break;
        }
        if crate::process::window_pid(h) != me {
            out.push(h);
        }
        prev = Some(h);
        if out.len() > 64 {
            break;
        }
    }
    out
}

/// Every taskbar window of Explorer (primary and per-monitor ones).
pub fn explorer_taskbars() -> Vec<HWND> {
    let mut v = windows_of(PRIMARY);
    v.extend(windows_of(SECONDARY));
    v
}

/// Whether `hwnd` is one of Explorer's taskbar windows.
pub fn is_explorer_taskbar(hwnd: HWND) -> bool {
    explorer_taskbars().contains(&hwnd)
}

fn state(primary: HWND) -> usize {
    let mut d = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: primary,
        ..Default::default()
    };
    // SAFETY: valid APPBARDATA.
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut d) }
}

fn set_state(primary: HWND, s: usize) {
    let mut d = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: primary,
        lParam: LPARAM(s as isize),
        ..Default::default()
    };
    // SAFETY: valid APPBARDATA.
    unsafe { SHAppBarMessage(ABM_SETSTATE, &mut d) };
}

fn read_saved(file: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(file).ok()?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("autohide_was"))
        .map(|v| v.trim_start_matches([' ', '=']).trim() == "true")
}

/// Hide Windows' taskbar (no-op when Explorer has none). Idempotent: an
/// existing state file keeps the state saved first.
pub fn hide(state_file: &Path) {
    let Some(&primary) = windows_of(PRIMARY).first() else {
        return;
    };
    let current = state(primary);
    if read_saved(state_file).is_none() {
        let was = current & ABS_AUTOHIDE != 0;
        if let Some(dir) = state_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(state_file, format!("autohide_was = {was}\n")) {
            // Never change a setting we could not record.
            tracing::warn!(error = %e, "cannot save Windows' taskbar state; not hiding it");
            return;
        }
        tracing::info!(autohide_was = was, "hiding Windows' taskbar");
    }
    if current & ABS_AUTOHIDE == 0 {
        set_state(primary, (current & ABS_ALWAYSONTOP) | ABS_AUTOHIDE);
    }
    reassert_hidden();
}

/// Hide any of Explorer's taskbars that became visible again (Explorer
/// shows them on some events, and after it restarts).
pub fn reassert_hidden() {
    for h in explorer_taskbars() {
        // SAFETY: plain calls on a foreign window.
        unsafe {
            if IsWindowVisible(h).as_bool() {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }
}

/// Bring Windows' taskbar back with its saved auto-hide state, if we hid
/// it (state file present). Safe to call from any process, any time.
pub fn restore(state_file: &Path) {
    let Some(was) = read_saved(state_file) else {
        return;
    };
    let Some(&primary) = windows_of(PRIMARY).first() else {
        // Explorer is not running: keep the file, the next restore (once
        // Explorer is back) still knows the user's setting.
        return;
    };
    let current = state(primary);
    let s = (current & ABS_ALWAYSONTOP) | if was { ABS_AUTOHIDE } else { 0 };
    set_state(primary, s);
    for h in explorer_taskbars() {
        // SAFETY: plain call on a foreign window.
        let _ = unsafe { ShowWindow(h, SW_SHOWNA) };
    }
    let _ = std::fs::remove_file(state_file);
    tracing::info!(autohide = was, "Windows' taskbar restored");
}
