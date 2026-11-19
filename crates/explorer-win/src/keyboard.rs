// SPDX-License-Identifier: GPL-3.0-or-later
//! Low-level input hooks: "Win key alone opens the launcher" and "a click
//! outside closes the popup".
//!
//! Windows silently removes a low-level hook whose callback takes longer
//! than `LowLevelHooksTimeout`, and it never says so. Hooks installed on a
//! busy UI thread die that way during sign-in, when the shell starts with
//! everything else (round-4 test: after signing in again with explorer.rs as
//! the shell, the Win key did nothing). So each hook here runs on its own
//! thread that does nothing else, and the Win key hook has a watchdog: input
//! newer than anything the hooks saw ([`GetLastInputInfo`]) means they were
//! removed, and they are installed again.
//!
//! The callbacks only feed the pure [`WinKeyDetector`] and post messages to
//! the owning window. Keys and clicks are never swallowed.

use std::cell::{Cell, RefCell};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use explorer_core::hotkey::{KeyAction, WinKeyDetector, VK_MASK};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetLastInputInfo, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    LASTINPUTINFO, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, GetWindowThreadProcessId, PeekMessageW,
    PostMessageW, PostThreadMessageW, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx,
    WindowFromPoint, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_QUIT,
    WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_TIMER, WM_USER, WM_XBUTTONDOWN,
};

/// Posted to the target window when the Win key was tapped alone.
pub const WM_APP_WINKEY: u32 = WM_APP + 0x102;

/// How often the watchdog compares the hooks' last event with the
/// system's last input.
const WATCHDOG: Duration = Duration::from_secs(2);
/// Input this much newer than the hooks' last event means they are gone.
const STALE_MS: i32 = 1500;

/// Thread message asking a [`ClickWatcher`] thread to (un)install its hook.
const WM_ARM: u32 = WM_APP + 1;

fn is_button_down(wparam: WPARAM) -> bool {
    matches!(
        wparam.0 as u32,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
    )
}

/// A thread with a message loop that hosts hooks. `setup` runs on it after
/// its message queue exists; it returns the per-message handler.
struct HookThread {
    tid: u32,
    join: Option<JoinHandle<()>>,
}

impl HookThread {
    fn spawn<F>(name: &str, setup: F) -> anyhow::Result<Self>
    where
        F: FnOnce() -> anyhow::Result<Box<dyn FnMut(&MSG)>> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<anyhow::Result<u32>>();
        let join = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let mut msg = MSG::default();
                // SAFETY: creates this thread's message queue so that
                // PostThreadMessageW works as soon as the id is known.
                unsafe {
                    let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
                }
                let mut on_msg = match setup() {
                    Ok(f) => f,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                // SAFETY: plain query.
                let _ = tx.send(Ok(unsafe { GetCurrentThreadId() }));
                // SAFETY: standard message loop on this thread.
                while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
                    on_msg(&msg);
                    // SAFETY: as above.
                    unsafe { DispatchMessageW(&msg) };
                }
            })?;
        let tid = rx
            .recv()
            .map_err(|_| anyhow::anyhow!("the hook thread ended early"))??;
        Ok(Self {
            tid,
            join: Some(join),
        })
    }

    fn post(&self, msg: u32, wparam: usize) {
        // SAFETY: posting to our own thread.
        let _ = unsafe { PostThreadMessageW(self.tid, msg, WPARAM(wparam), LPARAM(0)) };
    }
}

impl Drop for HookThread {
    fn drop(&mut self) {
        self.post(WM_QUIT, 0);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Win key
// ---------------------------------------------------------------------------

struct WinState {
    detector: WinKeyDetector,
    /// `HWND` as an integer (the thread owns a copy, HWND is not Send).
    target: isize,
    mask: bool,
}

thread_local! {
    static WIN: RefCell<Option<WinState>> = const { RefCell::new(None) };
    /// Timestamp (GetTickCount base) of the last event either hook saw.
    static LAST_SEEN: Cell<u32> = const { Cell::new(0) };
}

struct WinHooks {
    kb: HHOOK,
    mouse: Option<HHOOK>,
}

impl WinHooks {
    fn install() -> anyhow::Result<Self> {
        let hinst = crate::window::module_instance();
        // SAFETY: 'static callbacks; removed in Drop on the same thread.
        let kb = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), Some(hinst), 0) }?;
        // SAFETY: as above.
        let mouse =
            unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(win_mouse_proc), Some(hinst), 0) }.ok();
        LAST_SEEN.with(|c| c.set(last_input().unwrap_or(0)));
        Ok(Self { kb, mouse })
    }
}

