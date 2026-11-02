// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows implementation of the Start menu ("launcher").
//!
//! One popup window anchored above the taskbar, hidden while idle. Three
//! views, Windows 11 style:
//! - **Home**: search box, pinned apps grid, "Most used" apps, footer with
//!   the user name and a power menu;
//! - **All apps**: alphabetical list with letter headers;
//! - **Search**: fuzzy results (any typing switches here).
//!
//! The query box is a native EDIT control (IME, clipboard for free);
//! everything else is painted with Direct2D. Navigation keys are
//! intercepted in the message loop before the EDIT control sees them. App
//! indexing and icon extraction run on a worker thread.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use explorer_core::catalog::{
    scan_folder, unix_now, AppEntry, AppSource, Catalog, LaunchHistory, LaunchTarget,
};
use explorer_core::config::{Alignment, Config, Edge};
use explorer_core::image::SharedImage;
use explorer_core::ipc::{LauncherCommand, RunMode, LAUNCHER_WINDOW_CLASS};
use explorer_core::layout::{grid, nav, Dir, Rect};
use explorer_core::paths::AppPaths;
use explorer_core::search::Searcher;
use explorer_core::start::{letter_of, Pins};
use explorer_core::theme::Palette;
use explorer_win::gfx::{glyph, Align, Color, Gfx, Painter, Surface};
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
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, InvalidateRect,
    MonitorFromPoint, SetBkColor, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, FF_DONTCARE, FW_NORMAL, HBRUSH, HDC, HFONT, HGDIOBJ, MONITOR_DEFAULTTONEAREST,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT,
};
use explorer_win::windows::Win32::System::Shutdown::{
    ExitWindowsEx, LockWorkStation, EWX_LOGOFF, SHUTDOWN_REASON,
};
use explorer_win::windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL, VK_SHIFT,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DestroyMenu, GetClientRect, GetCursorPos,
    GetWindowTextLengthW, GetWindowTextW, MoveWindow, PostMessageW, SendMessageW, SetWindowPos,
    SetWindowTextW, ShowWindow, TrackPopupMenu, ES_AUTOHSCROLL, HMENU, HWND_TOPMOST, MF_SEPARATOR,
    MF_STRING, MSG, SWP_SHOWWINDOW, SW_HIDE, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WINDOW_STYLE, WM_ACTIVATE, WM_APP, WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT, WM_DESTROY,
    WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT,
    WM_RBUTTONUP, WM_SETFONT, WM_SETTINGCHANGE, WM_SIZE, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP, WS_VISIBLE,
};

use crate::Args;

const WM_APP_INDEXED: u32 = WM_APP + 10;
const WM_APP_NAVKEY: u32 = WM_APP + 11;
#[cfg(feature = "everything")]
const WM_APP_FILES: u32 = WM_APP + 12;
const WM_MOUSELEAVE: u32 = 0x02A3;

const EN_CHANGE: u32 = 0x0300;

const VK_UP: u32 = 0x26;
const VK_DOWN: u32 = 0x28;
const VK_LEFT: u32 = 0x25;
const VK_RIGHT: u32 = 0x27;
const VK_PRIOR: u32 = 0x21;
const VK_NEXT: u32 = 0x22;
const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;
const VK_TAB: u32 = 0x09;
const NAV_KEYS: &[u32] = &[
    VK_UP, VK_DOWN, VK_PRIOR, VK_NEXT, VK_RETURN, VK_ESCAPE, VK_TAB,
];

/// Re-index on show when the catalog is older than this.
const REINDEX_AFTER: Duration = Duration::from_secs(120);
/// A Toggle arriving this soon after we hid because of deactivation is
/// the same click that deactivated us (e.g. on the Start button): stay
/// hidden instead of reopening.
const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(400);
/// Icons are extracted once at this size and scaled when drawn.
const ICON_PX: u32 = 48;

// Logical metrics.
const PAD: f32 = 24.0;
const SEARCH_H: f32 = 36.0;
const HEADER_H: f32 = 32.0;
const PIN_COLS: usize = 6;
const PIN_CELL_H: f32 = 84.0;
const PIN_ROWS: usize = 3;
const REC_CELL_H: f32 = 56.0;
const ROW_H: f32 = 40.0;
const LETTER_H: f32 = 32.0;
const FOOTER_H: f32 = 64.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Home,
    AllApps,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    App,
    Shortcut,
    #[cfg_attr(not(feature = "everything"), allow(dead_code))]
    File,
}

impl RowKind {
    fn label(self) -> &'static str {
        match self {
            RowKind::App => "App",
            RowKind::Shortcut => "Shortcut",
            RowKind::File => "File",
        }
    }
}

