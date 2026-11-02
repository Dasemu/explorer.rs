// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows implementation of the taskbar.
//!
//! Structure (all on one UI thread):
//! - a hidden **controller** window owns the [`TaskbarModel`], receives
//!   shell-hook / WinEvent notifications, the Win-key signal, timers and
//!   display changes, and owns the bar windows;
//! - one **bar** window per monitor: an AppBar that paints the model and
//!   handles mouse input.
//!
//! Shared state lives in an `Rc<Shared>` with `RefCell`s that are only
//! borrowed for short, non-reentrant sections.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use explorer_core::clock;
use explorer_core::config::{Config, MonitorSelection};
use explorer_core::ipc::{LauncherCommand, RunMode, TASKBAR_WINDOW_CLASS};
use explorer_core::layout::{bar_layout, BarHit, BarLayout, Rect};
use explorer_core::paths::AppPaths;
use explorer_core::supervisor::{EXIT_RESTART, EXIT_SHUTDOWN_ALL};
use explorer_core::taskbar_model::{Button, ClickAction, TaskbarModel, WindowEvent};
use explorer_core::window_filter::FilterRules;
use explorer_win::appbar::{AppBar, ABN_FULLSCREENAPP, ABN_POSCHANGED};
use explorer_win::gfx::{theme, Align, Gfx, Surface};
use explorer_win::keyboard::{WinKeyHook, WM_APP_WINKEY};
use explorer_win::monitors::{self, Monitor};
use explorer_win::shellhook::{self, events as ev, ShellHooks, WM_APP_WINEVENT};
use explorer_win::util::{hwnd_to_u64, u64_to_hwnd, wide, ComApartment};
use explorer_win::window::{self, CreateParams, Handler};
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use explorer_win::windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, MonitorFromWindow, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use explorer_win::windows::Win32::System::Shutdown::{ExitWindowsEx, EWX_LOGOFF, SHUTDOWN_REASON};
use explorer_win::windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, DestroyWindow, GetCursorPos, KillTimer,
    PostMessageW, SetTimer, SetWindowPos, TrackPopupMenu, HWND_NOTOPMOST, HWND_TOPMOST,
    MA_NOACTIVATE, MF_SEPARATOR, MF_STRING, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_ACTIVATE, WM_APP, WM_CREATE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONUP, WM_SIZE, WM_TIMER, WM_WINDOWPOSCHANGED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

use crate::Args;

const CONTROLLER_CLASS: &str = "ExplorerRs.TaskbarController";

const WM_APP_INIT: u32 = WM_APP + 1;
const WM_APP_APPBAR: u32 = WM_APP + 2;
const WM_APP_POSITION: u32 = WM_APP + 3;

/// WinUser.h value; not exported by the `windows` crate module we use.
const WM_MOUSELEAVE: u32 = 0x02A3;

const TIMER_CLOCK: usize = 1;
const TIMER_RESYNC: usize = 2;
const TIMER_REBUILD: usize = 3;

/// Full re-enumeration interval: a safety net for missed notifications.
const RESYNC_MS: u32 = 30_000;

struct Shared {
    gfx: Gfx,
    config: Config,
    mode: RunMode,
    model: RefCell<TaskbarModel>,
    clock_text: RefCell<(String, String)>,
    bars: RefCell<Vec<HWND>>,
    controller: Cell<HWND>,
}

impl Shared {
    fn invalidate_bars(&self) {
        for &b in self.bars.borrow().iter() {
            // SAFETY: repaint request on our own window.
            let _ = unsafe { InvalidateRect(Some(b), None, false) };
        }
    }

    fn update_clock(&self) -> bool {
        let now = explorer_win::time::now_local();
        let t = &self.config.taskbar;
        let next = (
            clock::format(&t.clock_format, &now),
            clock::format(&t.date_format, &now),
        );
        let mut cur = self.clock_text.borrow_mut();
        if *cur != next {
            *cur = next;
            true
        } else {
            false
        }
    }
}

