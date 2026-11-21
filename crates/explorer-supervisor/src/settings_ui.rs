// SPDX-License-Identifier: GPL-3.0-or-later
//! The settings window (`explorer-rs --settings`).
//!
//! Plain Win32 controls (common controls v6 through the manifest), like the
//! setup wizard (`explorer-setup/src/win/ui.rs`, docs/decisions/0001): a
//! list of sections on the left, the selected section's controls in a
//! scrollable panel on the right, and the buttons in a bottom bar. What the
//! controls mean, and how the form becomes TOML, is the portable
//! `explorer_supervisor::settings` model; this file only shows it.
//!
//! Light/dark follows `general.theme` (and with "system", Windows' app
//! mode) using the same techniques as the setup wizard: the documented
//! DWMWA_USE_IMMERSIVE_DARK_MODE title bar, custom WM_CTLCOLOR* colours,
//! and the undocumented, cosmetic "DarkMode_Explorer"/"DarkMode_CFD" visual
//! style classes (controls keep their light look where those are missing).
//! Check boxes are glyph-only controls next to a clickable label, because
//! themed buttons ignore WM_CTLCOLORSTATIC text colours.
//!
//! Saving never restarts anything by itself: the running supervisor
//! already watches the config file and restarts the components whose
//! settings changed. "Save and restart" additionally asks the guardian for
//! the same restart as Ctrl+Alt+Shift+R (the guardian stays up and starts
//! everything again, so the user is never left without a shell).

use std::path::{Path, PathBuf};

use explorer_core::config::{ThemeMode, DEFAULT_CONFIG_TOML};
use explorer_core::i18n::{tr, trf, Lang};
use explorer_supervisor::settings::{self, Input, Kind, Loaded, SECTIONS};
use explorer_win::util::WideString;
use explorer_win::window::{self, CreateParams, Handler};
use explorer_win::windows::core::{w, BOOL, PCWSTR};
use explorer_win::windows::Win32::Foundation::{
    COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use explorer_win::windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use explorer_win::windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetDC,
    MapWindowPoints, ReleaseDC, SelectObject, SetBkColor, SetTextColor, DT_CALCRECT,
    DT_EDITCONTROL, DT_NOPREFIX, DT_WORDBREAK, FW_SEMIBOLD, HBRUSH, HDC, HFONT, HGDIOBJ,
};
use explorer_win::windows::Win32::UI::Controls::{
    InitCommonControlsEx, SetScrollInfo, SetWindowTheme, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
};
use explorer_win::windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, SystemParametersInfoForDpi,
};
use explorer_win::windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowW, GetClientRect,
    GetParent, GetScrollInfo, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    IsDialogMessageW, IsIconic, LoadImageW, MessageBoxW, PostMessageW, PostQuitMessage,
    RegisterClassExW, ScrollWindowEx, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, SystemParametersInfoW, BM_CLICK, BM_GETCHECK,
    BM_SETCHECK, BN_CLICKED, BN_SETFOCUS, BS_AUTOCHECKBOX, BS_NOTIFY, BS_PUSHBUTTON, CBN_SETFOCUS,
    CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CS_HREDRAW, CS_VREDRAW,
    EN_SETFOCUS, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_NUMBER, ES_WANTRETURN,
    GWLP_WNDPROC, HICON, HMENU, ICON_BIG, ICON_SMALL, IDCANCEL, IDNO, IDYES, IMAGE_ICON,
    LBN_SELCHANGE, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LB_ADDSTRING, LB_GETCURSEL, LB_SETCURSEL,
    LB_SETITEMHEIGHT, LR_DEFAULTCOLOR, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION,
    MB_ICONWARNING, MB_OK, MB_YESNO, MB_YESNOCANCEL, MESSAGEBOX_STYLE, MINMAXINFO,
    NONCLIENTMETRICSW, SB_BOTTOM, SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBTRACK,
    SB_TOP, SB_VERT, SCROLLINFO, SIF_DISABLENOSCROLL, SIF_PAGE, SIF_POS, SIF_RANGE, SIF_TRACKPOS,
    SM_CXSCREEN, SM_CYSCREEN, SPI_GETNONCLIENTMETRICS, STN_CLICKED, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_ERASE, SW_HIDE, SW_INVALIDATE, SW_RESTORE, SW_SCROLLCHILDREN, SW_SHOW, WA_INACTIVE,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED,
    WM_ERASEBKGND, WM_GETMINMAXINFO, WM_MOUSEWHEEL, WM_SETFONT, WM_SETICON, WM_SIZE, WM_VSCROLL,
    WNDCLASSEXW, WNDPROC, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT,
    WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

const CLASS: &str = "ExplorerRs.Settings";
const PANEL_CLASS: &str = "ExplorerRs.SettingsPanel";
/// Single-instance mutex: a second `--settings` brings the first to front.
const MUTEX: &str = "ExplorerRs.Settings";
const WM_APP_INIT: u32 = WM_APP + 1;

const SS_LEFT: u32 = 0x0;
const SS_NOPREFIX: u32 = 0x80;
const SS_NOTIFY: u32 = 0x100;

// Control ids. Field `i` uses FIELD_BASE + i * FIELD_STRIDE + sub-id.
const ID_LIST: u16 = 10;
const ID_EDIT_FILE: u16 = 11;
const ID_SAVE: u16 = 12;
const ID_SAVE_RESTART: u16 = 13;
/// IDCANCEL, so Esc (IsDialogMessage) closes the window.
const ID_CLOSE: u16 = IDCANCEL.0 as u16;
const FIELD_BASE: u16 = 1000;
const FIELD_STRIDE: u16 = 16;
const SUB_CTL: u16 = 0;
const SUB_LABEL: u16 = 1;
/// Flag `k`: glyph at SUB_FLAG + 2k, its label at SUB_FLAG + 2k + 1.
const SUB_FLAG: u16 = 2;

// Layout in logical pixels (96 DPI).
const W: i32 = 880;
const H: i32 = 640;
const MIN_W: i32 = 760;
const MIN_H: i32 = 420;
const BAR: i32 = 56;
const SIDE: i32 = 210;
const M: i32 = 24;
const GLYPH: i32 = 20;
const LINE: i32 = 40;

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

#[derive(Clone, Copy)]
struct Colors {
    dark: bool,
    bg: COLORREF,
    bar: COLORREF,
    text: COLORREF,
    sub: COLORREF,
    edit: COLORREF,
}

impl Colors {
    fn new(dark: bool) -> Self {
        if dark {
            Self {
                dark,
                bg: rgb(0x20, 0x20, 0x20),
                bar: rgb(0x2B, 0x2B, 0x2B),
                text: rgb(0xFF, 0xFF, 0xFF),
                sub: rgb(0xC5, 0xC5, 0xC5),
                edit: rgb(0x2D, 0x2D, 0x2D),
            }
        } else {
            Self {
                dark,
                bg: rgb(0xFF, 0xFF, 0xFF),
                bar: rgb(0xF3, 0xF3, 0xF3),
                text: rgb(0x1A, 0x1A, 0x1A),
                sub: rgb(0x5C, 0x5C, 0x5C),
                edit: rgb(0xFF, 0xFF, 0xFF),
            }
        }
    }
}

