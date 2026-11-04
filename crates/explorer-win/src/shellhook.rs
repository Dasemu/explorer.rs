// SPDX-License-Identifier: GPL-3.0-or-later
//! Window lifecycle notifications.
//!
//! Two complementary sources feed the taskbar:
//! - `RegisterShellHookWindow`: HSHELL_WINDOWCREATED / DESTROYED /
//!   (RUDE)APPACTIVATED / FLASH / REDRAW, delivered as a registered
//!   "SHELLHOOK" message. Documented but "not intended for general use".
//! - `SetWinEventHook` (out-of-context): title changes, show/hide,
//!   cloak/uncloak, foreground changes and minimise. Catches what the
//!   shell hook misses, notably UWP cloaking and title updates.
//!
//! WinEvent callbacks are free functions without user data, so they post a
//! private message (`WM_APP_WINEVENT`) to the controller window stored in a
//! thread-local; the hook runs on the thread that installed it.

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DeregisterShellHookWindow, GetAncestor, PostMessageW, RegisterShellHookWindow, CHILDID_SELF,
    EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_NAMECHANGE,
    EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, GA_ROOT, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_APP,
};

use crate::ipc::registered_message;

/// Private message: `wParam` = event id, `lParam` = HWND.
pub const WM_APP_WINEVENT: u32 = WM_APP + 0x101;

pub const HSHELL_WINDOWCREATED: usize = 1;
pub const HSHELL_WINDOWDESTROYED: usize = 2;
pub const HSHELL_WINDOWACTIVATED: usize = 4;
pub const HSHELL_REDRAW: usize = 6;
pub const HSHELL_FLASH: usize = 0x8006;
pub const HSHELL_RUDEAPPACTIVATED: usize = 0x8004;
pub const HSHELL_WINDOWREPLACED: usize = 13;

thread_local! {
    static TARGET: Cell<Option<HWND>> = const { Cell::new(None) };
}

/// Registered shell hook + WinEvent hooks. Unregisters on drop.
pub struct ShellHooks {
    hwnd: HWND,
    shell_msg: u32,
    hooks: Vec<HWINEVENTHOOK>,
}

impl ShellHooks {
    pub fn install(target: HWND) -> Self {
        TARGET.with(|t| t.set(Some(target)));
        let shell_msg = registered_message("SHELLHOOK");
        // SAFETY: target is a live top-level window owned by this thread.
        if !unsafe { RegisterShellHookWindow(target) }.as_bool() {
            tracing::warn!("RegisterShellHookWindow failed");
        }
        let ranges = [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
        ];
        let mut hooks = Vec::new();
        for (min, max) in ranges {
            // SAFETY: out-of-context hook with a 'static callback.
            let h = unsafe {
                SetWinEventHook(
                    min,
                    max,
                    None,
                    Some(win_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if h.is_invalid() {
                tracing::warn!(min, max, "SetWinEventHook failed");
            } else {
                hooks.push(h);
            }
        }
        Self {
            hwnd: target,
            shell_msg,
            hooks,
        }
    }

    /// The registered "SHELLHOOK" message id to match in the window proc.
    pub fn shell_message(&self) -> u32 {
        self.shell_msg
    }
}

impl Drop for ShellHooks {
    fn drop(&mut self) {
        // SAFETY: undoing our own registrations.
        unsafe {
            let _ = DeregisterShellHookWindow(self.hwnd);
            for h in self.hooks.drain(..) {
                let _ = UnhookWinEvent(h);
            }
        }
        TARGET.with(|t| t.set(None));
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.is_invalid() || id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 {
        return;
    }
    // Only top-level windows matter. A destroyed window has no ancestor any
    // more, so let DESTROY through unconditionally.
    if event != EVENT_OBJECT_DESTROY {
        // SAFETY: GetAncestor accepts any handle.
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        if root != hwnd {
            return;
        }
    }
    if let Some(target) = TARGET.with(|t| t.get()) {
        // SAFETY: posting to our own window.
        let _ = unsafe {
            PostMessageW(
                Some(target),
                WM_APP_WINEVENT,
                WPARAM(event as usize),
                LPARAM(hwnd.0 as isize),
            )
        };
    }
}

/// Event ids re-exported for matching in the controller.
pub mod events {
    pub use windows::Win32::UI::WindowsAndMessaging::{
        EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_NAMECHANGE,
        EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
        EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
    };
}
