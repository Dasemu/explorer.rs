// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows implementation of the taskbar.
//!
//! Structure (all windows on one UI thread):
//! - a hidden **controller** window owns the [`TaskbarModel`], receives
//!   shell-hook / WinEvent notifications, the Win-key signal, timers,
//!   theme, power and display changes, worker results, and owns the bar
//!   windows and the tray host;
//! - one **bar** window per monitor ([`crate::bar`]): an AppBar that paints
//!   pinned and running apps (grouped per app), the tray, the system glyphs
//!   and the clock, and handles mouse input. The tray is shown on the
//!   primary monitor's bar;
//! - one **flyout** window ([`crate::flyout`]) reused for every popup:
//!   window picker / hover thumbnails, "…" overflow, tray overflow, Quick
//!   settings and the calendar;
//! - worker threads ([`crate::workers`]) for icons, the app catalog and the
//!   system status.
//!
//! Shared state lives in an `Rc<Shared>` with `RefCell`s that are only
//! borrowed for short, non-reentrant sections (never across calls that can
//! dispatch messages to our own windows).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use explorer_core::calendar;
use explorer_core::catalog::{Catalog, LaunchTarget};
use explorer_core::clock::{self, DateTime};
use explorer_core::config::{Alignment, Config, MonitorSelection};
use explorer_core::i18n::Lang;
use explorer_core::image::SharedImage;
use explorer_core::ipc::{LauncherCommand, RunMode, TASKBAR_WINDOW_CLASS};
use explorer_core::paths::AppPaths;
use explorer_core::shellpath::{file_stem, looks_like_path, same_path, KnownFolders};
use explorer_core::start::{Pins, DEFAULT_TASKBAR_PIN_IDS};
use explorer_core::taskbar_groups::{
    build_buttons, AppButton, PinnedApp, ShowDesktop, ShowDesktopAction, WindowIdent,
};
use explorer_core::taskbar_model::{TaskbarModel, WindowEvent};
use explorer_core::theme::Palette;
use explorer_core::tray_promotion::NotifySetting;
use explorer_core::window_filter::FilterRules;
use explorer_win::gfx::Gfx;
use explorer_win::keyboard::{WinKeyHook, WM_APP_WINKEY};
use explorer_win::monitors;
use explorer_win::shellhook::{self, events as ev, ShellHooks, WM_APP_WINEVENT};
use explorer_win::tray::{TrayHost, WM_APP_TRAY_CHANGED};
use explorer_win::util::{hwnd_to_u64, u64_to_hwnd, ComApartment};
use explorer_win::window::{self, CreateParams, Handler};
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use explorer_win::windows::Win32::Graphics::Gdi::InvalidateRect;
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, KillTimer, PostMessageW, SetTimer, ShowWindowAsync, SW_MINIMIZE, SW_RESTORE,
    WM_APP, WM_DESTROY, WM_DISPLAYCHANGE, WM_POWERBROADCAST, WM_SETTINGCHANGE, WM_TIMER,
    WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::bar::Bar;
use crate::flyout;
use crate::workers::{
    CatalogApp, ShellDone, ShellJob, ShellWorker, SysCmd, SysStatus, SysWorker, WM_APP_SHELL_DONE,
    WM_APP_SYS_CHANGED,
};
use crate::Args;

const CONTROLLER_CLASS: &str = "ExplorerRs.TaskbarController";

const WM_APP_INIT: u32 = WM_APP + 1;

const TIMER_CLOCK: usize = 1;
const TIMER_RESYNC: usize = 2;
const TIMER_REBUILD: usize = 3;
/// Hover-thumbnail ("peek") leave detection while it is visible.
pub(crate) const TIMER_PEEK: usize = 4;

/// Full re-enumeration interval: a safety net for missed notifications.
const RESYNC_MS: u32 = 30_000;
/// Re-read a window's AppUserModelID at most this often (apps such as
/// Chrome set it shortly after creating the window).
const IDENT_REFRESH: Duration = Duration::from_secs(2);
/// Re-enumerate shell:AppsFolder (new installs) this often.
const CATALOG_REFRESH: Duration = Duration::from_secs(15 * 60);
/// After a "Show desktop" minimize, activations within this window are
/// part of the minimize sequence, not the user restoring a window.
const SHOW_DESKTOP_SETTLE: Duration = Duration::from_millis(1500);

