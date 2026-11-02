// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows implementation of the launcher.
//!
//! One popup window, hidden while idle. The query box is a native EDIT
//! control (IME, clipboard, selection for free); results are painted with
//! Direct2D. Navigation keys are intercepted in the message loop before the
//! EDIT control sees them. App indexing runs on a worker thread so the UI
//! never waits on the Shell namespace.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use explorer_core::catalog::{
    scan_folder, unix_now, AppEntry, AppSource, Catalog, LaunchHistory, LaunchTarget,
};
use explorer_core::config::Config;
use explorer_core::ipc::{LauncherCommand, RunMode, LAUNCHER_WINDOW_CLASS};
use explorer_core::layout::{scroll_to_visible, Rect};
use explorer_core::paths::AppPaths;
use explorer_core::search::Searcher;
use explorer_win::gfx::{theme, Align, Gfx, Surface};
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
use explorer_win::windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_CONTROL, VK_SHIFT,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetClientRect, GetCursorPos, GetWindowTextLengthW, GetWindowTextW, MoveWindow,
    PostMessageW, SendMessageW, SetWindowPos, SetWindowTextW, ShowWindow, ES_AUTOHSCROLL, HMENU,
    HWND_TOPMOST, MSG, SWP_SHOWWINDOW, SW_HIDE, WINDOW_STYLE, WM_ACTIVATE, WM_APP, WM_COMMAND,
    WM_CREATE, WM_CTLCOLOREDIT, WM_DESTROY, WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_MOUSEWHEEL, WM_PAINT, WM_SETFONT, WM_SIZE, WS_CHILD, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::Args;

const WM_APP_INDEXED: u32 = WM_APP + 10;
const WM_APP_NAVKEY: u32 = WM_APP + 11;
#[cfg(feature = "everything")]
const WM_APP_FILES: u32 = WM_APP + 12;

const EN_CHANGE: u32 = 0x0300;

const VK_UP: u32 = 0x26;
const VK_DOWN: u32 = 0x28;
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

/// Logical metrics.
const PAD: f32 = 12.0;
const INPUT_H: f32 = 40.0;
const ROW_H: f32 = 40.0;

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

