// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows implementation of the Start menu ("launcher").
//!
//! One popup window anchored above the taskbar, hidden while idle but kept
//! warm (catalog, icons, pins and recent files stay in memory). Views,
//! Windows 11 style:
//! - **Home**: search box, pinned apps grid (paged, 6×3), "Recommended"
//!   (recent files, then frequently used apps), footer;
//! - **All apps**: alphabetical list with letter headers; a letter header
//!   opens a letter picker to jump;
//! - **Search**: sectioned results (Best match, Apps, Settings, Documents,
//!   Commands) on the left, a detail panel with actions on the right.
//!
//! The query box is a native EDIT control (IME, clipboard for free) with a
//! painted placeholder (`cue.rs`); everything else is painted with
//! Direct2D. Navigation keys are intercepted in the message loop before the
//! EDIT control sees them; keyboard focus moves between "zones" with the
//! pure rules of `explorer_core::results`. File-system and Shell work runs
//! on worker threads (`worker.rs`, `files.rs`); the UI thread only runs the
//! in-memory app/settings search (well under a millisecond for ~200 apps
//! and ~85 settings entries).
//!
//! Every user-visible string goes through `explorer_core::i18n::tr`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant, SystemTime};

use explorer_core::catalog::{
    scan_folder, unix_now, AppEntry, AppSource, Catalog, LaunchHistory, LaunchTarget,
};
use explorer_core::config::{Alignment, Config, Edge};
use explorer_core::i18n::{tr, trf, Lang};
use explorer_core::image::SharedImage;
use explorer_core::ipc::{LauncherCommand, RunMode, LAUNCHER_WINDOW_CLASS};
use explorer_core::layout::{grid, Rect};
use explorer_core::paths::AppPaths;
use explorer_core::recent::RecentFile;
use explorer_core::results::{
    self, actions, header_rows, Action, Arrow, Focus, Limits, ResultKind, Section, Zone, ZoneKind,
};
use explorer_core::run_command::{classify, RunCommand, RunKind};
use explorer_core::search::Searcher;
use explorer_core::settings_pages::{EntryKind, SettingsEntry};
use explorer_core::start::{letter_of, page_count, page_range, Pins, DEFAULT_TASKBAR_PIN_IDS};
use explorer_core::supervisor::EXIT_RESTART;
use explorer_core::theme::Palette;
use explorer_win::gfx::{glyph, Align, Color, Gfx, Painter, Surface};
use explorer_win::tooltip::Tooltip;
use explorer_win::util::{wide, ComApartment, WideString};
use explorer_win::window::{self, CreateParams, Handler};
use explorer_win::windows::core::PCWSTR;
use explorer_win::windows::Win32::Foundation::{
    COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use explorer_win::windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use explorer_win::windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint,
    InvalidateRect, MonitorFromPoint, SetBkColor, SetTextColor, ValidateRect, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, FF_DONTCARE, FW_NORMAL, HBRUSH, HDC, HFONT, HGDIOBJ,
    MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
};
use explorer_win::windows::Win32::System::Shutdown::{
    ExitWindowsEx, LockWorkStation, EWX_LOGOFF, SHUTDOWN_REASON,
};
use explorer_win::windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL, VK_SHIFT,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DestroyMenu, GetClientRect, GetCursorPos,
    GetWindowTextLengthW, GetWindowTextW, KillTimer, MoveWindow, PostMessageW, SendMessageW,
    SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, TrackPopupMenu, ES_AUTOHSCROLL, HMENU,
    HWND_TOPMOST, MF_SEPARATOR, MF_STRING, MSG, SWP_NOACTIVATE, SW_HIDE, SW_SHOW, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_STYLE, WM_ACTIVATE, WM_APP,
    WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN,
    WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP,
    WM_SETFONT, WM_SETTINGCHANGE, WM_SIZE, WM_TIMER, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP, WS_VISIBLE,
};

use crate::model::{self, Row, SearchInput, Target};
use crate::worker::{Job, Reply, Workers, ICON_PX};
use crate::Args;

const WM_APP_INDEXED: u32 = WM_APP + 10;
const WM_APP_NAVKEY: u32 = WM_APP + 11;
#[cfg(any(feature = "everything", feature = "file-index"))]
const WM_APP_FILES: u32 = WM_APP + 12;
const WM_APP_WORKER: u32 = WM_APP + 13;
const WM_MOUSELEAVE: u32 = 0x02A3;

const EN_CHANGE: u32 = 0x0300;
const STATUS_TIMER: usize = 1;
const STATUS_FOR: Duration = Duration::from_millis(1600);

const VK_UP: u32 = 0x26;
const VK_DOWN: u32 = 0x28;
const VK_LEFT: u32 = 0x25;
const VK_RIGHT: u32 = 0x27;
const VK_PRIOR: u32 = 0x21;
const VK_NEXT: u32 = 0x22;
const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;
const VK_TAB: u32 = 0x09;
const VK_APPS: u32 = 0x5D;
const NAV_KEYS: &[u32] = &[
    VK_UP, VK_DOWN, VK_PRIOR, VK_NEXT, VK_RETURN, VK_ESCAPE, VK_TAB, VK_APPS,
];

/// Re-index on show when the catalog is older than this.
const REINDEX_AFTER: Duration = Duration::from_secs(120);
/// A Toggle arriving this soon after we hid because of deactivation is
/// the same click that deactivated us (e.g. on the Start button): stay
/// hidden instead of reopening.
const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(400);

// Logical metrics.
const PAD: f32 = 24.0;
const SEARCH_H: f32 = 36.0;
const HEADER_H: f32 = 32.0;
const PIN_COLS: usize = 6;
const PIN_ROWS: usize = 3;
const PINS_PER_PAGE: usize = PIN_COLS * PIN_ROWS;
const PIN_CELL_H: f32 = 84.0;
const DOTS_W: f32 = 20.0;
const REC_COLS: usize = 2;
const REC_MAX: usize = 6;
const REC_CELL_H: f32 = 56.0;
const ROW_H: f32 = 40.0;
const LETTER_H: f32 = 32.0;
const SECTION_H: f32 = 30.0;
const RESULT_H: f32 = 44.0;
const BEST_H: f32 = 68.0;
const ACTION_H: f32 = 36.0;
const FOOTER_H: f32 = 64.0;
const LETTER_COLS: usize = 6;
const LETTER_CELL: f32 = 52.0;

/// Fluent / MDL2 glyphs not in `explorer_win::gfx::glyph`.
mod g {
    pub const MORE: &str = "\u{E712}";
    pub const FOLDER: &str = "\u{E8B7}";
    pub const DOCUMENT: &str = "\u{E8A5}";
    pub const OPEN: &str = "\u{E8A7}";
    pub const SHIELD: &str = "\u{EA18}";
    pub const FOLDER_OPEN: &str = "\u{E838}";
    pub const COPY: &str = "\u{E8C8}";
    pub const PIN: &str = "\u{E718}";
    pub const UNPIN: &str = "\u{E77A}";
    pub const DELETE: &str = "\u{E74D}";
    pub const CANCEL: &str = "\u{E711}";
    pub const CALCULATOR: &str = "\u{E8EF}";
    pub const COMMAND: &str = "\u{E756}";
    pub const PLAY: &str = "\u{E768}";
    pub const REPAIR: &str = "\u{E90F}";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Home,
    AllApps,
    /// Letter picker of All apps.
    Letters,
    Search,
}

// Focus zones, per view (see `Launcher::zones`).
const Z_PINNED: usize = 0;
const Z_REC: usize = 1;
const Z_HOME_FOOTER: usize = 2;
const Z_ALLAPPS_BTN: usize = 3;

const Z_LIST: usize = 0;
const Z_ALL_FOOTER: usize = 1;
const Z_BACK: usize = 2;

const Z_LETTERS: usize = 0;

const Z_RESULTS: usize = 0;
const Z_ACTIONS: usize = 1;
const Z_SEARCH_FOOTER: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FooterButton {
    User,
    More,
    Files,
    Settings,
    Power,
}

const FOOTER: [FooterButton; 5] = [
    FooterButton::User,
    FooterButton::More,
    FooterButton::Files,
    FooterButton::Settings,
    FooterButton::Power,
];

/// A clickable thing on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    /// A focusable item: (zone, index) of the current view.
    Item(usize, usize),
    /// Page indicator of the pinned grid.
    PageDot(usize),
    /// A letter header of All apps (opens the letter picker).
    Letter(usize),
}

enum IndexMsg {
    Catalog(Vec<AppEntry>),
    Icons(HashMap<String, SharedImage>),
}

/// Index all app sources. Must run with COM initialised.
///
/// `shell:AppsFolder` is the primary source: it already contains every Start
/// Menu shortcut, with the *localized* display name (the `.lnk` file names
/// are often English; Windows localizes them through desktop.ini). Scanning
/// the Start Menu folders directly is only a fallback.
fn index_apps(extra_folders: &[String]) -> Vec<AppEntry> {
    let started = Instant::now();
    let mut v = match explorer_win::apps::apps_folder() {
        Ok(a) if !a.is_empty() => a,
        Ok(_) | Err(_) => {
            tracing::warn!("shell:AppsFolder unavailable; scanning Start Menu folders instead");
            explorer_win::apps::start_menu_folders()
                .iter()
                .flat_map(|f| scan_folder(f, AppSource::StartMenu))
                .collect()
        }
    };
    for f in extra_folders {
        v.extend(scan_folder(Path::new(f), AppSource::Extra));
    }
    tracing::info!(
        entries = v.len(),
        ms = started.elapsed().as_millis() as u64,
        "indexed apps"
    );
    v
}