#[derive(Debug, Clone)]
struct Row {
    name: String,
    detail: String,
    target: LaunchTarget,
    kind: RowKind,
}

impl Row {
    fn from_entry(e: &AppEntry) -> Self {
        Self {
            name: e.name.clone(),
            detail: e.keywords.first().cloned().unwrap_or_default(),
            target: e.target.clone(),
            kind: match e.target {
                LaunchTarget::AppsFolder(_) => RowKind::App,
                LaunchTarget::Path(_) => RowKind::Shortcut,
            },
        }
    }
}

/// A clickable thing on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    /// Index into `self.rows` (meaning depends on the view).
    Entry(usize),
    AllApps,
    Back,
    User,
    Power,
}

enum WorkerMsg {
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

    window::register_class(LAUNCHER_WINDOW_CLASS, None);
    let main_hwnd = Rc::new(Cell::new(HWND::default()));
    let edit_hwnd = Rc::new(Cell::new(HWND::default()));
    let launcher = Launcher {
        gfx,
        history: LaunchHistory::load(&paths.history_file()),
        pins: Pins::load(&paths.pins_file()),
        paths,
        config,
        palette: Palette::from_system(&Default::default(), explorer_core::theme::Mode::Dark),
        user_name: explorer_win::theme::user_display_name(),
        catalog: Catalog::default(),
        icons: HashMap::new(),
        searcher: Searcher::new(),
        view: View::Home,
        rows: Vec::new(),
        letters: Vec::new(),
        items: Vec::new(),
        selected: None,
        scroll: 0.0,
        content_h: 0.0,
        indexing: false,
        indexed_at: None,
        index_rx: None,
        hidden_at: None,
        surface: None,
        scale: 1.0,
        edit: HWND::default(),
        edit_font: None,
        edit_brush: None,
        edit_brush_color: 0,
        edit_shared: edit_hwnd.clone(),
        launcher_msg: explorer_win::ipc::launcher_message(),
        query: String::new(),
        tracking: false,
        #[cfg(feature = "everything")]
        files: None,
        #[cfg(feature = "everything")]
        files_generation: 0,
        #[cfg(feature = "everything")]
        file_rows: Vec::new(),
    };
    let hwnd = match window::create(
        CreateParams {
            class: LAUNCHER_WINDOW_CLASS,
            title: "Start",
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

struct Launcher {
    gfx: Gfx,
    config: Config,
    paths: AppPaths,
    history: LaunchHistory,
    pins: Pins,
    palette: Palette,
    user_name: String,
    catalog: Catalog,
    icons: HashMap<String, SharedImage>,
    searcher: Searcher,
    view: View,
    /// Entries of the current view (Home: pinned then most used).
    rows: Vec<Row>,
    /// All apps view: (row index, letter) headers.
    letters: Vec<(usize, char)>,
    /// Clickable items with their rects (window coordinates, after scroll)
    /// from the last paint.
    items: Vec<(Item, Rect)>,
    selected: Option<Item>,
    /// Scroll offset of list views, physical pixels.
    scroll: f32,
    content_h: f32,
    indexing: bool,
    indexed_at: Option<Instant>,
    index_rx: Option<Receiver<WorkerMsg>>,
    hidden_at: Option<Instant>,
    surface: Option<Surface>,
    scale: f32,
    edit: HWND,
    edit_font: Option<HFONT>,
    edit_brush: Option<HBRUSH>,
    edit_brush_color: u32,
    edit_shared: Rc<Cell<HWND>>,
    launcher_msg: u32,
    query: String,
    tracking: bool,
    #[cfg(feature = "everything")]
    files: Option<crate::everything::EverythingWorker>,
    #[cfg(feature = "everything")]
    files_generation: u64,
    #[cfg(feature = "everything")]
    file_rows: Vec<Row>,
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
                let _ = AppendMenuW(menu, MF_STRING, *id as usize, PCWSTR(w.as_ptr()));
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

const CMD_PIN: u32 = 1;
const CMD_UNPIN: u32 = 2;
const CMD_FRONT: u32 = 3;
const CMD_ADMIN: u32 = 4;
const CMD_LOCATION: u32 = 5;
const CMD_LOCK: u32 = 20;
const CMD_SIGN_OUT: u32 = 21;
const CMD_SLEEP: u32 = 22;
const CMD_SHUTDOWN: u32 = 23;
const CMD_RESTART: u32 = 24;

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
    }

    fn start_indexing(&mut self, hwnd: HWND) {
        if self.indexing {
            return;
        }
        self.indexing = true;
        let (tx, rx) = channel();
        self.index_rx = Some(rx);
        let extra = self.config.launcher.extra_folders.clone();
        let target = hwnd.0 as usize;
        std::thread::spawn(move || {
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
            if tx.send(WorkerMsg::Catalog(entries)).is_err() {
                return;
            }
            post();
            let started = Instant::now();
            let icons: HashMap<String, SharedImage> = targets
                .iter()
                .filter_map(|t| explorer_win::icons::for_target(t, ICON_PX).map(|i| (t.key(), i)))
                .collect();
            tracing::info!(
                icons = icons.len(),
                ms = started.elapsed().as_millis() as u64,
                "extracted app icons"
            );
            if tx.send(WorkerMsg::Icons(icons)).is_ok() {
                post();
            }
        });
    }

    /// Rebuild `rows` for the current view.
    fn rebuild_rows(&mut self, hwnd: HWND) {
        self.letters.clear();
        self.rows = match self.view {
            View::Home => {
                let mut rows: Vec<Row> = self
                    .pins
                    .resolve(&self.catalog)
                    .into_iter()
                    .map(Row::from_entry)
                    .collect();
                let pinned = rows.len();
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
                rows.extend(used.into_iter().take(6).map(|(e, _)| Row::from_entry(e)));
                // Keep at most one screen of pins.
                let max_pins = PIN_COLS * PIN_ROWS;
                if pinned > max_pins {
                    rows.drain(max_pins..pinned);
                }
                rows
            }
            View::AllApps => {
                let rows: Vec<Row> = self.catalog.entries().iter().map(Row::from_entry).collect();
                let mut last = None;
                for (i, r) in rows.iter().enumerate() {
                    let l = letter_of(&r.name);
                    if last != Some(l) {
                        self.letters.push((i, l));
                        last = Some(l);
                    }
                }
                rows
            }
            View::Search => {
                let max = self.config.launcher.max_results;
                let hits = self.searcher.search(
                    &self.catalog,
                    &self.history,
                    &self.query,
                    max,
                    unix_now(),
                );
                #[allow(unused_mut)]
                let mut rows: Vec<Row> = hits.iter().map(|h| Row::from_entry(h.entry)).collect();
                #[cfg(feature = "everything")]
                {
                    if self.config.launcher.everything.enabled {
                        if self.files.is_none() {
                            self.files = Some(crate::everything::EverythingWorker::start(
                                hwnd,
                                WM_APP_FILES,
                                self.config.launcher.everything.max_results as u32,
                            ));
                        }
                        self.files_generation += 1;
                        if let Some(f) = &self.files {
                            f.query(self.files_generation, self.query.trim());
                        }
                        rows.extend(self.file_rows.iter().cloned());
                    }
                }
                rows
            }
        };
        self.scroll = 0.0;
        self.selected = if self.rows.is_empty() {
            None
        } else {
            Some(Item::Entry(0))
        };
        invalidate(hwnd);
    }

    fn set_view(&mut self, hwnd: HWND, view: View) {
        self.view = view;
        self.rebuild_rows(hwnd);
    }

    fn read_query(&mut self) {
        // SAFETY: buffer sized from GetWindowTextLengthW.
        unsafe {
            let len = GetWindowTextLengthW(self.edit).max(0) as usize;
            let mut buf = vec![0u16; len + 1];
            let n = GetWindowTextW(self.edit, &mut buf).max(0) as usize;
            self.query = String::from_utf16_lossy(&buf[..n]);
        }
    }

    fn apply_dpi(&mut self, hwnd: HWND) {
        self.scale = explorer_win::dpi::scale(explorer_win::dpi::window_dpi(hwnd));
        // SAFETY: GDI font lifetime managed here; old font deleted after
        // the control switched to the new one.
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
        self.layout_edit(hwnd);
    }

    fn search_rect(&self, w: f32) -> Rect {
        Rect::new(
            self.s(PAD),
            self.s(PAD),
            w - 2.0 * self.s(PAD),
            self.s(SEARCH_H),
        )
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

    fn show(&mut self, hwnd: HWND) {
        self.reload_palette();
        // Anchor to the monitor under the mouse cursor, above the taskbar
        // (the work area excludes it), centred or left like the taskbar.
        let mut pt = POINT::default();
        // SAFETY: plain FFI calls with valid pointers.
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            if let Some(m) = explorer_win::monitors::query(mon) {
                self.scale = explorer_win::dpi::scale(m.dpi);
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
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
            }
            self.apply_dpi(hwnd);
            // Clearing the text sends EN_CHANGE re-entrantly; that nested
            // message is dropped by the window plumbing, so reset here.
            let empty = wide("");
            let _ = SetWindowTextW(self.edit, PCWSTR(empty.as_ptr()));
            self.query.clear();
            explorer_win::activate::force_foreground(hwnd);
            let _ = SetFocus(Some(self.edit));
        }
        if self.indexed_at.is_none_or(|t| t.elapsed() > REINDEX_AFTER) {
            self.start_indexing(hwnd);
        }
        self.set_view(hwnd, View::Home);
    }

    fn hide(&mut self, hwnd: HWND) {
        // SAFETY: hiding our own window.
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    }

    fn launch(&mut self, hwnd: HWND, row: Row, elevated: bool) {
        self.hide(hwnd);
        tracing::info!(name = %row.name, target = ?row.target, elevated, "launching");
        if row.kind != RowKind::File {
            self.history.record_now(&row.target);
            if let Err(e) = self.history.save(&self.paths.history_file()) {
                tracing::warn!(error = %e, "cannot save launch history");
            }
        }
        let target = row.target;
        std::thread::spawn(move || {
            let _com = ComApartment::sta();
            if let Err(e) = explorer_win::apps::launch(&target, elevated) {
                tracing::error!(error = %e, "launch failed");
            }
        });
    }

    fn activate_item(&mut self, hwnd: HWND, item: Item, elevated: bool) {
        match item {
            Item::Entry(i) => {
                if let Some(row) = self.rows.get(i).cloned() {
                    self.launch(hwnd, row, elevated);
                }
            }
            Item::AllApps => self.set_view(hwnd, View::AllApps),
            Item::Back => self.set_view(hwnd, View::Home),
            Item::User => {
                self.hide(hwnd);
                let _ = explorer_win::apps::shell_execute("ms-settings:accounts", false);
            }
            Item::Power => self.power_menu(hwnd),
        }
    }

    fn power_menu(&mut self, hwnd: HWND) {
        let cmd = popup_menu(
            hwnd,
            &[
                (CMD_LOCK, "Lock"),
                (CMD_SIGN_OUT, "Sign out"),
                (0, ""),
                (CMD_SLEEP, "Sleep"),
                (CMD_SHUTDOWN, "Shut down"),
                (CMD_RESTART, "Restart"),
            ],
        );
        if cmd == 0 {
            return;
        }
        self.hide(hwnd);
        let run = |args: &[&str]| {
            if let Err(e) = std::process::Command::new("shutdown.exe")
                .args(args)
                .spawn()
            {
                tracing::error!(error = %e, "shutdown.exe failed");
            }
        };
        // SAFETY: plain FFI calls.
        unsafe {
            match cmd {
                CMD_LOCK => {
                    let _ = LockWorkStation();
                }
                CMD_SIGN_OUT => {
                    let _ = ExitWindowsEx(EWX_LOGOFF, SHUTDOWN_REASON(0));
                }
                CMD_SLEEP => {
                    let _ = explorer_win::windows::Win32::System::Power::SetSuspendState(
                        false, false, false,
                    );
                }
                CMD_SHUTDOWN => run(&["/s", "/t", "0"]),
                CMD_RESTART => run(&["/r", "/t", "0"]),
                _ => {}
            }
        }
    }

    fn context_menu(&mut self, hwnd: HWND, i: usize) {
        let Some(row) = self.rows.get(i).cloned() else {
            return;
        };
        if row.kind == RowKind::File {
            return;
        }
        let pinned = self.pins.contains(&row.target);
        let mut items: Vec<(u32, &str)> = Vec::new();
        if pinned {
            items.push((CMD_UNPIN, "Unpin from Start"));
            items.push((CMD_FRONT, "Move to front"));
        } else {
            items.push((CMD_PIN, "Pin to Start"));
        }
        let packaged = matches!(&row.target, LaunchTarget::AppsFolder(id) if explorer_core::catalog::is_aumid(id));
        if !packaged {
            items.push((0, ""));
            items.push((CMD_ADMIN, "Run as administrator"));
        }
        if matches!(row.target, LaunchTarget::Path(_)) {
            items.push((CMD_LOCATION, "Open file location"));
        }
        match popup_menu(hwnd, &items) {
            CMD_PIN => {
                self.pins.pin(&row.target);
                self.save_pins();
                self.rebuild_rows(hwnd);
            }
            CMD_UNPIN => {
                self.pins.unpin(&row.target);
                self.save_pins();
                self.rebuild_rows(hwnd);
            }
            CMD_FRONT => {
                self.pins.move_to_front(&row.target);
                self.save_pins();
                self.rebuild_rows(hwnd);
            }
            CMD_ADMIN => self.launch(hwnd, row, true),
            CMD_LOCATION => {
                if let LaunchTarget::Path(p) = &row.target {
                    self.hide(hwnd);
                    let _ = std::process::Command::new("explorer.exe")
                        .arg(format!("/select,{}", p.display()))
                        .spawn();
                }
            }
            _ => {}
        }
    }

    fn save_pins(&self) {
        if let Err(e) = self.pins.save(&self.paths.pins_file()) {
            tracing::warn!(error = %e, "cannot save pins");
        }
    }

    fn selectable(&self) -> Vec<(Item, Rect)> {
        self.items
            .iter()
            .filter(|(i, _)| matches!(i, Item::Entry(_)))
            .copied()
            .collect()
    }

    fn on_nav_key(&mut self, hwnd: HWND, vk: u32) {
        match vk {
            VK_ESCAPE => {
                if !self.query.is_empty() {
                    let empty = wide("");
                    // SAFETY: our own control; EN_CHANGE updates the view.
                    let _ = unsafe { SetWindowTextW(self.edit, PCWSTR(empty.as_ptr())) };
                    self.query.clear();
                    self.set_view(hwnd, View::Home);
                } else if self.view != View::Home {
                    self.set_view(hwnd, View::Home);
                } else {
                    self.hide(hwnd);
                }
                return;
            }
            VK_RETURN => {
                // SAFETY: plain key-state queries.
                let (ctrl, shift) = unsafe {
                    (
                        GetKeyState(VK_CONTROL.0 as i32) < 0,
                        GetKeyState(VK_SHIFT.0 as i32) < 0,
                    )
                };
                if let Some(item) = self.selected {
                    self.activate_item(hwnd, item, ctrl && shift);
                }
                return;
            }
            _ => {}
        }
        let sel = self.selectable();
        if sel.is_empty() {
            return;
        }
        let rects: Vec<Rect> = sel.iter().map(|(_, r)| *r).collect();
        let cur = self
            .selected
            .and_then(|s| sel.iter().position(|(i, _)| *i == s));
        let next = match (cur, vk) {
            (None, _) => Some(0),
            (Some(c), VK_DOWN) => nav(&rects, c, Dir::Down).or_else(|| {
                // List views: the next item may be scrolled out of view.
                matches!(self.selected, Some(Item::Entry(e)) if e + 1 < self.rows.len())
                    .then_some(usize::MAX)
            }),
            (Some(c), VK_TAB) => Some((c + 1) % rects.len()),
            (Some(c), VK_UP) => nav(&rects, c, Dir::Up).or_else(|| {
                matches!(self.selected, Some(Item::Entry(e)) if e > 0).then_some(usize::MAX - 1)
            }),
            (Some(c), VK_LEFT) => nav(&rects, c, Dir::Left),
            (Some(c), VK_RIGHT) => nav(&rects, c, Dir::Right),
            (Some(_), VK_NEXT) => {
                self.scroll_by(hwnd, self.s(ROW_H) * 8.0);
                None
            }
            (Some(_), VK_PRIOR) => {
                self.scroll_by(hwnd, -self.s(ROW_H) * 8.0);
                None
            }
            _ => None,
        };
        match next {
            Some(usize::MAX) => {
                if let Some(Item::Entry(e)) = self.selected {
                    self.selected = Some(Item::Entry(e + 1));
                    self.scroll_by(hwnd, self.s(ROW_H));
                }
            }
            Some(n) if n == usize::MAX - 1 => {
                if let Some(Item::Entry(e)) = self.selected {
                    self.selected = Some(Item::Entry(e - 1));
                    self.scroll_by(hwnd, -self.s(ROW_H));
                }
            }
            Some(n) => {
                self.selected = Some(sel[n].0);
                self.ensure_visible(hwnd, sel[n].1);
            }
            None => {}
        }
        invalidate(hwnd);
    }

    fn list_area(&self, w: f32, h: f32) -> Rect {
        let top = self.s(PAD + SEARCH_H + 16.0 + HEADER_H + 8.0);
        Rect::new(
            self.s(PAD) - self.s(8.0),
            top,
            w - 2.0 * self.s(PAD) + self.s(16.0),
            h - top - self.s(FOOTER_H) - self.s(8.0),
        )
    }

    fn scroll_by(&mut self, hwnd: HWND, dy: f32) {
        if self.view == View::Home {
            return;
        }
        let (w, h) = client_size(hwnd);
        let area = self.list_area(w, h);
        let max = (self.content_h - area.h).max(0.0);
        self.scroll = (self.scroll + dy).clamp(0.0, max);
        invalidate(hwnd);
    }

    fn ensure_visible(&mut self, hwnd: HWND, r: Rect) {
        if self.view == View::Home {
            return;
        }
        let (w, h) = client_size(hwnd);
        let area = self.list_area(w, h);
        if r.y < area.y {
            self.scroll_by(hwnd, r.y - area.y);
        } else if r.bottom() > area.bottom() {
            self.scroll_by(hwnd, r.bottom() - area.bottom());
        }
    }

    fn hit(&self, lparam: LPARAM) -> Option<Item> {
        let x = (lparam.0 & 0xffff) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        self.items
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(i, _)| *i)
    }

    fn icon_for(&self, t: &LaunchTarget) -> Option<SharedImage> {
        self.icons.get(&t.key()).cloned()
    }

    fn paint(&mut self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: BeginPaint/EndPaint pairing.
        unsafe { BeginPaint(hwnd, &mut ps) };
        let (w, h) = client_size(hwnd);
        let sc = self.scale;
        let s = |v: f32| v * sc;
        let pal = self.palette;
        let mut items: Vec<(Item, Rect)> = Vec::new();
        let search = self.search_rect(w);
        let header_y = search.bottom() + s(16.0);
        let list = self.list_area(w, h);
        let footer = Rect::new(0.0, h - s(FOOTER_H), w, s(FOOTER_H));
        let rows = &self.rows;
        let icons: Vec<Option<SharedImage>> =
            rows.iter().map(|r| self.icon_for(&r.target)).collect();
        let selected = self.selected;
        let view = self.view;
        let pinned_count = if view == View::Home {
            self.pins
                .resolve(&self.catalog)
                .len()
                .min(PIN_COLS * PIN_ROWS)
        } else {
            0
        };
        let letters = &self.letters;
        let scroll = self.scroll;
        let mut content_h = 0.0;
        let status = if self.catalog.is_empty() && self.indexing {
            Some("Loading apps…")
        } else if view == View::Search && rows.is_empty() {
            Some("No results")
        } else {
            None
        };
        let user_name = self.user_name.clone();
        let surface = self.surface.get_or_insert_with(|| Surface::new(hwnd));
        surface.draw(&self.gfx, Color(pal.panel_bg), |p| {
            // Border.
            p.stroke_rounded(
                Rect::new(0.5, 0.5, w - 1.0, h - 1.0),
                s(8.0),
                1.0,
                Color(pal.border),
            );
            // Search box.
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

            let header = |p: &Painter<'_>, title: &str| {
                p.text(
                    title,
                    Rect::new(s(PAD + 8.0), header_y, s(300.0), s(HEADER_H)),
                    s(14.0),
                    Color(pal.text),
                    Align::Left,
                    true,
                );
            };
            let pill_button = |p: &Painter<'_>,
                               items: &mut Vec<(Item, Rect)>,
                               label: &str,
                               item: Item,
                               left_chevron: bool| {
                let r = Rect::new(w - s(PAD) - s(104.0), header_y + s(2.0), s(104.0), s(28.0));
                let hovered = selected == Some(item);
                p.fill_rounded(
                    r,
                    s(4.0),
                    Color(if hovered { pal.selection } else { pal.input_bg }),
                );
                let (tr, gr) = if left_chevron {
                    (
                        Rect::new(r.x + s(22.0), r.y, r.w - s(28.0), r.h),
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
                    tr,
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
                items.push((item, r));
            };

            match view {
                View::Home => {
                    header(p, "Pinned");
                    pill_button(p, &mut items, "All apps", Item::AllApps, false);
                    let grid_area = Rect::new(
                        s(PAD) - s(8.0),
                        header_y + s(HEADER_H + 8.0),
                        w - 2.0 * s(PAD) + s(16.0),
                        s(PIN_CELL_H) * PIN_ROWS as f32,
                    );
                    let cells = grid(grid_area, PIN_COLS, s(PIN_CELL_H), pinned_count);
                    for (i, cell) in cells.iter().enumerate() {
                        let cell = Rect::new(
                            cell.x + s(2.0),
                            cell.y + s(2.0),
                            cell.w - s(4.0),
                            cell.h - s(4.0),
                        );
                        if selected == Some(Item::Entry(i)) {
                            p.fill_rounded(cell, s(4.0), Color(pal.selection));
                        }
                        let icon_r = Rect::new(
                            cell.x + (cell.w - s(32.0)) / 2.0,
                            cell.y + s(12.0),
                            s(32.0),
                            s(32.0),
                        );
                        match &icons[i] {
                            Some(img) => p.image(img, icon_r),
                            None => p.glyph(glyph::APP, icon_r, s(24.0), Color(pal.text_dim)),
                        }
                        p.text(
                            &rows[i].name,
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
                        items.push((Item::Entry(i), cell));
                    }
                    if pinned_count == 0 && status.is_none() {
                        p.text(
                            "Right-click an app in All apps to pin it here",
                            grid_area,
                            s(13.0),
                            Color(pal.text_dim),
                            Align::Center,
                            false,
                        );
                    }
                    // Most used.
                    let rec_y = grid_area.bottom() + s(12.0);
                    p.text(
                        "Most used",
                        Rect::new(s(PAD + 8.0), rec_y, s(300.0), s(HEADER_H)),
                        s(14.0),
                        Color(pal.text),
                        Align::Left,
                        true,
                    );
                    let rec_area = Rect::new(
                        s(PAD) - s(8.0),
                        rec_y + s(HEADER_H + 4.0),
                        w - 2.0 * s(PAD) + s(16.0),
                        (footer.y - s(8.0)) - (rec_y + s(HEADER_H + 4.0)),
                    );
                    let used = rows.len() - pinned_count;
                    if used == 0 && status.is_none() {
                        p.text(
                            "Apps you open often will appear here",
                            Rect::new(rec_area.x, rec_area.y, rec_area.w, s(40.0)),
                            s(13.0),
                            Color(pal.text_dim),
                            Align::Center,
                            false,
                        );
                    }
                    for (k, cell) in grid(rec_area, 2, s(REC_CELL_H), used).iter().enumerate() {
                        let i = pinned_count + k;
                        let cell = Rect::new(
                            cell.x + s(2.0),
                            cell.y + s(2.0),
                            cell.w - s(4.0),
                            cell.h - s(4.0),
                        );
                        if selected == Some(Item::Entry(i)) {
                            p.fill_rounded(cell, s(4.0), Color(pal.selection));
                        }
                        let icon_r = Rect::new(
                            cell.x + s(12.0),
                            cell.y + (cell.h - s(32.0)) / 2.0,
                            s(32.0),
                            s(32.0),
                        );
                        if let Some(img) = &icons[i] {
                            p.image(img, icon_r);
                        }
                        p.text(
                            &rows[i].name,
                            Rect::new(
                                icon_r.right() + s(12.0),
                                cell.y,
                                cell.right() - icon_r.right() - s(20.0),
                                cell.h,
                            ),
                            s(13.0),
                            Color(pal.text),
                            Align::Left,
                            false,
                        );
                        items.push((Item::Entry(i), cell));
                    }
                }
                View::AllApps | View::Search => {
                    if view == View::AllApps {
                        header(p, "All apps");
                        pill_button(p, &mut items, "Back", Item::Back, true);
                    } else {
                        header(
                            p,
                            if rows.is_empty() {
                                "Search"
                            } else {
                                "Best matches"
                            },
                        );
                    }
                    // Lay out rows (with letter headers in All apps),
                    // clipped to the list area.
                    let mut y = list.y - scroll;
                    p.clipped(list, |p| {
                        let mut li = 0;
                        for (i, row) in rows.iter().enumerate() {
                            if view == View::AllApps
                                && letters.get(li).is_some_and(|(at, _)| *at == i)
                            {
                                let (_, l) = letters[li];
                                li += 1;
                                let r = Rect::new(list.x + s(12.0), y, s(40.0), s(LETTER_H));
                                if r.bottom() > list.y && r.y < list.bottom() {
                                    p.text(
                                        &l.to_string(),
                                        r,
                                        s(13.0),
                                        Color(pal.accent_text),
                                        Align::Left,
                                        true,
                                    );
                                }
                                y += s(LETTER_H);
                            }
                            let r = Rect::new(list.x, y, list.w, s(ROW_H));
                            y += s(ROW_H);
                            if r.bottom() <= list.y || r.y >= list.bottom() {
                                continue;
                            }
                            if selected == Some(Item::Entry(i)) {
                                p.fill_rounded(r, s(4.0), Color(pal.selection));
                                if view == View::Search {
                                    p.fill_rounded(
                                        Rect::new(r.x, r.y + s(10.0), s(3.0), r.h - s(20.0)),
                                        s(1.5),
                                        Color(pal.accent_text),
                                    );
                                }
                            }
                            let icon_r = Rect::new(
                                r.x + s(12.0),
                                r.y + (r.h - s(24.0)) / 2.0,
                                s(24.0),
                                s(24.0),
                            );
                            if let Some(img) = &icons[i] {
                                p.image(img, icon_r);
                            }
                            let label_w = s(160.0);
                            p.text(
                                &row.name,
                                Rect::new(
                                    icon_r.right() + s(12.0),
                                    r.y,
                                    r.w - label_w - s(60.0),
                                    r.h,
                                ),
                                s(13.0),
                                Color(pal.text),
                                Align::Left,
                                false,
                            );
                            if view == View::Search {
                                let right = if row.detail.is_empty() {
                                    row.kind.label().to_string()
                                } else {
                                    format!("{} · {}", row.detail, row.kind.label())
                                };
                                p.text(
                                    &right,
                                    Rect::new(r.right() - label_w - s(12.0), r.y, label_w, r.h),
                                    s(11.0),
                                    Color(pal.text_dim),
                                    Align::Right,
                                    false,
                                );
                            }
                            // Clip hit rect to the visible list area.
                            let top = r.y.max(list.y);
                            let bottom = r.bottom().min(list.bottom());
                            items.push((Item::Entry(i), Rect::new(r.x, top, r.w, bottom - top)));
                        }
                    });
                    content_h = y + scroll - list.y;
                    // Mask anything drawn above the list (letter headers
                    // scrolled under the header area).
                }
            }
            if let Some(msg) = status {
                p.text(
                    msg,
                    Rect::new(list.x, list.y, list.w, s(48.0)),
                    s(13.0),
                    Color(pal.text_dim),
                    Align::Center,
                    false,
                );
            }

            // Footer.
            p.fill(footer, Color(theme_mix(pal.panel_bg, pal.light)));
            p.fill(Rect::new(0.0, footer.y, w, 1.0), Color(pal.border));
            let user_r = Rect::new(s(PAD), footer.y + s(12.0), s(260.0), footer.h - s(24.0));
            if selected == Some(Item::User) {
                p.fill_rounded(user_r, s(4.0), Color(pal.selection));
            }
            p.glyph(
                glyph::USER,
                Rect::new(user_r.x + s(8.0), user_r.y, s(28.0), user_r.h),
                s(16.0),
                Color(pal.text),
            );
            p.text(
                &user_name,
                Rect::new(user_r.x + s(44.0), user_r.y, user_r.w - s(52.0), user_r.h),
                s(13.0),
                Color(pal.text),
                Align::Left,
                false,
            );
            items.push((Item::User, user_r));
            let power_r = Rect::new(
                w - s(PAD) - s(40.0),
                footer.y + s(12.0),
                s(40.0),
                footer.h - s(24.0),
            );
            if selected == Some(Item::Power) {
                p.fill_rounded(power_r, s(4.0), Color(pal.selection));
            }
            p.glyph(glyph::POWER, power_r, s(16.0), Color(pal.text));
            items.push((Item::Power, power_r));
        });
        self.items = items;
        self.content_h = content_h;
        // SAFETY: matches BeginPaint.
        let _ = unsafe { EndPaint(hwnd, &ps) };
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
                self.apply_dpi(hwnd);
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
                        WorkerMsg::Catalog(entries) => {
                            self.catalog = Catalog::build(entries);
                            self.indexed_at = Some(Instant::now());
                            if self.pins.seed_defaults(&self.catalog) {
                                self.save_pins();
                            }
                            self.rebuild_rows(hwnd);
                        }
                        WorkerMsg::Icons(icons) => {
                            self.icons = icons;
                            self.indexing = false;
                            self.index_rx = None;
                            invalidate(hwnd);
                        }
                    }
                }
                Some(LRESULT(0))
            }
            #[cfg(feature = "everything")]
            WM_APP_FILES => {
                let mut changed = false;
                if let Some(f) = &self.files {
                    while let Ok((gen, paths)) = f.results.try_recv() {
                        if gen == self.files_generation {
                            self.file_rows = paths
                                .into_iter()
                                .map(|p: std::path::PathBuf| Row {
                                    name: p
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_default(),
                                    detail: p
                                        .parent()
                                        .map(|d| d.to_string_lossy().into_owned())
                                        .unwrap_or_default(),
                                    target: LaunchTarget::Path(p),
                                    kind: RowKind::File,
                                })
                                .collect();
                            changed = true;
                        }
                    }
                }
                if changed && self.view == View::Search {
                    self.rows.retain(|r| r.kind != RowKind::File);
                    self.rows.extend(self.file_rows.iter().cloned());
                    invalidate(hwnd);
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
                    self.read_query();
                    let view = if self.query.trim().is_empty() {
                        View::Home
                    } else {
                        View::Search
                    };
                    self.set_view(hwnd, view);
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
                if let Some(item) = self.hit(lparam) {
                    if self.selected != Some(item) {
                        self.selected = Some(item);
                        invalidate(hwnd);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                // SAFETY: keep keyboard focus in the query box.
                let _ = unsafe { SetFocus(Some(self.edit)) };
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if let Some(item) = self.hit(lparam) {
                    self.activate_item(hwnd, item, false);
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                if let Some(Item::Entry(i)) = self.hit(lparam) {
                    self.context_menu(hwnd, i);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as f32;
                self.scroll_by(hwnd, -delta / 120.0 * self.s(ROW_H) * 3.0);
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
                self.apply_dpi(hwnd);
                Some(LRESULT(0))
            }
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
