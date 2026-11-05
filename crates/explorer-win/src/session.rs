// SPDX-License-Identifier: GPL-3.0-or-later
//! Session plumbing for the guardian and supervisor:
//! - a listener thread with a hidden top-level window (sign-out/shutdown:
//!   `WM_QUERYENDSESSION` / `WM_ENDSESSION` only reach top-level windows)
//!   and, optionally, a message-only window owning the emergency hotkeys;
//! - session-scoped named events ("run once per sign-in" guards);
//! - the undocumented shell-ready events Winlogon waits for (ADR 0007).
//!
//! No COM is used here, so the guardian can use it.

use std::sync::Arc;

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, LPARAM, LRESULT, RECT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::System::Threading::{
    CreateEventW, OpenEventW, OpenMutexW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
    SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_MESSAGE, WM_DESTROY, WM_ENDSESSION, WM_HOTKEY, WM_QUERYENDSESSION, WS_EX_TOOLWINDOW,
    WS_POPUP,
};

use crate::util::WideString;
use crate::window::{self, CreateParams, Handler};

/// Something the listener thread observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysEvent {
    /// Ctrl+Alt+Shift+E: start explorer.exe now.
    EmergencyExplorer,
    /// Ctrl+Alt+Shift+R: restart all explorer.rs components.
    RestartAll,
    /// Windows asks whether the session may end (may still be cancelled).
    QueryEndSession,
    /// The session is ending (sign-out, restart, shutdown). The callback
    /// runs synchronously; after it returns the process may be killed.
    SessionEnding,
    /// `ExplorerRs.Shutdown` received: stop gracefully (setup,
    /// `--restore-explorer`, or the guardian stopping its supervisor).
    Shutdown,
    /// `ExplorerRs.Restored` received (guardian only): `--restore-explorer`
    /// restored Windows Explorer as the shell; record it in the crash log.
    Restored,
}

/// The registered `ExplorerRs.Restored` message.
pub fn restored_message() -> u32 {
    crate::ipc::registered_message(explorer_core::ipc::MSG_RESTORED)
}

/// Map our registered messages to events (shared by both windows).
fn registered_event(msg: u32) -> Option<SysEvent> {
    if msg < explorer_core::ipc::REGISTERED_FIRST {
        return None;
    }
    if msg == crate::window::shutdown_message() {
        Some(SysEvent::Shutdown)
    } else if msg == restored_message() {
        Some(SysEvent::Restored)
    } else {
        None
    }
}

pub type Sink = Arc<dyn Fn(SysEvent) + Send + Sync>;

const HOTKEY_EXPLORER: i32 = 0xE001;
const HOTKEY_RESTART: i32 = 0xE002;
const VK_E: u32 = 0x45;
const VK_R: u32 = 0x52;

struct SessionWatch {
    sink: Sink,
}

