// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal window-class/window-procedure plumbing.
//!
//! A window owns a boxed [`Handler`] stored in `GWLP_USERDATA`. The handler
//! lives in a `RefCell`; if a message re-enters the window procedure while
//! the handler is already borrowed (e.g. `SetWindowPos` sending
//! `WM_WINDOWPOSCHANGED` synchronously), the nested message falls back to
//! `DefWindowProcW` instead of creating an aliasing `&mut`.

use std::cell::RefCell;

use anyhow::{Context, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW, LoadCursorW,
    PostQuitMessage, RegisterClassExW, SetWindowLongPtrW, TranslateMessage, CREATESTRUCTW,
    CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HMENU, IDC_ARROW, MSG, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_NCCREATE, WM_NCDESTROY, WNDCLASSEXW,
};

use crate::util::WideString;

/// Per-window message handler.
pub trait Handler {
    /// Return `Some(result)` if handled, `None` to fall through to
    /// `DefWindowProcW`.
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT>;
}

type Slot = RefCell<Box<dyn Handler>>;

pub fn module_instance() -> HINSTANCE {
    // SAFETY: NULL module name returns the exe's own handle.
    unsafe { GetModuleHandleW(None) }
        .map(|h| HINSTANCE(h.0))
        .unwrap_or_default()
}

/// Register a window class using our shared window procedure. Registering
/// the same class twice is harmless (the second call fails and is ignored).
pub fn register_class(name: &str, background: Option<HBRUSH>) {
    let class = WideString::new(name);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        lpfnWndProc: Some(wnd_proc),
        hInstance: module_instance(),
        // SAFETY: loading a stock cursor.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        hbrBackground: background.unwrap_or_default(),
        lpszClassName: class.pcwstr(),
        ..Default::default()
    };
    // SAFETY: wc is fully initialised; the class name is copied by the OS.
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        tracing::debug!(
            class = name,
            "RegisterClassExW returned 0 (already registered?)"
        );
    }
}

pub struct CreateParams<'a> {
    pub class: &'a str,
    pub title: &'a str,
    pub style: WINDOW_STYLE,
    pub ex_style: WINDOW_EX_STYLE,
    pub rect: RECT,
    pub parent: Option<HWND>,
}

/// Create a window driven by `handler`.
pub fn create(p: CreateParams<'_>, handler: Box<dyn Handler>) -> Result<HWND> {
    let class = WideString::new(p.class);
    let title = WideString::new(p.title);
    let slot: Box<Slot> = Box::new(RefCell::new(handler));
    let raw = Box::into_raw(slot);
    // SAFETY: `raw` is handed to WM_NCCREATE which stores it; ownership is
    // reclaimed in WM_NCDESTROY, or below if creation fails.
    let hwnd = unsafe {
        CreateWindowExW(
            p.ex_style,
            class.pcwstr(),
            title.pcwstr(),
            p.style,
            p.rect.left,
            p.rect.top,
            p.rect.right - p.rect.left,
            p.rect.bottom - p.rect.top,
            p.parent,
            None::<HMENU>,
            Some(module_instance()),
            Some(raw as *const core::ffi::c_void),
        )
    };
    match hwnd {
        Ok(h) => Ok(h),
        Err(e) => {
            // If WM_NCCREATE never ran, nobody took ownership. If it ran and
            // creation failed later, WM_NCDESTROY already freed it and reset
            // the pointer; we cannot distinguish, so leak rather than risk a
            // double free. Window creation failure is fatal for the caller.
            Err(e).with_context(|| format!("CreateWindowExW({})", p.class))
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: all pointer handling below follows the ownership protocol
    // described in `create`.
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Slot;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        if msg == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let slot = Box::from_raw(ptr);
            let r = match slot.try_borrow_mut() {
                Ok(mut h) => h.handle(hwnd, msg, wparam, lparam),
                Err(_) => None,
            };
            drop(slot);
            return r.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam));
        }
        let slot = &*ptr;
        let r = match slot.try_borrow_mut() {
            Ok(mut h) => h.handle(hwnd, msg, wparam, lparam),
            Err(_) => None,
        };
        r.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam))
    }
}

/// Standard message loop. `pre_dispatch` may consume a message (return
/// true) before translation, e.g. to steal navigation keys from an EDIT
/// control. Returns the `WM_QUIT` exit code.
pub fn run_message_loop(mut pre_dispatch: impl FnMut(&MSG) -> bool) -> i32 {
    let mut msg = MSG::default();
    loop {
        // SAFETY: msg is a valid out-pointer.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match r.0 {
            0 => return msg.wParam.0 as i32,
            -1 => {
                tracing::error!("GetMessageW failed");
                return 1;
            }
            _ => {}
        }
        if pre_dispatch(&msg) {
            continue;
        }
        // SAFETY: msg came from GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

pub fn quit(code: i32) {
    // SAFETY: plain FFI call.
    unsafe { PostQuitMessage(code) };
}

/// `PCWSTR` null helper.
pub fn null_pcwstr() -> PCWSTR {
    PCWSTR::null()
}
