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
use explorer_core::config::{Alignment, AutoHide, Config, MonitorSelection};
use explorer_core::display::{self, DisplayChange, MonitorKey};
use explorer_core::i18n::Lang;
use explorer_core::image::SharedImage;
use explorer_core::ipc::{LauncherCommand, RunMode, TASKBAR_WINDOW_CLASS};
use explorer_core::paths::AppPaths;
use explorer_core::shell_keys::{self, ShellAction, ShellHotkey};
use explorer_core::shellpath::{file_stem, looks_like_path, same_path, KnownFolders};
use explorer_core::start::{Pins, DEFAULT_TASKBAR_PIN_IDS};
use explorer_core::taskbar_groups::{
    build_buttons, AppButton, PinnedApp, ShowDesktop, ShowDesktopAction, WindowIdent,
};
use explorer_core::taskbar_model::{TaskbarModel, WindowEvent};
use explorer_core::theme::Palette;
use explorer_core::tray_promotion::NotifySetting;
use explorer_core::update::{self, UpdateState};
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
    WM_APP, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_HOTKEY, WM_POWERBROADCAST, WM_SETTINGCHANGE,
    WM_TIMER, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::bar::Bar;
use crate::flyout;
use crate::update_toast;
use crate::workers::{
    CatalogApp, ShellDone, ShellJob, ShellWorker, SysCmd, SysStatus, SysWorker, WM_APP_SHELL_DONE,
    WM_APP_SYS_CHANGED,
};
use crate::Args;

const CONTROLLER_CLASS: &str = "ExplorerRs.TaskbarController";

const WM_APP_INIT: u32 = WM_APP + 1;
/// The background update check finished (state file written).
const WM_APP_UPDATE_CHECKED: u32 = WM_APP + 30;
/// Close the update reminder (posted by the reminder itself).
const WM_APP_HIDE_UPDATE: u32 = WM_APP + 31;

const TIMER_CLOCK: usize = 1;
const TIMER_RESYNC: usize = 2;
const TIMER_REBUILD: usize = 3;
/// Hover-thumbnail ("peek") leave detection while it is visible.
pub(crate) const TIMER_PEEK: usize = 4;

