// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal window-class/window-procedure plumbing.
//!
//! A window owns a boxed [`Handler`] stored in `GWLP_USERDATA`. The handler
//! lives in a `RefCell`; if a message re-enters the window procedure while
//! the handler is already borrowed (e.g. `SetWindowPos` sending
//! `WM_WINDOWPOSCHANGED` synchronously), the nested message falls back to
//! `DefWindowProcW` instead of creating an aliasing `&mut` — except posted
//! private/registered messages (`explorer_core::ipc::deferrable_while_busy`),
//! which are queued and handled as soon as the running handler returns, so
//! e.g. a worker's "results ready" message posted while a popup menu is open
//! is not lost. (Re-posting them with `PostMessageW` would spin: the modal
//! menu loop would fetch the message again at once while the handler is
//! still borrowed.)
//!
//! Component lifecycle: a component calls [`init_component`] on its UI
//! thread. The shared window procedure then handles the registered
//! `ExplorerRs.Shutdown` message (graceful stop, exit code 0). Deliberate
//! exits go through [`quit`]; a main window that is destroyed for any other
//! reason calls [`quit_unexpected`], and [`exit_code`] then yields
//! `EXIT_RESTART` so the supervisor restarts the component.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
    InSendMessage, IsWindow, LoadCursorW, PostMessageW, PostQuitMessage, PostThreadMessageW,
    RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow, SetWindowLongPtrW,
    TrackPopupMenu, TranslateMessage, CREATESTRUCTW, CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW,
    GWLP_USERDATA, HMENU, IDC_ARROW, MSG, TRACK_POPUP_MENU_FLAGS, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_NCCREATE, WM_NCDESTROY, WM_NULL, WM_QUIT, WNDCLASSEXW,
};

use crate::util::WideString;

/// Per-window message handler.
pub trait Handler {
    /// Return `Some(result)` if handled, `None` to fall through to
    /// `DefWindowProcW`.
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT>;
}

/// Per-window state: the handler and the messages deferred while it ran.
struct Slot {
    handler: RefCell<Box<dyn Handler>>,
    deferred: RefCell<VecDeque<(u32, WPARAM, LPARAM)>>,
}

impl Slot {
    /// Handle the messages that arrived while the handler was busy.
    ///
    /// # Safety
    /// Must be called from the window procedure of `hwnd`, with no borrow
    /// of the handler alive.
    unsafe fn drain_deferred(&self, hwnd: HWND) {
        loop {
            // SAFETY: plain query.
            if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                self.deferred.borrow_mut().clear();
                return;
            }
            let Some((msg, wparam, lparam)) = self.deferred.borrow_mut().pop_front() else {
                return;
            };
            match self.handler.try_borrow_mut() {
                Ok(mut h) => {
                    let _ = h.handle(hwnd, msg, wparam, lparam);
                }
                Err(_) => {
                    self.deferred.borrow_mut().push_front((msg, wparam, lparam));
                    return;
                }
            }
        }
    }
}

// --- component lifecycle -------------------------------------------------

/// No deliberate exit code recorded yet.
const NO_INTENT: i32 = i32::MIN;
static INTENTIONAL_EXIT: AtomicI32 = AtomicI32::new(NO_INTENT);
static MAIN_THREAD: AtomicU32 = AtomicU32::new(0);
static COMPONENT: AtomicBool = AtomicBool::new(false);

/// The registered `ExplorerRs.Shutdown` message.
pub fn shutdown_message() -> u32 {
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(|| {
        let w = WideString::new(explorer_core::ipc::MSG_SHUTDOWN);
        // SAFETY: valid string.
        unsafe { RegisterWindowMessageW(w.pcwstr()) }
    })
}

/// Mark this process as a supervised component and this thread as its UI
/// thread: the shared window procedure then turns `ExplorerRs.Shutdown`
/// into a graceful exit with code 0. Call before creating windows.
pub fn init_component() {
    // SAFETY: plain query.
    MAIN_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    COMPONENT.store(true, Ordering::SeqCst);
    let _ = shutdown_message();
}

