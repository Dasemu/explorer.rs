// SPDX-License-Identifier: GPL-3.0-or-later
//! Notification-area host: a window of class `Shell_TrayWnd` that receives
//! `Shell_NotifyIconW` traffic (`WM_COPYDATA`, `dwData == 1`).
//!
//! Protocol parsing and the icon model are pure and live in
//! `explorer_core::tray`. No code from other shells was copied; behaviour
//! follows public documentation and observation.
//!
//! **Dev mode** (explorer.exe running): applications locate the tray with
//! `FindWindow("Shell_TrayWnd")`, which returns the first match in Z order.
//! We keep our (invisible, topmost) window in front of explorer's so we see
//! the traffic, and forward *every* message to explorer's real tray window
//! so explorer keeps working exactly as before. If our window disappears,
//! applications simply talk to explorer again.
//!
//! **Shell mode**: we are the only tray. `SHAppBarMessage` traffic
//! (`dwData == 0`) is answered with 0 for now (TODO: AppBar server).
//!
//! Unverified on Windows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use explorer_core::image::SharedImage;
use explorer_core::ipc::RunMode;
use explorer_core::tray::{
    self, click_messages, IconKey, TrayButton, TrayModel, COPYDATA_NOTIFYICON, NIM_ADD,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowExW, FindWindowW, PostMessageW, SendMessageTimeoutW,
    SendNotifyMessageW, SetWindowPos, HICON, HWND_BROADCAST, HWND_TOPMOST, SMTO_ABORTIFHUNG,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WM_APP, WM_CLOSE, WM_COMMAND, WM_COPYDATA, WM_USER,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::ipc::registered_message;
use crate::util::{u64_to_hwnd, WideString};
use crate::window::{self, CreateParams, Handler};

/// Window class applications look for.
pub const TRAY_WINDOW_CLASS: &str = "Shell_TrayWnd";

/// Posted to the notify window when the visible tray state changed.
pub const WM_APP_TRAY_CHANGED: u32 = WM_APP + 0x110;

/// Shared tray state, owned by the taskbar UI thread.
#[derive(Default)]
pub struct TrayState {
    pub model: TrayModel,
    pub images: HashMap<IconKey, SharedImage>,
}

pub type SharedTray = Rc<RefCell<TrayState>>;

/// Icon size used for conversion (covers up to 200 % scaling of 16 px).
const ICON_PX: u32 = 32;

struct TrayWindow {
    state: SharedTray,
    notify: HWND,
    forward: bool,
    explorer_tray: Option<HWND>,
    taskbar_created: u32,
}

fn tray_windows() -> Vec<HWND> {
    let class = WideString::new(TRAY_WINDOW_CLASS);
    let mut out = Vec::new();
    let mut prev: Option<HWND> = None;
    // SAFETY: FindWindowExW with valid class string; bounded loop.
    unsafe {
        for _ in 0..16 {
            match FindWindowExW(None, prev, class.pcwstr(), None) {
                Ok(h) => {
                    out.push(h);
                    prev = Some(h);
                }
                Err(_) => break,
            }
        }
    }
    out
}

impl TrayWindow {
    fn find_explorer_tray(&mut self, me: HWND) {
        self.explorer_tray = tray_windows().into_iter().find(|&h| h != me);
    }

    fn forward(&mut self, me: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if !self.forward {
            return None;
        }
        if self
            .explorer_tray
            .is_none_or(|h| !crate::winlist::exists(h))
        {
            self.find_explorer_tray(me);
        }
        let target = self.explorer_tray?;
        let mut result = 0usize;
        // SAFETY: cross-process send; WM_COPYDATA and other system messages
        // are marshalled by the OS. Bounded wait.
        let ok = unsafe {
            SendMessageTimeoutW(
                target,
                msg,
                wparam,
                lparam,
                SMTO_ABORTIFHUNG,
                1000,
                Some(&mut result),
            )
        };
        (ok.0 != 0).then_some(LRESULT(result as isize))
    }

    fn on_notify_icon(&mut self, payload: &[u8]) -> Option<bool> {
        let cmd = match tray::parse(payload) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(?e, len = payload.len(), "unparseable tray payload");
                return None;
            }
        };
        tracing::trace!(message = cmd.message, hwnd = cmd.data.hwnd, uid = cmd.data.uid, tip = %cmd.data.tip, "tray");
        let mut st = self.state.borrow_mut();
        let applied = st.model.apply(&cmd);
        let key = cmd.data.key();
        if applied.icon_changed {
            if let Some(icon) = st.model.get(key) {
                // Handles are 32-bit, sign-extended on 64-bit Windows.
                let h = HICON(icon.hicon as i32 as isize as *mut core::ffi::c_void);
                match crate::icons::from_hicon(h, ICON_PX) {
                    Some(img) => {
                        st.images.insert(key, img);
                    }
                    None => {
                        st.images.remove(&key);
                    }
                }
            }
        }
        if cmd.message == tray::NIM_DELETE && applied.ok {
            st.images.remove(&key);
        }
        if applied.changed || applied.icon_changed {
            // SAFETY: posting to our own taskbar controller.
            let _ = unsafe {
                PostMessageW(Some(self.notify), WM_APP_TRAY_CHANGED, WPARAM(0), LPARAM(0))
            };
        }
        Some(applied.ok)
    }
}