/// Index all app sources. Must run with COM initialised.
fn index_apps(extra_folders: &[String]) -> Vec<AppEntry> {
    let started = Instant::now();
    let mut v = Vec::new();
    for f in explorer_win::apps::start_menu_folders() {
        v.extend(scan_folder(&f, AppSource::StartMenu));
    }
    match explorer_win::apps::apps_folder() {
        Ok(a) => v.extend(a),
        Err(e) => tracing::warn!(error = %e, "shell:AppsFolder enumeration failed"),
    }
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
    let history = LaunchHistory::load(&paths.history_file());
    let launcher = Launcher {
        gfx,
        history_path: paths.history_file(),
        history,
        config,
        catalog: Catalog::default(),
        searcher: Searcher::new(),
        rows: Vec::new(),
        selected: 0,
        scroll: 0,
        indexing: false,
        indexed_at: None,
        index_rx: None,
        surface: None,
        scale: 1.0,
        edit: HWND::default(),
        edit_font: None,
        edit_brush: None,
        edit_shared: edit_hwnd.clone(),
        launcher_msg: explorer_win::ipc::launcher_message(),
        query: String::new(),
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
            title: "explorer.rs launcher",
            style: WS_POPUP,
            ex_style: WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            rect: RECT {
                left: 0,
                top: 0,
                right: 640,
                bottom: 460,
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
        // Steal navigation keys from the EDIT control.
        if msg.message == WM_KEYDOWN
            && msg.hwnd == edit_hwnd.get()
            && NAV_KEYS.contains(&(msg.wParam.0 as u32))
        {
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
        false
    });
    tracing::info!(code, "launcher exiting");
    code
}

struct Launcher {
    gfx: Gfx,
    config: Config,
    history_path: PathBuf,
    history: LaunchHistory,
    catalog: Catalog,
    searcher: Searcher,
    rows: Vec<Row>,
    selected: usize,
    scroll: usize,
    indexing: bool,
    indexed_at: Option<Instant>,
    index_rx: Option<Receiver<Vec<AppEntry>>>,
    surface: Option<Surface>,
    scale: f32,
    edit: HWND,
    edit_font: Option<HFONT>,
    edit_brush: Option<HBRUSH>,
    edit_shared: Rc<Cell<HWND>>,
    launcher_msg: u32,
    query: String,
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

impl Launcher {
    fn s(&self, v: f32) -> f32 {
        v * self.scale
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
            let entries = index_apps(&extra);
            if tx.send(entries).is_ok() {
                // SAFETY: posting to the launcher window.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(target as *mut _)),
                        WM_APP_INDEXED,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
        });
    }

    fn refresh_results(&mut self, hwnd: HWND) {
        let max = self.config.launcher.max_results;
        let hits = self
            .searcher
            .search(&self.catalog, &self.history, &self.query, max, unix_now());
        let mut rows: Vec<Row> = hits
            .into_iter()
            .map(|h| {
                let kind = match (&h.entry.target, h.entry.source) {
                    (LaunchTarget::AppsFolder(_), _) => RowKind::App,
                    _ => RowKind::Shortcut,
                };
                Row {
                    name: h.entry.name.clone(),
                    detail: h.entry.keywords.first().cloned().unwrap_or_default(),
                    target: h.entry.target.clone(),
                    kind,
                }
            })
            .collect();
        #[cfg(feature = "everything")]
        {
            if self.config.launcher.everything.enabled && !self.query.trim().is_empty() {
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
            } else {
                self.file_rows.clear();
            }
            rows.extend(self.file_rows.iter().cloned());
        }
        self.rows = std::mem::take(&mut rows);
        self.selected = 0;
        self.scroll = 0;
        // SAFETY: repaint request on our own window.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
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

    fn rows_fit(&self, hwnd: HWND) -> usize {
        let mut rc = RECT::default();
        // SAFETY: valid out-pointer.
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let top = self.s(PAD + INPUT_H + PAD);
        let avail = (rc.bottom - rc.top) as f32 - top - self.s(PAD);
        (avail / self.s(ROW_H)).floor().max(0.0) as usize
    }

    fn apply_dpi(&mut self, hwnd: HWND) {
        self.scale = explorer_win::dpi::scale(explorer_win::dpi::window_dpi(hwnd));
        // SAFETY: GDI font lifetime managed here; old font deleted after
        // the control switched to the new one.
        unsafe {
            let face = WideString::new("Segoe UI");
            let font = CreateFontW(
                -(self.s(16.0) as i32),
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

    fn layout_edit(&self, hwnd: HWND) {
        let mut rc = RECT::default();
        // SAFETY: valid out-pointer; moving our own child control.
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
            let x = self.s(PAD + 12.0) as i32;
            let h = self.s(24.0) as i32;
            let y = (self.s(PAD) + (self.s(INPUT_H) - h as f32) / 2.0) as i32;
            let w = (rc.right - rc.left) - 2 * x;
            let _ = MoveWindow(self.edit, x, y, w, h, true);
        }
    }

    fn show(&mut self, hwnd: HWND) {
        // Place on the monitor under the mouse cursor.
        let mut pt = POINT::default();
        // SAFETY: plain FFI calls with valid pointers.
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            if let Some(m) = explorer_win::monitors::query(mon) {
                self.scale = explorer_win::dpi::scale(m.dpi);
                let w = self.s(self.config.launcher.width as f32) as i32;
                let h = self.s(self.config.launcher.height as f32) as i32;
                let work = m.work;
                let x = work.left + ((work.right - work.left) - w) / 2;
                let y = work.top + ((work.bottom - work.top) - h) / 4;
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
            }
            self.apply_dpi(hwnd);
            // Clearing the text sends EN_CHANGE re-entrantly; that nested
            // message is dropped by the window plumbing, so refresh here.
            let empty = wide("");
            let _ = SetWindowTextW(self.edit, PCWSTR(empty.as_ptr()));
            self.query.clear();
            explorer_win::activate::force_foreground(hwnd);
            let _ = SetFocus(Some(self.edit));
        }
        if self.indexed_at.is_none_or(|t| t.elapsed() > REINDEX_AFTER) {
            self.start_indexing(hwnd);
        }
        self.refresh_results(hwnd);
    }

    fn hide(&mut self, hwnd: HWND) {
        // SAFETY: hiding our own window.
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    }

    fn launch_selected(&mut self, hwnd: HWND, elevated: bool) {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return;
        };
        self.hide(hwnd);
        tracing::info!(name = %row.name, target = ?row.target, elevated, "launching");
        if row.kind != RowKind::File {
            self.history.record_now(&row.target);
            if let Err(e) = self.history.save(&self.history_path) {
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

    fn on_nav_key(&mut self, hwnd: HWND, vk: u32) {
        let n = self.rows.len();
        let page = self.rows_fit(hwnd).max(1);
        match vk {
            VK_ESCAPE => {
                self.hide(hwnd);
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
                self.launch_selected(hwnd, ctrl && shift);
                return;
            }
            VK_DOWN | VK_TAB if n > 0 => self.selected = (self.selected + 1) % n,
            VK_UP if n > 0 => self.selected = (self.selected + n - 1) % n,
            VK_NEXT if n > 0 => self.selected = (self.selected + page).min(n - 1),
            VK_PRIOR => self.selected = self.selected.saturating_sub(page),
            _ => return,
        }
        self.scroll = scroll_to_visible(self.selected, self.scroll, page, n);
        // SAFETY: repaint request on our own window.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }

    fn row_at(&self, hwnd: HWND, lparam: LPARAM) -> Option<usize> {
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        let top = self.s(PAD + INPUT_H + PAD);
        if y < top {
            return None;
        }
        let idx = self.scroll + ((y - top) / self.s(ROW_H)) as usize;
        (idx < self.rows.len() && idx < self.scroll + self.rows_fit(hwnd)).then_some(idx)
    }

    fn paint(&mut self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: BeginPaint/EndPaint pairing.
        unsafe { BeginPaint(hwnd, &mut ps) };
        let mut rc = RECT::default();
        // SAFETY: valid out-pointer.
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let w = (rc.right - rc.left) as f32;
        let fit = self.rows_fit(hwnd);
        let sc = self.scale;
        let s = |v: f32| v * sc;
        let rows = &self.rows;
        let (selected, scroll) = (self.selected, self.scroll);
        let status = if rows.is_empty() {
            Some(if self.indexing && self.catalog.is_empty() {
                "Indexing apps…"
            } else if self.query.trim().is_empty() {
                "Type to search apps"
            } else {
                "No matching apps"
            })
        } else {
            None
        };
        let surface = self.surface.get_or_insert_with(|| Surface::new(hwnd));
        surface.draw(&self.gfx, theme::PANEL_BG, |p| {
            p.fill_rounded(
                Rect::new(s(PAD), s(PAD), w - 2.0 * s(PAD), s(INPUT_H)),
                s(6.0),
                theme::BUTTON_BG,
            );
            p.fill(
                Rect::new(
                    s(PAD) + s(6.0),
                    s(PAD) + s(INPUT_H) - s(2.0),
                    w - 2.0 * s(PAD) - s(12.0),
                    s(2.0),
                ),
                theme::ACCENT,
            );
            let top = s(PAD + INPUT_H + PAD);
            if let Some(msg) = status {
                p.text(
                    msg,
                    Rect::new(s(PAD), top, w - 2.0 * s(PAD), s(ROW_H)),
                    s(13.0),
                    theme::TEXT_DIM,
                    Align::Center,
                    false,
                );
            }
            for (i, row) in rows.iter().enumerate().skip(scroll).take(fit) {
                let y = top + (i - scroll) as f32 * s(ROW_H);
                let r = Rect::new(s(PAD), y, w - 2.0 * s(PAD), s(ROW_H));
                if i == selected {
                    p.fill_rounded(r, s(4.0), theme::SELECTION);
                    p.fill_rounded(
                        Rect::new(r.x, r.y + s(10.0), s(3.0), r.h - s(20.0)),
                        s(1.5),
                        theme::ACCENT,
                    );
                }
                let label_w = s(90.0);
                p.text(
                    &row.name,
                    Rect::new(r.x + s(14.0), r.y, r.w - label_w - s(28.0), r.h),
                    s(14.0),
                    theme::TEXT,
                    Align::Left,
                    false,
                );
                let right = if row.detail.is_empty() {
                    row.kind.label().to_string()
                } else {
                    format!("{} · {}", row.detail, row.kind.label())
                };
                p.text(
                    &right,
                    Rect::new(r.right() - label_w - s(140.0), r.y, label_w + s(130.0), r.h),
                    s(11.0),
                    theme::TEXT_DIM,
                    Align::Right,
                    false,
                );
            }
        });
        // SAFETY: matches BeginPaint.
        let _ = unsafe { EndPaint(hwnd, &ps) };
    }
}

impl Handler for Launcher {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if msg == self.launcher_msg && msg != 0 {
            match LauncherCommand::from_wparam(wparam.0) {
                Some(LauncherCommand::Show) => self.show(hwnd),
                Some(LauncherCommand::Hide) => self.hide(hwnd),
                Some(LauncherCommand::Toggle) => {
                    if is_visible(hwnd) {
                        self.hide(hwnd)
                    } else {
                        self.show(hwnd)
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
                // SAFETY: plain GDI call.
                self.edit_brush =
                    Some(unsafe { CreateSolidBrush(COLORREF(bgr(theme::BUTTON_BG.0))) });
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
                if let Some(rx) = self.index_rx.take() {
                    if let Ok(entries) = rx.try_recv() {
                        self.catalog = Catalog::build(entries);
                        self.indexed_at = Some(Instant::now());
                    }
                }
                self.indexing = false;
                self.refresh_results(hwnd);
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
                                .map(|p| Row {
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
                if changed {
                    self.rows.retain(|r| r.kind != RowKind::File);
                    self.rows.extend(self.file_rows.iter().cloned());
                    // SAFETY: repaint request.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
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
                    self.refresh_results(hwnd);
                }
                Some(LRESULT(0))
            }
            WM_CTLCOLOREDIT => {
                let hdc = HDC(wparam.0 as *mut _);
                // SAFETY: HDC supplied by the system for this message.
                unsafe {
                    SetTextColor(hdc, COLORREF(bgr(theme::TEXT.0)));
                    SetBkColor(hdc, COLORREF(bgr(theme::BUTTON_BG.0)));
                }
                self.edit_brush.map(|b| LRESULT(b.0 as isize))
            }
            WM_ACTIVATE => {
                // Hide when the user clicks elsewhere.
                if wparam.0 & 0xffff == 0 && is_visible(hwnd) {
                    self.hide(hwnd);
                }
                None
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                if let Some(i) = self.row_at(hwnd, lparam) {
                    self.selected = i;
                    if msg == WM_LBUTTONDBLCLK {
                        self.launch_selected(hwnd, false);
                    } else {
                        // SAFETY: repaint request.
                        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                    }
                }
                // SAFETY: keep keyboard focus in the query box.
                let _ = unsafe { SetFocus(Some(self.edit)) };
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16;
                let vk = if delta > 0 { VK_UP } else { VK_DOWN };
                self.on_nav_key(hwnd, vk);
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

/// 0xRRGGBB -> COLORREF (0x00BBGGRR).
fn bgr(rgb: u32) -> u32 {
    ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff)
}