pub fn run(args: Args) -> i32 {
    explorer_win::console::attach_parent();
    explorer_win::dpi::enable_per_monitor_v2();
    let _com = ComApartment::sta();

    let paths = AppPaths::from_env();
    let _ = paths.ensure();
    let config_path = args.config.clone().unwrap_or_else(|| paths.config_file());
    let (config, config_err) = Config::load_or_default(&config_path);

    if args.dump_monitors {
        let text: String = monitors::all()
            .iter()
            .map(|m| format!("{}\n", m.describe()))
            .collect();
        return explorer_core::dump::emit(&text, args.output.as_deref());
    }
    if args.dump_windows {
        let text = explorer_win::winlist::dump(&FilterRules::from_config(&config.taskbar));
        return explorer_core::dump::emit(&text, args.output.as_deref());
    }

    let _log = explorer_core::logging::init(
        "taskbar",
        &paths.log_dir,
        &config.general.log_level,
        config.general.log_keep_files,
    );
    if let Some(e) = config_err {
        tracing::error!(error = %e, "config error, using defaults");
    }
    let Some(_instance) = explorer_win::single_instance::acquire("ExplorerRs.Taskbar") else {
        tracing::warn!("another taskbar is already running; exiting");
        return 0;
    };

    let mode = RunMode::from_env();
    tracing::info!(mode = mode.as_str(), "taskbar starting");
    let gfx = match Gfx::new() {
        Ok(g) => g,
        Err(e) => {
            tracing::error!(error = %e, "cannot initialise Direct2D");
            return 1;
        }
    };
    let shared = Rc::new(Shared {
        gfx,
        model: RefCell::new(TaskbarModel::new(FilterRules::from_config(&config.taskbar))),
        config,
        mode,
        clock_text: RefCell::new((String::new(), String::new())),
        bars: RefCell::new(Vec::new()),
        controller: Cell::new(HWND::default()),
    });

    window::register_class(CONTROLLER_CLASS, None);
    window::register_class(TASKBAR_WINDOW_CLASS, None);

    let controller = match window::create(
        CreateParams {
            class: CONTROLLER_CLASS,
            title: "explorer.rs taskbar controller",
            style: WS_POPUP,
            ex_style: WS_EX_TOOLWINDOW,
            rect: RECT::default(),
            parent: None,
        },
        Box::new(Controller {
            shared: shared.clone(),
            hooks: None,
            winkey: None,
            taskbar_created_msg: explorer_win::ipc::registered_message("TaskbarCreated"),
        }),
    ) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "cannot create controller window");
            return 1;
        }
    };
    shared.controller.set(controller);
    // SAFETY: posting to our own window.
    let _ = unsafe { PostMessageW(Some(controller), WM_APP_INIT, WPARAM(0), LPARAM(0)) };

    let code = window::run_message_loop(|_| false);
    tracing::info!(code, "taskbar exiting");
    code
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

struct Controller {
    shared: Rc<Shared>,
    hooks: Option<ShellHooks>,
    winkey: Option<WinKeyHook>,
    taskbar_created_msg: u32,
}

impl Controller {
    fn rebuild_bars(&mut self) {
        let old: Vec<HWND> = self.shared.bars.borrow_mut().drain(..).collect();
        for b in old {
            // SAFETY: destroying our own windows.
            let _ = unsafe { DestroyWindow(b) };
        }
        let mut mons = monitors::all();
        if self.shared.config.taskbar.monitors == MonitorSelection::Primary {
            mons.retain(|m| m.primary);
        }
        for m in mons {
            tracing::info!(monitor = %m.describe(), "creating bar");
            let thickness = thickness(&self.shared.config, m.dpi);
            let rect = explorer_win::appbar::bar_rect(
                m.rect,
                self.shared.config.taskbar.position,
                thickness,
            );
            let res = window::create(
                CreateParams {
                    class: TASKBAR_WINDOW_CLASS,
                    title: "explorer.rs taskbar",
                    style: WS_POPUP,
                    ex_style: WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                    rect,
                    parent: None,
                },
                Box::new(Bar {
                    shared: self.shared.clone(),
                    monitor: m,
                    appbar: None,
                    surface: None,
                    layout: None,
                    hover: BarHit::Empty,
                    tracking: false,
                }),
            );
            match res {
                Ok(h) => self.shared.bars.borrow_mut().push(h),
                Err(e) => tracing::error!(error = %e, "cannot create bar window"),
            }
        }
    }

    fn resync(&mut self) {
        let snaps = explorer_win::winlist::snapshot_all();
        if self.shared.model.borrow_mut().reset(snaps) {
            self.shared.invalidate_bars();
        }
    }