pub fn run(args: Args) -> i32 {
    explorer_win::console::attach_parent();
    explorer_win::dpi::enable_per_monitor_v2();
    let _com = ComApartment::sta();

    let paths = AppPaths::from_env();
    let _ = paths.ensure();
    let config_path = args.config.clone().unwrap_or_else(|| paths.config_file());
    let (config, config_err) = Config::load_or_default(&config_path);

    if args.dump_apps {
        let catalog = Catalog::build(index_apps(&config.launcher.extra_folders));
        let mut text = String::new();
        for e in catalog.entries() {
            text.push_str(&format!(
                "{:<48} {:<10?} {:?}\n",
                e.name, e.source, e.target
            ));
        }
        text.push_str(&format!("-- {} entries\n", catalog.len()));
        return explorer_core::dump::emit(&text, args.output.as_deref());
    }

    let _log = explorer_core::logging::init(
        "launcher",
        &paths.log_dir,
        &config.general.log_level,
        config.general.log_keep_files,
    );
    if let Some(e) = config_err {
        tracing::error!(error = %e, "config error, using defaults");
    }
    let Some(_instance) = explorer_win::single_instance::acquire("ExplorerRs.Launcher") else {
        tracing::warn!("another launcher is already running; exiting");
        return 0;
    };
    let mode = RunMode::from_env();
    tracing::info!(mode = mode.as_str(), "launcher starting");

    let gfx = match Gfx::new() {
        Ok(g) => g,
        Err(e) => {
            tracing::error!(error = %e, "cannot initialise Direct2D");
            return 1;
        }
    };

    let lang = explorer_win::theme::ui_lang(config.general.language);
    window::register_class(LAUNCHER_WINDOW_CLASS, None);
    let main_hwnd = Rc::new(Cell::new(HWND::default()));
    let edit_hwnd = Rc::new(Cell::new(HWND::default()));
    let launcher = Launcher {
        gfx,
        lang,
        history: LaunchHistory::load(&paths.history_file()),
        pins: Pins::load(&paths.pins_file()),
        taskbar_pins: TaskbarPins::new(paths.taskbar_pins_file()),
        paths,
        config,
        palette: Palette::from_system(&Default::default(), explorer_core::theme::Mode::Dark),
        user_name: explorer_win::theme::user_display_name(),
        avatar: None,
        catalog: Catalog::default(),
        icons: HashMap::new(),
        icons_requested: HashSet::new(),
        searcher: Searcher::new(),
        view: View::Home,
        pin_page: 0,
        pinned: Vec::new(),
        recommended: Vec::new(),
        recent: Vec::new(),
        utc_offset: 0,
        all: Vec::new(),
        letters: Vec::new(),
        results: Vec::new(),
        detail: 0,
        detail_actions: Vec::new(),
        file_paths: Vec::new(),
        command: None,
        generation: 0,
        tools: None,
        focus: None,
        hover: None,
        focus_visible: false,
        status: None,
        hits: Vec::new(),
        scroll: 0.0,
        indexing: false,
        indexed_at: None,
        index_rx: None,
        workers: None,
        hidden_at: None,
        surface: None,
        scale: 1.0,
        edit: HWND::default(),
        edit_font: None,
        edit_font_scale: 0.0,
        edit_brush: None,
        edit_brush_color: 0,
        edit_shared: edit_hwnd.clone(),
        launcher_msg: explorer_win::ipc::launcher_message(),
        query: String::new(),
        tracking: false,
        last_mouse: None,
        tooltip: None,
        tooltip_for: None,
        #[cfg(any(feature = "everything", feature = "file-index"))]
        files: None,
    };
    let hwnd = match window::create(
        CreateParams {
            class: LAUNCHER_WINDOW_CLASS,
            title: tr(lang, "Start", "Inicio"),
            style: WS_POPUP,
            ex_style: WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            rect: RECT {
                left: 0,
                top: 0,
                right: 640,
                bottom: 720,
            },
            parent: None,
        },
        Box::new(launcher),
    ) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "cannot create launcher window");
            return 1;
        }
    };
    main_hwnd.set(hwnd);
    if args.show {
        // SAFETY: posting to our own window.
        let _ = unsafe {
            PostMessageW(
                Some(hwnd),
                explorer_win::ipc::launcher_message(),
                WPARAM(LauncherCommand::Show as usize),
                LPARAM(0),
            )
        };
    }

    let code = window::run_message_loop(move |msg: &MSG| {
        // Steal navigation keys from the EDIT control. Left/Right are only
        // stolen while the query is empty (otherwise they move the caret).
        // The context-menu key opens our menu (on key down); swallow its key
        // up so the EDIT control does not open its own as well.
        if msg.message == WM_KEYUP && msg.hwnd == edit_hwnd.get() && msg.wParam.0 as u32 == VK_APPS
        {
            return true;
        }
        if msg.message == WM_KEYDOWN && msg.hwnd == edit_hwnd.get() {
            let vk = msg.wParam.0 as u32;
            let arrows_free = (vk == VK_LEFT || vk == VK_RIGHT)
                // SAFETY: plain query on our own control.
                && unsafe { GetWindowTextLengthW(edit_hwnd.get()) } == 0;
            if NAV_KEYS.contains(&vk) || arrows_free {
                // SAFETY: synchronous message to our own window.
                unsafe {
                    SendMessageW(
                        main_hwnd.get(),
                        WM_APP_NAVKEY,
                        Some(msg.wParam),
                        Some(LPARAM(0)),
                    )
                };
                return true;
            }
        }
        false
    });
    tracing::info!(code, "launcher exiting");
    code
}

/// The taskbar's pin file, re-read when it changes on disk (the taskbar
/// writes it too).
struct TaskbarPins {
    path: PathBuf,
    mtime: Option<SystemTime>,
    pins: Pins,
}

impl TaskbarPins {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            mtime: None,
            pins: Pins::default(),
        }
    }

    fn refresh(&mut self) -> &mut Pins {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        if mtime != self.mtime || mtime.is_none() {
            self.pins = Pins::load(&self.path);
            self.mtime = mtime;
        }
        &mut self.pins
    }

    fn contains(&mut self, t: &LaunchTarget, catalog: &Catalog) -> bool {
        let pins = self.refresh();
        if !pins.initialized {
            // Not written yet: the taskbar shows its defaults.
            let mut seeded = pins.clone();
            seeded.seed_with(DEFAULT_TASKBAR_PIN_IDS, catalog);
            return seeded.contains(t);
        }
        pins.contains(t)
    }

    fn set(&mut self, t: &LaunchTarget, pinned: bool, catalog: &Catalog) {
        let path = self.path.clone();
        let pins = self.refresh();
        if !pins.initialized {
            pins.seed_with(DEFAULT_TASKBAR_PIN_IDS, catalog);
        }
        if pinned {
            pins.pin(t);
        } else {
            pins.unpin(t);
        }
        if let Err(e) = pins.save(&path) {
            tracing::warn!(error = %e, "cannot save taskbar pins");
        }
        self.mtime = None; // re-read our own write next time
    }
}

struct Launcher {
    gfx: Gfx,
    lang: Lang,
    config: Config,
    paths: AppPaths,
    history: LaunchHistory,
    pins: Pins,
    taskbar_pins: TaskbarPins,
    palette: Palette,
    user_name: String,
    avatar: Option<SharedImage>,
    catalog: Catalog,
    icons: HashMap<String, SharedImage>,
    icons_requested: HashSet<String>,
    searcher: Searcher,
    view: View,
    // Home.
    pin_page: usize,
    pinned: Vec<Row>,
    recommended: Vec<Row>,
    recent: Vec<RecentFile>,
    utc_offset: i64,
    // All apps.
    all: Vec<Row>,
    /// (row index, letter) of each letter header.
    letters: Vec<(usize, char)>,
    // Search.
    results: Vec<(Section, Row)>,
    /// Result shown in the detail panel.
    detail: usize,
    detail_actions: Vec<Action>,
    file_paths: Vec<PathBuf>,
    command: Option<RunCommand>,
    /// Bumped on every query change; stale async answers are dropped.
    generation: u64,
    /// Settings-table entries whose required program exists (None = not
    /// checked yet: entries with a requirement are hidden).
    tools: Option<HashSet<&'static str>>,
    // Interaction.
    focus: Focus,
    hover: Option<Hit>,
    /// Draw the keyboard focus ring (after keyboard navigation only).
    focus_visible: bool,
    /// Transient message ("Copied") and when it was set.
    status: Option<(String, Instant)>,
    hits: Vec<(Hit, Rect)>,
    /// Scroll offset of the list of the current view, physical pixels.
    scroll: f32,
    // Background work.
    indexing: bool,
    indexed_at: Option<Instant>,
    index_rx: Option<Receiver<IndexMsg>>,
    workers: Option<Workers>,
    #[cfg(any(feature = "everything", feature = "file-index"))]
    files: Option<crate::files::FileWorker>,
    // Window.
    hidden_at: Option<Instant>,
    surface: Option<Surface>,
    scale: f32,
    edit: HWND,
    edit_font: Option<HFONT>,
    edit_font_scale: f32,
    edit_brush: Option<HBRUSH>,
    edit_brush_color: u32,
    edit_shared: Rc<Cell<HWND>>,
    launcher_msg: u32,
    query: String,
    tracking: bool,
    last_mouse: Option<(i32, i32)>,
    tooltip: Option<Tooltip>,
    tooltip_for: Option<Hit>,
}

fn is_visible(hwnd: HWND) -> bool {
    // SAFETY: plain query.
    unsafe { explorer_win::windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd) }
        .as_bool()
}

fn invalidate(hwnd: HWND) {
    // SAFETY: repaint request on our own window.
    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
}

fn client_size(hwnd: HWND) -> (f32, f32) {
    let mut rc = RECT::default();
    // SAFETY: valid out-pointer.
    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
    ((rc.right - rc.left) as f32, (rc.bottom - rc.top) as f32)
}

/// 0xRRGGBB -> COLORREF (0x00BBGGRR).
fn bgr(rgb: u32) -> u32 {
    ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff)
}

/// Where a popup menu opens: at the mouse, or above a button (keyboard).
enum MenuAt {
    Cursor,
    Above(HWND, Rect),
}

/// Show a popup menu; `items` with id 0 are separators. Returns the chosen
/// id or 0.
fn popup_menu(owner: HWND, items: &[(u32, String)], at: MenuAt) -> u32 {
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
                let _ = AppendMenuW(menu, MF_STRING, *id as usize, PCWSTR(w.as_ptr()));
            }
        }
        let mut flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY;
        let mut pt = POINT::default();
        match at {
            MenuAt::Cursor => {
                let _ = GetCursorPos(&mut pt);
            }
            MenuAt::Above(hwnd, r) => {
                pt = POINT {
                    x: r.x as i32,
                    y: r.y as i32,
                };
                let _ = ClientToScreen(hwnd, &mut pt);
                flags |= TPM_LEFTALIGN | TPM_BOTTOMALIGN;
            }
        }
        let r = TrackPopupMenu(menu, flags, pt.x, pt.y, None, owner, None);
        let _ = DestroyMenu(menu);
        r.0 as u32
    }
}

const CMD_LOCK: u32 = 20;
const CMD_SIGN_OUT: u32 = 21;
const CMD_SLEEP: u32 = 22;
const CMD_HIBERNATE: u32 = 23;
const CMD_SHUTDOWN: u32 = 24;
const CMD_RESTART: u32 = 25;
const CMD_ACCOUNT: u32 = 26;
const CMD_RESTART_START: u32 = 27;
const CMD_START_SETTINGS: u32 = 28;
const CMD_MOVE_FRONT: u32 = 100;
/// Context-menu ids of actions: CMD_ACTION_BASE + index.
const CMD_ACTION_BASE: u32 = 200;

fn action_glyph(a: Action, kind: ResultKind) -> &'static str {
    match a {
        Action::Open => match kind {
            ResultKind::Command => g::PLAY,
            ResultKind::Calc => g::COPY,
            _ => g::OPEN,
        },
        Action::RunAsAdmin => g::SHIELD,
        Action::OpenLocation => g::FOLDER_OPEN,
        Action::CopyPath | Action::CopyResult => g::COPY,
        Action::PinStart | Action::PinTaskbar => g::PIN,
        Action::UnpinStart | Action::UnpinTaskbar => g::UNPIN,
        Action::Uninstall => g::DELETE,
        Action::RemoveFromList => g::CANCEL,
    }
}

fn kind_glyph(kind: ResultKind) -> &'static str {
    match kind {
        ResultKind::App => glyph::APP,
        ResultKind::Tool => g::REPAIR,
        ResultKind::Setting => glyph::SETTINGS,
        ResultKind::File => g::DOCUMENT,
        ResultKind::Folder => g::FOLDER,
        ResultKind::Command => g::COMMAND,
        ResultKind::Calc => g::CALCULATOR,
    }
}

impl Launcher {
    fn s(&self, v: f32) -> f32 {
        v * self.scale
    }

    fn reload_palette(&mut self) {
        self.palette = Palette::from_system(
            &explorer_win::theme::system_theme(),
            self.config.general.theme.to_theme_mode(),
        );
        if self.edit_brush.is_none() || self.edit_brush_color != self.palette.input_bg {
            // SAFETY: GDI brush lifetime managed here.
            unsafe {
                if let Some(old) = self.edit_brush.take() {
                    let _ = DeleteObject(HGDIOBJ(old.0));
                }
                self.edit_brush = Some(CreateSolidBrush(COLORREF(bgr(self.palette.input_bg))));
            }
            self.edit_brush_color = self.palette.input_bg;
        }
        self.lang = explorer_win::theme::ui_lang(self.config.general.language);
        crate::cue::set(
            self.edit,
            tr(
                self.lang,
                "Search for apps, settings, and documents",
                "Buscar aplicaciones, configuración y documentos",
            ),
            self.palette.text_dim,
        );
    }

    // ---------------------------------------------------------------
    // Background work
    // ---------------------------------------------------------------