/// Graceful stop requested (shutdown message): exit 0 from the UI thread.
fn request_shutdown() {
    tracing::info!("graceful shutdown requested");
    INTENTIONAL_EXIT.store(explorer_core::supervisor::EXIT_STOP, Ordering::SeqCst);
    let main = MAIN_THREAD.load(Ordering::SeqCst);
    // SAFETY: plain FFI calls.
    unsafe {
        if main == 0 || main == GetCurrentThreadId() {
            PostQuitMessage(explorer_core::supervisor::EXIT_STOP);
        } else if let Err(e) = PostThreadMessageW(main, WM_QUIT, WPARAM(0), LPARAM(0)) {
            tracing::warn!(error = %e, "cannot stop the UI thread");
        }
    }
}

/// The exit code a component should return after its message loop ended:
/// the deliberate code from [`quit`], or `EXIT_RESTART` when the loop ended
/// for any other reason.
pub fn exit_code() -> i32 {
    let v = INTENTIONAL_EXIT.load(Ordering::SeqCst);
    explorer_core::supervisor::component_exit_code((v != NO_INTENT).then_some(v))
}

/// The main window was destroyed without a deliberate [`quit`] (or after
/// one): end the message loop; [`exit_code`] decides the code.
pub fn quit_unexpected() {
    if INTENTIONAL_EXIT.load(Ordering::SeqCst) == NO_INTENT {
        tracing::warn!("main window destroyed unexpectedly; exiting for a restart");
    }
    // SAFETY: plain FFI call.
    unsafe { PostQuitMessage(exit_code()) };
}

/// Show a popup menu owned by `owner` and return the chosen command (the
/// flags should include `TPM_RETURNCMD`). Follows the `TrackPopupMenu`
/// remarks: the owner is made the foreground window first (otherwise the
/// menu does not close when the user clicks elsewhere) and a `WM_NULL` is
/// posted afterwards (otherwise a second menu may close immediately).
pub fn track_popup_menu(
    menu: HMENU,
    flags: TRACK_POPUP_MENU_FLAGS,
    x: i32,
    y: i32,
    owner: HWND,
) -> u32 {
    // SAFETY: `menu` is a live popup menu; `owner` is our own window.
    unsafe {
        let _ = SetForegroundWindow(owner);
        let r = TrackPopupMenu(menu, flags, x, y, None, owner, None);
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        r.0 as u32
    }
}

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
    let slot: Box<Slot> = Box::new(Slot {
        handler: RefCell::new(handler),
        deferred: RefCell::new(VecDeque::new()),
    });
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
            // If the handler is currently borrowed, the window is being
            // destroyed from inside its own handler (DestroyWindow called
            // in `handle`). Freeing the slot now would pull it out from
            // under that live borrow, so leak it instead: a small, bounded
            // leak is far better than a use-after-free.
            let can_free = (*ptr).handler.try_borrow_mut().is_ok();
            if !can_free {
                tracing::warn!("window destroyed from inside its own handler; leaking its state");
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let slot = Box::from_raw(ptr);
            let r = slot.handler.borrow_mut().handle(hwnd, msg, wparam, lparam);
            drop(slot);
            return r.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam));
        }
        if COMPONENT.load(Ordering::Relaxed) && msg == shutdown_message() {
            request_shutdown();
            return LRESULT(0);
        }
        let slot = &*ptr;
        let r = match slot.handler.try_borrow_mut() {
            Ok(mut h) => {
                let r = h.handle(hwnd, msg, wparam, lparam);
                drop(h);
                if !slot.deferred.borrow().is_empty() {
                    slot.drain_deferred(hwnd);
                }
                r
            }
            Err(_) => {
                if explorer_core::ipc::deferrable_while_busy(msg, InSendMessage().as_bool()) {
                    let mut q = slot.deferred.borrow_mut();
                    if q.len() < explorer_core::ipc::MAX_DEFERRED {
                        q.push_back((msg, wparam, lparam));
                    } else {
                        tracing::warn!(
                            msg,
                            "handler busy and deferral queue full; message dropped"
                        );
                    }
                    return LRESULT(0);
                }
                None
            }
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

/// Deliberate exit with `code` (records it for [`exit_code`]).
pub fn quit(code: i32) {
    INTENTIONAL_EXIT.store(code, Ordering::SeqCst);
    // SAFETY: plain FFI call.
    unsafe { PostQuitMessage(code) };
}

/// `PCWSTR` null helper.
pub fn null_pcwstr() -> PCWSTR {
    PCWSTR::null()
}
