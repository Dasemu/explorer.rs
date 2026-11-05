// SPDX-License-Identifier: GPL-3.0-or-later
//! A single tracking tooltip (common-controls `tooltips_class32`) that we
//! position manually; used for icon-only taskbar buttons and tray icons.

use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, TOOLTIPS_CLASSW, TTF_ABSOLUTE,
    TTF_TRACK, TTM_ADDTOOLW, TTM_SETMAXTIPWIDTH, TTM_TRACKACTIVATE, TTM_TRACKPOSITION,
    TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SendMessageW, HMENU, WINDOW_STYLE, WS_EX_TOPMOST, WS_POPUP,
};

use crate::util::wide;

pub struct Tooltip {
    hwnd: HWND,
    owner: HWND,
    text: Vec<u16>,
    visible: bool,
}

impl Tooltip {
    pub fn new(owner: HWND) -> Option<Self> {
        // The tooltip window class is registered by comctl32 only after
        // InitCommonControlsEx.
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_WIN95_CLASSES,
        };
        // SAFETY: valid struct; idempotent.
        let _ = unsafe { InitCommonControlsEx(&icc) };
        // SAFETY: standard common-control creation.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST,
                TOOLTIPS_CLASSW,
                w!(""),
                WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                0,
                0,
                0,
                0,
                Some(owner),
                None::<HMENU>,
                Some(crate::window::module_instance()),
                None,
            )
        }
        .ok()?;
        let mut t = Self {
            hwnd,
            owner,
            text: wide(""),
            visible: false,
        };
        let mut ti = t.info();
        // SAFETY: ti valid for the call.
        unsafe {
            SendMessageW(
                hwnd,
                TTM_ADDTOOLW,
                None,
                Some(LPARAM(&mut ti as *mut _ as isize)),
            );
            SendMessageW(hwnd, TTM_SETMAXTIPWIDTH, None, Some(LPARAM(480)));
        }
        Some(t)
    }

    fn info(&mut self) -> TTTOOLINFOW {
        TTTOOLINFOW {
            // V2 size (without lpReserved) works with comctl32 v5 and v6;
            // the binaries ship a comctl32 v6 manifest; V2 size works with both.
            cbSize: (std::mem::size_of::<TTTOOLINFOW>() - std::mem::size_of::<usize>()) as u32,
            uFlags: TTF_TRACK | TTF_ABSOLUTE,
            hwnd: self.owner,
            uId: 1,
            rect: RECT::default(),
            lpszText: PWSTR(self.text.as_mut_ptr()),
            ..Default::default()
        }
    }

    /// Show `text` with its top-left corner near screen point (x, y).
    pub fn show(&mut self, text: &str, x: i32, y: i32) {
        let new = wide(text);
        let mut ti_changed = false;
        if new != self.text {
            self.text = new;
            ti_changed = true;
        }
        let mut ti = self.info();
        // SAFETY: ti valid for the calls.
        unsafe {
            if ti_changed {
                SendMessageW(
                    self.hwnd,
                    TTM_UPDATETIPTEXTW,
                    None,
                    Some(LPARAM(&mut ti as *mut _ as isize)),
                );
            }
            let pos = ((y as u16 as u32) << 16) | (x as u16 as u32);
            SendMessageW(
                self.hwnd,
                TTM_TRACKPOSITION,
                None,
                Some(LPARAM(pos as isize)),
            );
            if !self.visible {
                SendMessageW(
                    self.hwnd,
                    TTM_TRACKACTIVATE,
                    Some(WPARAM(1)),
                    Some(LPARAM(&mut ti as *mut _ as isize)),
                );
                self.visible = true;
            }
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            let mut ti = self.info();
            // SAFETY: ti valid for the call.
            unsafe {
                SendMessageW(
                    self.hwnd,
                    TTM_TRACKACTIVATE,
                    Some(WPARAM(0)),
                    Some(LPARAM(&mut ti as *mut _ as isize)),
                )
            };
            self.visible = false;
        }
    }
}

impl Drop for Tooltip {
    fn drop(&mut self) {
        // SAFETY: our own window.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}