/// Dark colours? `general.theme` wins; "system" follows Windows' app mode,
/// and high contrast always gets the light palette.
fn wants_dark(theme: ThemeMode) -> bool {
    use explorer_win::windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    let mut hc = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: valid struct with cbSize.
    let hc_on = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            Some((&mut hc as *mut HIGHCONTRASTW).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok()
        && (hc.dwFlags & HCF_HIGHCONTRASTON) == HCF_HIGHCONTRASTON;
    if hc_on {
        return false;
    }
    match theme {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => {
            explorer_win::theme::read_dword(
                r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "AppsUseLightTheme",
            )
            .unwrap_or(1)
                == 0
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Text,
    Sub,
    Edit,
    Side,
}

/// The controls of one field.
struct Row {
    field: &'static settings::Field,
    section: usize,
    label: HWND,
    /// Check box glyph, combo box or edit; unused (default) for flags.
    ctl: HWND,
    /// Flags: (glyph, label) per choice.
    flags: Vec<(HWND, HWND)>,
    help: HWND,
}

struct Settings {
    hwnd: HWND,
    panel: HWND,
    lang: Lang,
    path: PathBuf,
    config_override: Option<PathBuf>,
    /// The file as last read or written.
    loaded: Loaded,
    dpi: u32,
    colors: Colors,
    font: HFONT,
    title_font: HFONT,
    list: HWND,
    title: HWND,
    rows: Vec<Row>,
    buttons: [HWND; 4],
    section: usize,
    scroll: i32,
    content_h: i32,
}

fn hmenu(id: u16) -> Option<HMENU> {
    Some(HMENU(id as usize as *mut core::ffi::c_void))
}

fn make(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    ex: WINDOW_EX_STYLE,
    id: u16,
) -> HWND {
    let t = WideString::new(text);
    // SAFETY: standard child-window creation with valid strings.
    unsafe {
        CreateWindowExW(
            ex,
            class,
            t.pcwstr(),
            style | WS_CHILD,
            0,
            0,
            10,
            10,
            Some(parent),
            hmenu(id),
            Some(window::module_instance()),
            None,
        )
    }
    .unwrap_or_default()
}

fn set_text(h: HWND, s: &str) {
    let t = WideString::new(s);
    // SAFETY: valid window and string.
    let _ = unsafe { SetWindowTextW(h, t.pcwstr()) };
}

fn get_text(h: HWND) -> String {
    // SAFETY: plain query; the buffer is sized from it.
    let len = unsafe { GetWindowTextLengthW(h) }.max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    // SAFETY: valid buffer.
    let n = unsafe { GetWindowTextW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n.min(len)])
}

fn is_checked(h: HWND) -> bool {
    // SAFETY: plain message.
    unsafe { SendMessageW(h, BM_GETCHECK, None, None) }.0 == 1
}

fn set_checked(h: HWND, on: bool) {
    // SAFETY: plain message.
    unsafe { SendMessageW(h, BM_SETCHECK, Some(WPARAM(on as usize)), None) };
}

fn show(h: HWND, on: bool) {
    // SAFETY: valid child window.
    let _ = unsafe { ShowWindow(h, if on { SW_SHOW } else { SW_HIDE }) };
}

fn message_box(owner: Option<HWND>, text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    let t = WideString::new(text);
    // SAFETY: valid strings.
    unsafe { MessageBoxW(owner, t.pcwstr(), w!("explorer.rs"), style) }.0
}

/// Windows edit controls want CRLF line breaks.
fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn field_id(i: usize, sub: u16) -> u16 {
    FIELD_BASE + i as u16 * FIELD_STRIDE + sub
}

impl Settings {
    fn s(&self, en: &'static str, es: &'static str) -> &'static str {
        tr(self.lang, en, es)
    }

    fn px(&self, v: i32) -> i32 {
        v * self.dpi as i32 / 96
    }

    fn create_controls(&mut self) {
        let lang = self.lang;
        let none = WINDOW_EX_STYLE(0);
        let st = WINDOW_STYLE(SS_LEFT | SS_NOPREFIX);
        let dark = self.colors.dark;
        let theme = |h: HWND, class: PCWSTR| {
            if dark {
                // SAFETY: valid window; failure leaves the light theme.
                let _ = unsafe { SetWindowTheme(h, class, PCWSTR::null()) };
            }
        };

        // Every control is created visible and hidden explicitly when not
        // on the current page (an invisible-by-default control is easy to
        // forget to show).
        self.list = make(
            self.hwnd,
            w!("LISTBOX"),
            "",
            WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32) | WS_TABSTOP | WS_VISIBLE,
            none,
            ID_LIST,
        );
        theme(self.list, w!("DarkMode_Explorer"));
        for sec in SECTIONS {
            let t = WideString::new(sec.title.get(lang));
            // SAFETY: valid list box and string.
            unsafe {
                SendMessageW(
                    self.list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(t.pcwstr().0 as isize)),
                )
            };
        }

        // Scrollable panel holding the fields. WS_EX_CONTROLPARENT lets Tab
        // move into it; the scroll bar is always shown so its width never
        // changes the layout.
        let panel_class = WideString::new(PANEL_CLASS);
        // SAFETY: standard child-window creation.
        self.panel = unsafe {
            CreateWindowExW(
                WS_EX_CONTROLPARENT,
                panel_class.pcwstr(),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_CLIPCHILDREN,
                0,
                0,
                10,
                10,
                Some(self.hwnd),
                None,
                Some(window::module_instance()),
                None,
            )
        }
        .unwrap_or_default();
        theme(self.panel, w!("DarkMode_Explorer"));
        let p = self.panel;
        self.title = make(p, w!("STATIC"), "", st | WS_VISIBLE, none, 0);

        let check = WINDOW_STYLE((BS_AUTOCHECKBOX | BS_NOTIFY) as u32) | WS_TABSTOP | WS_VISIBLE;
        let clickable = st | WINDOW_STYLE(SS_NOTIFY) | WS_VISIBLE;
        let mut i = 0usize;
        for (si, sec) in SECTIONS.iter().enumerate() {
            for field in sec.fields {
                let label_text = field.label.get(lang);
                let label = make(
                    p,
                    w!("STATIC"),
                    label_text,
                    clickable,
                    none,
                    field_id(i, SUB_LABEL),
                );
                let mut flags = Vec::new();
                let ctl = match field.kind {
                    Kind::Bool => {
                        let h = make(
                            p,
                            w!("BUTTON"),
                            label_text,
                            check,
                            none,
                            field_id(i, SUB_CTL),
                        );
                        theme(h, w!("DarkMode_Explorer"));
                        h
                    }
                    Kind::Choice(opts) => {
                        let h = make(
                            p,
                            w!("COMBOBOX"),
                            "",
                            WINDOW_STYLE(CBS_DROPDOWNLIST as u32)
                                | WS_TABSTOP
                                | WS_VSCROLL
                                | WS_VISIBLE,
                            none,
                            field_id(i, SUB_CTL),
                        );
                        theme(h, w!("DarkMode_CFD"));
                        for o in opts {
                            let t = WideString::new(o.label.get(lang));
                            // SAFETY: valid combo box and string.
                            unsafe {
                                SendMessageW(
                                    h,
                                    CB_ADDSTRING,
                                    None,
                                    Some(LPARAM(t.pcwstr().0 as isize)),
                                )
                            };
                        }
                        h
                    }
                    Kind::Int { .. } | Kind::Text => {
                        let number = if matches!(field.kind, Kind::Int { .. }) {
                            ES_NUMBER as u32
                        } else {
                            0
                        };
                        let h = make(
                            p,
                            w!("EDIT"),
                            "",
                            WINDOW_STYLE(ES_AUTOHSCROLL as u32 | number) | WS_TABSTOP | WS_VISIBLE,
                            WS_EX_CLIENTEDGE,
                            field_id(i, SUB_CTL),
                        );
                        theme(h, w!("DarkMode_CFD"));
                        h
                    }
                    Kind::List => {
                        let h = make(
                            p,
                            w!("EDIT"),
                            "",
                            WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32)
                                | WS_VSCROLL
                                | WS_TABSTOP
                                | WS_VISIBLE,
                            WS_EX_CLIENTEDGE,
                            field_id(i, SUB_CTL),
                        );
                        theme(h, w!("DarkMode_Explorer"));
                        h
                    }
                    Kind::Flags(opts) => {
                        for (k, o) in opts.iter().enumerate() {
                            let k = k as u16;
                            let text = o.label.get(lang);
                            let g = make(
                                p,
                                w!("BUTTON"),
                                text,
                                check,
                                none,
                                field_id(i, SUB_FLAG + 2 * k),
                            );
                            theme(g, w!("DarkMode_Explorer"));
                            let l = make(
                                p,
                                w!("STATIC"),
                                text,
                                clickable,
                                none,
                                field_id(i, SUB_FLAG + 2 * k + 1),
                            );
                            flags.push((g, l));
                        }
                        HWND::default()
                    }
                };
                let help = make(
                    p,
                    w!("STATIC"),
                    field.help.get(lang),
                    st | WS_VISIBLE,
                    none,
                    0,
                );
                self.rows.push(Row {
                    field,
                    section: si,
                    label,
                    ctl,
                    flags,
                    help,
                });
                i += 1;
            }
        }

        let push = WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP | WS_VISIBLE;
        let labels = [
            (
                ID_EDIT_FILE,
                self.s("Edit config file…", "Editar archivo de configuración…"),
            ),
            (
                ID_SAVE_RESTART,
                self.s(
                    "Save and restart explorer.rs",
                    "Guardar y reiniciar explorer.rs",
                ),
            ),
            (ID_SAVE, self.s("Save", "Guardar")),
            (ID_CLOSE, self.s("Close", "Cerrar")),
        ];
        for (k, (id, text)) in labels.into_iter().enumerate() {
            self.buttons[k] = make(self.hwnd, w!("BUTTON"), text, push, none, id);
            theme(self.buttons[k], w!("DarkMode_Explorer"));
        }

        let roles = self.roles();
        PAINT.with(|p| {
            if let Some(p) = p.borrow_mut().as_mut() {
                p.roles = roles;
            }
        });
        self.fill(&settings::inputs_from(&self.loaded.config));
    }

    fn roles(&self) -> Vec<(isize, Role)> {
        let mut v = vec![(self.list.0 as isize, Role::Side)];
        for r in &self.rows {
            v.push((r.help.0 as isize, Role::Sub));
            if matches!(r.field.kind, Kind::Int { .. } | Kind::Text | Kind::List) {
                v.push((r.ctl.0 as isize, Role::Edit));
            }
        }
        v
    }

    fn update_fonts(&mut self) {
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        // SAFETY: valid struct with cbSize.
        let ok = unsafe {
            SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                ncm.cbSize,
                Some((&mut ncm as *mut NONCLIENTMETRICSW).cast()),
                0,
                self.dpi,
            )
        }
        .is_ok();
        // The message font is Segoe UI (Segoe UI Variable on Windows 11).
        let mut lf = ncm.lfMessageFont;
        if !ok {
            lf.lfHeight = -(12 * self.dpi as i32 / 96);
        }
        let mut tf = lf;
        tf.lfHeight = lf.lfHeight * 5 / 3;
        tf.lfWeight = FW_SEMIBOLD.0 as i32;
        // SAFETY: valid LOGFONTs; old fonts deleted after controls switch.
        unsafe {
            let font = CreateFontIndirectW(&lf);
            let title_font = CreateFontIndirectW(&tf);
            let mut all = vec![self.list];
            all.extend(self.buttons);
            for r in &self.rows {
                all.extend([r.label, r.ctl, r.help]);
                for (g, l) in &r.flags {
                    all.extend([*g, *l]);
                }
            }
            for h in all.into_iter().filter(|h| !h.is_invalid()) {
                SendMessageW(
                    h,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            SendMessageW(
                self.title,
                WM_SETFONT,
                Some(WPARAM(title_font.0 as usize)),
                Some(LPARAM(1)),
            );
            SendMessageW(
                self.list,
                LB_SETITEMHEIGHT,
                Some(WPARAM(0)),
                Some(LPARAM(self.px(32) as isize)),
            );
            if !self.font.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
            if !self.title_font.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.title_font.0));
            }
            self.font = font;
            self.title_font = title_font;
        }
    }

    /// Put `inputs` into the controls.
    fn fill(&self, inputs: &[Input]) {
        for (r, input) in self.rows.iter().zip(inputs) {
            match input {
                Input::Bool(b) => set_checked(r.ctl, *b),
                Input::Choice(i) => {
                    // SAFETY: plain message.
                    unsafe { SendMessageW(r.ctl, CB_SETCURSEL, Some(WPARAM(*i)), None) };
                }
                Input::Text(t) => set_text(r.ctl, &to_crlf(t)),
                Input::Flags(on) => {
                    for ((g, _), on) in r.flags.iter().zip(on) {
                        set_checked(*g, *on);
                    }
                }
            }
        }
    }

    /// Read the controls.
    fn inputs(&self) -> Vec<Input> {
        self.rows
            .iter()
            .map(|r| match r.field.kind {
                Kind::Bool => Input::Bool(is_checked(r.ctl)),
                Kind::Choice(_) => {
                    // SAFETY: plain message.
                    let i = unsafe { SendMessageW(r.ctl, CB_GETCURSEL, None, None) }.0;
                    Input::Choice(i.max(0) as usize)
                }
                Kind::Int { .. } | Kind::Text | Kind::List => Input::Text(get_text(r.ctl)),
                Kind::Flags(_) => {
                    Input::Flags(r.flags.iter().map(|(g, _)| is_checked(*g)).collect())
                }
            })
            .collect()
    }

    /// Unsaved changes in the form? An invalid entry counts as a change.
    fn dirty(&self) -> bool {
        match settings::apply(self.loaded.base_text(), &self.loaded.config, &self.inputs()) {
            Ok(a) => !a.changed.is_empty(),
            Err(_) => true,
        }
    }

    fn place(&self, h: HWND, x: i32, y: i32, w: i32, hgt: i32) {
        // SAFETY: valid child window.
        let _ = unsafe { SetWindowPos(h, None, x, y, w, hgt, SWP_NOZORDER | SWP_NOACTIVATE) };
    }

    /// Height of `text` wrapped to `width` pixels in the normal font.
    fn text_height(&self, hdc: HDC, text: &str, width: i32) -> i32 {
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: width.max(1),
            bottom: 0,
        };
        // SAFETY: valid DC, buffer and RECT.
        unsafe {
            DrawTextW(
                hdc,
                &mut buf,
                &mut rc,
                DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX | DT_EDITCONTROL,
            )
        };
        rc.bottom - rc.top
    }

    fn layout(&mut self) {
        let mut rc = RECT::default();
        // SAFETY: our own window.
        let _ = unsafe { GetClientRect(self.hwnd, &mut rc) };
        let (w, h) = (rc.right, rc.bottom);
        let bar = self.px(BAR);
        let side = self.px(SIDE);
        let gap = self.px(8);
        self.place(self.list, gap, gap, side - 2 * gap, h - bar - 2 * gap);
        self.place(self.panel, side, 0, w - side, h - bar);

        // Buttons: "Edit config file…" on the left, the rest on the right.
        let (bh, by) = (self.px(30), h - bar + (bar - self.px(30)) / 2);
        let wide = self.px(230);
        let narrow = self.px(96);
        let m = self.px(M);
        self.place(self.buttons[0], m, by, wide, bh);
        let close_x = w - m - narrow;
        let save_x = close_x - gap - narrow;
        self.place(self.buttons[3], close_x, by, narrow, bh);
        self.place(self.buttons[2], save_x, by, narrow, bh);
        self.place(self.buttons[1], save_x - gap - wide, by, wide, bh);
        self.layout_panel();
    }

    /// Place the current section's controls at the current scroll offset
    /// and update the scroll bar.
    fn layout_panel(&mut self) {
        let mut rc = RECT::default();
        // SAFETY: valid window.
        let _ = unsafe { GetClientRect(self.panel, &mut rc) };
        let (pw, ph) = (rc.right, rc.bottom);
        let m = self.px(M);
        let cw = (pw - 2 * m).max(self.px(200));
        let glyph = self.px(GLYPH);
        let indent = glyph + self.px(4);
        let row_h = self.px(24);

        // SAFETY: DC of our own window, released below.
        let hdc = unsafe { GetDC(Some(self.panel)) };
        // SAFETY: valid DC and font.
        let old = unsafe { SelectObject(hdc, HGDIOBJ(self.title_font.0)) };
        let mut y = self.px(16);
        let title_h = self.text_height(hdc, SECTIONS[self.section].title.get(self.lang), cw);
        // SAFETY: valid DC and font.
        unsafe { SelectObject(hdc, HGDIOBJ(self.font.0)) };
        self.place(self.title, m, y - self.scroll, cw, title_h + self.px(2));
        y += title_h + self.px(16);

        for r in self.rows.iter().filter(|r| r.section == self.section) {
            let label = r.field.label.get(self.lang);
            let help = r.field.help.get(self.lang);
            match r.field.kind {
                Kind::Bool => {
                    let lh = self.text_height(hdc, label, cw - indent).max(self.px(20));
                    self.place(r.ctl, m, y - self.scroll, glyph, self.px(20));
                    self.place(
                        r.label,
                        m + indent,
                        y + self.px(2) - self.scroll,
                        cw - indent,
                        lh,
                    );
                    y += lh + self.px(4);
                    let hh = self.text_height(hdc, help, cw - indent);
                    self.place(
                        r.help,
                        m + indent,
                        y - self.scroll,
                        cw - indent,
                        hh + self.px(2),
                    );
                    y += hh;
                }
                Kind::Flags(_) => {
                    let lh = self.text_height(hdc, label, cw);
                    self.place(r.label, m, y - self.scroll, cw, lh + self.px(2));
                    y += lh + self.px(6);
                    for (g, l) in &r.flags {
                        self.place(*g, m, y - self.scroll, glyph, self.px(20));
                        self.place(
                            *l,
                            m + indent,
                            y + self.px(2) - self.scroll,
                            cw - indent,
                            self.px(20),
                        );
                        y += self.px(26);
                    }
                    let hh = self.text_height(hdc, help, cw);
                    self.place(r.help, m, y - self.scroll, cw, hh + self.px(2));
                    y += hh;
                }
                _ => {
                    let lh = self.text_height(hdc, label, cw);
                    self.place(r.label, m, y - self.scroll, cw, lh + self.px(2));
                    y += lh + self.px(4);
                    match r.field.kind {
                        // A drop-down's height includes its open list.
                        Kind::Choice(_) => {
                            self.place(
                                r.ctl,
                                m,
                                y - self.scroll,
                                cw.min(self.px(340)),
                                self.px(300),
                            );
                        }
                        Kind::Int { .. } => {
                            self.place(r.ctl, m, y - self.scroll, self.px(120), row_h);
                        }
                        Kind::Text => {
                            self.place(r.ctl, m, y - self.scroll, cw.min(self.px(340)), row_h);
                        }
                        _ => {
                            self.place(r.ctl, m, y - self.scroll, cw, self.px(88));
                            y += self.px(88) - row_h;
                        }
                    }
                    y += row_h + self.px(4);
                    let hh = self.text_height(hdc, help, cw);
                    self.place(r.help, m, y - self.scroll, cw, hh + self.px(2));
                    y += hh;
                }
            }
            y += self.px(18);
        }
        // SAFETY: restoring the DC's font and releasing the DC.
        unsafe {
            SelectObject(hdc, old);
            ReleaseDC(Some(self.panel), hdc);
        }
        self.content_h = y;
        let max = (self.content_h - ph).max(0);
        if self.scroll > max {
            let dy = self.scroll - max;
            self.scroll = max;
            // SAFETY: valid window; moves the children back into range.
            unsafe { self.scroll_children(dy) };
        }
        self.update_scrollbar(ph);
    }

    fn update_scrollbar(&self, page: i32) {
        let si = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS | SIF_DISABLENOSCROLL,
            nMin: 0,
            nMax: (self.content_h - 1).max(0),
            nPage: page.max(0) as u32,
            nPos: self.scroll,
            nTrackPos: 0,
        };
        // SAFETY: valid struct.
        unsafe { SetScrollInfo(self.panel, SB_VERT, &si, true) };
    }

    /// Move the panel's children by `dy` pixels.
    ///
    /// # Safety
    /// `self.panel` must be a valid window.
    unsafe fn scroll_children(&self, dy: i32) {
        // SAFETY: per the contract.
        unsafe {
            ScrollWindowEx(
                self.panel,
                0,
                dy,
                None,
                None,
                None,
                None,
                SW_SCROLLCHILDREN | SW_INVALIDATE | SW_ERASE,
            )
        };
    }

    fn panel_height(&self) -> i32 {
        let mut rc = RECT::default();
        // SAFETY: valid window.
        let _ = unsafe { GetClientRect(self.panel, &mut rc) };
        rc.bottom
    }

    fn scroll_to(&mut self, pos: i32) {
        let ph = self.panel_height();
        let pos = pos.clamp(0, (self.content_h - ph).max(0));
        if pos == self.scroll {
            return;
        }
        let dy = self.scroll - pos;
        self.scroll = pos;
        // SAFETY: valid window.
        unsafe { self.scroll_children(dy) };
        self.update_scrollbar(ph);
    }

    fn on_vscroll(&mut self, wparam: WPARAM) {
        let code = (wparam.0 & 0xFFFF) as i32;
        let page = self.panel_height();
        let line = self.px(LINE);
        let pos = match code {
            c if c == SB_LINEUP.0 => self.scroll - line,
            c if c == SB_LINEDOWN.0 => self.scroll + line,
            c if c == SB_PAGEUP.0 => self.scroll - page,
            c if c == SB_PAGEDOWN.0 => self.scroll + page,
            c if c == SB_TOP.0 => 0,
            c if c == SB_BOTTOM.0 => self.content_h,
            c if c == SB_THUMBTRACK.0 => {
                let mut si = SCROLLINFO {
                    cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_TRACKPOS,
                    ..Default::default()
                };
                // SAFETY: valid struct.
                let _ = unsafe { GetScrollInfo(self.panel, SB_VERT, &mut si) };
                si.nTrackPos
            }
            _ => return,
        };
        self.scroll_to(pos);
    }

    fn on_wheel(&mut self, wparam: WPARAM) {
        let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
        self.scroll_to(self.scroll - delta * self.px(LINE) * 3 / 120);
    }

    /// Scroll so that `ctl` (a child of the panel) is fully visible.
    fn ensure_visible(&mut self, ctl: HWND) {
        let mut rc = RECT::default();
        // SAFETY: valid windows; converts screen to panel coordinates.
        unsafe {
            if GetWindowRect(ctl, &mut rc).is_err() {
                return;
            }
            let mut pts = [
                POINT {
                    x: rc.left,
                    y: rc.top,
                },
                POINT {
                    x: rc.right,
                    y: rc.bottom,
                },
            ];
            MapWindowPoints(None, Some(self.panel), &mut pts);
            rc.top = pts[0].y;
            rc.bottom = pts[1].y;
        }
        // A drop-down's window rect includes its closed list height only.
        let bottom = rc.bottom.min(rc.top + self.px(100));
        let ph = self.panel_height();
        let margin = self.px(12);
        if rc.top < margin {
            self.scroll_to(self.scroll + rc.top - margin);
        } else if bottom > ph - margin {
            self.scroll_to(self.scroll + bottom - ph + margin);
        }
    }

    fn show_section(&mut self, section: usize) {
        self.section = section.min(SECTIONS.len() - 1);
        // SAFETY: plain message.
        unsafe { SendMessageW(self.list, LB_SETCURSEL, Some(WPARAM(self.section)), None) };
        set_text(self.title, SECTIONS[self.section].title.get(self.lang));
        for r in &self.rows {
            let on = r.section == self.section;
            for h in [r.label, r.ctl, r.help] {
                if !h.is_invalid() {
                    show(h, on);
                }
            }
            for (g, l) in &r.flags {
                show(*g, on);
                show(*l, on);
            }
        }
        // Back to the top. Placing the controls with scroll = 0 is enough:
        // `layout_panel` positions them absolutely.
        self.scroll = 0;
        self.layout_panel();
        // SAFETY: valid window.
        let _ = unsafe {
            explorer_win::windows::Win32::Graphics::Gdi::InvalidateRect(
                Some(self.panel),
                None,
                true,
            )
        };
    }

    /// Show the field `i` and give it the focus.
    fn focus_field(&mut self, i: usize) {
        let Some(r) = self.rows.get(i) else { return };
        let (section, target) = (
            r.section,
            if r.ctl.is_invalid() {
                r.flags.first().map(|f| f.0).unwrap_or(r.label)
            } else {
                r.ctl
            },
        );
        if section != self.section {
            self.show_section(section);
        }
        // SAFETY: valid window.
        let _ = unsafe { SetFocus(Some(target)) };
        self.ensure_visible(target);
    }

    fn on_command(&mut self, id: u16, code: u16, ctl: HWND) {
        let code = code as u32;
        if id == ID_LIST {
            if code == LBN_SELCHANGE {
                // SAFETY: plain message.
                let sel = unsafe { SendMessageW(self.list, LB_GETCURSEL, None, None) }.0;
                if sel >= 0 {
                    self.show_section(sel as usize);
                }
            }
            return;
        }
        if code == EN_SETFOCUS || code == CBN_SETFOCUS || code == BN_SETFOCUS {
            if !ctl.is_invalid() && ctl != self.list && !self.buttons.contains(&ctl) {
                self.ensure_visible(ctl);
            }
            return;
        }
        let click = code == BN_CLICKED || code == STN_CLICKED;
        if !click {
            return;
        }
        match id {
            ID_EDIT_FILE => self.edit_file(),
            ID_SAVE => {
                self.save(false);
            }
            ID_SAVE_RESTART => {
                self.save(true);
            }
            ID_CLOSE => self.close(),
            id if id >= FIELD_BASE => {
                let i = ((id - FIELD_BASE) / FIELD_STRIDE) as usize;
                let sub = (id - FIELD_BASE) % FIELD_STRIDE;
                let Some(r) = self.rows.get(i) else { return };
                let click_on = |h: HWND| {
                    // SAFETY: simulates a click: focus, state and BN_CLICKED.
                    unsafe { SendMessageW(h, BM_CLICK, None, None) };
                };
                if sub == SUB_LABEL {
                    match r.field.kind {
                        Kind::Bool => click_on(r.ctl),
                        Kind::Flags(_) => {}
                        _ => {
                            // SAFETY: valid window.
                            let _ = unsafe { SetFocus(Some(r.ctl)) };
                        }
                    }
                } else if sub >= SUB_FLAG && (sub - SUB_FLAG) % 2 == 1 {
                    if let Some((g, _)) = r.flags.get(((sub - SUB_FLAG) / 2) as usize) {
                        click_on(*g);
                    }
                }
            }
            _ => {}
        }
    }

    /// The advanced path: open the file in Notepad, as `--edit-config`.
    fn edit_file(&mut self) {
        let created = self.loaded.text.is_none();
        if crate::cli::edit_config(self.config_override.as_deref()) != 0 {
            let msg = trf(
                self.lang,
                format!("Cannot open {} in Notepad.", self.path.display()),
                format!(
                    "No se puede abrir {} en el Bloc de notas.",
                    self.path.display()
                ),
            );
            message_box(Some(self.hwnd), &msg, MB_OK | MB_ICONERROR);
            return;
        }
        // A file just created from the default is what this window was
        // already editing; it must not count as a change made elsewhere.
        if created {
            if let Ok(text) = std::fs::read_to_string(&self.path) {
                if text == DEFAULT_CONFIG_TOML {
                    self.loaded.text = Some(text);
                }
            }
        }
    }

    /// When the window comes back to the front: if the file changed on disk
    /// (e.g. edited in Notepad) and the form has no unsaved changes, show
    /// the new values.
    fn refresh_from_disk(&mut self) {
        let Ok(disk) = settings::load(&self.path) else {
            return;
        };
        if disk.text == self.loaded.text || self.dirty() {
            return;
        }
        self.loaded = disk;
        self.fill(&settings::inputs_from(&self.loaded.config));
    }

    /// Save the form. Returns true if the file now holds the form's values.
    fn save(&mut self, restart: bool) -> bool {
        let inputs = self.inputs();
        let disk = match settings::load(&self.path) {
            Ok(d) => d,
            Err(e) => {
                let msg = trf(
                    self.lang,
                    format!("The configuration file has an error, so it was not changed. Fix it with \"Edit config file…\".\n\n{e}"),
                    format!("El archivo de configuración tiene un error, así que no se ha cambiado. Corrígelo con \"Editar archivo de configuración…\".\n\n{e}"),
                );
                message_box(Some(self.hwnd), &msg, MB_OK | MB_ICONERROR);
                return false;
            }
        };
        if disk.text != self.loaded.text {
            // Changed elsewhere since it was read. Only the keys changed in
            // this window are written, so applying them on top of the new
            // contents keeps the other program's edits.
            let q = self.s(
                "The configuration file was changed by another program since this window read it.\n\nSave anyway? Only the settings you changed here are written; everything else in the file is kept.",
                "Otro programa ha cambiado el archivo de configuración desde que esta ventana lo leyó.\n\n¿Guardar de todos modos? Solo se escriben los ajustes que has cambiado aquí; el resto del archivo se conserva.",
            );
            if message_box(Some(self.hwnd), q, MB_YESNO | MB_ICONWARNING) != IDYES.0 {
                return false;
            }
        }
        let applied = match settings::apply(disk.base_text(), &self.loaded.config, &inputs) {
            Ok(a) => a,
            Err(e) => {
                message_box(Some(self.hwnd), e.message(self.lang), MB_OK | MB_ICONERROR);
                if let Some(i) = e.field {
                    self.focus_field(i);
                }
                return false;
            }
        };
        let changed = !applied.changed.is_empty();
        if changed {
            if let Err(e) = settings::write_config(&self.path, &applied.text) {
                let msg = trf(
                    self.lang,
                    format!("Cannot save {}:\n{e}", self.path.display()),
                    format!("No se puede guardar {}:\n{e}", self.path.display()),
                );
                message_box(Some(self.hwnd), &msg, MB_OK | MB_ICONERROR);
                return false;
            }
            tracing::info!(path = %self.path.display(), keys = applied.changed.len(), "settings saved");
        }
        let language_changed = applied
            .changed
            .iter()
            .any(|&i| self.rows[i].field.path == "general.language");
        self.loaded = Loaded {
            text: if changed || disk.text.is_some() {
                Some(applied.text)
            } else {
                None
            },
            config: applied.config,
        };
        // Show the values as saved (e.g. trimmed list items).
        self.fill(&settings::inputs_from(&self.loaded.config));

        let running = explorer_win::process::guardian_window().is_some();
        let mut msg = if restart {
            if explorer_win::session::request_restart_all() {
                trf(
                    self.lang,
                    format!(
                        "{}explorer.rs is restarting its taskbar, Start menu and other parts.",
                        if changed { "Saved. " } else { "" }
                    ),
                    format!(
                        "{}explorer.rs está reiniciando su barra de tareas, menú Inicio y demás partes.",
                        if changed { "Guardado. " } else { "" }
                    ),
                )
            } else if changed {
                self.s(
                    "Saved. explorer.rs is not running; the changes apply the next time it starts.",
                    "Guardado. explorer.rs no se está ejecutando; los cambios se aplicarán la próxima vez que se inicie.",
                )
                .into()
            } else {
                self.s(
                    "explorer.rs is not running, so there is nothing to restart.",
                    "explorer.rs no se está ejecutando, así que no hay nada que reiniciar.",
                )
                .into()
            }
        } else if !changed {
            self.s(
                "Nothing to save: no setting was changed.",
                "Nada que guardar: no se ha cambiado ningún ajuste.",
            )
            .into()
        } else if running {
            self.s(
                "Saved. explorer.rs applies the changes within a few seconds: the parts that use them restart by themselves.",
                "Guardado. explorer.rs aplica los cambios en unos segundos: las partes afectadas se reinician solas.",
            )
            .into()
        } else {
            self.s(
                "Saved. explorer.rs is not running; the changes apply the next time it starts.",
                "Guardado. explorer.rs no se está ejecutando; los cambios se aplicarán la próxima vez que se inicie.",
            )
            .into()
        };
        if language_changed {
            msg.push_str(self.s(
                "\n\nThis window keeps its language until it is opened again.",
                "\n\nEsta ventana mantiene su idioma hasta que se vuelva a abrir.",
            ));
        }
        message_box(Some(self.hwnd), &msg, MB_OK | MB_ICONINFORMATION);
        true
    }

    fn close(&mut self) {
        if self.dirty() {
            let q = self.s("Save your changes?", "¿Guardar los cambios?");
            match message_box(Some(self.hwnd), q, MB_YESNOCANCEL | MB_ICONQUESTION) {
                r if r == IDYES.0 => {
                    if !self.save(false) {
                        return;
                    }
                }
                r if r == IDNO.0 => {}
                _ => return,
            }
        }
        // SAFETY: our own window.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

impl Handler for Settings {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as u16;
                let code = ((wparam.0 >> 16) & 0xFFFF) as u16;
                let ctl = HWND(lparam.0 as *mut core::ffi::c_void);
                self.on_command(id, code, ctl);
                Some(LRESULT(0))
            }
            // Forwarded by the panel (its scroll bar and unhandled wheel).
            WM_VSCROLL => {
                self.on_vscroll(wparam);
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.on_wheel(wparam);
                Some(LRESULT(0))
            }
            WM_SIZE => {
                const SIZE_MINIMIZED: usize = 1;
                if wparam.0 != SIZE_MINIMIZED {
                    self.layout();
                }
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xFFFF) as u32 != WA_INACTIVE {
                    self.refresh_from_disk();
                }
                None
            }
            WM_GETMINMAXINFO => {
                // SAFETY: lparam points to the MINMAXINFO for this message.
                let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                mmi.ptMinTrackSize.x = self.px(MIN_W);
                mmi.ptMinTrackSize.y = self.px(MIN_H);
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.dpi = ((wparam.0 >> 16) & 0xFFFF) as u32;
                PAINT.with(|p| {
                    if let Some(p) = p.borrow_mut().as_mut() {
                        p.dpi = self.dpi;
                    }
                });
                self.update_fonts();
                // SAFETY: lparam points to the suggested RECT.
                let rc = unsafe { *(lparam.0 as *const RECT) };
                // SAFETY: our own window; WM_SIZE re-lays out.
                let _ = unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        rc.left,
                        rc.top,
                        rc.right - rc.left,
                        rc.bottom - rc.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                };
                self.layout();
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.close();
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                // SAFETY: GDI objects created by us.
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(self.font.0));
                    let _ = DeleteObject(HGDIOBJ(self.title_font.0));
                    if let Some(p) = PAINT.with(|p| p.borrow_mut().take()) {
                        for b in [p.bg_brush, p.bar_brush, p.edit_brush] {
                            let _ = DeleteObject(HGDIOBJ(b.0));
                        }
                    }
                    PostQuitMessage(0);
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// Handler wrapper: the window is initialised on WM_APP_INIT, when its
/// handle is known and no other borrow is active.
struct Host(Settings);