/// Full re-enumeration interval: a safety net for missed notifications.
const RESYNC_MS: u32 = 30_000;
/// Re-hide Windows' taskbar if Explorer showed it again.
const TIMER_HIDE_WIN_TASKBAR: usize = 41;
/// Re-checks whether the foreground window is full screen (games often
/// switch after they were activated).
const TIMER_FULLSCREEN: usize = 42;
const FULLSCREEN_CHECK_MS: u32 = 500;
/// Self-update: is a check due, or a reminder? First a while after start
/// (not during sign-in), then every 15 minutes. The check itself runs at
/// most once a day (`explorer_core::update::should_check`).
const TIMER_UPDATE: usize = 43;
const UPDATE_FIRST_MS: u32 = 90_000;
const UPDATE_EVERY_MS: u32 = 15 * 60_000;
const HIDE_WIN_TASKBAR_MS: u32 = 2_000;
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

    /// Returns true if the file changed and was re-read. A file that could
    /// not be read keeps the pins in memory and is retried next time.
    pub fn reload_if_changed(&mut self) -> bool {
        let m = mtime(&self.path);
        if m != self.mtime || self.pins.unreadable {
            match self.pins.reload(&self.path) {
                explorer_core::start::Reload::Retry => false,
                _ => {
                    self.mtime = mtime(&self.path);
                    true
                }
            }
        } else {
            false
        }
    }

    pub fn save(&mut self) {
        self.pins.retry_if_unreadable(&self.path);
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
    /// Full-screen windows per monitor, (monitor, window) as integers; read
    /// by each bar on WM_APP_FULLSCREEN.
    pub fullscreen: RefCell<Vec<(isize, isize)>>,
    /// Closes a sticky flyout on a click on a window that never activates.
    pub flyout_clicks: RefCell<Option<explorer_win::keyboard::ClickWatcher>>,
    pub flyout_closed: RefCell<Option<(flyout::Opener, Instant)>>,
    /// The bars auto-hide (config, or Windows' setting); see
    /// `Controller::check_auto_hide`.
    pub auto_hide: Cell<bool>,
    show_desktop: RefCell<ShowDesktop>,
    show_desktop_at: Cell<Option<Instant>>,
    pub controller: Cell<HWND>,
    /// The "update available" reminder, if shown.
    pub update_toast: Cell<HWND>,
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
        // Not 0: the supervisor retries (the previous taskbar may still be
        // dying after a restart).
        return explorer_core::supervisor::EXIT_ALREADY_RUNNING;
    };

    window::init_component();
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
        fullscreen: RefCell::new(Vec::new()),
        flyout_clicks: RefCell::new(None),
        flyout_closed: RefCell::new(None),
        auto_hide: Cell::new(auto_hide_wanted(
            config.taskbar.auto_hide,
            &paths.windows_taskbar_state_file(),
        )),
        show_desktop: RefCell::new(ShowDesktop::default()),
        show_desktop_at: Cell::new(None),
        controller: Cell::new(HWND::default()),
        update_toast: Cell::new(HWND::default()),
        config,
    });
    shared.reload_appearance();

    window::register_class(CONTROLLER_CLASS, None);
    window::register_class(TASKBAR_WINDOW_CLASS, None);
    flyout::register_class();
    update_toast::register_class();

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
            win_taskbar_state: (mode == RunMode::Dev && shared.config.taskbar.hide_windows_taskbar)
                .then(|| paths.windows_taskbar_state_file()),
            taskbar_state_file: paths.windows_taskbar_state_file(),
            update_state_file: paths.update_state_file(),
            update_checking: false,
            update_shown: false,
            bar_monitors: Vec::new(),
            hotkeys: Vec::new(),
            print_screen_wanted: None,
            windows_build: explorer_win::registry::windows_build(),
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

    let loop_code = window::run_message_loop(|_| false);
    // Give Windows' taskbar back on every exit path of the message loop
    // (a crash is covered by the supervisor and the guardian).
    if mode == RunMode::Dev {
        explorer_win::win_taskbar::restore(&paths.windows_taskbar_state_file());
    }
    // Release thumbnails and the tray (and its window) before COM goes away.
    update_toast::hide(&shared);
    shared.flyout.borrow_mut().take();
    shared.tray.borrow_mut().take();
    let code = window::exit_code();
    tracing::info!(code, loop_code, "taskbar exiting");
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
    /// Set while Windows' taskbar is to be hidden (dev mode + config).
    win_taskbar_state: Option<std::path::PathBuf>,
    /// Where the user's own Windows taskbar auto-hide choice is kept while
    /// we hide that taskbar (read for `auto_hide = "system"`).
    taskbar_state_file: std::path::PathBuf,
    /// `%LOCALAPPDATA%\explorer-rs\update.toml`.
    update_state_file: std::path::PathBuf,
    /// A background update check is running.
    update_checking: bool,
    /// The update reminder was shown by this taskbar (at most once per
    /// sign-in session; restarts are covered by the 3-day interval).
    update_shown: bool,
    /// The monitors the current bars were built for.
    bar_monitors: Vec<MonitorKey>,
    /// Shell mode: Explorer's shortcuts we registered (hotkey ids).
    hotkeys: Vec<i32>,
    /// Shell mode: whether the lone Print Screen key should open the screen
    /// clip overlay, as last evaluated (None: not yet).
    print_screen_wanted: Option<bool>,
    /// Windows' build number (for the Print Screen default).
    windows_build: Option<u32>,
}

/// Ask the controller to close the update reminder (the reminder must not
/// destroy itself from inside its own window procedure).
pub(crate) fn post_hide_update_toast(shared: &Shared) {
    // SAFETY: posting to our own window.
    let _ = unsafe {
        PostMessageW(
            Some(shared.controller.get()),
            WM_APP_HIDE_UPDATE,
            WPARAM(0),
            LPARAM(0),
        )
    };
}

/// Whether the bars should auto-hide.
fn auto_hide_wanted(setting: AutoHide, state_file: &std::path::Path) -> bool {
    match setting {
        AutoHide::Always => true,
        AutoHide::Never => false,
        AutoHide::System => explorer_win::win_taskbar::system_autohide(state_file),
    }
}

