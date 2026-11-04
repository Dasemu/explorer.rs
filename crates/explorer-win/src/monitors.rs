// SPDX-License-Identifier: GPL-3.0-or-later
//! Monitor enumeration.

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

use crate::util::from_wide;

#[derive(Debug, Clone)]
pub struct Monitor {
    pub handle: HMONITOR,
    pub device: String,
    pub rect: RECT,
    pub work: RECT,
    pub primary: bool,
    pub dpi: u32,
}

impl Monitor {
    pub fn describe(&self) -> String {
        let r = self.rect;
        let w = self.work;
        format!(
            "{} primary={} rect=({},{})-({},{}) work=({},{})-({},{}) dpi={} scale={:.0}%",
            self.device,
            self.primary,
            r.left,
            r.top,
            r.right,
            r.bottom,
            w.left,
            w.top,
            w.right,
            w.bottom,
            self.dpi,
            self.dpi as f32 / 96.0 * 100.0
        )
    }
}

unsafe extern "system" fn enum_proc(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the &mut Vec passed by `all()` for the duration of
    // the EnumDisplayMonitors call.
    let out = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
    if let Some(m) = query(hmon) {
        out.push(m);
    }
    BOOL(1)
}

/// Information about one monitor.
pub fn query(hmon: HMONITOR) -> Option<Monitor> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: info is a properly sized MONITORINFOEXW.
    let ok = unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _ as *mut _) };
    if !ok.as_bool() {
        return None;
    }
    let (mut dx, mut dy) = (96u32, 96u32);
    // SAFETY: out-pointers are valid locals.
    let _ = unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
    Some(Monitor {
        handle: hmon,
        device: from_wide(&info.szDevice),
        rect: info.monitorInfo.rcMonitor,
        work: info.monitorInfo.rcWork,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        dpi: dx,
    })
}

/// All monitors, primary first.
pub fn all() -> Vec<Monitor> {
    let mut out: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only dereferences `out` during this call.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_proc),
            LPARAM(&mut out as *mut Vec<Monitor> as isize),
        )
    };
    out.sort_by_key(|m| !m.primary);
    out
}