impl Handler for Host {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let s = &mut self.0;
        if msg == WM_APP_INIT && s.hwnd.is_invalid() {
            s.hwnd = hwnd;
            s.create_controls();
            s.update_fonts();
            s.layout();
            s.show_section(0);
            // SAFETY: valid window.
            let _ = unsafe { SetFocus(Some(s.list)) };
            return Some(LRESULT(0));
        }
        if s.hwnd.is_invalid() {
            // Before init: only the window must be closable.
            return match msg {
                WM_DESTROY => {
                    // SAFETY: plain call.
                    unsafe { PostQuitMessage(0) };
                    Some(LRESULT(0))
                }
                _ => None,
            };
        }
        s.handle(hwnd, msg, wparam, lparam)
    }
}

/// Colours for WM_CTLCOLOR* and backgrounds, answered by subclass
/// procedures from this thread-local state. Controls ask for colours
/// synchronously while the handler may be borrowed (e.g. a static
/// repainting inside SetWindowTextW); going through the handler would then
/// fall back to default colours. Same design as the setup wizard.
struct PaintState {
    colors: Colors,
    bg_brush: HBRUSH,
    bar_brush: HBRUSH,
    edit_brush: HBRUSH,
    dpi: u32,
    roles: Vec<(isize, Role)>,
}

