// SPDX-License-Identifier: GPL-3.0-or-later
//! System tray host (feature `tray`) — SCAFFOLD, not functional yet.
//!
//! How the notification-area protocol works (from public documentation and
//! observed behaviour; no code copied from other projects):
//! - Applications call `Shell_NotifyIconW`, which finds the window of class
//!   `Shell_TrayWnd` and sends it `WM_COPYDATA` with `dwData == 1`. The
//!   payload is an undocumented wrapper: a 32-bit magic/signature, the
//!   `NIM_*` message, then a `NOTIFYICONDATAW` with 32-bit handle fields
//!   (layout must be verified on a real machine before parsing).
//! - When a tray host starts it must broadcast the registered message
//!   `TaskbarCreated` so running apps re-add their icons.
//! - `SHAppBarMessage` also arrives at `Shell_TrayWnd` as `WM_COPYDATA`
//!   (`dwData == 0`); a full shell must answer it (AppBar server).
//!
//! Only valid in shell mode: in dev mode explorer.exe already owns
//! `Shell_TrayWnd` and creating a second one would steal icons from it.
//!
//! TODO(tray):
//! 1. Verify the WM_COPYDATA payload layout on Windows 11 (x64 + WOW64 apps).
//! 2. Parse NIM_ADD/MODIFY/DELETE/SETVERSION into an icon model in
//!    explorer-core (pure, tested).
//! 3. Render icons (HICON -> D2D bitmap) and forward mouse events with the
//!    correct `NOTIFYICON_VERSION_4` semantics.
//! 4. Implement the AppBar server side for `dwData == 0`.

use anyhow::Result;
use explorer_core::ipc::RunMode;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, HWND_BROADCAST};

use crate::ipc::registered_message;

/// Window class apps look for when calling `Shell_NotifyIconW`.
pub const TRAY_WINDOW_CLASS: &str = "Shell_TrayWnd";

/// Tell running applications to (re-)register their tray icons.
pub fn broadcast_taskbar_created() -> Result<()> {
    let msg = registered_message("TaskbarCreated");
    // SAFETY: broadcast post without pointers.
    unsafe { PostMessageW(Some(HWND_BROADCAST), msg, WPARAM(0), LPARAM(0))? };
    Ok(())
}

/// Entry point for the future tray host. Currently refuses to run outside
/// shell mode and otherwise only logs that the feature is a scaffold.
pub fn start(mode: RunMode) -> Result<()> {
    if mode != RunMode::Shell {
        tracing::info!("tray: dev mode, explorer.exe owns the notification area; not starting");
        return Ok(());
    }
    tracing::warn!("tray: feature compiled in but the tray host is not implemented yet (TODO)");
    Ok(())
}