impl Handler for SessionWatch {
    fn handle(&mut self, _: HWND, msg: u32, wparam: WPARAM, _: LPARAM) -> Option<LRESULT> {
        if let Some(ev) = registered_event(msg) {
            (self.sink)(ev);
            return Some(LRESULT(0));
        }
        match msg {
            WM_QUERYENDSESSION => {
                (self.sink)(SysEvent::QueryEndSession);
                Some(LRESULT(1))
            }
            WM_ENDSESSION => {
                if wparam.0 != 0 {
                    (self.sink)(SysEvent::SessionEnding);
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

struct Hotkeys {
    sink: Sink,
}

impl Handler for Hotkeys {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, _: LPARAM) -> Option<LRESULT> {
        if let Some(ev) = registered_event(msg) {
            (self.sink)(ev);
            return Some(LRESULT(0));
        }
        match msg {
            WM_HOTKEY => {
                match wparam.0 as i32 {
                    HOTKEY_EXPLORER => (self.sink)(SysEvent::EmergencyExplorer),
                    HOTKEY_RESTART => (self.sink)(SysEvent::RestartAll),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                // SAFETY: ids registered on this window.
                unsafe {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_EXPLORER);
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_RESTART);
                }
                None
            }
            _ => None,
        }
    }
}

fn register_hotkeys(hwnd: HWND) {
    let mods = MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT;
    for (id, vk, name) in [
        (HOTKEY_EXPLORER, VK_E, "Ctrl+Alt+Shift+E"),
        (HOTKEY_RESTART, VK_R, "Ctrl+Alt+Shift+R"),
    ] {
        // SAFETY: valid window owned by this thread.
        match unsafe { RegisterHotKey(Some(hwnd), id, mods, vk) } {
            Ok(()) => tracing::info!(hotkey = name, "emergency hotkey registered"),
            Err(e) => tracing::error!(
                hotkey = name,
                error = %e,
                "could not register emergency hotkey (taken by another program?)"
            ),
        }
    }
}

/// Start the listener thread. `hotkeys`: also register the emergency
/// hotkeys (guardian only). Returns false if the thread could not start.
pub fn spawn_listener(hotkeys: bool, sink: Sink) -> bool {
    std::thread::Builder::new()
        .name("session-listener".into())
        .spawn(move || {
            window::register_class("ExplorerRs.SessionWatch", None);
            let watch = window::create(
                CreateParams {
                    class: "ExplorerRs.SessionWatch",
                    title: "explorer.rs session",
                    style: WS_POPUP,
                    ex_style: WS_EX_TOOLWINDOW,
                    rect: RECT::default(),
                    parent: None,
                },
                Box::new(SessionWatch { sink: sink.clone() }),
            );
            if let Err(e) = &watch {
                tracing::error!(error = %e, "session watch window failed; sign-out is not detected");
            }
            if hotkeys {
                window::register_class(explorer_core::ipc::GUARDIAN_WINDOW_CLASS, None);
                match window::create(
                    CreateParams {
                        class: explorer_core::ipc::GUARDIAN_WINDOW_CLASS,
                        title: "",
                        style: Default::default(),
                        ex_style: Default::default(),
                        rect: RECT::default(),
                        parent: Some(HWND_MESSAGE),
                    },
                    Box::new(Hotkeys { sink }),
                ) {
                    Ok(h) => register_hotkeys(h),
                    Err(e) => tracing::error!(error = %e, "hotkey window failed"),
                }
            }
            window::run_message_loop(|_| false);
        })
        .is_ok()
}

/// A named (session-local) manual-reset event.
pub struct NamedEvent(HANDLE);

// SAFETY: event handles may be used from any thread.
unsafe impl Send for NamedEvent {}
unsafe impl Sync for NamedEvent {}

impl Drop for NamedEvent {
    fn drop(&mut self) {
        // SAFETY: handle owned by us.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl NamedEvent {
    /// Create or open `Local\<name>` (manual reset, initially unset). The
    /// object lives as long as any process holds a handle.
    pub fn create(name: &str) -> Option<Self> {
        let full = WideString::new(&format!("Local\\{name}"));
        // SAFETY: valid name string.
        match unsafe { CreateEventW(None, true, false, full.pcwstr()) } {
            Ok(h) => Some(Self(h)),
            Err(e) => {
                tracing::warn!(name, error = %e, "CreateEventW failed");
                None
            }
        }
    }

    pub fn is_set(&self) -> bool {
        // SAFETY: valid handle; zero timeout.
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }

    pub fn set(&self) {
        // SAFETY: valid handle.
        let _ = unsafe { SetEvent(self.0) };
    }

    /// Set the event and return true if this call was the first one in the
    /// session (i.e. the caller should do the once-per-session work).
    /// Callers are serialized by the supervisor's single-instance mutex.
    pub fn claim(&self) -> bool {
        if self.is_set() {
            return false;
        }
        self.set();
        true
    }
}

/// Name of the event that marks "shell duties done in this sign-in". The
/// guardian keeps a handle open for the whole session so the event survives
/// supervisor restarts.
pub const SHELL_DUTIES_EVENT: &str = "ExplorerRs.ShellDutiesClaimed";

/// Tell Winlogon / the logon UI that the shell is up (what explorer.exe
/// does). Undocumented, best effort: missing events are ignored.
pub fn signal_shell_ready() {
    for name in [
        "msgina: ShellReadyEvent",
        "ShellDesktopSwitchEvent",
        "Global\\msgina: ShellReadyEvent",
    ] {
        let w = WideString::new(name);
        // SAFETY: valid name; handle closed below.
        match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, w.pcwstr()) } {
            Ok(h) => {
                // SAFETY: valid handle.
                let r = unsafe { SetEvent(h) };
                // SAFETY: handle owned by us.
                let _ = unsafe { CloseHandle(h) };
                tracing::info!(event = name, ok = r.is_ok(), "signalled shell-ready event");
            }
            Err(e) => tracing::debug!(event = name, error = %e, "shell-ready event not present"),
        }
    }
}

/// Does the session-local mutex `Local\<name>` exist (i.e. is the process
/// holding it running)? Used by `--status`.
pub fn mutex_exists(name: &str) -> bool {
    let w = WideString::new(&format!("Local\\{name}"));
    // SAFETY: valid name; handle closed below.
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w.pcwstr()) } {
        Ok(h) => {
            // SAFETY: handle owned by us.
            let _ = unsafe { CloseHandle(h) };
            true
        }
        Err(_) => false,
    }
}

/// Ask the running guardian to restart all components (same as
/// Ctrl+Alt+Shift+R). Returns false if no guardian window was found.
pub fn request_restart_all() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
    let Some(h) = crate::process::guardian_window() else {
        return false;
    };
    // SAFETY: posting never blocks.
    unsafe {
        PostMessageW(
            Some(h),
            WM_HOTKEY,
            WPARAM(HOTKEY_RESTART as usize),
            LPARAM(0),
        )
    }
    .is_ok()
}