thread_local! {
    static PAINT: std::cell::RefCell<Option<PaintState>> = const { std::cell::RefCell::new(None) };
    static PREV_PROC: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

impl PaintState {
    fn ctl_color(&self, msg: u32, hdc: HDC, ctl: HWND) -> LRESULT {
        let role = self
            .roles
            .iter()
            .find(|(h, _)| *h == ctl.0 as isize)
            .map(|(_, r)| *r)
            .unwrap_or(match msg {
                // Combo box drop-down lists.
                WM_CTLCOLORLISTBOX | WM_CTLCOLOREDIT => Role::Edit,
                _ => Role::Text,
            });
        let c = &self.colors;
        let (fg, bg, brush) = match (msg, role) {
            // Push buttons sit in the bottom bar.
            (WM_CTLCOLORBTN, _) => (c.text, c.bar, self.bar_brush),
            (_, Role::Side) => (c.text, c.bar, self.bar_brush),
            (_, Role::Edit) => (c.text, c.edit, self.edit_brush),
            (_, Role::Sub) => (c.sub, c.bg, self.bg_brush),
            (_, Role::Text) => (c.text, c.bg, self.bg_brush),
        };
        // SAFETY: HDC provided by the control for this message.
        unsafe {
            SetTextColor(hdc, fg);
            SetBkColor(hdc, bg);
        }
        LRESULT(brush.0 as isize)
    }

    /// Main window: sidebar and bottom bar in the bar colour.
    fn erase_main(&self, hwnd: HWND, hdc: HDC) {
        let mut rc = RECT::default();
        // SAFETY: valid window and DC.
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
            FillRect(hdc, &rc, self.bar_brush);
        }
    }

    fn erase_panel(&self, hwnd: HWND, hdc: HDC) {
        let mut rc = RECT::default();
        // SAFETY: valid window and DC.
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
            FillRect(hdc, &rc, self.bg_brush);
        }
    }
}

