// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows implementation of the taskbar.
//!
//! Structure (all on one UI thread):
//! - a hidden **controller** window owns the [`TaskbarModel`], receives
//!   shell-hook / WinEvent notifications, the Win-key signal, timers,
//!   theme and display changes, and owns the bar windows and the tray host;
//! - one **bar** window per monitor: an AppBar that paints the model and
//!   handles mouse input. The tray is shown on the primary monitor's bar.
//!
//! Shared state lives in an `Rc<Shared>` with `RefCell`s that are only
//! borrowed for short, non-reentrant sections.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use explorer_core::clock;
use explorer_core::config::{Alignment, Config, Edge, MonitorSelection};
use explorer_core::image::SharedImage;
use explorer_core::ipc::{LauncherCommand, RunMode, TASKBAR_WINDOW_CLASS};
use explorer_core::layout::{bar_layout, BarHit, BarLayout, BarSpec, Rect};
use explorer_core::paths::AppPaths;
use explorer_core::supervisor::{EXIT_RESTART, EXIT_SHUTDOWN_ALL};
use explorer_core::taskbar_model::{Button, ClickAction, TaskbarModel, WindowEvent};
use explorer_core::theme::Palette;
use explorer_core::tray::{IconKey, TrayButton};
use explorer_core::window_filter::FilterRules;
use explorer_win::appbar::{AppBar, ABN_FULLSCREENAPP, ABN_POSCHANGED};
use explorer_win::gfx::{Align, Color, Gfx, Surface};
use explorer_win::keyboard::{WinKeyHook, WM_APP_WINKEY};
use explorer_win::monitors::{self, Monitor};
use explorer_win::shellhook::{self, events as ev, ShellHooks, WM_APP_WINEVENT};
use explorer_win::tooltip::Tooltip;
use explorer_win::tray::{TrayHost, WM_APP_TRAY_CHANGED};
use explorer_win::util::{hwnd_to_u64, u64_to_hwnd, wide, ComApartment};
use explorer_win::window::{self, CreateParams, Handler};
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use explorer_win::windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, InvalidateRect, MonitorFromWindow,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use explorer_win::windows::Win32::System::Shutdown::{ExitWindowsEx, EWX_LOGOFF, SHUTDOWN_REASON};
use explorer_win::windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, DestroyWindow, GetClientRect, GetCursorPos,
    KillTimer, PostMessageW, SetTimer, SetWindowPos, TrackPopupMenu, HWND_NOTOPMOST, HWND_TOPMOST,
    MA_NOACTIVATE, MF_SEPARATOR, MF_STRING, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_ACTIVATE, WM_APP, WM_CREATE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_SIZE, WM_TIMER, WM_WINDOWPOSCHANGED,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
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

/// Window icons are fetched once at this size and scaled when drawn.
const WINDOW_ICON_PX: u32 = 48;