    fn start_indexing(&mut self, hwnd: HWND) {
        if self.indexing {
            return;
        }
        self.indexing = true;
        let (tx, rx) = channel();
        self.index_rx = Some(rx);
        let extra = self.config.launcher.extra_folders.clone();
        let target = hwnd.0 as usize;
        let r = std::thread::Builder::new()
            .name("start-index".into())
            .spawn(move || {
                let _com = ComApartment::sta();
                let post = || {
                    // SAFETY: posting to the launcher window.
                    let _ = unsafe {
                        PostMessageW(
                            Some(HWND(target as *mut _)),
                            WM_APP_INDEXED,
                            WPARAM(0),
                            LPARAM(0),
                        )
                    };
                };
                let entries = index_apps(&extra);
                let targets: Vec<LaunchTarget> = entries.iter().map(|e| e.target.clone()).collect();
                if tx.send(IndexMsg::Catalog(entries)).is_err() {
                    return;
                }
                post();
                let started = Instant::now();
                let icons: HashMap<String, SharedImage> = targets
                    .iter()
                    .filter_map(|t| {
                        explorer_win::icons::for_target(t, ICON_PX).map(|i| (t.key(), i))
                    })
                    .collect();
                tracing::info!(
                    icons = icons.len(),
                    ms = started.elapsed().as_millis() as u64,
                    "extracted app icons"
                );
                if tx.send(IndexMsg::Icons(icons)).is_ok() {
                    post();
                }
            });
        if let Err(e) = r {
            tracing::error!(error = %e, "cannot start indexing thread");
            self.indexing = false;
        }
    }

    fn send_job(&self, job: Job) {
        if let Some(w) = &self.workers {
            w.send(job);
        }
    }