fn is_ctl_color(msg: u32) -> bool {
    matches!(
        msg,
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX
    )
}

/// Subclass procedure of the main window: colours and background.
unsafe extern "system" fn main_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
    let ctl = HWND(lparam.0 as *mut core::ffi::c_void);
    let handled = PAINT.with(|p| {
        let p = p.try_borrow().ok()?;
        let p = p.as_ref()?;
        match msg {
            m if is_ctl_color(m) => Some(p.ctl_color(m, hdc, ctl)),
            WM_ERASEBKGND => {
                p.erase_main(hwnd, hdc);
                Some(LRESULT(1))
            }
            _ => None,
        }
    });
    if let Some(r) = handled {
        return r;
    }
    let prev = PREV_PROC.with(|c| c.get());
    // SAFETY: `prev` is the window procedure we replaced (never 0 once
    // subclassed; this procedure is only installed after that).
    unsafe {
        let prev: WNDPROC = std::mem::transmute::<isize, WNDPROC>(prev);
        CallWindowProcW(prev, hwnd, msg, wparam, lparam)
    }
}

/// Window procedure of the scrollable panel. Notifications, colour
/// requests, scrolling and the mouse wheel go to the main window, which
/// owns the state; the panel itself only paints its background.
unsafe extern "system" fn panel_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_ERASEBKGND {
        let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
        let done = PAINT.with(|p| {
            let p = p.try_borrow().ok()?;
            p.as_ref()?.erase_panel(hwnd, hdc);
            Some(())
        });
        if done.is_some() {
            return LRESULT(1);
        }
    }
    if msg == WM_CLOSE {
        // A multi-line edit sends WM_CLOSE to its parent on Esc; the
        // default handling would destroy the panel.
        return LRESULT(0);
    }
    if msg == WM_COMMAND || msg == WM_VSCROLL || msg == WM_MOUSEWHEEL || is_ctl_color(msg) {
        // SAFETY: plain queries and a synchronous send to our own window.
        if let Ok(parent) = unsafe { GetParent(hwnd) } {
            return unsafe { SendMessageW(parent, msg, Some(wparam), Some(lparam)) };
        }
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn register_panel_class() {
    let class = WideString::new(PANEL_CLASS);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(panel_proc),
        hInstance: window::module_instance(),
        lpszClassName: class.pcwstr(),
        ..Default::default()
    };
    // SAFETY: wc is fully initialised; the class name is copied.
    unsafe { RegisterClassExW(&wc) };
}

