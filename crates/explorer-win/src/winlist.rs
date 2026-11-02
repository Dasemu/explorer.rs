// SPDX-License-Identifier: GPL-3.0-or-later
//! Top-level window enumeration and snapshots for the taskbar filter.

use explorer_core::window_filter::{WindowInfo, UWP_CORE_CLASS, UWP_FRAME_CLASS};
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GetClassNameW, GetWindow, GetWindowLongW, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, GWL_EXSTYLE, GWL_STYLE, GW_OWNER,
};

use crate::util::{hwnd_to_u64, WideString};

unsafe extern "system" fn enum_proc(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the &mut Vec passed by `top_level_windows`.
    let out = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
    out.push(hwnd);
    BOOL(1)
}

/// All top-level windows in Z order.
pub fn top_level_windows() -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
    // SAFETY: the callback only touches `out` during the call.
    let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(&mut out as *mut Vec<HWND> as isize)) };
    out
}

pub fn exists(hwnd: HWND) -> bool {
    // SAFETY: IsWindow accepts any value.
    unsafe { IsWindow(Some(hwnd)) }.as_bool()
}

pub fn is_minimized(hwnd: HWND) -> bool {
    // SAFETY: IsIconic accepts any value.
    unsafe { IsIconic(hwnd) }.as_bool()
}

pub fn title(hwnd: HWND) -> String {
    // SAFETY: buffer sized from GetWindowTextLengthW (+1 for NUL).
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: valid buffer.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn cloaked(hwnd: HWND) -> u32 {
    let mut v: u32 = 0;
    // SAFETY: out-pointer to a u32, size given.
    let _ = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut v as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
    };
    v
}

pub fn process_id(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: valid out-pointer.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// Full image path of a process, if accessible.
pub fn process_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    // SAFETY: handle closed below; buffer and length are consistent.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

fn has_uwp_core_child(hwnd: HWND) -> bool {
    let class = WideString::new(UWP_CORE_CLASS);
    // SAFETY: valid class string kept alive for the call.
    unsafe { FindWindowExW(Some(hwnd), None, class.pcwstr(), None) }.is_ok()
}

/// Collect the filter-relevant properties of `hwnd`.
pub fn snapshot(hwnd: HWND) -> WindowInfo {
    let class_name = class_name(hwnd);
    // SAFETY: GetWindowLongW/GetWindow accept any handle.
    let (style, ex_style, has_owner) = unsafe {
        (
            GetWindowLongW(hwnd, GWL_STYLE) as u32,
            GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
            GetWindow(hwnd, GW_OWNER).is_ok(),
        )
    };
    let uwp_has_core_child = (class_name == UWP_FRAME_CLASS).then(|| has_uwp_core_child(hwnd));
    WindowInfo {
        hwnd: hwnd_to_u64(hwnd),
        title: title(hwnd),
        class_name,
        style,
        ex_style,
        has_owner,
        cloaked: cloaked(hwnd),
        process_name: process_path(process_id(hwnd)).map(|p| file_name(&p)),
        uwp_has_core_child,
    }
}

/// Snapshots of every top-level window.
pub fn snapshot_all() -> Vec<WindowInfo> {
    top_level_windows().into_iter().map(snapshot).collect()
}

/// Human-readable dump for `--dump-windows`.
pub fn dump(rules: &explorer_core::window_filter::FilterRules) -> String {
    let mut s = String::new();
    for w in snapshot_all() {
        let reason = rules.classify(&w);
        s.push_str(&format!(
            "{:#010x} {:<6} {:<16?} class={:?} title={:?} style={:#010x} ex={:#010x} owner={} cloaked={} proc={:?} uwp_core={:?}\n",
            w.hwnd,
            if reason.is_shown() { "SHOWN" } else { "hidden" },
            reason,
            w.class_name,
            w.title,
            w.style,
            w.ex_style,
            w.has_owner,
            w.cloaked,
            w.process_name.as_deref().unwrap_or("?"),
            w.uwp_has_core_child,
        ));
    }
    s
}