/// Identity of one window (see `explorer_core::taskbar_groups`).
pub(crate) struct WinIdent {
    pub app_id: Option<String>,
    pub exe: Option<String>,
    pub app_name: Option<String>,
    checked: Instant,
}

/// The taskbar pins file, re-read when its modification time changes (the
/// Start menu writes it too).
pub(crate) struct PinStore {
    pub pins: Pins,
    path: PathBuf,
    mtime: Option<SystemTime>,
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl PinStore {
    fn load(path: PathBuf) -> Self {
        Self {
            pins: Pins::load(&path),
            mtime: mtime(&path),
            path,
        }
    }

    /// Returns true if the file changed and was re-read.
    pub fn reload_if_changed(&mut self) -> bool {
        let m = mtime(&self.path);
        if m != self.mtime {
            self.pins = Pins::load(&self.path);
            self.mtime = m;
            true
        } else {
            false
        }
    }

    pub fn save(&mut self) {
        if let Err(e) = self.pins.save(&self.path) {
            tracing::warn!(error = %e, path = %self.path.display(), "cannot save taskbar pins");
        }
        self.mtime = mtime(&self.path);
    }
}

pub(crate) struct Shared {
    pub gfx: Gfx,
    pub config: Config,
    pub mode: RunMode,
    pub lang: Lang,
    pub folders: KnownFolders,
    pub model: RefCell<TaskbarModel>,
    pub idents: RefCell<HashMap<u64, WinIdent>>,
    /// (time, date) lines of the clock.
    pub clock_text: RefCell<(String, String)>,
    pub now: Cell<Option<DateTime>>,
    pub bars: RefCell<Vec<HWND>>,
    pub palette: Cell<Palette>,
    pub centered: Cell<bool>,
    pub window_icons: RefCell<HashMap<u64, Option<SharedImage>>>,
    /// Icons of pinned apps by pin key.
    pub app_icons: RefCell<HashMap<String, Option<SharedImage>>>,
    pub tray: RefCell<Option<TrayHost>>,
    pub tray_settings: RefCell<Vec<NotifySetting>>,
    /// Tray owner window -> process image path.
    pub tray_exe: RefCell<HashMap<u32, Option<String>>>,
    pub pins: RefCell<PinStore>,
    /// Pins resolved against the catalog, in display order.
    pub pinned: RefCell<Vec<PinnedApp>>,
    pub catalog: RefCell<Option<Vec<CatalogApp>>>,
    catalog_at: Cell<Option<Instant>>,
    /// `.lnk` path (lower case) -> target executable.
    links: RefCell<HashMap<String, Option<String>>>,
    pub shell: RefCell<Option<ShellWorker>>,
    pub sys: RefCell<Option<SysWorker>>,
    pub sys_status: RefCell<SysStatus>,
    pub flyout: RefCell<Option<flyout::State>>,
    pub flyout_hwnd: Cell<HWND>,
    pub flyout_closed: RefCell<Option<(flyout::Opener, Instant)>>,
    show_desktop: RefCell<ShowDesktop>,
    show_desktop_at: Cell<Option<Instant>>,
    pub controller: Cell<HWND>,
}

impl Shared {
    pub fn invalidate_bars(&self) {
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
        let day_changed = self.now.get().is_none_or(|p| p.day != now.day);
        self.now.set(Some(now));
        let mut cur = self.clock_text.borrow_mut();
        if *cur != next || day_changed {
            *cur = next;
            true
        } else {
            false
        }
    }