    fn apply(&mut self, e: WindowEvent) {
        if self.shared.model.borrow_mut().apply(e) {
            self.shared.invalidate_bars();
        }
    }

    fn upsert(&mut self, hwnd: HWND) {
        if explorer_win::winlist::exists(hwnd) {
            self.apply(WindowEvent::Upsert(explorer_win::winlist::snapshot(hwnd)));
        } else {
            self.apply(WindowEvent::Destroyed(hwnd_to_u64(hwnd)));
        }
    }

    fn on_shell_hook(&mut self, code: usize, hwnd: HWND) {
        use shellhook::*;
        match code {
            HSHELL_WINDOWCREATED | HSHELL_REDRAW | HSHELL_WINDOWREPLACED => self.upsert(hwnd),
            HSHELL_WINDOWDESTROYED => self.apply(WindowEvent::Destroyed(hwnd_to_u64(hwnd))),
            HSHELL_WINDOWACTIVATED | HSHELL_RUDEAPPACTIVATED => {
                if !hwnd.is_invalid() {
                    self.upsert(hwnd);
                }
                self.apply(WindowEvent::Activated(hwnd_to_u64(hwnd)));
            }
            HSHELL_FLASH => self.apply(WindowEvent::Flash(hwnd_to_u64(hwnd))),
            _ => {}
        }
    }

    fn on_win_event(&mut self, event: u32, hwnd: HWND) {
        match event {
            ev::EVENT_OBJECT_DESTROY => self.apply(WindowEvent::Destroyed(hwnd_to_u64(hwnd))),
            ev::EVENT_SYSTEM_FOREGROUND => {
                self.upsert(hwnd);
                self.apply(WindowEvent::Activated(hwnd_to_u64(hwnd)));
            }
            _ => self.upsert(hwnd),
        }
    }
}