impl Controller {
    /// Timer: start the daily check if due, else see whether to remind.
    fn update_tick(&mut self) {
        if !explorer_win::update::check_updates_enabled(&self.shared.config.general)
            || self.update_checking
        {
            return;
        }
        let state = UpdateState::load(&self.update_state_file);
        if update::should_check(&state, update::unix_now()) {
            self.update_checking = true;
            let file = self.update_state_file.clone();
            let controller = self.shared.controller.get().0 as isize;
            // Network I/O off the UI thread. Failures are only logged.
            std::thread::spawn(move || {
                let _ = explorer_win::update::check_and_record(&file);
                // SAFETY: posting to our own window (gone = harmless).
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(controller as *mut core::ffi::c_void)),
                        WM_APP_UPDATE_CHECKED,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            });
        } else {
            self.maybe_remind(state);
        }
    }

    fn maybe_remind(&mut self, mut state: UpdateState) {
        let now = update::unix_now();
        let current = update::Version::current();
        let ctx = update::RemindContext {
            enabled: explorer_win::update::check_updates_enabled(&self.shared.config.general),
            shown_this_session: self.update_shown || update_toast::is_shown(&self.shared),
            fullscreen: !self.shared.fullscreen.borrow().is_empty(),
        };
        if !update::should_remind(&state, current, now, ctx) {
            return;
        }
        let Some((_, version)) = state.available(current) else {
            return;
        };
        // Recorded when shown: "Later" (or ignoring it) waits the full
        // interval.
        state.mark_reminded(version, now);
        if let Err(e) = state.save(&self.update_state_file) {
            // Without the record it would come back at every start.
            tracing::warn!(error = %e, "cannot save the update state; not reminding");
            return;
        }
        self.update_shown = true;
        update_toast::show(&self.shared, version);
    }

    fn post_bars(&self, msg: u32) {
        for &b in self.shared.bars.borrow().iter() {
            // SAFETY: posting to our own window.
            let _ = unsafe { PostMessageW(Some(b), msg, WPARAM(0), LPARAM(0)) };
        }
    }

    /// Follow Windows' "Automatically hide the taskbar" setting.
    fn check_auto_hide(&mut self) {
        let on = auto_hide_wanted(
            self.shared.config.taskbar.auto_hide,
            &self.taskbar_state_file,
        );
        if on == self.shared.auto_hide.get() {
            return;
        }
        tracing::info!(auto_hide = on, "taskbar auto-hide changed");
        self.shared.auto_hide.set(on);
        for &b in self.shared.bars.borrow().iter() {
            // SAFETY: posting to our own window.
            let _ =
                unsafe { PostMessageW(Some(b), crate::bar::WM_APP_AUTOHIDE, WPARAM(0), LPARAM(0)) };
        }
    }

    /// Tell the bars when a monitor starts or stops showing a full-screen
    /// app. Explorer's AppBar manager does that with ABN_FULLSCREENAPP, but
    /// it is not there when explorer.rs is the shell.
    fn check_fullscreen(&mut self, force: bool) {
        let now = explorer_win::winlist::fullscreen_windows();
        if now == *self.shared.fullscreen.borrow() && !force {
            return;
        }
        if now != *self.shared.fullscreen.borrow() {
            tracing::info!(monitors = now.len(), windows = ?now.iter().map(|p| p.1).collect::<Vec<_>>(), "full-screen apps changed");
        }
        if !now.is_empty() {
            // Never over a game, video or presentation.
            update_toast::hide(&self.shared);
        }
        *self.shared.fullscreen.borrow_mut() = now;
        for &b in self.shared.bars.borrow().iter() {
            // SAFETY: posting to our own window.
            let _ = unsafe {
                PostMessageW(Some(b), crate::bar::WM_APP_FULLSCREEN, WPARAM(0), LPARAM(0))
            };
        }
    }

    /// The monitors that get a bar.
    fn bar_targets(&self) -> Vec<monitors::Monitor> {
        let mut mons = monitors::all();
        if self.shared.config.taskbar.monitors == MonitorSelection::Primary {
            mons.retain(|m| m.primary);
        }
        mons
    }

    fn rebuild_bars(&mut self) {
        flyout::close(&self.shared);
        let old: Vec<HWND> = self.shared.bars.borrow_mut().drain(..).collect();
        for b in old {
            // SAFETY: destroying our own windows (from the controller, never
            // from inside the bar's own handler).
            let _ = unsafe { DestroyWindow(b) };
        }
        // Full screen first: a bar created over a full-screen game must
        // start below it (`Bar::create` reads this), not topmost.
        self.check_fullscreen(true);
        let mons = self.bar_targets();
        self.bar_monitors = mons.iter().map(|m| m.key()).collect();
        for m in mons {
            tracing::info!(monitor = %m.describe(), "creating bar");
            match Bar::create(self.shared.clone(), m) {
                Ok(h) => self.shared.bars.borrow_mut().push(h),
                Err(e) => tracing::error!(error = %e, "cannot create bar window"),
            }
        }
    }

    /// Display settings changed (debounced). Rebuild the bars only when
    /// the set of monitors changed; a new resolution, position or scale
    /// (a game switching to exclusive full screen) only moves them.
    fn on_display_change(&mut self) {
        let mons = self.bar_targets();
        for m in &mons {
            tracing::info!(monitor = %m.describe(), "display changed");
        }
        let keys: Vec<MonitorKey> = mons.iter().map(|m| m.key()).collect();
        let change = display::classify(&self.bar_monitors, &keys);
        tracing::info!(?change, monitors = keys.len(), "display change");
        match change {
            DisplayChange::Rebuild => self.rebuild_bars(),
            DisplayChange::Reposition => {
                self.bar_monitors = keys;
                // The bars learn about a full-screen window before they
                // move (messages are handled in order).
                self.check_fullscreen(true);
                flyout::close(&self.shared);
                self.post_bars(crate::bar::WM_APP_POSITION);
            }
            DisplayChange::Unchanged => {
                self.check_fullscreen(true);
                self.post_bars(crate::bar::WM_APP_CHECK_RESERVE);
            }
        }
    }

    /// Shell mode: provide the Explorer shortcuts that Windows leaves to
    /// the shell (`explorer_core::shell_keys`).
    fn register_shell_keys(&mut self, hwnd: HWND) {
        if !shell_keys::wanted(self.shared.mode) {
            return;
        }
        let mut failed = Vec::new();
        for k in shell_keys::win_hotkeys() {
            match explorer_win::keyboard::register_hotkey(hwnd, k) {
                Ok(()) => self.hotkeys.push(k.id),
                Err(e) => {
                    tracing::warn!(key = k.name, error = %e, "cannot register shortcut (another program holds it?)");
                    failed.push(k.name);
                }
            }
        }
        tracing::info!(
            registered = self.hotkeys.len(),
            ?failed,
            "shell shortcuts registered"
        );
        self.update_print_screen(hwnd);
    }

    /// Follow Windows' "Use the Print screen key to open screen capture"
    /// setting. When it is off the key is left to Windows (clipboard copy).
    fn update_print_screen(&mut self, hwnd: HWND) {
        if !shell_keys::wanted(self.shared.mode) {
            return;
        }
        let flag = explorer_win::registry::read_dword(
            explorer_win::registry::Root::CurrentUser,
            r"Control Panel\Keyboard",
            "PrintScreenKeyForSnippingEnabled",
        );
        let want = shell_keys::print_screen_opens_snipping(flag, self.windows_build);
        if self.print_screen_wanted == Some(want) {
            return;
        }
        self.print_screen_wanted = Some(want);
        match shell_keys::print_screen_hotkey(want) {
            Some(k) => match explorer_win::keyboard::register_hotkey(hwnd, &k) {
                Ok(()) => {
                    self.hotkeys.push(k.id);
                    tracing::info!(?flag, "Print Screen opens the screen clip overlay");
                }
                Err(e) => tracing::warn!(error = %e, "cannot register the Print Screen key"),
            },
            None => {
                if let Some(i) = self
                    .hotkeys
                    .iter()
                    .position(|&id| id == shell_keys::PRINT_SCREEN_ID)
                {
                    explorer_win::keyboard::unregister_hotkey(hwnd, shell_keys::PRINT_SCREEN_ID);
                    self.hotkeys.remove(i);
                }
                tracing::info!(?flag, build = ?self.windows_build, "Print Screen left to Windows");
            }
        }
    }

    fn on_shell_key(&mut self, k: ShellHotkey) {
        tracing::info!(key = k.name, action = ?k.action, "shell shortcut");
        match k.action {
            ShellAction::ScreenClip => crate::workers::open_uri("ms-screenclip:"),
            ShellAction::ScreenshotToFile => {
                let lang = self.shared.lang;
                let r = std::thread::Builder::new()
                    .name("screenshot".into())
                    .spawn(move || {
                        let _com = ComApartment::mta();
                        match explorer_win::screenshot::save_screen(lang) {
                            Ok(p) => tracing::info!(path = %p.display(), "screenshot saved"),
                            Err(e) => tracing::warn!(error = %e, "screenshot failed"),
                        }
                    });
                if let Err(e) = r {
                    tracing::error!(error = %e, "cannot spawn screenshot thread");
                }
            }
            // As the Start menu's "File Explorer" button does: with our tray
            // window present explorer.exe opens a window, it does not take
            // over as the shell.
            ShellAction::FileExplorer => crate::workers::open_uri("explorer.exe"),
            ShellAction::Run => {
                flyout::close(&self.shared);
                // As for the Win key: let the Start menu take the focus.
                explorer_win::keyboard::claim_foreground_rights();
                explorer_win::ipc::send_launcher_command(LauncherCommand::Show);
            }
            ShellAction::Settings => crate::workers::open_uri("ms-settings:"),
            ShellAction::ShowDesktop => self.shared.toggle_show_desktop(),
            ShellAction::QuickSettings | ShellAction::Notifications => {
                let which = if k.action == ShellAction::QuickSettings {
                    crate::bar::FLYOUT_QUICK
                } else {
                    crate::bar::FLYOUT_CALENDAR
                };
                // The primary monitor's bar (bars are created primary
                // first) has the system glyphs and the clock.
                if let Some(&b) = self.shared.bars.borrow().first() {
                    // SAFETY: posting to our own window.
                    let _ = unsafe {
                        PostMessageW(
                            Some(b),
                            crate::bar::WM_APP_OPEN_FLYOUT,
                            WPARAM(which),
                            LPARAM(0),
                        )
                    };
                }
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
                self.check_fullscreen(false);
                self.rehide_win_taskbar();
                if self.shared.mode == RunMode::Dev {
                    for &b in self.shared.bars.borrow().iter() {
                        // SAFETY: posting to our own window.
                        let _ = unsafe {
                            PostMessageW(Some(b), crate::bar::WM_APP_RAISE, WPARAM(0), LPARAM(0))
                        };
                    }
                }
            }
            ev::EVENT_OBJECT_SHOW
                if self.win_taskbar_state.is_some()
                    && explorer_win::win_taskbar::is_explorer_taskbar(hwnd) =>
            {
                // Explorer showed its taskbar again (Win key, the auto-hide
                // reveal at the screen edge, notifications): hide it now
                // rather than at the next timer tick.
                self.rehide_win_taskbar();
            }
            _ => self.upsert(hwnd),
        }
    }

    /// Hide Windows' taskbar again if Explorer showed it, while we are the
    /// ones hiding it. Once someone else restored it (Ctrl+Alt+Shift+E,
    /// --restore-explorer: the state file is gone), stop for good instead
    /// of hiding it right back.
    fn rehide_win_taskbar(&mut self) {
        let Some(f) = &self.win_taskbar_state else {
            return;
        };
        if f.exists() {
            explorer_win::win_taskbar::reassert_hidden();
        } else {
            tracing::warn!("Windows' taskbar was restored by someone else; no longer hiding it");
            self.win_taskbar_state = None;
            // SAFETY: our own timer.
            let _ =
                unsafe { KillTimer(Some(self.shared.controller.get()), TIMER_HIDE_WIN_TASKBAR) };
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
            if let Some(f) = &self.win_taskbar_state {
                // Explorer restarted and created a visible taskbar again.
                explorer_win::win_taskbar::hide(f);
            }
            self.rebuild_bars();
            if self.shared.fullscreen.borrow().is_empty() {
                if let Some(t) = self.shared.tray.borrow().as_ref() {
                    t.ensure_front();
                }
            }
            return Some(LRESULT(0));
        }
        match msg {
            WM_APP_INIT => {
                if let Some(f) = &self.win_taskbar_state {
                    explorer_win::win_taskbar::hide(f);
                    // SAFETY: timer on our own window.
                    unsafe {
                        SetTimer(
                            Some(hwnd),
                            TIMER_HIDE_WIN_TASKBAR,
                            HIDE_WIN_TASKBAR_MS,
                            None,
                        )
                    };
                }
                self.hooks = Some(ShellHooks::install(hwnd));
                self.register_shell_keys(hwnd);
                if self.shared.config.launcher.win_key_opens_launcher {
                    match WinKeyHook::install(hwnd, self.shared.mode == RunMode::Dev) {
                        Ok(k) => {
                            tracing::info!("Win key hook installed");
                            self.winkey = Some(k)
                        }
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
                    SetTimer(Some(hwnd), TIMER_FULLSCREEN, FULLSCREEN_CHECK_MS, None);
                    if explorer_win::update::check_updates_enabled(&self.shared.config.general) {
                        SetTimer(Some(hwnd), TIMER_UPDATE, UPDATE_FIRST_MS, None);
                    }
                }
                Some(LRESULT(0))
            }
            WM_APP_UPDATE_CHECKED => {
                self.update_checking = false;
                self.maybe_remind(UpdateState::load(&self.update_state_file));
                Some(LRESULT(0))
            }
            WM_APP_HIDE_UPDATE => {
                update_toast::hide(&self.shared);
                Some(LRESULT(0))
            }
            WM_APP_WINEVENT => {
                self.on_win_event(wparam.0 as u32, HWND(lparam.0 as *mut _));
                Some(LRESULT(0))
            }
            WM_APP_WINKEY => {
                tracing::debug!("Win key tapped: toggling the Start menu");
                flyout::close(&self.shared);
                // The key went to the foreground app, not to us: become the
                // last input sender so the launcher may take the focus
                // (otherwise typing can go to the previous window).
                explorer_win::keyboard::claim_foreground_rights();
                explorer_win::ipc::send_launcher_command(LauncherCommand::Toggle);
                // An auto-hidden bar comes up with the Start menu.
                for &b in self.shared.bars.borrow().iter() {
                    // SAFETY: posting to our own window.
                    let _ = unsafe {
                        PostMessageW(Some(b), crate::bar::WM_APP_REVEAL, WPARAM(0), LPARAM(0))
                    };
                }
                // Explorer may reveal its (auto-hidden) taskbar on the key.
                self.rehide_win_taskbar();
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
                // Theme ("ImmersiveColorSet"), taskbar alignment or auto-hide
                // ("TraySettings") changed. Cheap reads; the bars only move
                // when auto-hide really changed, so our own SPI_SETWORKAREA
                // notifications cannot loop.
                if let Some(f) = &self.win_taskbar_state {
                    explorer_win::win_taskbar::follow_user_change(f);
                }
                self.check_auto_hide();
                self.post_bars(crate::bar::WM_APP_CHECK_RESERVE);
                self.update_print_screen(hwnd);
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
                        let fullscreen = !self.shared.fullscreen.borrow().is_empty();
                        let pruned = match self.shared.tray.borrow().as_ref() {
                            Some(t) => {
                                // Not while a game is full screen: no
                                // Z-order changes of ours then.
                                if !fullscreen {
                                    t.ensure_front();
                                }
                                self.ticks % 5 == 0 && t.prune()
                            }
                            None => false,
                        };
                        if self.ticks % 30 == 0 {
                            self.shared.tray_exe.borrow_mut().clear();
                            // Settings may not broadcast a change for it.
                            self.update_print_screen(hwnd);
                        }
                        if self.ticks % 2 == 0 {
                            if let Some(f) = &self.win_taskbar_state {
                                explorer_win::win_taskbar::follow_user_change(f);
                            }
                            self.post_bars(crate::bar::WM_APP_CHECK_RESERVE);
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
                    TIMER_FULLSCREEN => self.check_fullscreen(false),
                    TIMER_UPDATE => {
                        // SAFETY: re-arming our own timer (after the first,
                        // shorter delay).
                        unsafe { SetTimer(Some(hwnd), TIMER_UPDATE, UPDATE_EVERY_MS, None) };
                        self.update_tick();
                    }
                    TIMER_HIDE_WIN_TASKBAR => self.rehide_win_taskbar(),
                    TIMER_REBUILD => {
                        // SAFETY: our own timer.
                        let _ = unsafe { KillTimer(Some(hwnd), TIMER_REBUILD) };
                        self.on_display_change();
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
            // Only the graceful shutdown message or the quit menu end the
            // taskbar; a WM_CLOSE from another program is ignored.
            WM_CLOSE => Some(LRESULT(0)),
            WM_HOTKEY => {
                if let Some(k) = shell_keys::by_id(wparam.0 as i32) {
                    self.on_shell_key(k);
                }
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                for id in self.hotkeys.drain(..) {
                    explorer_win::keyboard::unregister_hotkey(hwnd, id);
                }
                self.winkey = None;
                self.hooks = None;
                window::quit_unexpected();
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}