    /// Ask the icon worker for icons of `rows` not cached yet. App icons
    /// come in bulk from the indexer, so they are only requested once it
    /// is done (e.g. for apps installed later).
    fn request_icons<'a>(&mut self, rows: impl IntoIterator<Item = &'a Row>) {
        let mut want = Vec::new();
        for r in rows {
            let Some(t) = r.launch_target() else {
                continue;
            };
            if self.indexing && matches!(t, LaunchTarget::AppsFolder(_)) {
                continue;
            }
            let k = t.key();
            if !self.icons.contains_key(&k) && self.icons_requested.insert(k) {
                want.push(t);
            }
        }
        if !want.is_empty() {
            self.send_job(Job::Icons(want));
        }
    }

    fn on_worker(&mut self, hwnd: HWND) {
        let mut replies = Vec::new();
        if let Some(w) = &self.workers {
            while let Ok(r) = w.replies.try_recv() {
                replies.push(r);
            }
        }
        let mut research = false;
        for r in replies {
            match r {
                Reply::Icons(v) => {
                    for (k, img) in v {
                        self.icons.insert(k, img);
                    }
                }
                Reply::Resolved {
                    generation,
                    command,
                } => {
                    if generation == self.generation && command.is_some() {
                        self.command = command;
                        research = true;
                    }
                }
                Reply::Recent(files) => {
                    self.recent = files;
                    self.rebuild_home();
                }
                Reply::Avatar(img) => self.avatar = img,
                Reply::Tools(ids) => {
                    self.tools = Some(ids.into_iter().collect());
                    research = true;
                }
            }
        }
        if research && self.view == View::Search {
            self.run_search(true);
        }
        invalidate(hwnd);
    }

    // ---------------------------------------------------------------
    // Models of the views
    // ---------------------------------------------------------------

    fn rebuild_home(&mut self) {
        let lang = self.lang;
        self.pinned = self
            .pins
            .resolve(&self.catalog)
            .into_iter()
            .map(|e| Row::app(e, lang))
            .collect();
        let pages = page_count(self.pinned.len(), PINS_PER_PAGE);
        self.pin_page = self.pin_page.min(pages - 1);
        let now = unix_now();
        let mut used: Vec<(&AppEntry, u32)> = self
            .catalog
            .entries()
            .iter()
            .filter(|e| !self.pins.contains(&e.target))
            .map(|e| (e, self.history.boost(&e.target, now)))
            .filter(|(_, b)| *b > 0)
            .collect();
        used.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
        let used: Vec<&AppEntry> = used.into_iter().take(REC_MAX).map(|(e, _)| e).collect();
        self.recommended = model::recommended(
            lang,
            &self.recent,
            &used,
            now as i64 + self.utc_offset,
            self.utc_offset,
            REC_MAX,
        );
        let rows: Vec<Row> = self
            .pinned
            .iter()
            .chain(self.recommended.iter())
            .cloned()
            .collect();
        self.request_icons(rows.iter());
    }

    fn rebuild_all(&mut self) {
        let lang = self.lang;
        self.all = self
            .catalog
            .entries()
            .iter()
            .map(|e| Row::app(e, lang))
            .collect();
        self.letters.clear();
        let mut last = None;
        for (i, r) in self.all.iter().enumerate() {
            let l = letter_of(&r.name);
            if last != Some(l) {
                self.letters.push((i, l));
                last = Some(l);
            }
        }
    }

    /// Re-run the search for the current query. `keep_focus` keeps the
    /// focused result (by name) when asynchronous results arrive.
    fn run_search(&mut self, keep_focus: bool) {
        let focused_name = match (keep_focus, self.focus) {
            (true, Some((Z_RESULTS, i))) => self.results.get(i).map(|(_, r)| r.name.clone()),
            _ => None,
        };
        let tools = self.tools.clone();
        let available = move |e: &SettingsEntry| match &tools {
            Some(set) => set.contains(e.id),
            None => e.requires.is_none(),
        };
        let max_files = self.max_file_results();
        let limits = Limits {
            apps: self.config.launcher.max_results.clamp(1, 12),
            settings: 4,
            documents: max_files,
            commands: 2,
        };
        self.results = model::search(
            &mut self.searcher,
            &SearchInput {
                query: &self.query,
                lang: self.lang,
                catalog: &self.catalog,
                history: &self.history,
                now: unix_now(),
                files: &self.file_paths,
                command: self.command.clone(),
                tool_available: &available,
                limits,
            },
        );
        let idx = focused_name
            .and_then(|n| self.results.iter().position(|(_, r)| r.name == n))
            .unwrap_or(0);
        if !keep_focus || matches!(self.focus, Some((Z_RESULTS, _)) | None) {
            self.focus = (!self.results.is_empty()).then_some((Z_RESULTS, idx));
            self.detail = idx;
        } else {
            self.detail = self.detail.min(self.results.len().saturating_sub(1));
        }
        if !keep_focus {
            self.scroll = 0.0;
        }
        self.refresh_detail_actions();
        let rows: Vec<Row> = self.results.iter().map(|(_, r)| r.clone()).collect();
        self.request_icons(rows.iter());
    }

    fn max_file_results(&self) -> usize {
        #[cfg(any(feature = "everything", feature = "file-index"))]
        {
            if crate::files::enabled(&self.config) {
                return crate::files::max_results(&self.config) as usize;
            }
        }
        0
    }

    /// Query changed: start the asynchronous parts, then search.
    fn on_query_changed(&mut self, hwnd: HWND) {
        self.generation += 1;
        self.file_paths.clear();
        self.command = None;
        self.status = None;
        let q = self.query.trim().to_string();
        if q.is_empty() {
            self.set_view(hwnd, View::Home);
            return;
        }
        match classify(&q) {
            Some(c) if c.kind == RunKind::Program => self.send_job(Job::Resolve {
                generation: self.generation,
                command: c,
            }),
            Some(c) => self.command = Some(c),
            None => {}
        }
        #[cfg(any(feature = "everything", feature = "file-index"))]
        {
            if crate::files::enabled(&self.config) && !q.starts_with('>') {
                if self.files.is_none() {
                    self.files = Some(crate::files::FileWorker::start(
                        hwnd,
                        WM_APP_FILES,
                        crate::files::max_results(&self.config),
                    ));
                }
                if let Some(f) = &self.files {
                    f.query(self.generation, &q);
                }
            }
        }
        self.view = View::Search;
        self.run_search(false);
        invalidate(hwnd);
    }

    fn set_view(&mut self, hwnd: HWND, view: View) {
        let prev = self.view;
        self.view = view;
        self.hover = None;
        match view {
            View::Home => {
                self.rebuild_home();
                self.focus = None;
                self.scroll = 0.0;
            }
            View::AllApps => {
                if self.all.len() != self.catalog.len() || self.all.is_empty() {
                    self.rebuild_all();
                }
                if prev != View::Letters {
                    self.scroll = 0.0;
                    self.focus = None;
                }
                let rows: Vec<Row> = self.all.clone();
                self.request_icons(rows.iter());
            }
            View::Letters => {
                self.focus = Some((Z_LETTERS, 0));
            }
            View::Search => self.run_search(false),
        }
        invalidate(hwnd);
    }

    fn read_query(&mut self) {
        // SAFETY: buffer sized from GetWindowTextLengthW.
        unsafe {
            let len = GetWindowTextLengthW(self.edit).max(0) as usize;
            let mut buf = vec![0u16; len + 1];
            let n = GetWindowTextW(self.edit, &mut buf).max(0) as usize;
            self.query = String::from_utf16_lossy(&buf[..n.min(len)]);
        }
    }

    fn clear_query(&mut self) {
        let empty = wide("");
        // SAFETY: our own control. The EN_CHANGE it sends re-enters the
        // window procedure and is dropped; state is reset here instead.
        let _ = unsafe { SetWindowTextW(self.edit, PCWSTR(empty.as_ptr())) };
        self.query.clear();
        self.generation += 1;
        self.file_paths.clear();
        self.command = None;
        self.results.clear();
    }

    // ---------------------------------------------------------------
    // Rows, focus and actions
    // ---------------------------------------------------------------

    fn pin_rows_on_page(&self) -> &[Row] {
        &self.pinned[page_range(self.pinned.len(), PINS_PER_PAGE, self.pin_page)]
    }

    fn zones(&self) -> Vec<Zone> {
        let footer = Zone::new(ZoneKind::Row, FOOTER.len()).tab_each();
        match self.view {
            View::Home => vec![
                Zone::new(
                    ZoneKind::Grid { cols: PIN_COLS },
                    self.pin_rows_on_page().len(),
                ),
                Zone::new(ZoneKind::Grid { cols: REC_COLS }, self.recommended.len()),
                footer,
                Zone::new(ZoneKind::Row, 1).contained(),
            ],
            View::AllApps => vec![
                Zone::new(ZoneKind::List, self.all.len()),
                footer,
                Zone::new(ZoneKind::Row, 1).contained(),
            ],
            View::Letters => vec![Zone::new(
                ZoneKind::Grid { cols: LETTER_COLS },
                self.letters.len(),
            )],
            View::Search => vec![
                Zone::new(ZoneKind::List, self.results.len()),
                Zone::new(ZoneKind::List, self.detail_actions.len())
                    .tab_each()
                    .contained(),
                footer,
            ],
        }
    }

    fn footer_zone(&self) -> Option<usize> {
        match self.view {
            View::Home => Some(Z_HOME_FOOTER),
            View::AllApps => Some(Z_ALL_FOOTER),
            View::Search => Some(Z_SEARCH_FOOTER),
            View::Letters => None,
        }
    }

    /// The row behind a focusable item, if it is one.
    fn row_at(&self, z: usize, i: usize) -> Option<&Row> {
        match (self.view, z) {
            (View::Home, Z_PINNED) => self.pin_rows_on_page().get(i),
            (View::Home, Z_REC) => self.recommended.get(i),
            (View::AllApps, Z_LIST) => self.all.get(i),
            (View::Search, Z_RESULTS) => self.results.get(i).map(|(_, r)| r),
            _ => None,
        }
    }

    fn pin_states(&mut self, row: &Row) -> (bool, bool) {
        match &row.target {
            Target::App(t) => (
                self.pins.contains(t),
                self.taskbar_pins.contains(t, &self.catalog),
            ),
            _ => (false, false),
        }
    }

    fn row_actions(&mut self, row: &Row) -> Vec<Action> {
        let (ps, pt) = self.pin_states(row);
        actions(row.kind, &row.action_context(ps, pt))
    }

    fn refresh_detail_actions(&mut self) {
        self.detail_actions = match self.results.get(self.detail).map(|(_, r)| r.clone()) {
            Some(row) => self.row_actions(&row),
            None => Vec::new(),
        };
    }

    fn set_focus(&mut self, hwnd: HWND, f: Focus) {
        self.focus = f;
        if let (View::Search, Some((Z_RESULTS, i))) = (self.view, f) {
            if self.detail != i {
                self.detail = i;
                self.refresh_detail_actions();
            }
        }
        if let Some((z, i)) = f {
            self.ensure_visible(hwnd, z, i);
        }
        invalidate(hwnd);
    }

    fn launch_target(&mut self, hwnd: HWND, target: LaunchTarget, elevated: bool, record: bool) {
        self.hide(hwnd);
        tracing::info!(target = ?target, elevated, "launching");
        if record {
            self.history.record_now(&target);
            if let Err(e) = self.history.save(&self.paths.history_file()) {
                tracing::warn!(error = %e, "cannot save launch history");
            }
        }
        crate::sys::spawn_com("start-launch", move || {
            explorer_win::apps::launch(&target, elevated)
        });
    }

    fn shell(&mut self, hwnd: HWND, file: String, args: String, elevated: bool) {
        self.hide(hwnd);
        tracing::info!(%file, %args, elevated, "running");
        crate::sys::spawn_com("start-run", move || {
            crate::sys::shell_run(&file, &args, elevated)
        });
    }

    fn show_status(&mut self, hwnd: HWND, text: &str) {
        self.status = Some((text.to_string(), Instant::now()));
        // SAFETY: timer on our own window.
        unsafe {
            SetTimer(
                Some(hwnd),
                STATUS_TIMER,
                STATUS_FOR.as_millis() as u32,
                None,
            )
        };
        invalidate(hwnd);
    }

    fn copy(&mut self, hwnd: HWND, text: &str) {
        let lang = self.lang;
        if crate::sys::copy_to_clipboard(hwnd, text) {
            self.show_status(hwnd, tr(lang, "Copied", "Copiado"));
        } else {
            self.show_status(hwnd, tr(lang, "Could not copy", "No se pudo copiar"));
        }
    }

    /// Run `action` on `row`. `elevated` turns Open into Run as administrator
    /// where that makes sense (Ctrl+Shift+Enter).
    fn run_action(&mut self, hwnd: HWND, row: Row, action: Action, elevated: bool) {
        let elevated_ok = row.action_context(false, false).elevatable;
        match action {
            Action::Open | Action::RunAsAdmin => {
                let elevated = (action == Action::RunAsAdmin || elevated) && elevated_ok;
                match row.target {
                    Target::App(t) => self.launch_target(hwnd, t, elevated, true),
                    Target::File { path, .. } => {
                        self.launch_target(hwnd, LaunchTarget::Path(path), false, false)
                    }
                    Target::Setting(e) => {
                        let elevated = elevated && e.kind == EntryKind::Tool;
                        self.shell(hwnd, e.target.into(), e.args.into(), elevated)
                    }
                    Target::Command(c) => self.run_command(hwnd, c, elevated),
                    Target::Calc(text) => self.copy(hwnd, &text),
                }
            }
            Action::OpenLocation => {
                if let Some(p) = row.location() {
                    let p = p.to_path_buf();
                    self.hide(hwnd);
                    crate::sys::open_location(&p);
                }
            }
            Action::CopyPath => {
                if let Some(p) = row.location() {
                    let s = p.to_string_lossy().into_owned();
                    self.copy(hwnd, &s);
                }
            }
            Action::CopyResult => {
                if let Target::Calc(text) = &row.target {
                    let t = text.clone();
                    self.copy(hwnd, &t);
                }
            }
            Action::PinStart | Action::UnpinStart => {
                if let Target::App(t) = &row.target {
                    if action == Action::PinStart {
                        self.pins.pin(t);
                    } else {
                        self.pins.unpin(t);
                    }
                    self.save_pins();
                    self.after_pin_change(hwnd);
                }
            }
            Action::PinTaskbar | Action::UnpinTaskbar => {
                if let Target::App(t) = &row.target {
                    self.taskbar_pins
                        .set(t, action == Action::PinTaskbar, &self.catalog);
                    self.after_pin_change(hwnd);
                }
            }
            Action::Uninstall => self.shell(
                hwnd,
                "ms-settings:appsfeatures".into(),
                String::new(),
                false,
            ),
            Action::RemoveFromList => {
                if let Target::File {
                    recent_shortcut: Some(lnk),
                    ..
                } = &row.target
                {
                    if let Err(e) = std::fs::remove_file(lnk) {
                        tracing::warn!(error = %e, "cannot remove recent item");
                    }
                    let lnk = lnk.clone();
                    self.recent.retain(|f| f.shortcut != lnk);
                    self.rebuild_home();
                    self.clamp_focus();
                    invalidate(hwnd);
                }
            }
        }
    }

    fn after_pin_change(&mut self, hwnd: HWND) {
        if self.view == View::Home {
            self.rebuild_home();
            self.clamp_focus();
        }
        self.refresh_detail_actions();
        invalidate(hwnd);
    }

    fn clamp_focus(&mut self) {
        let zones = self.zones();
        if let Some((z, i)) = self.focus {
            match zones.get(z) {
                Some(zz) if zz.len == 0 => self.focus = None,
                Some(zz) if i >= zz.len => self.focus = Some((z, zz.len - 1)),
                None => self.focus = None,
                _ => {}
            }
        }
    }

    fn run_command(&mut self, hwnd: HWND, c: RunCommand, elevated: bool) {
        match c.kind {
            RunKind::Shell => self.shell(
                hwnd,
                "cmd.exe".into(),
                format!("/k {}", c.program),
                elevated,
            ),
            RunKind::Path => {
                let file = crate::sys::expand_env(&c.program);
                self.shell(hwnd, file, c.args, false)
            }
            RunKind::Program => self.shell(hwnd, c.program, c.args, elevated),
        }
    }

    fn save_pins(&self) {
        if let Err(e) = self.pins.save(&self.paths.pins_file()) {
            tracing::warn!(error = %e, "cannot save pins");
        }
    }

    /// Enter / click on a focusable item.
    fn activate(&mut self, hwnd: HWND, z: usize, i: usize, elevated: bool) {
        if Some(z) == self.footer_zone() {
            if let Some(b) = FOOTER.get(i).copied() {
                self.footer(hwnd, b, i);
            }
            return;
        }
        match (self.view, z) {
            (View::Home, Z_ALLAPPS_BTN) => self.set_view(hwnd, View::AllApps),
            (View::AllApps, Z_BACK) => self.set_view(hwnd, View::Home),
            (View::Letters, Z_LETTERS) => {
                if let Some(&(row, _)) = self.letters.get(i) {
                    self.set_view(hwnd, View::AllApps);
                    self.scroll_to_row(hwnd, row, true);
                    self.focus = Some((Z_LIST, row));
                    invalidate(hwnd);
                }
            }
            (View::Search, Z_ACTIONS) => {
                let row = self.results.get(self.detail).map(|(_, r)| r.clone());
                if let (Some(row), Some(&a)) = (row, self.detail_actions.get(i)) {
                    self.run_action(hwnd, row, a, false);
                }
            }
            _ => {
                if let Some(row) = self.row_at(z, i).cloned() {
                    self.run_action(hwnd, row, Action::Open, elevated);
                }
            }
        }
    }

    fn footer(&mut self, hwnd: HWND, b: FooterButton, i: usize) {
        let rect = self
            .hits
            .iter()
            .find(|(h, _)| *h == Hit::Item(self.footer_zone().unwrap_or(0), i))
            .map(|(_, r)| *r);
        let at = |r: Option<Rect>| match r {
            Some(r) => MenuAt::Above(hwnd, r),
            None => MenuAt::Cursor,
        };
        match b {
            FooterButton::User => self.user_menu(hwnd, at(rect)),
            FooterButton::More => self.more_menu(hwnd, at(rect)),
            FooterButton::Files => {
                self.shell(hwnd, "explorer.exe".into(), String::new(), false);
            }
            FooterButton::Settings => {
                self.shell(hwnd, "ms-settings:".into(), String::new(), false);
            }
            FooterButton::Power => self.power_menu(hwnd, at(rect)),
        }
    }

    fn user_menu(&mut self, hwnd: HWND, at: MenuAt) {
        let l = self.lang;
        let cmd = popup_menu(
            hwnd,
            &[
                (
                    CMD_ACCOUNT,
                    tr(
                        l,
                        "Change account settings",
                        "Cambiar la configuración de la cuenta",
                    )
                    .into(),
                ),
                (0, String::new()),
                (CMD_LOCK, tr(l, "Lock", "Bloquear").into()),
                (CMD_SIGN_OUT, tr(l, "Sign out", "Cerrar sesión").into()),
            ],
            at,
        );
        invalidate(hwnd);
        match cmd {
            CMD_ACCOUNT => self.shell(hwnd, "ms-settings:accounts".into(), String::new(), false),
            0 => {}
            c => self.power_command(hwnd, c),
        }
    }

    fn more_menu(&mut self, hwnd: HWND, at: MenuAt) {
        let l = self.lang;
        let cmd = popup_menu(
            hwnd,
            &[
                (
                    CMD_START_SETTINGS,
                    tr(l, "Start settings", "Configuración de Inicio").into(),
                ),
                (0, String::new()),
                (
                    CMD_RESTART_START,
                    tr(l, "Restart Start menu", "Reiniciar el menú Inicio").into(),
                ),
            ],
            at,
        );
        invalidate(hwnd);
        match cmd {
            CMD_START_SETTINGS => self.shell(
                hwnd,
                "ms-settings:personalization-start".into(),
                String::new(),
                false,
            ),
            CMD_RESTART_START => {
                // The supervisor restarts a component that exits with
                // EXIT_RESTART immediately.
                tracing::info!("restart requested from the Start menu");
                self.hide(hwnd);
                window::quit(EXIT_RESTART);
            }
            _ => {}
        }
    }

    fn power_menu(&mut self, hwnd: HWND, at: MenuAt) {
        let l = self.lang;
        let (sleep, hibernate) = crate::sys::power_capabilities();
        let mut items: Vec<(u32, String)> = vec![
            (CMD_LOCK, tr(l, "Lock", "Bloquear").into()),
            (CMD_SIGN_OUT, tr(l, "Sign out", "Cerrar sesión").into()),
            (0, String::new()),
        ];
        if sleep {
            items.push((CMD_SLEEP, tr(l, "Sleep", "Suspender").into()));
        }
        if hibernate {
            items.push((CMD_HIBERNATE, tr(l, "Hibernate", "Hibernar").into()));
        }
        items.push((CMD_SHUTDOWN, tr(l, "Shut down", "Apagar").into()));
        items.push((CMD_RESTART, tr(l, "Restart", "Reiniciar").into()));
        let cmd = popup_menu(hwnd, &items, at);
        invalidate(hwnd);
        if cmd != 0 {
            self.power_command(hwnd, cmd);
        }
    }

    fn power_command(&mut self, hwnd: HWND, cmd: u32) {
        self.hide(hwnd);
        let run = |args: &[&str]| {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            if let Err(e) = std::process::Command::new("shutdown.exe")
                .args(args)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
            {
                tracing::error!(error = %e, "shutdown.exe failed");
            }
        };
        tracing::info!(cmd, "power command");
        match cmd {
            CMD_LOCK => {
                // SAFETY: plain FFI call.
                let _ = unsafe { LockWorkStation() };
            }
            CMD_SIGN_OUT => {
                // SAFETY: plain FFI call.
                let _ = unsafe { ExitWindowsEx(EWX_LOGOFF, SHUTDOWN_REASON(0)) };
            }
            CMD_SLEEP => crate::sys::suspend(false),
            CMD_HIBERNATE => crate::sys::suspend(true),
            CMD_SHUTDOWN => run(&["/s", "/t", "0"]),
            CMD_RESTART => run(&["/r", "/t", "0"]),
            _ => {}
        }
    }

    fn context_menu(&mut self, hwnd: HWND, z: usize, i: usize, at: MenuAt) {
        let Some(row) = self.row_at(z, i).cloned() else {
            return;
        };
        let lang = self.lang;
        let acts = self.row_actions(&row);
        let mut items: Vec<(u32, String)> = acts
            .iter()
            .enumerate()
            .map(|(k, a)| {
                (
                    CMD_ACTION_BASE + k as u32,
                    a.label(row.kind, lang).to_string(),
                )
            })
            .collect();
        let pinned_item = self.view == View::Home && z == Z_PINNED;
        if pinned_item && (self.pin_page > 0 || i > 0) {
            items.insert(
                1,
                (
                    CMD_MOVE_FRONT,
                    tr(lang, "Move to front", "Mover al principio").into(),
                ),
            );
        }
        let cmd = popup_menu(hwnd, &items, at);
        invalidate(hwnd);
        if cmd == CMD_MOVE_FRONT {
            if let Target::App(t) = &row.target {
                self.pins.move_to_front(t);
                self.save_pins();
                self.pin_page = 0;
                self.after_pin_change(hwnd);
            }
        } else if cmd >= CMD_ACTION_BASE {
            if let Some(&a) = acts.get((cmd - CMD_ACTION_BASE) as usize) {
                self.run_action(hwnd, row, a, false);
            }
        }
    }

    // ---------------------------------------------------------------
    // Keyboard
    // ---------------------------------------------------------------

    fn on_nav_key(&mut self, hwnd: HWND, vk: u32) {
        // SAFETY: plain key-state queries.
        let (ctrl, shift) = unsafe {
            (
                GetKeyState(VK_CONTROL.0 as i32) < 0,
                GetKeyState(VK_SHIFT.0 as i32) < 0,
            )
        };
        match vk {
            VK_ESCAPE => {
                if !self.query.is_empty() {
                    self.clear_query();
                    self.set_view(hwnd, View::Home);
                } else if self.view == View::Letters {
                    self.set_view(hwnd, View::AllApps);
                } else if self.view != View::Home {
                    self.set_view(hwnd, View::Home);
                } else {
                    self.hide(hwnd);
                }
                return;
            }
            VK_RETURN => {
                if let Some((z, i)) = self.focus {
                    self.activate(hwnd, z, i, ctrl && shift);
                }
                return;
            }
            VK_APPS => {
                if let Some((z, i)) = self.focus {
                    let r = self
                        .hits
                        .iter()
                        .find(|(h, _)| *h == Hit::Item(z, i))
                        .map(|(_, r)| *r);
                    if let Some(r) = r {
                        let below = Rect::new(r.x, r.bottom(), r.w, 0.0);
                        self.context_menu(hwnd, z, i, MenuAt::Above(hwnd, below));
                    }
                }
                return;
            }
            VK_PRIOR | VK_NEXT => {
                let down = vk == VK_NEXT;
                match self.view {
                    View::Home => self.change_page(hwnd, if down { 1 } else { -1 }),
                    _ => {
                        let (w, h) = client_size(hwnd);
                        let page = self.list_area(w, h).h * 0.9;
                        self.scroll_by(hwnd, if down { page } else { -page });
                    }
                }
                return;
            }
            _ => {}
        }
        self.focus_visible = true;
        let zones = self.zones();
        let next = match vk {
            VK_TAB => results::tab(&zones, self.focus, !shift),
            VK_UP => results::arrow(&zones, self.focus, Arrow::Up),
            VK_DOWN => results::arrow(&zones, self.focus, Arrow::Down),
            VK_LEFT => results::arrow(&zones, self.focus, Arrow::Left),
            VK_RIGHT => results::arrow(&zones, self.focus, Arrow::Right),
            _ => self.focus,
        };
        self.set_focus(hwnd, next);
    }

    fn change_page(&mut self, hwnd: HWND, delta: i32) {
        let pages = page_count(self.pinned.len(), PINS_PER_PAGE) as i32;
        let p = (self.pin_page as i32 + delta).clamp(0, pages - 1) as usize;
        if p != self.pin_page {
            self.pin_page = p;
            if matches!(self.focus, Some((Z_PINNED, _))) {
                self.focus = Some((Z_PINNED, 0));
            }
            invalidate(hwnd);
        }
    }

    // ---------------------------------------------------------------
    // Geometry
    // ---------------------------------------------------------------

    fn search_rect(&self, w: f32) -> Rect {
        Rect::new(
            self.s(PAD),
            self.s(PAD),
            w - 2.0 * self.s(PAD),
            self.s(SEARCH_H),
        )
    }

    fn body_top(&self) -> f32 {
        self.s(PAD + SEARCH_H + 16.0)
    }

    fn footer_rect(&self, w: f32, h: f32) -> Rect {
        Rect::new(0.0, h - self.s(FOOTER_H), w, self.s(FOOTER_H))
    }

    /// Scrolling list area of All apps / Search (window coordinates).
    fn list_area(&self, w: f32, h: f32) -> Rect {
        let footer_y = h - self.s(FOOTER_H);
        match self.view {
            View::Search => {
                let top = self.body_top();
                let left = self.s(PAD) - self.s(8.0);
                let width = ((w - 2.0 * self.s(PAD)) * 0.56).max(self.s(200.0));
                Rect::new(left, top, width, footer_y - self.s(12.0) - top)
            }
            _ => {
                let top = self.body_top() + self.s(HEADER_H + 8.0);
                Rect::new(
                    self.s(PAD) - self.s(8.0),
                    top,
                    w - 2.0 * self.s(PAD) + self.s(16.0),
                    footer_y - self.s(8.0) - top,
                )
            }
        }
    }

    /// Content-space geometry of the All apps list: row rects (y, h) and
    /// letter header y positions; total height.
    fn all_geometry(&self) -> (Vec<(f32, f32)>, Vec<f32>, f32) {
        let mut y = 0.0;
        let mut rows = Vec::with_capacity(self.all.len());
        let mut headers = Vec::with_capacity(self.letters.len());
        let mut li = 0;
        for i in 0..self.all.len() {
            if self.letters.get(li).is_some_and(|(at, _)| *at == i) {
                headers.push(y);
                li += 1;
                y += self.s(LETTER_H);
            }
            rows.push((y, self.s(ROW_H)));
            y += self.s(ROW_H);
        }
        (rows, headers, y)
    }

    /// Content-space geometry of the search results: per row an optional
    /// section header y, and the row (y, h); total height.
    fn search_geometry(&self) -> (Vec<(Option<f32>, f32, f32)>, f32) {
        let headers = header_rows(&self.results);
        let mut y = 0.0;
        let mut out = Vec::with_capacity(self.results.len());
        for (i, (section, _)) in self.results.iter().enumerate() {
            let header = if headers.contains(&i) {
                let hy = y;
                y += self.s(SECTION_H);
                Some(hy)
            } else {
                None
            };
            let h = if *section == Section::BestMatch {
                self.s(BEST_H)
            } else {
                self.s(RESULT_H)
            };
            out.push((header, y, h));
            y += h;
        }
        (out, y)
    }

    fn content_height(&self) -> f32 {
        match self.view {
            View::AllApps => self.all_geometry().2,
            View::Search => self.search_geometry().1,
            _ => 0.0,
        }
    }

    fn scroll_by(&mut self, hwnd: HWND, dy: f32) {
        if !matches!(self.view, View::AllApps | View::Search) {
            return;
        }
        let (w, h) = client_size(hwnd);
        let area = self.list_area(w, h);
        let max = (self.content_height() - area.h).max(0.0);
        self.scroll = (self.scroll + dy).clamp(0.0, max);
        invalidate(hwnd);
    }

    /// Scroll so that list row `i` is visible (`top`: put it at the top).
    fn scroll_to_row(&mut self, hwnd: HWND, i: usize, top: bool) {
        let (y, rh, header) = match self.view {
            View::AllApps => {
                let (rows, headers, _) = self.all_geometry();
                let Some(&(y, h)) = rows.get(i) else { return };
                // Include the letter header just above the row.
                let hdr = headers
                    .iter()
                    .rev()
                    .copied()
                    .find(|hy| *hy <= y && y - *hy <= self.s(LETTER_H) + 0.5);
                (y, h, hdr)
            }
            View::Search => {
                let (rows, _) = self.search_geometry();
                let Some(&(hdr, y, h)) = rows.get(i) else {
                    return;
                };
                (y, h, hdr)
            }
            _ => return,
        };
        let (w, h) = client_size(hwnd);
        let area = self.list_area(w, h);
        let top_y = header.unwrap_or(y);
        if top || top_y < self.scroll {
            self.scroll = top_y;
        } else if y + rh > self.scroll + area.h {
            self.scroll = y + rh - area.h;
        }
        let max = (self.content_height() - area.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn ensure_visible(&mut self, hwnd: HWND, z: usize, i: usize) {
        let is_list = matches!(
            (self.view, z),
            (View::AllApps, Z_LIST) | (View::Search, Z_RESULTS)
        );
        if is_list {
            self.scroll_to_row(hwnd, i, false);
        }
    }

    fn hit(&self, lparam: LPARAM) -> Option<Hit> {
        let x = (lparam.0 & 0xffff) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        self.hits
            .iter()
            .rev()
            .find(|(_, r)| r.contains(x, y))
            .map(|(h, _)| *h)
    }

    fn icon_of(&self, row: &Row) -> Option<SharedImage> {
        row.icon_key().and_then(|k| self.icons.get(&k).cloned())
    }

    // ---------------------------------------------------------------
    // Window management
    // ---------------------------------------------------------------

    fn apply_dpi(&mut self, hwnd: HWND, scale: f32) {
        self.scale = scale;
        if (self.edit_font_scale - scale).abs() > f32::EPSILON || self.edit_font.is_none() {
            // SAFETY: GDI font lifetime managed here; the old font is
            // deleted after the control switched to the new one.
            unsafe {
                let face = WideString::new("Segoe UI Variable Text");
                let font = CreateFontW(
                    -(self.s(15.0) as i32),
                    0,
                    0,
                    0,
                    FW_NORMAL.0 as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    FF_DONTCARE.0 as u32,
                    face.pcwstr(),
                );
                SendMessageW(
                    self.edit,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
                if let Some(old) = self.edit_font.replace(font) {
                    let _ = DeleteObject(HGDIOBJ(old.0));
                }
            }
            self.edit_font_scale = scale;
        }
        self.layout_edit(hwnd);
    }

    fn layout_edit(&self, hwnd: HWND) {
        let (w, _) = client_size(hwnd);
        let r = self.search_rect(w);
        let h = self.s(22.0);
        let x = r.x + self.s(40.0);
        // SAFETY: moving our own child control.
        let _ = unsafe {
            MoveWindow(
                self.edit,
                x as i32,
                (r.y + (r.h - h) / 2.0) as i32,
                (r.right() - x - self.s(12.0)) as i32,
                h as i32,
                true,
            )
        };
    }

    fn centered(&self) -> bool {
        match self.config.taskbar.alignment {
            Alignment::System => explorer_win::theme::taskbar_centered(),
            Alignment::Center => true,
            Alignment::Left => false,
        }
    }

    /// Reset to the Home view with an empty query (also done on hide, so
    /// the next show only needs to paint).
    fn reset(&mut self, hwnd: HWND) {
        self.clear_query();
        self.status = None;
        self.focus_visible = false;
        self.pin_page = 0;
        self.set_view(hwnd, View::Home);
    }

    fn show(&mut self, hwnd: HWND) {
        self.reload_palette();
        self.utc_offset = crate::sys::utc_offset_secs();
        // Anchor to the monitor under the mouse cursor, above the taskbar
        // (the work area excludes it), centred or left like the taskbar.
        let mut pt = POINT::default();
        // SAFETY: plain FFI calls with valid pointers.
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            if let Some(m) = explorer_win::monitors::query(mon) {
                let scale = explorer_win::dpi::scale(m.dpi);
                self.scale = scale;
                let work = m.work;
                let ww = work.right - work.left;
                let wh = work.bottom - work.top;
                let margin = self.s(12.0) as i32;
                let w = (self.s(self.config.launcher.width as f32) as i32).min(ww - 2 * margin);
                let h = (self.s(self.config.launcher.height as f32) as i32).min(wh - 2 * margin);
                let x = if self.centered() {
                    work.left + (ww - w) / 2
                } else {
                    work.left + margin
                };
                let y = match self.config.taskbar.position {
                    Edge::Bottom => work.bottom - h - margin,
                    Edge::Top => work.top + margin,
                };
                // Position without showing; WM_SIZE re-enters and is
                // dropped, so resize the surface and EDIT here.
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
                if let Some(s) = self.surface.as_mut() {
                    s.resize();
                }
                self.apply_dpi(hwnd, scale);
            }
        }
        self.reset(hwnd);
        if self.indexed_at.is_none_or(|t| t.elapsed() > REINDEX_AFTER) {
            self.start_indexing(hwnd);
        }
        self.send_job(Job::Recent { max: REC_MAX });
        if self.avatar.is_none() {
            self.send_job(Job::Avatar);
        }
        // Paint the first frame synchronously right after showing, so the
        // menu never appears empty or with stale content.
        // SAFETY: our own window and control.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            explorer_win::activate::force_foreground(hwnd);
            let _ = SetFocus(Some(self.edit));
        }
        self.render(hwnd);
        // SAFETY: the frame was just drawn.
        let _ = unsafe { ValidateRect(Some(hwnd), None) };
    }

    fn hide(&mut self, hwnd: HWND) {
        if let Some(t) = self.tooltip.as_mut() {
            t.hide();
        }
        self.tooltip_for = None;
        if !is_visible(hwnd) {
            return;
        }
        // SAFETY: hiding our own window.
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
        self.reset(hwnd);
    }

    fn update_tooltip(&mut self, hwnd: HWND) {
        let footer = self.footer_zone();
        let target = match self.hover {
            Some(Hit::Item(z, i)) if Some(z) == footer => Some((Hit::Item(z, i), i)),
            _ => None,
        };
        if target.map(|t| t.0) == self.tooltip_for {
            return;
        }
        self.tooltip_for = target.map(|t| t.0);
        let l = self.lang;
        let Some((hit, i)) = target else {
            if let Some(t) = self.tooltip.as_mut() {
                t.hide();
            }
            return;
        };
        let text = match FOOTER.get(i) {
            Some(FooterButton::User) => self.user_name.clone(),
            Some(FooterButton::More) => tr(l, "More options", "Más opciones").into(),
            Some(FooterButton::Files) => tr(l, "File Explorer", "Explorador de archivos").into(),
            Some(FooterButton::Settings) => tr(l, "Settings", "Configuración").into(),
            Some(FooterButton::Power) => tr(l, "Power", "Inicio/apagado").into(),
            None => return,
        };
        let Some(r) = self.hits.iter().find(|(h, _)| *h == hit).map(|(_, r)| *r) else {
            return;
        };
        if self.tooltip.is_none() {
            self.tooltip = Tooltip::new(hwnd);
        }
        let mut pt = POINT {
            x: r.x as i32,
            y: r.y as i32,
        };
        // SAFETY: valid out-pointer.
        let _ = unsafe { ClientToScreen(hwnd, &mut pt) };
        let y = pt.y - self.s(34.0) as i32;
        if let Some(t) = self.tooltip.as_mut() {
            t.show(&text, pt.x, y);
        }
    }

    // ---------------------------------------------------------------
    // Painting
    // ---------------------------------------------------------------

    fn render(&mut self, hwnd: HWND) {
        let (w, h) = client_size(hwnd);
        let mut surface = self.surface.take().unwrap_or_else(|| Surface::new(hwnd));
        let mut hits = Vec::new();
        let bg = Color(self.palette.panel_bg);
        surface.draw(&self.gfx, bg, |p| self.draw(p, w, h, &mut hits));
        self.surface = Some(surface);
        self.hits = hits;
    }

    fn paint(&mut self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: BeginPaint/EndPaint pairing.
        unsafe { BeginPaint(hwnd, &mut ps) };
        self.render(hwnd);
        // SAFETY: matches BeginPaint.
        let _ = unsafe { EndPaint(hwnd, &ps) };
    }

    fn is_focused(&self, z: usize, i: usize) -> bool {
        self.focus == Some((z, i))
    }

    /// Background of an item: focused / hovered / nothing; plus the focus
    /// ring after keyboard navigation.
    fn item_bg(&self, p: &Painter<'_>, r: Rect, z: usize, i: usize) {
        let pal = &self.palette;
        let focused = self.is_focused(z, i);
        let hovered = self.hover == Some(Hit::Item(z, i));
        if focused {
            p.fill_rounded(r, self.s(4.0), Color(pal.selection));
        } else if hovered {
            p.fill_rounded(
                r,
                self.s(4.0),
                Color(explorer_core::theme::mix(pal.panel_bg, pal.selection, 0.6)),
            );
        }
        if focused && self.focus_visible {
            p.stroke_rounded(
                Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0),
                self.s(4.0),
                self.s(2.0).max(2.0),
                Color(pal.text),
            );
        }
    }

    fn draw_icon(&self, p: &Painter<'_>, row: &Row, r: Rect) {
        match self.icon_of(row) {
            Some(img) => p.image(&img, r),
            None => p.glyph(
                kind_glyph(row.kind),
                r,
                r.h * 0.7,
                Color(self.palette.text_dim),
            ),
        }
    }

    fn header(&self, p: &Painter<'_>, title: &str) {
        p.text(
            title,
            Rect::new(
                self.s(PAD + 8.0),
                self.body_top(),
                self.s(320.0),
                self.s(HEADER_H),
            ),
            self.s(14.0),
            Color(self.palette.text),
            Align::Left,
            true,
        );
    }

    /// The pill button at the right of a header ("All apps >", "< Back").
    fn pill(
        &self,
        p: &Painter<'_>,
        hits: &mut Vec<(Hit, Rect)>,
        w: f32,
        label: &str,
        z: usize,
        left_chevron: bool,
    ) {
        let s = |v: f32| self.s(v);
        let pal = &self.palette;
        let width = s(44.0 + 6.6 * label.chars().count() as f32);
        let r = Rect::new(w - s(PAD) - width, self.body_top() + s(2.0), width, s(28.0));
        let focused = self.is_focused(z, 0);
        let hovered = self.hover == Some(Hit::Item(z, 0));
        p.fill_rounded(
            r,
            s(4.0),
            Color(if focused || hovered {
                pal.selection
            } else {
                pal.input_bg
            }),
        );
        if focused && self.focus_visible {
            p.stroke_rounded(r, s(4.0), s(2.0).max(2.0), Color(pal.text));
        }
        let (tr_, gr) = if left_chevron {
            (
                Rect::new(r.x + s(24.0), r.y, r.w - s(30.0), r.h),
                Rect::new(r.x + s(4.0), r.y, s(20.0), r.h),
            )
        } else {
            (
                Rect::new(r.x + s(10.0), r.y, r.w - s(34.0), r.h),
                Rect::new(r.right() - s(24.0), r.y, s(20.0), r.h),
            )
        };
        p.text(
            label,
            tr_,
            s(12.0),
            Color(pal.text),
            if left_chevron {
                Align::Left
            } else {
                Align::Right
            },
            false,
        );
        p.glyph(
            if left_chevron {
                glyph::CHEVRON_LEFT
            } else {
                glyph::CHEVRON_RIGHT
            },
            gr,
            s(10.0),
            Color(pal.text),
        );
        hits.push((Hit::Item(z, 0), r));
    }

    fn draw(&self, p: &Painter<'_>, w: f32, h: f32, hits: &mut Vec<(Hit, Rect)>) {
        let s = |v: f32| self.s(v);
        let pal = &self.palette;
        // Border.
        p.stroke_rounded(
            Rect::new(0.5, 0.5, w - 1.0, h - 1.0),
            s(8.0),
            1.0,
            Color(pal.border),
        );
        // Search box (the EDIT control sits on top of it).
        let search = self.search_rect(w);
        p.fill_rounded(search, s(18.0), Color(pal.input_bg));
        p.fill_rounded(
            Rect::new(
                search.x + s(14.0),
                search.bottom() - s(2.0),
                search.w - s(28.0),
                s(2.0),
            ),
            s(1.0),
            Color(pal.accent_text),
        );
        p.glyph(
            glyph::SEARCH,
            Rect::new(search.x + s(10.0), search.y, s(24.0), search.h),
            s(14.0),
            Color(pal.text_dim),
        );
        if self.focus.is_none() && self.focus_visible {
            p.stroke_rounded(search, s(18.0), s(2.0).max(2.0), Color(pal.text));
        }

        match self.view {
            View::Home => self.draw_home(p, w, h, hits),
            View::AllApps => self.draw_all(p, w, h, hits),
            View::Letters => self.draw_letters(p, w, h, hits),
            View::Search => self.draw_search(p, w, h, hits),
        }
        self.draw_footer(p, w, h, hits);
    }

    fn draw_home(&self, p: &Painter<'_>, w: f32, h: f32, hits: &mut Vec<(Hit, Rect)>) {
        let s = |v: f32| self.s(v);
        let pal = &self.palette;
        let l = self.lang;
        self.header(p, tr(l, "Pinned", "Anclado"));
        self.pill(
            p,
            hits,
            w,
            tr(l, "All apps", "Todas las aplicaciones"),
            Z_ALLAPPS_BTN,
            false,
        );
        let pages = page_count(self.pinned.len(), PINS_PER_PAGE);
        let dots_w = if pages > 1 { s(DOTS_W) } else { 0.0 };
        let grid_area = Rect::new(
            s(PAD) - s(8.0),
            self.body_top() + s(HEADER_H + 8.0),
            w - 2.0 * s(PAD) + s(16.0) - dots_w,
            s(PIN_CELL_H) * PIN_ROWS as f32,
        );
        let pins = self.pin_rows_on_page();
        for (i, cell) in grid(grid_area, PIN_COLS, s(PIN_CELL_H), pins.len())
            .iter()
            .enumerate()
        {
            let cell = Rect::new(
                cell.x + s(2.0),
                cell.y + s(2.0),
                cell.w - s(4.0),
                cell.h - s(4.0),
            );
            self.item_bg(p, cell, Z_PINNED, i);
            let icon_r = Rect::new(
                cell.x + (cell.w - s(32.0)) / 2.0,
                cell.y + s(12.0),
                s(32.0),
                s(32.0),
            );
            self.draw_icon(p, &pins[i], icon_r);
            p.text(
                &pins[i].name,
                Rect::new(
                    cell.x + s(4.0),
                    icon_r.bottom() + s(6.0),
                    cell.w - s(8.0),
                    s(20.0),
                ),
                s(12.0),
                Color(pal.text),
                Align::Center,
                false,
            );
            hits.push((Hit::Item(Z_PINNED, i), cell));
        }
        if self.pinned.is_empty() && !self.catalog.is_empty() {
            p.text(
                tr(
                    l,
                    "Right-click an app in All apps to pin it here",
                    "Haz clic con el botón derecho en una aplicación de Todas las aplicaciones para anclarla aquí",
                ),
                grid_area,
                s(13.0),
                Color(pal.text_dim),
                Align::Center,
                false,
            );
        }
        // Page dots (vertical, right of the grid, like Windows 11).
        if pages > 1 {
            let dot = s(6.0);
            let step = s(16.0);
            let total = step * pages as f32;
            let x = grid_area.right() + (dots_w - dot) / 2.0;
            let mut y = grid_area.y + (grid_area.h - total) / 2.0;
            for page in 0..pages {
                let hit = Rect::new(grid_area.right(), y, dots_w, step);
                let d = Rect::new(x, y + (step - dot) / 2.0, dot, dot);
                let c = if page == self.pin_page {
                    pal.accent_text
                } else if self.hover == Some(Hit::PageDot(page)) {
                    pal.text
                } else {
                    pal.text_dim
                };
                p.fill_rounded(d, dot / 2.0, Color(c));
                hits.push((Hit::PageDot(page), hit));
                y += step;
            }
        }
        // Recommended.
        let rec_y = grid_area.bottom() + s(12.0);
        p.text(
            tr(l, "Recommended", "Recomendado"),
            Rect::new(s(PAD + 8.0), rec_y, s(320.0), s(HEADER_H)),
            s(14.0),
            Color(pal.text),
            Align::Left,
            true,
        );
        let footer_y = h - s(FOOTER_H);
        let rec_area = Rect::new(
            s(PAD) - s(8.0),
            rec_y + s(HEADER_H + 4.0),
            w - 2.0 * s(PAD) + s(16.0),
            (footer_y - s(8.0)) - (rec_y + s(HEADER_H + 4.0)),
        );
        if self.recommended.is_empty() && !self.indexing {
            p.text(
                tr(
                    l,
                    "Your recent files and most used apps will appear here",
                    "Aquí aparecerán tus archivos recientes y las aplicaciones que más usas",
                ),
                Rect::new(rec_area.x, rec_area.y, rec_area.w, s(40.0)),
                s(13.0),
                Color(pal.text_dim),
                Align::Center,
                false,
            );
        }
        let max_rows = ((rec_area.h / s(REC_CELL_H)).floor().max(0.0) as usize).max(1);
        let n = self.recommended.len().min(max_rows * REC_COLS);
        for (i, cell) in grid(rec_area, REC_COLS, s(REC_CELL_H), n)
            .iter()
            .enumerate()
        {
            let row = &self.recommended[i];
            let cell = Rect::new(
                cell.x + s(2.0),
                cell.y + s(2.0),
                cell.w - s(4.0),
                cell.h - s(4.0),
            );
            self.item_bg(p, cell, Z_REC, i);
            let icon_r = Rect::new(
                cell.x + s(12.0),
                cell.y + (cell.h - s(32.0)) / 2.0,
                s(32.0),
                s(32.0),
            );
            self.draw_icon(p, row, icon_r);
            let tx = icon_r.right() + s(12.0);
            let tw = cell.right() - tx - s(8.0);
            p.text(
                &row.name,
                Rect::new(tx, cell.y + s(8.0), tw, s(20.0)),
                s(13.0),
                Color(pal.text),
                Align::Left,
                false,
            );
            p.text(
                &row.detail,
                Rect::new(tx, cell.y + s(27.0), tw, s(18.0)),
                s(11.0),
                Color(pal.text_dim),
                Align::Left,
                false,
            );
            hits.push((Hit::Item(Z_REC, i), cell));
        }
        if self.catalog.is_empty() && self.indexing {
            self.status_text(
                p,
                grid_area,
                tr(l, "Loading apps…", "Cargando aplicaciones…"),
            );
        }
    }

    fn status_text(&self, p: &Painter<'_>, area: Rect, msg: &str) {
        p.text(
            msg,
            Rect::new(area.x, area.y, area.w, self.s(48.0)),
            self.s(13.0),
            Color(self.palette.text_dim),
            Align::Center,
            false,
        );
    }

    fn draw_all(&self, p: &Painter<'_>, w: f32, h: f32, hits: &mut Vec<(Hit, Rect)>) {
        let s = |v: f32| self.s(v);
        let pal = &self.palette;
        let l = self.lang;
        self.header(p, tr(l, "All apps", "Todas las aplicaciones"));
        self.pill(p, hits, w, tr(l, "Back", "Atrás"), Z_BACK, true);
        let list = self.list_area(w, h);
        if self.all.is_empty() {
            let msg = if self.indexing {
                tr(l, "Loading apps…", "Cargando aplicaciones…")
            } else {
                tr(l, "No apps found", "No se encontraron aplicaciones")
            };
            self.status_text(p, list, msg);
            return;
        }
        let (rows, headers, _) = self.all_geometry();
        let base = list.y - self.scroll;
        p.clipped(list, |p| {
            for (k, hy) in headers.iter().enumerate() {
                let r = Rect::new(list.x, base + hy, list.w, s(LETTER_H));
                if r.bottom() <= list.y || r.y >= list.bottom() {
                    continue;
                }
                let letter = self.letters.get(k).map(|(_, c)| *c).unwrap_or('#');
                let lr = Rect::new(r.x + s(4.0), r.y + s(2.0), s(32.0), r.h - s(4.0));
                if self.hover == Some(Hit::Letter(k)) {
                    p.fill_rounded(lr, s(4.0), Color(pal.selection));
                }
                p.text(
                    &letter.to_string(),
                    lr,
                    s(13.0),
                    Color(pal.accent_text),
                    Align::Center,
                    true,
                );
                let top = lr.y.max(list.y);
                let bottom = lr.bottom().min(list.bottom());
                hits.push((Hit::Letter(k), Rect::new(lr.x, top, lr.w, bottom - top)));
            }
            for (i, row) in self.all.iter().enumerate() {
                let (y, rh) = rows[i];
                let r = Rect::new(list.x, base + y, list.w, rh);
                if r.bottom() <= list.y || r.y >= list.bottom() {
                    continue;
                }
                self.item_bg(p, r, Z_LIST, i);
                let icon_r =
                    Rect::new(r.x + s(12.0), r.y + (r.h - s(24.0)) / 2.0, s(24.0), s(24.0));
                self.draw_icon(p, row, icon_r);
                p.text(
                    &row.name,
                    Rect::new(
                        icon_r.right() + s(12.0),
                        r.y,
                        r.right() - icon_r.right() - s(24.0),
                        r.h,
                    ),
                    s(13.0),
                    Color(pal.text),
                    Align::Left,
                    false,
                );
                let top = r.y.max(list.y);
                let bottom = r.bottom().min(list.bottom());
                hits.push((Hit::Item(Z_LIST, i), Rect::new(r.x, top, r.w, bottom - top)));
            }
        });
    }

    fn draw_letters(&self, p: &Painter<'_>, w: f32, h: f32, hits: &mut Vec<(Hit, Rect)>) {
        let s = |v: f32| self.s(v);
        let pal = &self.palette;
        let l = self.lang;
        self.header(p, tr(l, "All apps", "Todas las aplicaciones"));
        let list = self.list_area(w, h);
        let cols = LETTER_COLS as f32;
        let gw = s(LETTER_CELL) * cols;
        let area = Rect::new(
            list.x + (list.w - gw) / 2.0,
            list.y + s(8.0),
            gw,
            list.h - s(8.0),
        );
        for (i, cell) in grid(area, LETTER_COLS, s(LETTER_CELL), self.letters.len())
            .iter()
            .enumerate()
        {
            let cell = Rect::new(
                cell.x + s(3.0),
                cell.y + s(3.0),
                cell.w - s(6.0),
                cell.h - s(6.0),
            );
            self.item_bg(p, cell, Z_LETTERS, i);
            p.text(
                &self.letters[i].1.to_string(),
                cell,
                s(16.0),
                Color(pal.text),
                Align::Center,
                true,
            );
            hits.push((Hit::Item(Z_LETTERS, i), cell));
        }
    }

    fn draw_search(&self, p: &Painter<'_>, w: f32, h: f32, hits: &mut Vec<(Hit, Rect)>) {
        let s = |v: f32| self.s(v);
        let pal = &self.palette;
        let l = self.lang;
        let list = self.list_area(w, h);
        if self.results.is_empty() {
            let msg = if self.catalog.is_empty() && self.indexing {
                tr(l, "Loading apps…", "Cargando aplicaciones…").to_string()
            } else {
                trf(
                    l,
                    format!("No results for “{}”", self.query.trim()),
                    format!("No hay resultados para «{}»", self.query.trim()),
                )
            };
            p.text(
                &msg,
                Rect::new(s(PAD), list.y + s(16.0), w - 2.0 * s(PAD), s(32.0)),
                s(13.0),
                Color(pal.text_dim),
                Align::Center,
                false,
            );
            return;
        }
        let (geo, _) = self.search_geometry();
        let base = list.y - self.scroll;
        p.clipped(list, |p| {
            for (i, (section, row)) in self.results.iter().enumerate() {
                let (header, y, rh) = geo[i];
                if let Some(hy) = header {
                    let r = Rect::new(list.x + s(8.0), base + hy, list.w - s(16.0), s(SECTION_H));
                    if r.bottom() > list.y && r.y < list.bottom() {
                        p.text(
                            section.title(l),
                            r,
                            s(13.0),
                            Color(pal.text),
                            Align::Left,
                            true,
                        );
                    }
                }
                let r = Rect::new(list.x, base + y, list.w, rh);
                if r.bottom() <= list.y || r.y >= list.bottom() {
                    continue;
                }
                self.item_bg(p, r, Z_RESULTS, i);
                if self.detail == i {
                    p.fill_rounded(
                        Rect::new(r.x, r.y + r.h * 0.3, s(3.0), r.h * 0.4),
                        s(1.5),
                        Color(pal.accent_text),
                    );
                }
                let big = *section == Section::BestMatch;
                let isz = if big { s(40.0) } else { s(24.0) };
                let icon_r = Rect::new(r.x + s(12.0), r.y + (r.h - isz) / 2.0, isz, isz);
                self.draw_icon(p, row, icon_r);
                let tx = icon_r.right() + s(12.0);
                let tw = r.right() - tx - s(12.0);
                let (name_size, line_h) = if big {
                    (s(15.0), s(22.0))
                } else {
                    (s(13.0), s(19.0))
                };
                let top = r.y + (r.h - line_h - s(16.0)) / 2.0;
                p.text(
                    &row.name,
                    Rect::new(tx, top, tw, line_h),
                    name_size,
                    Color(pal.text),
                    Align::Left,
                    big,
                );
                let detail = if big || row.kind == ResultKind::File {
                    row.detail.clone()
                } else {
                    row.kind.label(l).to_string()
                };
                p.text(
                    &detail,
                    Rect::new(tx, top + line_h, tw, s(16.0)),
                    s(11.0),
                    Color(pal.text_dim),
                    Align::Left,
                    false,
                );
                let top = r.y.max(list.y);
                let bottom = r.bottom().min(list.bottom());
                hits.push((
                    Hit::Item(Z_RESULTS, i),
                    Rect::new(r.x, top, r.w, bottom - top),
                ));
            }
        });
        self.draw_detail(p, w, h, list, hits);
    }

    fn draw_detail(
        &self,
        p: &Painter<'_>,
        w: f32,
        h: f32,
        list: Rect,
        hits: &mut Vec<(Hit, Rect)>,
    ) {
        let s = |v: f32| self.s(v);
        let pal = &self.palette;
        let l = self.lang;
        let Some((_, row)) = self.results.get(self.detail) else {
            return;
        };
        let x = list.right() + s(12.0);
        let panel = Rect::new(
            x,
            list.y,
            w - s(PAD) - x,
            h - s(FOOTER_H) - s(12.0) - list.y,
        );
        if panel.w < s(120.0) {
            return;
        }
        p.fill_rounded(
            panel,
            s(8.0),
            Color(explorer_core::theme::mix(pal.panel_bg, pal.input_bg, 0.55)),
        );
        let icon_r = Rect::new(
            panel.x + (panel.w - s(56.0)) / 2.0,
            panel.y + s(24.0),
            s(56.0),
            s(56.0),
        );
        self.draw_icon(p, row, icon_r);
        let tx = panel.x + s(12.0);
        let tw = panel.w - s(24.0);
        let mut y = icon_r.bottom() + s(12.0);
        p.text(
            &row.name,
            Rect::new(tx, y, tw, s(24.0)),
            s(15.0),
            Color(pal.text),
            Align::Center,
            true,
        );
        y += s(24.0);
        p.text(
            row.kind.label(l),
            Rect::new(tx, y, tw, s(18.0)),
            s(12.0),
            Color(pal.text_dim),
            Align::Center,
            false,
        );
        y += s(18.0);
        let extra = match &row.target {
            Target::File { path, .. } => Some(path.to_string_lossy().into_owned()),
            Target::App(LaunchTarget::Path(p)) => Some(p.to_string_lossy().into_owned()),
            Target::Calc(_) | Target::Command(_) => Some(row.detail.clone()),
            _ => None,
        };
        if let Some(extra) = extra.filter(|e| !e.is_empty()) {
            p.text(
                &extra,
                Rect::new(tx, y, tw, s(16.0)),
                s(11.0),
                Color(pal.text_dim),
                Align::Center,
                false,
            );
            y += s(16.0);
        }
        if let Some((msg, _)) = &self.status {
            p.text(
                msg,
                Rect::new(tx, y, tw, s(18.0)),
                s(12.0),
                Color(pal.accent_text),
                Align::Center,
                true,
            );
        }
        y += s(22.0);
        p.fill(
            Rect::new(panel.x + s(12.0), y, panel.w - s(24.0), 1.0),
            Color(pal.border),
        );
        y += s(8.0);
        for (k, a) in self.detail_actions.iter().enumerate() {
            let r = Rect::new(panel.x + s(6.0), y, panel.w - s(12.0), s(ACTION_H));
            if r.bottom() > panel.bottom() - s(4.0) {
                break;
            }
            self.item_bg(p, r, Z_ACTIONS, k);
            p.glyph(
                action_glyph(*a, row.kind),
                Rect::new(r.x + s(8.0), r.y, s(24.0), r.h),
                s(14.0),
                Color(pal.text),
            );
            p.text(
                a.label(row.kind, l),
                Rect::new(r.x + s(40.0), r.y, r.w - s(48.0), r.h),
                s(13.0),
                Color(pal.text),
                Align::Left,
                false,
            );
            hits.push((Hit::Item(Z_ACTIONS, k), r));
            y += s(ACTION_H);
        }
    }

    fn draw_footer(&self, p: &Painter<'_>, w: f32, h: f32, hits: &mut Vec<(Hit, Rect)>) {
        let s = |v: f32| self.s(v);
        let pal = &self.palette;
        let footer = self.footer_rect(w, h);
        p.fill(footer, Color(theme_mix(pal.panel_bg, pal.light)));
        p.fill(Rect::new(0.0, footer.y, w, 1.0), Color(pal.border));
        let Some(z) = self.footer_zone() else {
            return;
        };
        let bh = footer.h - s(24.0);
        let by = footer.y + s(12.0);
        // User, left.
        let user_r = Rect::new(s(PAD), by, s(240.0).min(w * 0.45), bh);
        self.item_bg(p, user_r, z, 0);
        let pic = Rect::new(
            user_r.x + s(8.0),
            user_r.y + (bh - s(28.0)) / 2.0,
            s(28.0),
            s(28.0),
        );
        match &self.avatar {
            Some(img) => p.image(img, pic),
            None => {
                p.fill_rounded(pic, s(14.0), Color(pal.input_bg));
                p.glyph(glyph::USER, pic, s(14.0), Color(pal.text));
            }
        }
        p.text(
            &self.user_name,
            Rect::new(
                pic.right() + s(10.0),
                user_r.y,
                user_r.right() - pic.right() - s(16.0),
                bh,
            ),
            s(13.0),
            Color(pal.text),
            Align::Left,
            false,
        );
        hits.push((Hit::Item(z, 0), user_r));
        // Icon buttons, right to left: Power, Settings, Files, More.
        let bw = s(40.0);
        let mut x = w - s(PAD) - bw;
        for (i, glyph_str) in [
            (4usize, glyph::POWER),
            (3, glyph::SETTINGS),
            (2, g::FOLDER),
            (1, g::MORE),
        ] {
            let r = Rect::new(x, by, bw, bh);
            self.item_bg(p, r, z, i);
            p.glyph(glyph_str, r, s(16.0), Color(pal.text));
            hits.push((Hit::Item(z, i), r));
            x -= bw + s(4.0);
        }
    }
}

