// SPDX-License-Identifier: GPL-3.0-or-later
//! Screen-space reservation for the taskbar.
//!
//! Two strategies, chosen by run mode:
//! - **Dev mode** (explorer.exe running): the documented AppBar protocol,
//!   `SHAppBarMessage`. Explorer arbitrates between AppBars and adjusts the
//!   work area, so maximised windows do not overlap us.
//! - **Shell mode** (no explorer.exe): `SHAppBarMessage` talks to
//!   explorer's `Shell_TrayWnd` via `WM_COPYDATA`; with no explorer there is
//!   nobody to answer. We therefore set each monitor's work area directly
//!   with `SystemParametersInfoW(SPI_SETWORKAREA)` and restore it on exit.
//!   Other applications' AppBars are not arbitrated in this mode (TODO,
//!   would require implementing the AppBar server side; tray feature).

use explorer_core::config::Edge;
use explorer_core::ipc::RunMode;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABE_TOP, ABM_ACTIVATE, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE,
    ABM_SETPOS, ABM_WINDOWPOSCHANGED, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPIF_SENDCHANGE, SPI_SETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

pub struct AppBar {
    hwnd: HWND,
    mode: RunMode,
    registered: bool,
    /// Shell mode: monitor rect whose work area we changed.
    work_area_monitor: Option<RECT>,
}

/// Notification codes delivered in `wParam` of the callback message.
pub const ABN_STATECHANGE: usize = 0;
pub const ABN_POSCHANGED: usize = 1;
pub const ABN_FULLSCREENAPP: usize = 2;

fn data(hwnd: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        ..Default::default()
    }
}

/// Rectangle of a bar of `thickness` px along `edge` of `monitor`.
pub fn bar_rect(monitor: RECT, edge: Edge, thickness: i32) -> RECT {
    match edge {
        Edge::Bottom => RECT {
            top: monitor.bottom - thickness,
            ..monitor
        },
        Edge::Top => RECT {
            bottom: monitor.top + thickness,
            ..monitor
        },
    }
}

impl AppBar {
    pub fn new(hwnd: HWND, callback_msg: u32, mode: RunMode) -> Self {
        let mut registered = false;
        if mode == RunMode::Dev {
            let mut d = data(hwnd);
            d.uCallbackMessage = callback_msg;
            // SAFETY: d is a valid APPBARDATA.
            registered = unsafe { SHAppBarMessage(ABM_NEW, &mut d) } != 0;
            if !registered {
                tracing::warn!("ABM_NEW failed: no AppBar manager (explorer.exe not running?)");
            }
        }
        Self {
            hwnd,
            mode,
            registered,
            work_area_monitor: None,
        }
    }

    /// Reserve space and return the rectangle the window must occupy.
    pub fn reserve(&mut self, monitor: RECT, edge: Edge, thickness: i32) -> RECT {
        if self.registered {
            let mut d = data(self.hwnd);
            d.uEdge = match edge {
                Edge::Bottom => ABE_BOTTOM,
                Edge::Top => ABE_TOP,
            };
            d.rc = bar_rect(monitor, edge, thickness);
            // SAFETY: d is valid for both calls. QUERYPOS lets the shell
            // shrink our proposal around other AppBars; we then re-apply
            // our thickness from the adjusted edge.
            unsafe {
                SHAppBarMessage(ABM_QUERYPOS, &mut d);
                match edge {
                    Edge::Bottom => d.rc.top = d.rc.bottom - thickness,
                    Edge::Top => d.rc.bottom = d.rc.top + thickness,
                }
                SHAppBarMessage(ABM_SETPOS, &mut d);
            }
            return d.rc;
        }
        let rect = bar_rect(monitor, edge, thickness);
        if self.mode == RunMode::Shell {
            let mut work = monitor;
            match edge {
                Edge::Bottom => work.bottom = rect.top,
                Edge::Top => work.top = rect.bottom,
            }
            set_work_area(&work);
            self.work_area_monitor = Some(monitor);
        }
        rect
    }

    /// Must be forwarded from WM_ACTIVATE.
    pub fn on_activate(&self) {
        if self.registered {
            let mut d = data(self.hwnd);
            // SAFETY: valid APPBARDATA.
            unsafe { SHAppBarMessage(ABM_ACTIVATE, &mut d) };
        }
    }

    /// Must be forwarded from WM_WINDOWPOSCHANGED.
    pub fn on_window_pos_changed(&self) {
        if self.registered {
            let mut d = data(self.hwnd);
            // SAFETY: valid APPBARDATA.
            unsafe { SHAppBarMessage(ABM_WINDOWPOSCHANGED, &mut d) };
        }
    }

    /// Release the reserved space. Called from WM_DESTROY.
    pub fn remove(&mut self) {
        if self.registered {
            let mut d = data(self.hwnd);
            // SAFETY: valid APPBARDATA.
            unsafe { SHAppBarMessage(ABM_REMOVE, &mut d) };
            self.registered = false;
        }
        if let Some(m) = self.work_area_monitor.take() {
            set_work_area(&m);
        }
    }
}

impl Drop for AppBar {
    fn drop(&mut self) {
        self.remove();
    }
}

fn set_work_area(r: &RECT) {
    // SAFETY: r points to a valid RECT for the duration of the call.
    let res = unsafe {
        SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(r as *const RECT as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(SPIF_SENDCHANGE.0),
        )
    };
    if let Err(e) = res {
        tracing::warn!(error = %e, "SPI_SETWORKAREA failed");
    }
}