fn set_icon(hwnd: HWND) {
    // SAFETY: icon resource 1 is embedded by build.rs (absent in builds
    // without a resource compiler; then nothing happens).
    unsafe {
        for (which, size) in [(ICON_BIG, 32), (ICON_SMALL, 16)] {
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            if let Ok(h) = LoadImageW(
                Some(window::module_instance()),
                PCWSTR(std::ptr::without_provenance(1)), // MAKEINTRESOURCE(1)
                IMAGE_ICON,
                size * dpi / 96,
                size * dpi / 96,
                LR_DEFAULTCOLOR,
            ) {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(which as usize)),
                    Some(LPARAM(HICON(h.0).0 as isize)),
                );
            }
        }
    }
}

/// Bring an already open settings window to the front.
fn activate_existing() {
    let class = WideString::new(CLASS);
    // SAFETY: valid class string; plain window calls.
    unsafe {
        if let Ok(h) = FindWindowW(class.pcwstr(), PCWSTR::null()) {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
    }
}

/// `explorer-rs --settings`. `path` is the config file; `config_override`
/// is the `--config` argument, passed on to `--edit-config`.
pub fn run(path: PathBuf, config_override: Option<PathBuf>) -> i32 {
    let Some(_instance) = explorer_win::single_instance::acquire(MUTEX) else {
        activate_existing();
        return 0;
    };
    let loaded = match settings::load(&path) {
        Ok(l) => l,
        Err(e) => return broken_file(&path, config_override.as_deref(), &e),
    };
    let lang = explorer_win::theme::ui_lang(loaded.config.general.language);

    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES,
    };
    // SAFETY: valid struct.
    let _ = unsafe { InitCommonControlsEx(&icc) };
    explorer_win::dpi::enable_per_monitor_v2();
    window::register_class(CLASS, None);
    register_panel_class();

    let dark = wants_dark(loaded.config.general.theme);
    let colors = Colors::new(dark);
    // SAFETY: plain calls.
    let (bg_brush, bar_brush, edit_brush, dpi) = unsafe {
        (
            CreateSolidBrush(colors.bg),
            CreateSolidBrush(colors.bar),
            CreateSolidBrush(colors.edit),
            GetDpiForSystem(),
        )
    };
    let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: W * dpi as i32 / 96,
        bottom: H * dpi as i32 / 96,
    };
    // SAFETY: valid RECT.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut rc, style, false, WS_EX_CONTROLPARENT, dpi) };
    // SAFETY: plain queries.
    let (sw, sh) = unsafe {
        (
            explorer_win::windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(SM_CXSCREEN),
            explorer_win::windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(SM_CYSCREEN),
        )
    };
    let (ww, wh) = ((rc.right - rc.left).min(sw), (rc.bottom - rc.top).min(sh));
    let title = tr(lang, "explorer.rs settings", "Configuración de explorer.rs");
    let state = Settings {
        hwnd: HWND::default(),
        panel: HWND::default(),
        lang,
        path,
        config_override,
        loaded,
        dpi,
        colors,
        font: HFONT::default(),
        title_font: HFONT::default(),
        list: HWND::default(),
        title: HWND::default(),
        rows: Vec::new(),
        buttons: [HWND::default(); 4],
        section: 0,
        scroll: 0,
        content_h: 0,
    };
    let hwnd = match window::create(
        CreateParams {
            class: CLASS,
            title,
            style,
            ex_style: WS_EX_CONTROLPARENT,
            rect: RECT {
                left: (sw - ww).max(0) / 2,
                top: (sh - wh).max(0) / 2,
                right: (sw - ww).max(0) / 2 + ww,
                bottom: (sh - wh).max(0) / 2 + wh,
            },
            parent: None,
        },
        Box::new(Host(state)),
    ) {
        Ok(h) => h,
        Err(e) => {
            message_box(None, &format!("{e:#}"), MB_OK | MB_ICONERROR);
            return 1;
        }
    };
    if dark {
        let on = BOOL(1);
        // SAFETY: documented attribute (Windows 11 / Windows 10 20H1+).
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(20), // DWMWA_USE_IMMERSIVE_DARK_MODE
                (&on as *const BOOL).cast(),
                4,
            )
        };
    }
    PAINT.with(|p| {
        *p.borrow_mut() = Some(PaintState {
            colors,
            bg_brush,
            bar_brush,
            edit_brush,
            dpi,
            roles: Vec::new(),
        })
    });
    // SAFETY: subclassing our own window; the previous procedure is kept
    // for CallWindowProcW.
    unsafe {
        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, main_proc as *const () as isize);
        PREV_PROC.with(|c| c.set(prev));
    }
    set_icon(hwnd);
    // SAFETY: posting to our own window.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_INIT, WPARAM(0), LPARAM(0)) };
    // SAFETY: our own window.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    window::run_message_loop(|msg| {
        // SAFETY: msg came from GetMessageW; hwnd is our window.
        unsafe { IsDialogMessageW(hwnd, msg) }.as_bool()
    })
}

/// The config file cannot be read or parsed: never overwrite it from a
/// form; offer the text editor instead.
fn broken_file(path: &Path, config_override: Option<&Path>, error: &str) -> i32 {
    let lang = explorer_win::theme::ui_lang(Default::default());
    let q = trf(
        lang,
        format!(
            "Your explorer.rs configuration file has an error, so the settings window cannot show it:\n\n{error}\n\nOpen {} in Notepad to fix it?",
            path.display()
        ),
        format!(
            "Tu archivo de configuración de explorer.rs tiene un error, así que la ventana de configuración no puede mostrarlo:\n\n{error}\n\n¿Abrir {} en el Bloc de notas para corregirlo?",
            path.display()
        ),
    );
    if message_box(None, &q, MB_YESNO | MB_ICONWARNING) == IDYES.0 {
        return crate::cli::edit_config(config_override);
    }
    1
}