impl Drop for WinHooks {
    fn drop(&mut self) {
        // SAFETY: our own hooks, removed on the thread that set them.
        unsafe {
            let _ = UnhookWindowsHookEx(self.kb);
            if let Some(m) = self.mouse {
                let _ = UnhookWindowsHookEx(m);
            }
        }
    }
}

fn last_input() -> Option<u32> {
    let mut lii = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: valid struct with cbSize set.
    unsafe { GetLastInputInfo(&mut lii) }
        .as_bool()
        .then_some(lii.dwTime)
}

fn seen(time: u32) {
    LAST_SEEN.with(|c| c.set(time));
}

/// The Win key hook. Dropping it stops its thread and removes the hooks.
pub struct WinKeyHook {
    _thread: HookThread,
}

impl WinKeyHook {
    /// Start the hook thread. `mask_start_menu`: inject the mask key so
    /// explorer's Start menu does not open as well (dev mode).
    pub fn install(target: HWND, mask_start_menu: bool) -> anyhow::Result<Self> {
        let target = target.0 as isize;
        let thread = HookThread::spawn("winkey-hook", move || {
            WIN.with(|s| {
                *s.borrow_mut() = Some(WinState {
                    detector: WinKeyDetector::new(),
                    target,
                    mask: mask_start_menu,
                })
            });
            let mut hooks = Some(WinHooks::install()?);
            // SAFETY: thread timer (no window); posted as WM_TIMER with a
            // null hwnd to this thread's queue.
            let timer = unsafe { SetTimer(None, 0, WATCHDOG.as_millis() as u32, None) };
            let mut last_warn: Option<Instant> = None;
            let mut reinstalls = 0u32;
            // The hooks live in this closure: they are removed when the
            // thread's loop ends and the closure is dropped on it.
            Ok(Box::new(move |msg: &MSG| {
                if msg.message != WM_TIMER || !msg.hwnd.is_invalid() || msg.wParam.0 != timer {
                    return;
                }
                let Some(input) = last_input() else { return };
                let last = LAST_SEEN.with(|c| c.get());
                if (input.wrapping_sub(last) as i32) <= STALE_MS {
                    return;
                }
                // Input the hooks never saw: they were removed (or the
                // input went to the secure desktop / an elevated window,
                // where re-installing is harmless).
                reinstalls += 1;
                if last_warn.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
                    tracing::warn!(
                        reinstalls,
                        "input hooks were not called for recent input; installing them again"
                    );
                    last_warn = Some(Instant::now());
                }
                // Remove the old hooks before installing new ones.
                drop(hooks.take());
                WIN.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.detector = WinKeyDetector::new();
                    }
                });
                hooks = WinHooks::install()
                    .inspect_err(
                        |e| tracing::warn!(error = %e, "cannot install the keyboard hook again"),
                    )
                    .ok();
                seen(input);
            }) as Box<dyn FnMut(&MSG)>)
        })?;
        Ok(Self { _thread: thread })
    }
}

/// Inject the inert mask key (down + up). Besides masking the Win key,
/// this makes the calling process the last one to send input, which is
/// what Windows requires before `AllowSetForegroundWindow` may grant the
/// foreground to another process (the Start menu opened by the Win key).
pub fn claim_foreground_rights() {
    send_mask_key();
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
        seen(k.time);
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let injected = k.flags.0 & LLKHF_INJECTED.0 != 0;
        WIN.with(|s| {
            if let Ok(mut guard) = s.try_borrow_mut() {
                if let Some(st) = guard.as_mut() {
                    if st.detector.on_key(k.vkCode, down, injected) == KeyAction::MaskAndTrigger {
                        if st.mask {
                            send_mask_key();
                        }
                        let target = HWND(st.target as *mut core::ffi::c_void);
                        // SAFETY: posting to the owning window.
                        let _ = unsafe {
                            PostMessageW(Some(target), WM_APP_WINKEY, WPARAM(0), LPARAM(0))
                        };
                    }
                }
            }
        });
    }
    // SAFETY: chaining as required by the hook contract.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn win_mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_MOUSE_LL, lParam points to an MSLLHOOKSTRUCT.
        let m = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        seen(m.time);
        if is_button_down(wparam) {
            WIN.with(|s| {
                if let Ok(mut guard) = s.try_borrow_mut() {
                    if let Some(st) = guard.as_mut() {
                        st.detector.on_mouse_button();
                    }
                }
            });
        }
    }
    // SAFETY: chaining as required by the hook contract.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