    /// Full date for the clock tooltip.
    pub fn long_date(&self) -> String {
        self.now
            .get()
            .map(|n| calendar::long_date(self.lang, &n))
            .unwrap_or_default()
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

    fn shell_job(&self, job: ShellJob) {
        if let Some(w) = self.shell.borrow().as_ref() {
            w.send(job);
        }
    }

    pub fn sys_cmd(&self, cmd: SysCmd) {
        if let Some(w) = self.sys.borrow().as_ref() {
            w.send(cmd);
        }
    }

    // -- identities ---------------------------------------------------------

    fn catalog_match(&self, app_id: Option<&str>, exe: Option<&str>) -> Option<CatalogApp> {
        let cat = self.catalog.borrow();
        let cat = cat.as_ref()?;
        if let Some(id) = app_id {
            let l = id.to_lowercase();
            if let Some(c) = cat
                .iter()
                .find(|c| c.app_id.as_deref().is_some_and(|a| a.to_lowercase() == l))
            {
                return Some(c.clone());
            }
        }
        let exe = exe?;
        cat.iter()
            .find(|c| c.exe.as_deref().is_some_and(|e| same_path(e, exe)))
            .cloned()
    }

    fn compute_ident(&self, hwnd: u64) -> WinIdent {
        let h = u64_to_hwnd(hwnd);
        let app_id = explorer_win::icons::window_app_id(h);
        let exe = explorer_win::winlist::process_path(explorer_win::winlist::process_id(h));
        let app_name = self
            .catalog_match(app_id.as_deref(), exe.as_deref())
            .map(|c| c.entry.name);
        WinIdent {
            app_id,
            exe,
            app_name,
            checked: Instant::now(),
        }
    }

    /// Make sure every window in the model has an identity and an icon
    /// request; drop data of windows that are gone. `force` re-reads all
    /// identities (periodic resync).
    fn refresh_idents(&self, force: bool) {
        let hwnds: Vec<u64> = self
            .model
            .borrow()
            .buttons()
            .iter()
            .map(|b| b.hwnd)
            .collect();
        let stale: Vec<u64> = {
            let idents = self.idents.borrow();
            hwnds
                .iter()
                .copied()
                .filter(|h| {
                    idents.get(h).is_none_or(|i| {
                        force || i.checked.elapsed() > IDENT_REFRESH && i.app_id.is_none()
                    })
                })
                .collect()
        };
        let fresh: Vec<(u64, WinIdent)> =
            stale.iter().map(|&h| (h, self.compute_ident(h))).collect();
        {
            let mut idents = self.idents.borrow_mut();
            idents.retain(|h, _| hwnds.contains(h));
            idents.extend(fresh);
        }
        let mut icons = self.window_icons.borrow_mut();
        icons.retain(|h, _| hwnds.contains(h));
        for h in hwnds {
            if let std::collections::hash_map::Entry::Vacant(e) = icons.entry(h) {
                e.insert(None);
                self.shell_job(ShellJob::WindowIcon(h));
            }
        }
    }

    /// Windows (in taskbar order) passing `keep`, with their identities.
    pub fn window_idents(&self, keep: impl Fn(u64) -> bool) -> Vec<WindowIdent> {
        let model = self.model.borrow();
        let idents = self.idents.borrow();
        model
            .buttons()
            .iter()
            .filter(|b| keep(b.hwnd))
            .map(|b| {
                let id = idents.get(&b.hwnd);
                WindowIdent {
                    hwnd: b.hwnd,
                    title: b.title.clone(),
                    app_id: id.and_then(|i| i.app_id.clone()),
                    exe_path: id.and_then(|i| i.exe.clone()),
                    app_name: id.and_then(|i| i.app_name.clone()),
                    flashing: b.flashing,
                }
            })
            .collect()
    }

    /// Buttons for windows passing `keep`.
    pub fn buttons(&self, keep: impl Fn(u64) -> bool) -> Vec<AppButton> {
        let idents = self.window_idents(keep);
        let active = self.model.borrow().active();
        build_buttons(
            &self.pinned.borrow(),
            &idents,
            active,
            self.config.taskbar.combine,
        )
    }

    pub fn window_title(&self, hwnd: u64) -> Option<String> {
        self.model
            .borrow()
            .buttons()
            .iter()
            .find(|b| b.hwnd == hwnd)
            .map(|b| b.title.clone())
    }

    pub fn button_icon(&self, b: &AppButton) -> Option<SharedImage> {
        if let Some(i) = b.pin {
            let key = self.pinned.borrow().get(i).map(|p| p.key.clone());
            if let Some(Some(img)) = key.and_then(|k| self.app_icons.borrow().get(&k).cloned()) {
                return Some(img);
            }
        }
        let icons = self.window_icons.borrow();
        b.windows
            .iter()
            .find_map(|w| icons.get(w).cloned().flatten())
    }

    pub fn window_icon(&self, hwnd: u64) -> Option<SharedImage> {
        self.window_icons.borrow().get(&hwnd).cloned().flatten()
    }

    // -- pins ---------------------------------------------------------------

    /// Resolve the pin keys against the catalog (and resolved shortcuts).
    fn rebuild_pinned(&self) {
        let keys = self.pins.borrow().pins.keys.clone();
        let mut out = Vec::new();
        let mut missing_links = Vec::new();
        {
            let catalog = self.catalog.borrow();
            let links = self.links.borrow();
            for key in keys {
                if let Some(c) = catalog
                    .as_ref()
                    .and_then(|cat| cat.iter().find(|c| c.entry.target.key() == key))
                {
                    out.push(PinnedApp {
                        key,
                        name: c.entry.name.clone(),
                        target: c.entry.target.clone(),
                        app_id: c.app_id.clone(),
                        exe_path: c.exe.clone(),
                    });
                    continue;
                }
                let Some(path) = key.strip_prefix("path:").map(str::to_string) else {
                    // An AppsFolder pin not (or not yet) in the catalog:
                    // shown once the catalog lists it (uninstalled apps
                    // stay hidden, like the Start menu).
                    continue;
                };
                let lower = path.to_ascii_lowercase();
                let exe = if lower.ends_with(".exe") {
                    Some(path.clone())
                } else {
                    match links.get(&lower) {
                        Some(t) => t.clone(),
                        None => {
                            missing_links.push(path.clone());
                            None
                        }
                    }
                };
                let stem = file_stem(&path);
                let mut name: Vec<char> = stem.chars().collect();
                if let Some(c) = name.first_mut() {
                    *c = c.to_uppercase().next().unwrap_or(*c);
                }
                out.push(PinnedApp {
                    key,
                    name: name.into_iter().collect(),
                    target: LaunchTarget::Path(path.into()),
                    app_id: None,
                    exe_path: exe,
                });
            }
        }
        if !missing_links.is_empty() {
            {
                let mut links = self.links.borrow_mut();
                for l in &missing_links {
                    links.insert(l.to_ascii_lowercase(), None);
                }
            }
            self.shell_job(ShellJob::Links(missing_links));
        }
        {
            let mut icons = self.app_icons.borrow_mut();
            for p in &out {
                if !icons.contains_key(&p.key) {
                    icons.insert(p.key.clone(), None);
                    self.shell_job(ShellJob::TargetIcon(p.key.clone(), p.target.clone()));
                }
            }
        }
        *self.pinned.borrow_mut() = out;
    }

    /// Target that starts a new instance of the app behind `b`.
    pub fn launch_target(&self, b: &AppButton) -> Option<LaunchTarget> {
        if let Some(i) = b.pin {
            return self.pinned.borrow().get(i).map(|p| p.target.clone());
        }
        let w = *b.windows.first()?;
        let (app_id, exe) = {
            let idents = self.idents.borrow();
            let id = idents.get(&w)?;
            (id.app_id.clone(), id.exe.clone())
        };
        if let Some(c) = self.catalog_match(app_id.as_deref(), exe.as_deref()) {
            return Some(c.entry.target);
        }
        if let Some(a) = app_id.filter(|a| !looks_like_path(a)) {
            return Some(LaunchTarget::AppsFolder(a));
        }
        exe.map(|e| LaunchTarget::Path(e.into()))
    }

    pub fn launch_new(&self, b: &AppButton) {
        match self.launch_target(b) {
            Some(t) => crate::workers::launch(t),
            None => tracing::warn!(name = %b.name, "no launch target for this app"),
        }
    }

    /// Pin (or unpin) the app behind `b`; persisted immediately.
    pub fn set_pinned(&self, b: &AppButton, pin: bool) {
        let unpin_key = b
            .pin
            .and_then(|i| self.pinned.borrow().get(i).map(|p| p.key.clone()));
        let target = if pin { self.launch_target(b) } else { None };
        {
            let mut store = self.pins.borrow_mut();
            store.reload_if_changed();
            let changed = match (pin, target, unpin_key) {
                (true, Some(t), _) => store.pins.pin(&t),
                (false, _, Some(k)) => {
                    store.pins.initialized = true;
                    let before = store.pins.keys.len();
                    store.pins.keys.retain(|x| x != &k);
                    before != store.pins.keys.len()
                }
                _ => false,
            };
            if changed {
                store.save();
            }
        }
        self.rebuild_pinned();
        self.invalidate_bars();
    }

    /// Persist a new pin order after a drag.
    pub fn reorder_pins(&self, visible: &[String], from: usize, slot: usize) {
        let changed = {
            let mut store = self.pins.borrow_mut();
            store.reload_if_changed();
            let changed = explorer_core::taskbar_groups::reorder_pins(
                &mut store.pins.keys,
                visible,
                from,
                slot,
            );
            if changed {
                store.pins.initialized = true;
                store.save();
            }
            changed
        };
        if changed {
            self.rebuild_pinned();
        }
        self.invalidate_bars();
    }

    // -- show desktop -------------------------------------------------------

    pub fn toggle_show_desktop(&self) {
        let visible: Vec<u64> = {
            let model = self.model.borrow();
            explorer_win::winlist::top_level_windows()
                .into_iter()
                .map(hwnd_to_u64)
                .filter(|h| model.buttons().iter().any(|b| b.hwnd == *h))
                .filter(|&h| !explorer_win::winlist::is_minimized(u64_to_hwnd(h)))
                .collect()
        };
        let action = self.show_desktop.borrow_mut().toggle(&visible);
        self.show_desktop_at.set(Some(Instant::now()));
        match action {
            ShowDesktopAction::Minimize(list) => {
                for h in list {
                    // SAFETY: asynchronous show request; never blocks.
                    let _ = unsafe { ShowWindowAsync(u64_to_hwnd(h), SW_MINIMIZE) };
                }
            }
            ShowDesktopAction::Restore(list) => {
                for &h in &list {
                    // SAFETY: as above.
                    let _ = unsafe { ShowWindowAsync(u64_to_hwnd(h), SW_RESTORE) };
                }
                if let Some(&top) = list.last() {
                    explorer_win::activate::activate(u64_to_hwnd(top));
                }
            }
        }
    }

    // -- tray ---------------------------------------------------------------

    /// Process image path of a tray icon's owner window (cached).
    pub fn tray_owner_exe(&self, hwnd: u32) -> Option<String> {
        if let Some(v) = self.tray_exe.borrow().get(&hwnd) {
            return v.clone();
        }
        let h = u64_to_hwnd(hwnd as i32 as isize as u64);
        let exe = explorer_win::winlist::process_path(explorer_win::winlist::process_id(h));
        self.tray_exe.borrow_mut().insert(hwnd, exe.clone());
        exe
    }

    fn reload_tray_settings(&self) -> bool {
        if self.config.tray.show_all {
            return false;
        }
        let next = explorer_win::appident::notify_settings(&self.folders);
        let mut cur = self.tray_settings.borrow_mut();
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
    let lang = explorer_win::theme::ui_lang(config.general.language);
    let shared = Rc::new(Shared {
        gfx,
        model: RefCell::new(TaskbarModel::new(FilterRules::from_config(&config.taskbar))),
        lang,
        mode,
        folders: explorer_win::appident::known_folders(),
        idents: RefCell::new(HashMap::new()),
        clock_text: RefCell::new(Default::default()),
        now: Cell::new(None),
        bars: RefCell::new(Vec::new()),
        palette: Cell::new(Palette::from_system(
            &Default::default(),
            explorer_core::theme::Mode::Dark,
        )),
        centered: Cell::new(true),
        window_icons: RefCell::new(HashMap::new()),
        app_icons: RefCell::new(HashMap::new()),
        tray: RefCell::new(None),
        tray_settings: RefCell::new(Vec::new()),
        tray_exe: RefCell::new(HashMap::new()),
        pins: RefCell::new(PinStore::load(paths.taskbar_pins_file())),
        pinned: RefCell::new(Vec::new()),
        catalog: RefCell::new(None),
        catalog_at: Cell::new(None),
        links: RefCell::new(HashMap::new()),
        shell: RefCell::new(None),
        sys: RefCell::new(None),
        sys_status: RefCell::new(SysStatus::default()),
        flyout: RefCell::new(None),
        flyout_hwnd: Cell::new(HWND::default()),
        flyout_closed: RefCell::new(None),
        show_desktop: RefCell::new(ShowDesktop::default()),
        show_desktop_at: Cell::new(None),
        controller: Cell::new(HWND::default()),
        config,
    });
    shared.reload_appearance();

    window::register_class(CONTROLLER_CLASS, None);
    window::register_class(TASKBAR_WINDOW_CLASS, None);
    flyout::register_class();

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
    shared.controller.set(controller);
    // SAFETY: posting to our own window.
    let _ = unsafe { PostMessageW(Some(controller), WM_APP_INIT, WPARAM(0), LPARAM(0)) };

    let code = window::run_message_loop(|_| false);
    // Release thumbnails and the tray (and its window) before COM goes away.
    shared.flyout.borrow_mut().take();
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
        flyout::close(&self.shared);
        let old: Vec<HWND> = self.shared.bars.borrow_mut().drain(..).collect();
        for b in old {
            // SAFETY: destroying our own windows (from the controller, never
            // from inside the bar's own handler).
            let _ = unsafe { DestroyWindow(b) };
        }
        let mut mons = monitors::all();
        if self.shared.config.taskbar.monitors == MonitorSelection::Primary {
            mons.retain(|m| m.primary);
        }
        for m in mons {
            tracing::info!(monitor = %m.describe(), "creating bar");
            match Bar::create(self.shared.clone(), m) {
                Ok(h) => self.shared.bars.borrow_mut().push(h),
                Err(e) => tracing::error!(error = %e, "cannot create bar window"),
            }
        }
    }

    fn changed(&mut self) {
        self.shared.refresh_idents(false);
        self.shared.invalidate_bars();
        flyout::refresh(&self.shared);
    }

    fn resync(&mut self) {
        let snaps = explorer_win::winlist::snapshot_all();
        let changed = self.shared.model.borrow_mut().reset(snaps);
        self.shared.refresh_idents(true);
        if changed {
            self.changed();
        } else {
            self.shared.invalidate_bars();
        }
    }

    fn apply(&mut self, e: WindowEvent) {
        match &e {
            WindowEvent::Activated(h) => {
                let settled = self
                    .shared
                    .show_desktop_at
                    .get()
                    .is_none_or(|t| t.elapsed() > SHOW_DESKTOP_SETTLE);
                let is_button = self
                    .shared
                    .model
                    .borrow()
                    .buttons()
                    .iter()
                    .any(|b| b.hwnd == *h);
                if settled && is_button && self.shared.show_desktop.borrow().is_showing() {
                    self.shared.show_desktop.borrow_mut().reset();
                }
            }
            WindowEvent::Destroyed(h) => self.shared.show_desktop.borrow_mut().forget(*h),
            _ => {}
        }
        let changed = self.shared.model.borrow_mut().apply(e);
        if changed {
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

    fn request_catalog(&mut self) {
        self.shared.catalog_at.set(Some(Instant::now()));
        self.shared.shell_job(ShellJob::Catalog);
    }

    fn on_shell_done(&mut self) {
        let done: Vec<ShellDone> = match self.shared.shell.borrow().as_ref() {
            Some(w) => w.rx.try_iter().collect(),
            None => Vec::new(),
        };
        let mut repaint = false;
        let mut pins_changed = false;
        for d in done {
            match d {
                ShellDone::WindowIcon(h, img) => {
                    let mut icons = self.shared.window_icons.borrow_mut();
                    if let Some(slot) = icons.get_mut(&h) {
                        *slot = img;
                        repaint = true;
                    }
                }
                ShellDone::TargetIcon(k, img) => {
                    self.shared.app_icons.borrow_mut().insert(k, img);
                    repaint = true;
                }
                ShellDone::Links(v) => {
                    let mut links = self.shared.links.borrow_mut();
                    for (p, t) in v {
                        links.insert(p.to_ascii_lowercase(), t);
                    }
                    pins_changed = true;
                }
                ShellDone::Catalog(apps) => {
                    if apps.is_empty() && self.shared.catalog.borrow().is_some() {
                        continue; // keep the previous catalog on failure
                    }
                    let catalog = Catalog::build(apps.iter().map(|a| a.entry.clone()));
                    *self.shared.catalog.borrow_mut() = Some(apps);
                    {
                        let mut store = self.shared.pins.borrow_mut();
                        store.reload_if_changed();
                        if store.pins.seed_with(DEFAULT_TASKBAR_PIN_IDS, &catalog) {
                            tracing::info!(
                                pins = store.pins.keys.len(),
                                "seeded default taskbar pins"
                            );
                            store.save();
                        }
                    }
                    // App names of running windows come from the catalog.
                    let hwnds: Vec<u64> = self.shared.idents.borrow().keys().copied().collect();
                    for h in hwnds {
                        let ident = self.shared.compute_ident(h);
                        self.shared.idents.borrow_mut().insert(h, ident);
                    }
                    pins_changed = true;
                }
            }
        }
        if pins_changed {
            self.shared.rebuild_pinned();
            repaint = true;
        }
        if repaint {
            self.shared.invalidate_bars();
            flyout::invalidate(&self.shared);
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
                *self.shared.shell.borrow_mut() =
                    Some(ShellWorker::start(hwnd, self.shared.folders.clone()));
                *self.shared.sys.borrow_mut() = Some(SysWorker::start(hwnd));
                self.shared.update_clock();
                self.shared.reload_tray_settings();
                self.start_tray(hwnd);
                self.shared.rebuild_pinned();
                self.resync();
                flyout::create_window(&self.shared);
                self.rebuild_bars();
                self.request_catalog();
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
                flyout::close(&self.shared);
                explorer_win::ipc::send_launcher_command(LauncherCommand::Toggle);
                Some(LRESULT(0))
            }
            WM_APP_TRAY_CHANGED => {
                self.shared.invalidate_bars();
                flyout::refresh(&self.shared);
                Some(LRESULT(0))
            }
            WM_APP_SHELL_DONE => {
                self.on_shell_done();
                Some(LRESULT(0))
            }
            WM_APP_SYS_CHANGED => {
                let snap = self.shared.sys.borrow().as_ref().map(|s| s.snapshot());
                if let Some(mut snap) = snap {
                    // Keep the value under the user's finger while dragging.
                    if let Some(level) = flyout::dragging_volume(&self.shared) {
                        if let Some(a) = snap.audio.as_mut() {
                            a.level = level;
                        }
                    }
                    if let Some(level) = flyout::dragging_brightness(&self.shared) {
                        snap.brightness = Some(level);
                    }
                    *self.shared.sys_status.borrow_mut() = snap;
                    self.shared.invalidate_bars();
                    flyout::invalidate(&self.shared);
                }
                Some(LRESULT(0))
            }
            WM_POWERBROADCAST => {
                // PBT_APMPOWERSTATUSCHANGE and friends: re-read now.
                self.shared.sys_cmd(SysCmd::Refresh);
                Some(LRESULT(1))
            }
            WM_SETTINGCHANGE => {
                // Theme ("ImmersiveColorSet") or taskbar alignment changed.
                // Cheap registry reads; no repositioning, so our own
                // SPI_SETWORKAREA notifications cannot loop.
                if self.shared.reload_appearance() {
                    tracing::info!("appearance changed");
                    self.shared.invalidate_bars();
                    flyout::invalidate(&self.shared);
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
                        if self.ticks % 30 == 0 {
                            self.shared.tray_exe.borrow_mut().clear();
                        }
                        let settings = self.ticks % 5 == 0 && self.shared.reload_tray_settings();
                        if pruned || settings {
                            self.shared.invalidate_bars();
                        }
                        if self.ticks % 2 == 0 && self.shared.pins.borrow_mut().reload_if_changed()
                        {
                            tracing::info!("taskbar pins changed on disk");
                            self.shared.rebuild_pinned();
                            self.shared.invalidate_bars();
                        }
                        if self
                            .shared
                            .catalog_at
                            .get()
                            .is_some_and(|t| t.elapsed() > CATALOG_REFRESH)
                        {
                            self.request_catalog();
                        }
                    }
                    TIMER_RESYNC => self.resync(),
                    TIMER_REBUILD => {
                        // SAFETY: our own timer.
                        let _ = unsafe { KillTimer(Some(hwnd), TIMER_REBUILD) };
                        self.rebuild_bars();
                    }
                    TIMER_PEEK if !flyout::peek_tick(&self.shared) => {
                        // SAFETY: our own timer.
                        let _ = unsafe { KillTimer(Some(hwnd), TIMER_PEEK) };
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
