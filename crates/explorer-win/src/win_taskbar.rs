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

use explorer_core::taskbar_restore::{
    plan_restore, settings_autohide, settings_with_autohide, state_may_be_removed, RestoreAction,
    RestoreInput,
};

use crate::util::WideString;

const PRIMARY: &str = "Shell_TrayWnd";
const SECONDARY: &str = "Shell_SecondaryTrayWnd";
/// `ABS_AUTOHIDE` / `ABS_ALWAYSONTOP` (shellapi.h).
const ABS_AUTOHIDE: usize = 0x1;
const ABS_ALWAYSONTOP: usize = 0x2;

/// Windows of `class` that belong to `explorer.exe`. Our own tray host also
/// creates a `Shell_TrayWnd` (in dev mode and in shell mode) in the taskbar
/// process, which is never the caller, so ruling out only the caller's own
/// windows is not enough: a restore run from another process (the
/// supervisor, `--restore-explorer`, the guardian) would take our tray host
/// for Explorer's taskbar.
fn windows_of(class: &str) -> Vec<HWND> {
    let c = WideString::new(class);
    let mut out = Vec::new();
    let mut prev: Option<HWND> = None;
    loop {
        // SAFETY: valid class string; `prev` is a window from the last call.
        let next = unsafe { FindWindowExW(None, prev, c.pcwstr(), PCWSTR::null()) };
        let Ok(h) = next else { break };
        if h.is_invalid() {
            break;
        }
        if crate::process::image_of_window(h)
            .as_deref()
            .is_some_and(explorer_core::guardian::is_explorer_image)
        {
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
///
/// With Explorer running its taskbar is asked; without it (shell mode, or
/// before `explorer.exe` is started by `--restore-explorer` / uninstall) the
/// setting is written to the registry, where Explorer reads it on start.
/// The state file is removed only once the setting reads back as restored.
pub fn restore(state_file: &Path) {
    let primary = windows_of(PRIMARY).first().copied();
    let registry = read_settings();
    let action = plan_restore(RestoreInput {
        saved_autohide: read_saved(state_file),
        explorer_taskbar_present: primary.is_some(),
        registry_autohide: registry.as_deref().and_then(settings_autohide),
    });
    let confirmed = match action {
        RestoreAction::Nothing => return,
        RestoreAction::Defer => {
            tracing::warn!("cannot restore Windows' taskbar yet (no Explorer, no registry value); keeping the saved state");
            return;
        }
        RestoreAction::AlreadyRestored => None,
        RestoreAction::ViaExplorer { autohide } => {
            let Some(primary) = primary else { return };
            let current = state(primary);
            set_state(
                primary,
                (current & ABS_ALWAYSONTOP) | if autohide { ABS_AUTOHIDE } else { 0 },
            );
            for h in explorer_taskbars() {
                // SAFETY: plain call on a foreign window.
                let _ = unsafe { ShowWindow(h, SW_SHOWNA) };
            }
            Some(state(primary) & ABS_AUTOHIDE != 0)
        }
        RestoreAction::WriteRegistry { autohide } => {
            let ok = registry
                .as_deref()
                .and_then(|b| settings_with_autohide(b, autohide))
                .is_some_and(|b| write_settings(&b));
            if !ok {
                tracing::warn!("cannot write Windows' taskbar settings");
            }
            read_settings().as_deref().and_then(settings_autohide)
        }
    };
    if state_may_be_removed(action, confirmed) {
        let _ = std::fs::remove_file(state_file);
        tracing::info!(?action, "Windows' taskbar restored");
    } else {
        tracing::warn!(
            ?action,
            ?confirmed,
            "Windows' taskbar not confirmed restored; keeping the saved state"
        );
    }
}

fn read_settings() -> Option<Vec<u8>> {
    crate::registry::read_binary(crate::registry::Root::CurrentUser, STUCK_RECTS, "Settings")
}

fn write_settings(data: &[u8]) -> bool {
    crate::registry::write_binary(
        crate::registry::Root::CurrentUser,
        STUCK_RECTS,
        "Settings",
        data,
    )
}

/// Where Explorer keeps the taskbar's settings; byte 8, bit 0 of the
/// "Settings" value is "Automatically hide the taskbar".
const STUCK_RECTS: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StuckRects3";

/// Windows' "Automatically hide the taskbar" setting, as the user chose it.
///
/// While we hide Windows' taskbar we switched it to auto-hide ourselves,
/// so the user's own choice is the one saved in `state_file`. Otherwise
/// Explorer's taskbar answers, and without Explorer (shell mode) the
/// setting it would load at sign-in.
pub fn system_autohide(state_file: &Path) -> bool {
    if let Some(was) = read_saved(state_file) {
        return was;
    }
    if let Some(&primary) = windows_of(PRIMARY).first() {
        return state(primary) & ABS_AUTOHIDE != 0;
    }
    read_settings()
        .as_deref()
        .and_then(settings_autohide)
        .unwrap_or(false)
}

/// While we hide Windows' taskbar we keep it on auto-hide, and the user's
/// own choice in the state file. Turning auto-hide off in Windows' settings
/// then reaches Explorer's taskbar, which takes its screen space back (and
/// a hidden window still reserves it). Record that choice and put it back
/// on auto-hide. Returns true when the choice changed.
pub fn follow_user_change(state_file: &Path) -> bool {
    let Some(was) = read_saved(state_file) else {
        return false;
    };
    let Some(&primary) = windows_of(PRIMARY).first() else {
        return false;
    };
    let current = state(primary);
    if current & ABS_AUTOHIDE != 0 {
        return false;
    }
    if let Err(e) = std::fs::write(state_file, "autohide_was = false\n") {
        tracing::warn!(error = %e, "cannot update Windows' taskbar state");
    }
    tracing::info!(
        autohide_was = was,
        "Windows' auto-hide turned off in Settings; recorded"
    );
    set_state(primary, (current & ABS_ALWAYSONTOP) | ABS_AUTOHIDE);
    reassert_hidden();
    was
}