// ---------------------------------------------------------------------------
// Clicks outside a popup
// ---------------------------------------------------------------------------

thread_local! {
    /// (target HWND, message) of this thread's click watcher.
    static CLICK: Cell<(isize, u32)> = const { Cell::new((0, 0)) };
}

/// Reports mouse-button presses anywhere on screen while armed, as
/// `msg` posted to the target window with the screen point in `lparam`
/// (decode with [`click_point`]).
///
/// Popups close when they lose activation, but a click on a window that
/// never activates (our desktop and taskbar are `WS_EX_NOACTIVATE`; with
/// explorer.rs as the shell there is no Explorer desktop behind them)
/// changes nothing. The hook is installed only while a popup is open.
pub struct ClickWatcher {
    thread: HookThread,
}

impl ClickWatcher {
    pub fn start(target: HWND, msg: u32) -> anyhow::Result<Self> {
        let target = target.0 as isize;
        let thread = HookThread::spawn("click-hook", move || {
            CLICK.with(|c| c.set((target, msg)));
            let mut hook: Option<HHOOK> = None;
            Ok(Box::new(move |m: &MSG| {
                let want = match m.message {
                    WM_ARM => m.wParam.0 != 0,
                    WM_QUIT => false,
                    _ => return,
                };
                match (want, hook) {
                    (true, None) => {
                        let hinst = crate::window::module_instance();
                        // SAFETY: 'static callback; removed on this thread.
                        match unsafe {
                            SetWindowsHookExW(WH_MOUSE_LL, Some(click_proc), Some(hinst), 0)
                        } {
                            Ok(h) => hook = Some(h),
                            Err(e) => tracing::warn!(error = %e, "cannot install the click hook"),
                        }
                    }
                    (false, Some(h)) => {
                        // SAFETY: our own hook.
                        let _ = unsafe { UnhookWindowsHookEx(h) };
                        hook = None;
                    }
                    _ => {}
                }
            }) as Box<dyn FnMut(&MSG)>)
        })?;
        Ok(Self { thread })
    }

    /// Start (popup shown) or stop (popup hidden) reporting clicks.
    pub fn arm(&self, on: bool) {
        self.thread.post(WM_ARM, on as usize);
    }
}

impl Drop for ClickWatcher {
    fn drop(&mut self) {
        self.arm(false);
    }
}

unsafe extern "system" fn click_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && is_button_down(wparam) {
        // SAFETY: for WH_MOUSE_LL, lParam points to an MSLLHOOKSTRUCT.
        let m = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let (target, msg) = CLICK.with(|c| c.get());
        if target != 0 {
            let packed = (m.pt.x as i16 as u16 as u32) | ((m.pt.y as i16 as u16 as u32) << 16);
            let target = HWND(target as *mut core::ffi::c_void);
            // SAFETY: posting to the owning window.
            let _ = unsafe { PostMessageW(Some(target), msg, wparam, LPARAM(packed as isize)) };
        }
    }
    // SAFETY: chaining as required by the hook contract.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// The screen point of a [`ClickWatcher`] message.
pub fn click_point(lparam: LPARAM) -> POINT {
    POINT {
        x: (lparam.0 & 0xFFFF) as u16 as i16 as i32,
        y: ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
    }
}

/// Whether the window at `pt` belongs to another process (or there is no
/// window there): a click there should close our popup. Clicks on our own
/// windows (the popup itself, its menus and tooltips) are left alone.
pub fn click_is_foreign(pt: POINT) -> bool {
    // SAFETY: plain queries.
    unsafe {
        let w = WindowFromPoint(pt);
        if w.is_invalid() {
            return true;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(w, Some(&mut pid));
        pid != GetCurrentProcessId()
    }
}
