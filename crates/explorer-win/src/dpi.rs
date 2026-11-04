// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-monitor DPI awareness.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// Opt the process into Per-Monitor v2 DPI awareness. Must run before any
/// window is created. Failure (e.g. already set by a manifest) is harmless.
pub fn enable_per_monitor_v2() {
    // SAFETY: plain FFI call without pointers.
    if let Err(e) =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
    {
        tracing::debug!(error = %e, "SetProcessDpiAwarenessContext failed (already set?)");
    }
}

/// DPI of the monitor the window is on (96 if unknown).
pub fn window_dpi(hwnd: HWND) -> u32 {
    // SAFETY: GetDpiForWindow tolerates invalid handles (returns 0).
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => 96,
        d => d,
    }
}

pub fn scale(dpi: u32) -> f32 {
    dpi as f32 / 96.0
}