impl Handler for TrayWindow {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if msg == WM_CLOSE {
            // Never let another program close the tray host (it would lose
            // every icon); the taskbar owns its lifetime.
            return Some(LRESULT(0));
        }
        if msg == WM_COPYDATA {
            // SAFETY: for WM_COPYDATA, lParam points to a COPYDATASTRUCT
            // valid for the duration of the message.
            let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
            let ours = if cds.dwData == COPYDATA_NOTIFYICON && !cds.lpData.is_null() {
                // SAFETY: lpData/cbData describe a buffer owned by the
                // system for this message.
                let payload = unsafe {
                    std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize)
                };
                let message = payload
                    .get(4..8)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                self.on_notify_icon(payload).map(|ok| (ok, message))
            } else {
                None
            };
            let forwarded = self.forward(hwnd, msg, wparam, lparam);
            return Some(match (ours, forwarded) {
                // Our model decides NIM_ADD so re-adds after TaskbarCreated
                // (which explorer rejects as duplicates) still succeed.
                (Some((ok, Some(NIM_ADD))), _) => LRESULT(ok as isize),
                (_, Some(r)) => r,
                (Some((ok, _)), None) => LRESULT(ok as isize),
                (None, None) => LRESULT(0),
            });
        }
        // Dev mode: pass through messages other programs send to the tray
        // (WM_COMMAND shortcuts, WM_USER-range and registered messages).
        let external = msg == WM_COMMAND
            || (WM_USER..0x8000).contains(&msg)
            || (msg >= 0xC000 && msg != self.taskbar_created);
        if external {
            if let Some(r) = self.forward(hwnd, msg, wparam, lparam) {
                return Some(r);
            }
        }
        None
    }
}

/// Handle to the tray window.
pub struct TrayHost {
    hwnd: HWND,
    pub state: SharedTray,
    intercepting: bool,
}

impl TrayHost {
    /// Create the tray window. In dev mode only if `intercept` is set.
    pub fn start(mode: RunMode, intercept: bool, notify: HWND) -> Option<Self> {
        if mode == RunMode::Dev && !intercept {
            tracing::info!("tray: dev-mode interception disabled in config");
            return None;
        }
        window::register_class(TRAY_WINDOW_CLASS, None);
        let state: SharedTray = Rc::new(RefCell::new(TrayState::default()));
        let explorer_tray = tray_windows().into_iter().next();
        let forward = mode == RunMode::Dev;
        if forward && explorer_tray.is_none() {
            tracing::warn!("tray: dev mode but explorer's Shell_TrayWnd not found");
        }
        let hwnd = window::create(
            CreateParams {
                class: TRAY_WINDOW_CLASS,
                title: "",
                style: WS_POPUP,
                ex_style: WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                rect: RECT::default(),
                parent: None,
            },
            Box::new(TrayWindow {
                state: state.clone(),
                notify,
                forward,
                explorer_tray,
                taskbar_created: registered_message("TaskbarCreated"),
            }),
        )
        .map_err(|e| tracing::error!(error = %e, "tray: cannot create Shell_TrayWnd"))
        .ok()?;
        let host = Self {
            hwnd,
            state,
            intercepting: forward,
        };
        host.ensure_front();
        // Ask running applications to (re-)register their icons with us.
        // SAFETY: broadcast without pointers.
        let _ = unsafe {
            PostMessageW(
                Some(HWND_BROADCAST),
                registered_message("TaskbarCreated"),
                WPARAM(0),
                LPARAM(0),
            )
        };
        tracing::info!(mode = mode.as_str(), forward, "tray host started");
        Some(host)
    }

    /// Dev mode: keep our window first in Z order so `FindWindow` returns
    /// it. Call periodically (explorer re-asserts its own Z order).
    pub fn ensure_front(&self) {
        let class = WideString::new(TRAY_WINDOW_CLASS);
        // SAFETY: plain FFI calls on our own window.
        unsafe {
            let first = FindWindowW(class.pcwstr(), None).ok();
            if first != Some(self.hwnd) {
                if self.intercepting {
                    tracing::debug!("tray: re-raising Shell_TrayWnd above explorer's");
                }
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// Remove icons whose owner window died. Returns true if changed.
    pub fn prune(&self) -> bool {
        let mut st = self.state.borrow_mut();
        let removed = st
            .model
            .retain_alive(|h| crate::winlist::exists(u64_to_hwnd(h as i32 as isize as u64)));
        if removed {
            let alive: Vec<IconKey> = st.model.icons().iter().map(|i| i.key).collect();
            st.images.retain(|k, _| alive.contains(k));
        }
        removed
    }

    /// Forward a click on icon `key` to its application. `x`, `y` are screen
    /// coordinates.
    pub fn click(&self, key: IconKey, button: TrayButton, x: i32, y: i32) {
        let Some(icon) = self.state.borrow().model.get(key).cloned() else {
            return;
        };
        let target = u64_to_hwnd(icon.hwnd as i32 as isize as u64);
        let pid = crate::winlist::process_id(target);
        // SAFETY: plain FFI calls; messages carry no pointers.
        unsafe {
            if pid != 0 {
                // The app usually calls SetForegroundWindow to show its menu.
                let _ = AllowSetForegroundWindow(pid);
            }
            for (w, l) in click_messages(&icon, button, x as i16, y as i16) {
                let _ = SendNotifyMessageW(target, icon.callback_message, WPARAM(w), LPARAM(l));
            }
        }
    }
}
