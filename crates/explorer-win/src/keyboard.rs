// SPDX-License-Identifier: GPL-3.0-or-later
//! Low-level keyboard hook for "Win key alone opens the launcher".
//!
//! The hook runs on the installing thread's message loop and must return
//! quickly (Windows silently removes slow LL hooks after
//! `LowLevelHooksTimeout`). All it does is feed the pure
//! [`WinKeyDetector`] and, on a lone Win tap, inject the mask key and post
//! `WM_APP_WINKEY` to the target window. Keys are never swallowed.

use std::cell::RefCell;

use explorer_core::hotkey::{KeyAction, WinKeyDetector, VK_MASK};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
    LLKHF_INJECTED, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_MBUTTONDOWN, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_XBUTTONDOWN,
};

/// Posted to the target window when the Win key was tapped alone.
pub const WM_APP_WINKEY: u32 = WM_APP + 0x102;

struct State {
    detector: WinKeyDetector,
    target: HWND,
    mask: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub struct WinKeyHook {
    kb: HHOOK,
    mouse: Option<HHOOK>,
}

impl WinKeyHook {
    /// Install on the current thread. `mask_start_menu`: inject the mask key
    /// so explorer's Start menu does not open as well (dev mode).
    pub fn install(target: HWND, mask_start_menu: bool) -> anyhow::Result<Self> {
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                detector: WinKeyDetector::new(),
                target,
                mask: mask_start_menu,
            })
        });
        let hinst = crate::window::module_instance();
        // SAFETY: 'static callbacks; hooks removed in Drop.
        let kb = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), Some(hinst), 0) }?;
        let mouse =
            unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hinst), 0) }.ok();
        Ok(Self { kb, mouse })
    }
}

impl Drop for WinKeyHook {
    fn drop(&mut self) {
        // SAFETY: our own hooks.
        unsafe {
            let _ = UnhookWindowsHookEx(self.kb);
            if let Some(m) = self.mouse {
                let _ = UnhookWindowsHookEx(m);
            }
        }
        STATE.with(|s| *s.borrow_mut() = None);
    }
}

fn send_mask_key() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(VK_MASK as u16),
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
    // SAFETY: valid INPUT array.
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

unsafe extern "system" fn kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_KEYBOARD_LL, lParam points to a KBDLLHOOKSTRUCT.
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let injected = k.flags.0 & LLKHF_INJECTED.0 != 0;
        STATE.with(|s| {
            if let Ok(mut guard) = s.try_borrow_mut() {
                if let Some(st) = guard.as_mut() {
                    if st.detector.on_key(k.vkCode, down, injected) == KeyAction::MaskAndTrigger {
                        if st.mask {
                            send_mask_key();
                        }
                        // SAFETY: posting to our own window.
                        let _ = unsafe {
                            PostMessageW(Some(st.target), WM_APP_WINKEY, WPARAM(0), LPARAM(0))
                        };
                    }
                }
            }
        });
    }
    // SAFETY: chaining as required by the hook contract.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(
            wparam.0 as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        STATE.with(|s| {
            if let Ok(mut guard) = s.try_borrow_mut() {
                if let Some(st) = guard.as_mut() {
                    st.detector.on_mouse_button();
                }
            }
        });
    }
    // SAFETY: chaining as required by the hook contract.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