impl Handler for Controller {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if let Some(h) = &self.hooks {
            if msg == h.shell_message() {
                self.on_shell_hook(wparam.0, HWND(lparam.0 as *mut _));
                return Some(LRESULT(0));
            }
        }
        if msg == self.taskbar_created_msg && self.taskbar_created_msg != 0 {
            // explorer.exe restarted (dev mode): AppBar registrations are
            // lost, so recreate the bars.
            tracing::info!("TaskbarCreated received; rebuilding bars");
            self.rebuild_bars();
            return Some(LRESULT(0));
        }
        match msg {
            WM_APP_INIT => {
                self.hooks = Some(ShellHooks::install(hwnd));
                if self.shared.config.launcher.win_key_opens_launcher {
                    match WinKeyHook::install(hwnd, self.shared.mode == RunMode::Dev) {
                        Ok(k) => self.winkey = Some(k),
                        Err(e) => {
                            tracing::warn!(error = %e, "keyboard hook failed; Win key will not open the launcher")
                        }
                    }
                }
                self.shared.update_clock();
                self.resync();
                self.rebuild_bars();
                // SAFETY: timers on our own window.
                unsafe {
                    SetTimer(Some(hwnd), TIMER_CLOCK, 1000, None);
                    SetTimer(Some(hwnd), TIMER_RESYNC, RESYNC_MS, None);
                }
                #[cfg(feature = "tray")]
                if let Err(e) = explorer_win::tray::start(self.shared.mode) {
                    tracing::warn!(error = %e, "tray start failed");
                }
                Some(LRESULT(0))
            }
            WM_APP_WINEVENT => {
                self.on_win_event(wparam.0 as u32, HWND(lparam.0 as *mut _));
                Some(LRESULT(0))
            }
            WM_APP_WINKEY => {
                explorer_win::ipc::send_launcher_command(LauncherCommand::Toggle);
                Some(LRESULT(0))
            }
            WM_TIMER => {
                match wparam.0 {
                    TIMER_CLOCK => {
                        if self.shared.update_clock() {
                            self.shared.invalidate_bars();
                        }
                    }
                    TIMER_RESYNC => self.resync(),
                    TIMER_REBUILD => {
                        // SAFETY: our own timer.
                        let _ = unsafe { KillTimer(Some(hwnd), TIMER_REBUILD) };
                        self.rebuild_bars();
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_DISPLAYCHANGE => {
                // Debounce: several notifications arrive while monitors are
                // being (re)configured.
                // SAFETY: timer on our own window.
                unsafe { SetTimer(Some(hwnd), TIMER_REBUILD, 750, None) };
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                self.winkey = None;
                self.hooks = None;
                window::quit(0);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

fn thickness(config: &Config, dpi: u32) -> i32 {
    (config.taskbar.height as f32 * explorer_win::dpi::scale(dpi)).round() as i32
}

// ---------------------------------------------------------------------------
// Bar (one per monitor)
// ---------------------------------------------------------------------------

struct Bar {
    shared: Rc<Shared>,
    monitor: Monitor,
    appbar: Option<AppBar>,
    surface: Option<Surface>,
    layout: Option<(BarLayout, Vec<Button>)>,
    hover: BarHit,
    tracking: bool,
}

const CMD_CLOSE_WINDOW: u32 = 1;
const CMD_TASK_MANAGER: u32 = 10;
const CMD_RESTART_TASKBAR: u32 = 11;
const CMD_QUIT: u32 = 12;
const CMD_SIGN_OUT: u32 = 13;
const CMD_OPEN_LAUNCHER: u32 = 14;

impl Bar {
    fn scale(&self) -> f32 {
        explorer_win::dpi::scale(self.monitor.dpi)
    }

    fn visible_buttons(&self) -> Vec<Button> {
        let model = self.shared.model.borrow();
        let own_only = self.shared.config.taskbar.windows_on_own_monitor_only;
        model
            .buttons()
            .iter()
            .filter(|b| {
                !own_only || {
                    // SAFETY: MonitorFromWindow accepts any handle.
                    let m =
                        unsafe { MonitorFromWindow(u64_to_hwnd(b.hwnd), MONITOR_DEFAULTTONEAREST) };
                    m == self.monitor.handle
                }
            })
            .cloned()
            .collect()
    }

    fn compute_layout(&self, hwnd: HWND) -> (BarLayout, Vec<Button>) {
        let mut rc = RECT::default();
        // SAFETY: valid out-pointer.
        let _ = unsafe {
            explorer_win::windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc)
        };
        let buttons = self.visible_buttons();
        let t = &self.shared.config.taskbar;
        let l = bar_layout(
            (rc.right - rc.left) as f32,
            (rc.bottom - rc.top) as f32,
            self.scale(),
            buttons.len(),
            t.button_width as f32,
            t.show_clock,
        );
        (l, buttons)
    }

    fn position(&mut self, hwnd: HWND) {
        // Refresh monitor info: DPI or work area may have changed.
        if let Some(m) = monitors::query(self.monitor.handle) {
            self.monitor = m;
        }
        let thickness = thickness(&self.shared.config, self.monitor.dpi);
        let edge = self.shared.config.taskbar.position;
        let rect = match self.appbar.as_mut() {
            Some(ab) => ab.reserve(self.monitor.rect, edge, thickness),
            None => explorer_win::appbar::bar_rect(self.monitor.rect, edge, thickness),
        };
        // SAFETY: positioning our own window.
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        };
        if let Some(s) = self.surface.as_mut() {
            s.resize();
        }
        // SAFETY: repaint request on our own window.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }

    fn paint(&mut self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: BeginPaint/EndPaint pairing in WM_PAINT.
        unsafe { BeginPaint(hwnd, &mut ps) };
        let (layout, buttons) = self.compute_layout(hwnd);
        let scale = self.scale();
        let active = self.shared.model.borrow().active();
        let clock_text = self.shared.clock_text.borrow().clone();
        let hover = self.hover;
        let surface = self.surface.get_or_insert_with(|| Surface::new(hwnd));
        surface.draw(&self.shared.gfx, theme::BAR_BG, |p| {
            let inset = |r: Rect| {
                Rect::new(
                    r.x + 2.0 * scale,
                    r.y + 4.0 * scale,
                    r.w - 4.0 * scale,
                    r.h - 8.0 * scale,
                )
            };
            let radius = 4.0 * scale;
            if hover == BarHit::Start {
                p.fill_rounded(inset(layout.start), radius, theme::BUTTON_HOVER);
            }
            p.text(
                "\u{229E}",
                layout.start,
                20.0 * scale,
                theme::TEXT,
                Align::Center,
                false,
            );
            for (i, (r, b)) in layout.buttons.iter().zip(buttons.iter()).enumerate() {
                let is_active = active == Some(b.hwnd);
                let bg = if b.flashing {
                    theme::FLASH
                } else if is_active {
                    theme::BUTTON_ACTIVE
                } else if hover == BarHit::Button(i) {
                    theme::BUTTON_HOVER
                } else {
                    theme::BUTTON_BG
                };
                let ir = inset(*r);
                p.fill_rounded(ir, radius, bg);
                if is_active {
                    let bar_w = (ir.w * 0.4).max(8.0 * scale);
                    p.fill_rounded(
                        Rect::new(
                            ir.x + (ir.w - bar_w) / 2.0,
                            ir.bottom() - 3.0 * scale,
                            bar_w,
                            3.0 * scale,
                        ),
                        1.5 * scale,
                        theme::ACCENT,
                    );
                }
                let pad = 8.0 * scale;
                p.text(
                    &b.title,
                    Rect::new(ir.x + pad, ir.y, ir.w - 2.0 * pad, ir.h),
                    12.0 * scale,
                    theme::TEXT,
                    Align::Left,
                    false,
                );
            }
            if layout.overflow > 0 {
                // TODO(taskbar): overflow menu. For now show the count.
                if let Some(last) = layout.buttons.last() {
                    let r = Rect::new(last.right(), 0.0, 28.0 * scale, layout.start.h);
                    p.text(
                        &format!("+{}", layout.overflow),
                        r,
                        11.0 * scale,
                        theme::TEXT_DIM,
                        Align::Center,
                        false,
                    );
                }
            }
            if let Some(c) = layout.clock {
                if clock_text.1.is_empty() {
                    p.text(
                        &clock_text.0,
                        c,
                        12.0 * scale,
                        theme::TEXT,
                        Align::Center,
                        false,
                    );
                } else {
                    let half = c.h / 2.0;
                    p.text(
                        &clock_text.0,
                        Rect::new(c.x, c.y + 2.0 * scale, c.w, half - 2.0 * scale),
                        12.0 * scale,
                        theme::TEXT,
                        Align::Center,
                        false,
                    );
                    p.text(
                        &clock_text.1,
                        Rect::new(c.x, c.y + half, c.w, half - 2.0 * scale),
                        11.0 * scale,
                        theme::TEXT_DIM,
                        Align::Center,
                        false,
                    );
                }
            }
        });
        self.layout = Some((layout, buttons));
        // SAFETY: matches BeginPaint above.
        let _ = unsafe { EndPaint(hwnd, &ps) };
    }

    fn hit(&self, lparam: LPARAM) -> (BarHit, Option<Button>) {
        let x = (lparam.0 & 0xffff) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        let Some((layout, buttons)) = &self.layout else {
            return (BarHit::Empty, None);
        };
        let h = layout.hit(x, y);
        let b = match h {
            BarHit::Button(i) => buttons.get(i).cloned(),
            _ => None,
        };
        (h, b)
    }

    fn on_click(&mut self, lparam: LPARAM) {
        let (h, b) = self.hit(lparam);
        match (h, b) {
            (BarHit::Start, _) => {
                explorer_win::ipc::send_launcher_command(LauncherCommand::Toggle);
            }
            (BarHit::Button(_), Some(b)) => {
                let target = u64_to_hwnd(b.hwnd);
                let minimized = explorer_win::winlist::is_minimized(target);
                let action = self.shared.model.borrow().click_action(b.hwnd, minimized);
                match action {
                    ClickAction::Activate => explorer_win::activate::activate(target),
                    ClickAction::Minimize => explorer_win::activate::minimize(target),
                }
            }
            _ => {}
        }
    }

    fn on_right_click(&mut self, hwnd: HWND, lparam: LPARAM) {
        let (h, b) = self.hit(lparam);
        let mut items: Vec<(u32, &str)> = Vec::new();
        if let (BarHit::Button(_), Some(_)) = (h, &b) {
            items.push((CMD_CLOSE_WINDOW, "Close window"));
        } else {
            items.push((CMD_OPEN_LAUNCHER, "Open launcher"));
            items.push((CMD_TASK_MANAGER, "Task Manager"));
            items.push((0, ""));
            items.push((CMD_RESTART_TASKBAR, "Restart taskbar"));
            match self.shared.mode {
                RunMode::Dev => items.push((CMD_QUIT, "Quit explorer.rs")),
                RunMode::Shell => items.push((CMD_SIGN_OUT, "Sign out")),
            }
        }
        let cmd = popup_menu(hwnd, &items);
        match cmd {
            CMD_CLOSE_WINDOW => {
                if let Some(b) = b {
                    explorer_win::activate::close(u64_to_hwnd(b.hwnd));
                }
            }
            CMD_OPEN_LAUNCHER => {
                explorer_win::ipc::send_launcher_command(LauncherCommand::Show);
            }
            CMD_TASK_MANAGER => {
                if let Err(e) = explorer_win::apps::shell_execute("taskmgr.exe", false) {
                    tracing::warn!(error = %e, "cannot start Task Manager");
                }
            }
            CMD_RESTART_TASKBAR => window::quit(EXIT_RESTART),
            CMD_QUIT => window::quit(EXIT_SHUTDOWN_ALL),
            CMD_SIGN_OUT => {
                // SAFETY: plain FFI call.
                if let Err(e) = unsafe { ExitWindowsEx(EWX_LOGOFF, SHUTDOWN_REASON(0)) } {
                    tracing::error!(error = %e, "ExitWindowsEx failed");
                }
            }
            _ => {}
        }
    }
}

fn popup_menu(owner: HWND, items: &[(u32, &str)]) -> u32 {
    // SAFETY: menu created and destroyed here; strings outlive the calls.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return 0;
        };
        for (id, text) in items {
            if *id == 0 {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            } else {
                let w = wide(text);
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    *id as usize,
                    explorer_win::windows::core::PCWSTR(w.as_ptr()),
                );
            }
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let r = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            None,
            owner,
            None,
        );
        let _ = DestroyMenu(menu);
        r.0 as u32
    }
}

impl Handler for Bar {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_CREATE => {
                self.appbar = Some(AppBar::new(hwnd, WM_APP_APPBAR, self.shared.mode));
                // Position outside WM_CREATE to avoid re-entrancy.
                // SAFETY: posting to our own window.
                let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_POSITION, WPARAM(0), LPARAM(0)) };
                Some(LRESULT(0))
            }
            WM_APP_POSITION => {
                self.position(hwnd);
                Some(LRESULT(0))
            }
            WM_APP_APPBAR => {
                match wparam.0 {
                    ABN_POSCHANGED => self.position(hwnd),
                    ABN_FULLSCREENAPP => {
                        // Step out of the way of full-screen apps (games,
                        // videos, presentations) and come back afterwards.
                        let fullscreen = lparam.0 != 0;
                        let z = if fullscreen {
                            HWND_NOTOPMOST
                        } else {
                            HWND_TOPMOST
                        };
                        // SAFETY: z-order change on our own window.
                        let _ = unsafe {
                            SetWindowPos(
                                hwnd,
                                Some(z),
                                0,
                                0,
                                0,
                                0,
                                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                            )
                        };
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.position(hwnd);
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                if let Some(ab) = &self.appbar {
                    ab.on_activate();
                }
                None
            }
            WM_WINDOWPOSCHANGED => {
                if let Some(ab) = &self.appbar {
                    ab.on_window_pos_changed();
                }
                None
            }
            WM_SIZE => {
                if let Some(s) = self.surface.as_mut() {
                    s.resize();
                }
                None
            }
            WM_PAINT => {
                self.paint(hwnd);
                Some(LRESULT(0))
            }
            WM_MOUSEACTIVATE => {
                // Clicking the taskbar must not steal activation from the
                // window the user is about to minimise.
                Some(LRESULT(MA_NOACTIVATE as isize))
            }
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: valid struct.
                    self.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
                }
                let (h, _) = self.hit(lparam);
                if h != self.hover {
                    self.hover = h;
                    // SAFETY: repaint request on our own window.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.hover != BarHit::Empty {
                    self.hover = BarHit::Empty;
                    // SAFETY: repaint request on our own window.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.on_click(lparam);
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                self.on_right_click(hwnd, lparam);
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                if let Some(mut ab) = self.appbar.take() {
                    ab.remove();
                }
                None
            }
            _ => None,
        }
    }
}