/// Footer background: slightly darker (dark mode) / lighter (light mode).
fn theme_mix(panel: u32, light: bool) -> u32 {
    explorer_core::theme::mix(panel, if light { 0xFFFFFF } else { 0x000000 }, 0.12)
}

impl Handler for Launcher {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if msg == self.launcher_msg && msg != 0 {
            match LauncherCommand::from_wparam(wparam.0) {
                Some(LauncherCommand::Show) => self.show(hwnd),
                Some(LauncherCommand::Hide) => self.hide(hwnd),
                Some(LauncherCommand::Toggle) => {
                    if is_visible(hwnd) {
                        self.hide(hwnd);
                    } else if self
                        .hidden_at
                        .is_some_and(|t| t.elapsed() < TOGGLE_DEBOUNCE)
                    {
                        // The click on the Start button already closed us.
                        self.hidden_at = None;
                    } else {
                        self.show(hwnd);
                    }
                }
                None => {}
            }
            return Some(LRESULT(0));
        }
        match msg {
            WM_CREATE => {
                let class = wide("EDIT");
                // SAFETY: creating a standard child control.
                let edit = unsafe {
                    CreateWindowExW(
                        Default::default(),
                        PCWSTR(class.as_ptr()),
                        None,
                        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                        0,
                        0,
                        10,
                        10,
                        Some(hwnd),
                        None::<HMENU>,
                        Some(window::module_instance()),
                        None,
                    )
                };
                match edit {
                    Ok(e) => {
                        self.edit = e;
                        self.edit_shared.set(e);
                        crate::cue::install(e);
                    }
                    Err(e) => tracing::error!(error = %e, "cannot create EDIT control"),
                }
                self.reload_palette();
                let corner: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
                // SAFETY: valid attribute pointer and size (Windows 11 only;
                // failure on older systems is harmless).
                let _ = unsafe {
                    DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_WINDOW_CORNER_PREFERENCE,
                        &corner as *const _ as *const core::ffi::c_void,
                        std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
                    )
                };
                let scale = explorer_win::dpi::scale(explorer_win::dpi::window_dpi(hwnd));
                self.apply_dpi(hwnd, scale);
                self.workers = Some(Workers::start(hwnd, WM_APP_WORKER));
                self.send_job(Job::Tools);
                self.send_job(Job::Avatar);
                self.send_job(Job::Recent { max: REC_MAX });
                self.utc_offset = crate::sys::utc_offset_secs();
                self.start_indexing(hwnd);
                Some(LRESULT(0))
            }
            WM_APP_INDEXED => {
                let mut msgs = Vec::new();
                if let Some(rx) = &self.index_rx {
                    while let Ok(m) = rx.try_recv() {
                        msgs.push(m);
                    }
                }
                for m in msgs {
                    match m {
                        IndexMsg::Catalog(entries) => {
                            self.catalog = Catalog::build(entries);
                            self.indexed_at = Some(Instant::now());
                            if self.pins.seed_defaults(&self.catalog) {
                                self.save_pins();
                            }
                            self.rebuild_all();
                            match self.view {
                                View::Search => self.run_search(true),
                                View::Home => self.rebuild_home(),
                                _ => {}
                            }
                            self.clamp_focus();
                        }
                        IndexMsg::Icons(icons) => {
                            self.icons.extend(icons);
                            self.indexing = false;
                            self.index_rx = None;
                            // Icons the bulk pass missed (e.g. files).
                            self.icons_requested.clear();
                            self.rebuild_home();
                        }
                    }
                }
                invalidate(hwnd);
                Some(LRESULT(0))
            }
            WM_APP_WORKER => {
                self.on_worker(hwnd);
                Some(LRESULT(0))
            }
            #[cfg(any(feature = "everything", feature = "file-index"))]
            WM_APP_FILES => {
                let mut latest = None;
                if let Some(f) = &self.files {
                    while let Ok((gen, paths)) = f.results.try_recv() {
                        if gen == self.generation {
                            latest = Some(paths);
                        }
                    }
                }
                if let Some(paths) = latest {
                    self.file_paths = paths;
                    if self.view == View::Search {
                        self.run_search(true);
                        invalidate(hwnd);
                    }
                }
                Some(LRESULT(0))
            }
            WM_APP_NAVKEY => {
                self.on_nav_key(hwnd, wparam.0 as u32);
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                let notification = ((wparam.0 >> 16) & 0xffff) as u32;
                if notification == EN_CHANGE && HWND(lparam.0 as *mut _) == self.edit {
                    let before = self.query.clone();
                    self.read_query();
                    if before != self.query {
                        self.focus_visible = false;
                        self.on_query_changed(hwnd);
                    }
                }
                Some(LRESULT(0))
            }
            WM_CTLCOLOREDIT => {
                let hdc = HDC(wparam.0 as *mut _);
                // SAFETY: HDC supplied by the system for this message.
                unsafe {
                    SetTextColor(hdc, COLORREF(bgr(self.palette.text)));
                    SetBkColor(hdc, COLORREF(bgr(self.palette.input_bg)));
                }
                self.edit_brush.map(|b| LRESULT(b.0 as isize))
            }
            WM_ACTIVATE => {
                // Hide when the user clicks elsewhere (including the Start
                // button: see TOGGLE_DEBOUNCE).
                if wparam.0 & 0xffff == 0 && is_visible(hwnd) {
                    self.hide(hwnd);
                    self.hidden_at = Some(Instant::now());
                }
                None
            }
            WM_SETTINGCHANGE => {
                self.reload_palette();
                invalidate(hwnd);
                None
            }
            WM_TIMER => {
                if wparam.0 == STATUS_TIMER {
                    // SAFETY: our own timer.
                    let _ = unsafe { KillTimer(Some(hwnd), STATUS_TIMER) };
                    if self.status.take().is_some() {
                        invalidate(hwnd);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let pos = (
                    (lparam.0 & 0xffff) as i16 as i32,
                    ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                );
                // Windows sends WM_MOUSEMOVE without movement (after show,
                // scrolling, menus): only real moves change the hover.
                if self.last_mouse == Some(pos) {
                    return Some(LRESULT(0));
                }
                self.last_mouse = Some(pos);
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
                let hit = self.hit(lparam);
                if hit != self.hover {
                    self.hover = hit;
                    invalidate(hwnd);
                }
                self.update_tooltip(hwnd);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.last_mouse = None;
                if self.hover.take().is_some() {
                    invalidate(hwnd);
                }
                self.update_tooltip(hwnd);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                // SAFETY: keep keyboard focus in the query box.
                let _ = unsafe { SetFocus(Some(self.edit)) };
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.focus_visible = false;
                match self.hit(lparam) {
                    Some(Hit::Item(z, i)) => {
                        if self.view == View::Search && z == Z_RESULTS {
                            self.set_focus(hwnd, Some((z, i)));
                        }
                        self.activate(hwnd, z, i, false);
                    }
                    Some(Hit::PageDot(page)) => {
                        let delta = page as i32 - self.pin_page as i32;
                        self.change_page(hwnd, delta);
                    }
                    Some(Hit::Letter(_)) => self.set_view(hwnd, View::Letters),
                    None => {}
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                if let Some(Hit::Item(z, i)) = self.hit(lparam) {
                    self.context_menu(hwnd, z, i, MenuAt::Cursor);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as f32;
                if self.view == View::Home {
                    if delta < 0.0 {
                        self.change_page(hwnd, 1);
                    } else if delta > 0.0 {
                        self.change_page(hwnd, -1);
                    }
                } else {
                    self.scroll_by(hwnd, -delta / 120.0 * self.s(ROW_H) * 3.0);
                }
                self.last_mouse = None;
                Some(LRESULT(0))
            }
            WM_SIZE => {
                if let Some(s) = self.surface.as_mut() {
                    s.resize();
                }
                self.layout_edit(hwnd);
                None
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xffff) as u32;
                self.apply_dpi(hwnd, explorer_win::dpi::scale(dpi));
                invalidate(hwnd);
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_PAINT => {
                self.paint(hwnd);
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                // SAFETY: deleting our own GDI objects.
                unsafe {
                    if let Some(f) = self.edit_font.take() {
                        let _ = DeleteObject(HGDIOBJ(f.0));
                    }
                    if let Some(b) = self.edit_brush.take() {
                        let _ = DeleteObject(HGDIOBJ(b.0));
                    }
                }
                window::quit(0);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}