struct Shared {
    gfx: Gfx,
    config: Config,
    mode: RunMode,
    model: RefCell<TaskbarModel>,
    clock_text: RefCell<(String, String, String)>,
    bars: RefCell<Vec<HWND>>,
    palette: Cell<Palette>,
    centered: Cell<bool>,
    window_icons: RefCell<HashMap<u64, Option<SharedImage>>>,
    tray: RefCell<Option<TrayHost>>,
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
            clock::format("%a %d %b %Y", &now),
        );
        let mut cur = self.clock_text.borrow_mut();
        if *cur != next {
            *cur = next;
            true
        } else {
            false
        }
    }

    /// Re-read theme and alignment. Returns true if anything changed.
    fn reload_appearance(&self) -> bool {
        let pal = Palette::from_system(
            &explorer_win::theme::system_theme(),
            self.config.general.theme.to_theme_mode(),
        );
        let centered = match self.config.taskbar.alignment {
            Alignment::System => explorer_win::theme::taskbar_centered(),
            Alignment::Center => true,
            Alignment::Left => false,
        };
        let changed = pal != self.palette.get() || centered != self.centered.get();
        self.palette.set(pal);
        self.centered.set(centered);
        changed
    }

    /// Fetch icons for buttons that have none yet; drop icons of windows
    /// that are gone.
    fn refresh_window_icons(&self) {
        let hwnds: Vec<u64> = self
            .model
            .borrow()
            .buttons()
            .iter()
            .map(|b| b.hwnd)
            .collect();
        let mut icons = self.window_icons.borrow_mut();
        icons.retain(|h, _| hwnds.contains(h));
        for h in hwnds {
            if let std::collections::hash_map::Entry::Vacant(e) = icons.entry(h) {
                e.insert(explorer_win::icons::for_window(
                    u64_to_hwnd(h),
                    WINDOW_ICON_PX,
                ));
            }
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
        clock_text: RefCell::new(Default::default()),
        bars: RefCell::new(Vec::new()),
        palette: Cell::new(Palette::from_system(
            &Default::default(),
            explorer_core::theme::Mode::Dark,
        )),
        centered: Cell::new(true),
        window_icons: RefCell::new(HashMap::new()),
        tray: RefCell::new(None),
    });
    shared.reload_appearance();

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
            tray_started: None,
            ticks: 0,
        }),
    ) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "cannot create controller window");
            return 1;
        }
    };
    // SAFETY: posting to our own window.
    let _ = unsafe { PostMessageW(Some(controller), WM_APP_INIT, WPARAM(0), LPARAM(0)) };

    let code = window::run_message_loop(|_| false);
    // Drop the tray (and its window) before COM goes away.
    shared.tray.borrow_mut().take();
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
    tray_started: Option<Instant>,
    ticks: u64,
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
                    primary: m.primary,
                    monitor: m,
                    appbar: None,
                    surface: None,
                    layout: None,
                    hover: BarHit::Empty,
                    tracking: false,
                    tooltip: None,
                }),
            );
            match res {
                Ok(h) => self.shared.bars.borrow_mut().push(h),
                Err(e) => tracing::error!(error = %e, "cannot create bar window"),
            }
        }
    }

    fn changed(&mut self) {
        self.shared.refresh_window_icons();
        self.shared.invalidate_bars();
    }

    fn resync(&mut self) {
        let snaps = explorer_win::winlist::snapshot_all();
        if self.shared.model.borrow_mut().reset(snaps) {
            self.changed();
        }
    }

    fn apply(&mut self, e: WindowEvent) {
        if self.shared.model.borrow_mut().apply(e) {
            self.changed();
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

    fn start_tray(&mut self, hwnd: HWND) {
        let cfg = &self.shared.config.tray;
        if !cfg.enabled {
            return;
        }
        let host = TrayHost::start(self.shared.mode, cfg.intercept_in_dev_mode, hwnd);
        if host.is_some() {
            self.tray_started = Some(Instant::now());
        }
        *self.shared.tray.borrow_mut() = host;
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
            // Our own tray broadcasts TaskbarCreated at start-up; ignore the
            // echo. Otherwise explorer restarted (dev mode): AppBar
            // registrations are lost, so recreate the bars.
            if self
                .tray_started
                .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
            {
                return Some(LRESULT(0));
            }
            tracing::info!("TaskbarCreated received; rebuilding bars");
            self.rebuild_bars();
            if let Some(t) = self.shared.tray.borrow().as_ref() {
                t.ensure_front();
            }
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
                self.start_tray(hwnd);
                self.resync();
                self.shared.refresh_window_icons();
                self.rebuild_bars();
                // SAFETY: timers on our own window.
                unsafe {
                    SetTimer(Some(hwnd), TIMER_CLOCK, 1000, None);
                    SetTimer(Some(hwnd), TIMER_RESYNC, RESYNC_MS, None);
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
            WM_APP_TRAY_CHANGED => {
                self.shared.invalidate_bars();
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE => {
                // Theme ("ImmersiveColorSet") or taskbar alignment changed.
                // Cheap registry reads; no repositioning, so our own
                // SPI_SETWORKAREA notifications cannot loop.
                if self.shared.reload_appearance() {
                    tracing::info!("appearance changed");
                    self.shared.invalidate_bars();
                }
                None
            }
            WM_TIMER => {
                match wparam.0 {
                    TIMER_CLOCK => {
                        self.ticks += 1;
                        if self.shared.update_clock() {
                            self.shared.invalidate_bars();
                        }
                        let pruned = match self.shared.tray.borrow().as_ref() {
                            Some(t) => {
                                t.ensure_front();
                                self.ticks % 5 == 0 && t.prune()
                            }
                            None => false,
                        };
                        if pruned {
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
    primary: bool,
    appbar: Option<AppBar>,
    surface: Option<Surface>,
    layout: Option<Snapshot>,
    hover: BarHit,
    tracking: bool,
    tooltip: Option<Tooltip>,
}

/// What was painted, for hit-testing and tooltips.
struct Snapshot {
    layout: BarLayout,
    buttons: Vec<Button>,
    tray: Vec<(IconKey, String)>,
}

const CMD_CLOSE_WINDOW: u32 = 1;
const CMD_TASK_MANAGER: u32 = 10;
const CMD_RESTART_TASKBAR: u32 = 11;
const CMD_QUIT: u32 = 12;
const CMD_SIGN_OUT: u32 = 13;
const CMD_OPEN_LAUNCHER: u32 = 14;
const CMD_TASKBAR_SETTINGS: u32 = 15;

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

    fn tray_items(&self) -> Vec<(IconKey, String)> {
        if !self.primary {
            return Vec::new();
        }
        let tray = self.shared.tray.borrow();
        let Some(t) = tray.as_ref() else {
            return Vec::new();
        };
        let st = t.state.borrow();
        st.model.visible().map(|i| (i.key, i.tip.clone())).collect()
    }

    fn compute(&self, hwnd: HWND) -> Snapshot {
        let mut rc = RECT::default();
        // SAFETY: valid out-pointer.
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let buttons = self.visible_buttons();
        let tray = self.tray_items();
        let t = &self.shared.config.taskbar;
        let layout = bar_layout(&BarSpec {
            width: (rc.right - rc.left) as f32,
            height: (rc.bottom - rc.top) as f32,
            scale: self.scale(),
            n_buttons: buttons.len(),
            button_width: t.button_width as f32,
            labels: t.button_labels,
            centered: self.shared.centered.get(),
            show_clock: t.show_clock,
            tray_icons: tray.len(),
        });
        Snapshot {
            layout,
            buttons,
            tray,
        }
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
        let snap = self.compute(hwnd);
        let s = self.scale();
        let pal = self.shared.palette.get();
        let active = self.shared.model.borrow().active();
        let clock_text = self.shared.clock_text.borrow().clone();
        let hover = self.hover;
        let labels = self.shared.config.taskbar.button_labels;
        let icons: Vec<Option<SharedImage>> = {
            let cache = self.shared.window_icons.borrow();
            snap.buttons
                .iter()
                .map(|b| cache.get(&b.hwnd).cloned().flatten())
                .collect()
        };
        let tray_images: Vec<Option<SharedImage>> = {
            let tray = self.shared.tray.borrow();
            match tray.as_ref() {
                Some(t) => {
                    let st = t.state.borrow();
                    snap.tray
                        .iter()
                        .map(|(k, _)| st.images.get(k).cloned())
                        .collect()
                }
                None => Vec::new(),
            }
        };
        let l = &snap.layout;
        let full_width = {
            let mut rc = RECT::default();
            // SAFETY: valid out-pointer.
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            (rc.right - rc.left) as f32
        };
        let surface = self.surface.get_or_insert_with(|| Surface::new(hwnd));
        surface.draw(&self.shared.gfx, Color(pal.bar_bg), |p| {
            let inset =
                |r: Rect| Rect::new(r.x + 2.0 * s, r.y + 4.0 * s, r.w - 4.0 * s, r.h - 8.0 * s);
            let radius = 4.0 * s;
            // Hairline separating the bar from the desktop.
            p.fill(Rect::new(0.0, 0.0, full_width, 1.0), Color(pal.border));

            // Start button: four rounded tiles in the accent colour.
            if hover == BarHit::Start {
                p.fill_rounded(inset(l.start), radius, Color(pal.button_hover));
            }
            let tile = 9.0 * s;
            let gap = 1.5 * s;
            let total = tile * 2.0 + gap;
            let ox = l.start.x + (l.start.w - total) / 2.0;
            let oy = l.start.y + (l.start.h - total) / 2.0;
            for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                p.fill_rounded(
                    Rect::new(ox + dx * (tile + gap), oy + dy * (tile + gap), tile, tile),
                    1.5 * s,
                    Color(pal.accent_text),
                );
            }

            // Window buttons.
            for (i, (r, b)) in l.buttons.iter().zip(snap.buttons.iter()).enumerate() {
                let is_active = active == Some(b.hwnd);
                let ir = inset(*r);
                let bg = if b.flashing {
                    Some(pal.flash)
                } else if is_active {
                    Some(pal.button_active)
                } else if hover == BarHit::Button(i) {
                    Some(pal.button_hover)
                } else {
                    None
                };
                if let Some(bg) = bg {
                    p.fill_rounded(ir, radius, Color(bg));
                }
                let icon_size = 24.0 * s;
                let icon = icons.get(i).cloned().flatten();
                if labels {
                    let icon_r = Rect::new(
                        ir.x + 8.0 * s,
                        ir.y + (ir.h - icon_size) / 2.0,
                        icon_size,
                        icon_size,
                    );
                    if let Some(img) = &icon {
                        p.image(img, icon_r);
                    }
                    let tx = icon_r.right() + 8.0 * s;
                    p.text(
                        &b.title,
                        Rect::new(tx, ir.y, (ir.right() - tx - 6.0 * s).max(0.0), ir.h),
                        12.0 * s,
                        Color(pal.text),
                        Align::Left,
                        false,
                    );
                } else {
                    let icon_r = Rect::new(
                        ir.x + (ir.w - icon_size) / 2.0,
                        ir.y + (ir.h - icon_size) / 2.0 - s,
                        icon_size,
                        icon_size,
                    );
                    match &icon {
                        Some(img) => p.image(img, icon_r),
                        None => {
                            // No icon: first letter of the title.
                            let letter: String = b.title.chars().take(1).collect();
                            p.text(
                                &letter,
                                icon_r,
                                16.0 * s,
                                Color(pal.text),
                                Align::Center,
                                true,
                            );
                        }
                    }
                }
                // Running indicator: short and dim; long and accent if active.
                let (w, c) = if is_active {
                    (16.0 * s, pal.accent_text)
                } else {
                    (6.0 * s, pal.running_indicator)
                };
                p.fill_rounded(
                    Rect::new(ir.x + (ir.w - w) / 2.0, ir.bottom() - 3.0 * s, w, 3.0 * s),
                    1.5 * s,
                    Color(c),
                );
            }
            if l.overflow > 0 {
                // TODO(taskbar): overflow menu. For now show the count.
                if let Some(last) = l.buttons.last() {
                    let r = Rect::new(last.right(), 0.0, 28.0 * s, l.start.h);
                    p.text(
                        &format!("+{}", l.overflow),
                        r,
                        11.0 * s,
                        Color(pal.text_dim),
                        Align::Center,
                        false,
                    );
                }
            }

            // Tray.
            for (i, r) in l.tray.iter().enumerate() {
                if hover == BarHit::Tray(i) {
                    p.fill_rounded(inset(*r), radius, Color(pal.button_hover));
                }
                if let Some(Some(img)) = tray_images.get(i) {
                    let sz = 16.0 * s;
                    p.image(
                        img,
                        Rect::new(r.x + (r.w - sz) / 2.0, r.y + (r.h - sz) / 2.0, sz, sz),
                    );
                }
            }

            // Clock.
            if let Some(c) = l.clock {
                if hover == BarHit::Clock {
                    p.fill_rounded(inset(c), radius, Color(pal.button_hover));
                }
                if clock_text.1.is_empty() {
                    p.text(
                        &clock_text.0,
                        c,
                        12.0 * s,
                        Color(pal.text),
                        Align::Center,
                        false,
                    );
                } else {
                    let half = c.h / 2.0;
                    p.text(
                        &clock_text.0,
                        Rect::new(c.x, c.y + 4.0 * s, c.w, half - 4.0 * s),
                        12.0 * s,
                        Color(pal.text),
                        Align::Center,
                        false,
                    );
                    p.text(
                        &clock_text.1,
                        Rect::new(c.x, c.y + half, c.w, half - 4.0 * s),
                        12.0 * s,
                        Color(pal.text),
                        Align::Center,
                        false,
                    );
                }
            }
        });
        self.layout = Some(snap);
        // SAFETY: matches BeginPaint above.
        let _ = unsafe { EndPaint(hwnd, &ps) };
    }

    fn hit(&self, lparam: LPARAM) -> BarHit {
        let x = (lparam.0 & 0xffff) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        match &self.layout {
            Some(s) => s.layout.hit(x, y),
            None => BarHit::Empty,
        }
    }

    fn button(&self, i: usize) -> Option<Button> {
        self.layout.as_ref().and_then(|s| s.buttons.get(i).cloned())
    }

    fn tray_key(&self, i: usize) -> Option<IconKey> {
        self.layout
            .as_ref()
            .and_then(|s| s.tray.get(i).map(|t| t.0))
    }

    fn update_tooltip(&mut self, hwnd: HWND) {
        let (text, rect) = match (self.hover, &self.layout) {
            (BarHit::Start, Some(s)) => (Some("Start".to_string()), Some(s.layout.start)),
            (BarHit::Button(i), Some(s)) => (
                s.buttons.get(i).map(|b| b.title.clone()),
                s.layout.buttons.get(i).copied(),
            ),
            (BarHit::Tray(i), Some(s)) => (
                s.tray.get(i).map(|t| t.1.clone()),
                s.layout.tray.get(i).copied(),
            ),
            (BarHit::Clock, Some(s)) => (
                Some(self.shared.clock_text.borrow().2.clone()),
                s.layout.clock,
            ),
            _ => (None, None),
        };
        match (text.filter(|t| !t.is_empty()), rect) {
            (Some(text), Some(r)) => {
                if self.tooltip.is_none() {
                    self.tooltip = Tooltip::new(hwnd);
                }
                let mut pt = POINT {
                    x: r.x as i32,
                    y: 0,
                };
                // SAFETY: valid out-pointer.
                let _ = unsafe { ClientToScreen(hwnd, &mut pt) };
                let s = self.scale();
                let y = match self.shared.config.taskbar.position {
                    Edge::Bottom => pt.y - (34.0 * s) as i32,
                    Edge::Top => pt.y + r.h as i32 + (6.0 * s) as i32,
                };
                if let Some(t) = self.tooltip.as_mut() {
                    t.show(&text, pt.x, y);
                }
            }
            _ => {
                if let Some(t) = self.tooltip.as_mut() {
                    t.hide();
                }
            }
        }
    }

    fn on_click(&mut self, lparam: LPARAM, double: bool) {
        match self.hit(lparam) {
            BarHit::Start if !double => {
                explorer_win::ipc::send_launcher_command(LauncherCommand::Toggle);
            }
            BarHit::Button(i) if !double => {
                if let Some(b) = self.button(i) {
                    let target = u64_to_hwnd(b.hwnd);
                    let minimized = explorer_win::winlist::is_minimized(target);
                    let action = self.shared.model.borrow().click_action(b.hwnd, minimized);
                    match action {
                        ClickAction::Activate => explorer_win::activate::activate(target),
                        ClickAction::Minimize => explorer_win::activate::minimize(target),
                    }
                }
            }
            BarHit::Tray(i) => {
                let button = if double {
                    TrayButton::LeftDouble
                } else {
                    TrayButton::Left
                };
                self.click_tray(i, button);
            }
            _ => {}
        }
    }

    fn click_tray(&self, i: usize, button: TrayButton) {
        let Some(key) = self.tray_key(i) else {
            return;
        };
        let mut pt = POINT::default();
        // SAFETY: valid out-pointer.
        let _ = unsafe { GetCursorPos(&mut pt) };
        if let Some(t) = self.shared.tray.borrow().as_ref() {
            t.click(key, button, pt.x, pt.y);
        }
    }

    fn on_right_click(&mut self, hwnd: HWND, lparam: LPARAM) {
        let hit = self.hit(lparam);
        if let BarHit::Tray(i) = hit {
            self.click_tray(i, TrayButton::Right);
            return;
        }
        if let Some(t) = self.tooltip.as_mut() {
            t.hide();
        }
        let b = match hit {
            BarHit::Button(i) => self.button(i),
            _ => None,
        };
        let mut items: Vec<(u32, &str)> = Vec::new();
        if b.is_some() {
            items.push((CMD_CLOSE_WINDOW, "Close window"));
        } else {
            items.push((CMD_OPEN_LAUNCHER, "Open Start"));
            items.push((CMD_TASK_MANAGER, "Task Manager"));
            items.push((CMD_TASKBAR_SETTINGS, "Taskbar settings"));
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
            CMD_TASKBAR_SETTINGS => {
                if let Err(e) = explorer_win::apps::shell_execute("ms-settings:taskbar", false) {
                    tracing::warn!(error = %e, "cannot open taskbar settings");
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
        let texts: Vec<Vec<u16>> = items.iter().map(|(_, t)| wide(t)).collect();
        for ((id, _), w) in items.iter().zip(texts.iter()) {
            if *id == 0 {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            } else {
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
                let h = self.hit(lparam);
                if h != self.hover {
                    self.hover = h;
                    self.update_tooltip(hwnd);
                    // SAFETY: repaint request on our own window.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.hover != BarHit::Empty {
                    self.hover = BarHit::Empty;
                    self.update_tooltip(hwnd);
                    // SAFETY: repaint request on our own window.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.on_click(lparam, false);
                Some(LRESULT(0))
            }
            WM_LBUTTONDBLCLK => {
                self.on_click(lparam, true);
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                self.on_right_click(hwnd, lparam);
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                self.tooltip = None;
                if let Some(mut ab) = self.appbar.take() {
                    ab.remove();
                }
                None
            }
            _ => None,
        }
    }
}
