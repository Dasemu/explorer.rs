// SPDX-License-Identifier: GPL-3.0-or-later
//! The setup wizard: plain Win32 controls (common controls v6 through the
//! manifest), one top-level window, pages shown by hiding/showing controls.
//!
//! Install:   Welcome -> Options -> Progress -> Done
//! Uninstall: Confirm -> Progress -> Done
//!
//! Light/dark follows the signed-in user's "app mode". Dark mode uses the
//! documented DWMWA_USE_IMMERSIVE_DARK_MODE title bar attribute, custom
//! WM_CTLCOLOR* colours, and the (undocumented, cosmetic) "DarkMode_Explorer"
//! visual-style class for buttons and scroll bars; if that class is missing
//! the controls simply keep their light look (docs/decisions/0007).
//!
//! Check boxes and radio buttons are drawn as glyph-only controls next to a
//! clickable label, because themed buttons ignore WM_CTLCOLORSTATIC text
//! colours (black text on a dark background otherwise). The button keeps
//! its full caption (clipped) for screen readers.
//!
//! The work runs on a worker thread; it reports through a channel and a
//! posted WM_APP message, so the UI never blocks.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use explorer_core::i18n::{tr, trf, Lang};
use explorer_win::util::WideString;
use explorer_win::window::{self, CreateParams, Handler};
use explorer_win::windows::core::{w, PCWSTR};
use explorer_win::windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use explorer_win::windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use explorer_win::windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontIndirectW, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
    SetBkColor, SetTextColor, FW_SEMIBOLD, HBRUSH, HDC, HFONT, HGDIOBJ, PAINTSTRUCT,
};
use explorer_win::windows::Win32::UI::Controls::{
    InitCommonControlsEx, SetWindowTheme, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES,
    INITCOMMONCONTROLSEX, PBM_SETPOS, PBM_SETRANGE32, PROGRESS_CLASSW,
};
use explorer_win::windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, SystemParametersInfoForDpi,
};
use explorer_win::windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DestroyWindow, GetClientRect, GetSystemMetrics,
    IsDialogMessageW, LoadImageW, MessageBoxW, PostMessageW, PostQuitMessage, SendMessageW,
    SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, BM_CLICK, BM_GETCHECK,
    BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_DEFPUSHBUTTON, BS_PUSHBUTTON,
    ES_AUTOHSCROLL, ES_MULTILINE, ES_READONLY, GWLP_WNDPROC, HICON, HMENU, ICON_BIG, ICON_SMALL,
    IDNO, IMAGE_ICON, LR_DEFAULTCOLOR, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNO,
    MESSAGEBOX_STYLE, NONCLIENTMETRICSW, SM_CXSCREEN, SM_CYSCREEN, SPI_GETNONCLIENTMETRICS,
    STN_CLICKED, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED,
    WM_ERASEBKGND, WM_PAINT, WM_SETFONT, WM_SETICON, WNDPROC, WS_CAPTION, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_GROUP, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
    WS_TABSTOP, WS_VSCROLL,
};

use crate::cli::exit;
use crate::payload::Payload;
use crate::plan::{self, Choices, Facts, PreviousInstall, StartMode};
use crate::win::exec::{Event, Exec, Outcome};

const CLASS: &str = "ExplorerRs.Setup";
const WM_APP_EVENT: u32 = WM_APP + 1;
const SS_LEFT: u32 = 0x0;
const SS_NOTIFY: u32 = 0x100;

// Control ids. IDOK / IDCANCEL make Enter / Esc work via IsDialogMessage.
const ID_NEXT: u16 = 1;
const ID_CANCEL: u16 = 2;
const ID_BACK: u16 = 100;
const ID_R_ALONG: u16 = 110;
const ID_R_SHELL: u16 = 111;
const ID_R_NONE: u16 = 112;
const ID_L_ALONG: u16 = 120;
const ID_L_SHELL: u16 = 121;
const ID_L_NONE: u16 = 122;
const ID_C_SEARCH: u16 = 130;
const ID_L_SEARCH: u16 = 131;
const ID_C_LAUNCH: u16 = 132;
const ID_L_LAUNCH: u16 = 133;
const ID_C_DATA: u16 = 134;
const ID_L_DATA: u16 = 135;

// Client size in DIPs.
const W: i32 = 620;
const H: i32 = 540;
const BAR: i32 = 56;
const M: i32 = 24;
const GLYPH: i32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    Options,
    Confirm,
    Progress,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Text,
    Sub,
    Warn,
}

/// Everything the install wizard needs.
pub struct InstallUi {
    pub facts: Facts,
    pub previous: Option<PreviousInstall>,
    pub choices: Choices,
    pub payload: Arc<Payload>,
    pub lang: Lang,
    pub log_path: Option<PathBuf>,
}

/// Everything the uninstall wizard needs.
pub struct UninstallUi {
    pub facts: Facts,
    pub lang: Lang,
    pub log_path: Option<PathBuf>,
}

enum Kind {
    Install(InstallUi),
    Uninstall(UninstallUi),
}

impl Kind {
    fn lang(&self) -> Lang {
        match self {
            Kind::Install(i) => i.lang,
            Kind::Uninstall(u) => u.lang,
        }
    }
    fn facts(&self) -> &Facts {
        match self {
            Kind::Install(i) => &i.facts,
            Kind::Uninstall(u) => &u.facts,
        }
    }
    fn log_path(&self) -> Option<&PathBuf> {
        match self {
            Kind::Install(i) => i.log_path.as_ref(),
            Kind::Uninstall(u) => u.log_path.as_ref(),
        }
    }
}

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
    warn: COLORREF,
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
                warn: rgb(0xFF, 0x99, 0xA4),
            }
        } else {
            Self {
                dark,
                bg: rgb(0xFF, 0xFF, 0xFF),
                bar: rgb(0xF3, 0xF3, 0xF3),
                text: rgb(0x1A, 0x1A, 0x1A),
                sub: rgb(0x5C, 0x5C, 0x5C),
                warn: rgb(0xC4, 0x2B, 0x1C),
            }
        }
    }
}

/// "Apps use dark mode" for the interactive user (the elevated process's
/// HKCU may belong to another account), false in high-contrast mode.
fn wants_dark(sid: &str) -> bool {
    use explorer_win::windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
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
    let key = format!(r"{sid}\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let light = crate::win::reg::get_dword(crate::win::reg::Root::Hku, &key, "AppsUseLightTheme")
        .or_else(|| {
            explorer_win::theme::read_dword(
                r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "AppsUseLightTheme",
            )
        })
        .unwrap_or(1);
    light == 0
}

struct Ctl {
    hwnd: HWND,
    pages: &'static [Page],
    role: Role,
    title: bool,
}

struct Wizard {
    kind: Kind,
    hwnd: HWND,
    page: Page,
    dpi: u32,
    colors: Colors,
    bg_brush: HBRUSH,
    bar_brush: HBRUSH,
    font: HFONT,
    title_font: HFONT,
    ctls: Vec<Ctl>,
    // Named controls.
    title: HWND,
    body: HWND,
    back: HWND,
    next: HWND,
    cancel: HWND,
    radios: [HWND; 3],
    radio_labels: [HWND; 3],
    radio_descs: [HWND; 3],
    shell_now: HWND,
    mode_label: HWND,
    c_search: HWND,
    l_search: HWND,
    d_search: HWND,
    c_launch: HWND,
    l_launch: HWND,
    dir_label: HWND,
    dir_edit: HWND,
    c_data: HWND,
    l_data: HWND,
    status: HWND,
    progress: HWND,
    log: HWND,
    rx: Option<Receiver<Msg>>,
    running: bool,
    exit_code: i32,
}

enum Msg {
    Event(Event),
    Done(Result<Outcome, String>),
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

fn is_checked(h: HWND) -> bool {
    // SAFETY: plain message.
    unsafe { SendMessageW(h, BM_GETCHECK, None, None) }.0 == 1
}

fn set_checked(h: HWND, on: bool) {
    // SAFETY: plain message.
    unsafe { SendMessageW(h, BM_SETCHECK, Some(WPARAM(on as usize)), None) };
}

fn enable(h: HWND, on: bool) {
    // SAFETY: plain call.
    let _ = unsafe { EnableWindow(h, on) };
}

fn message_box(owner: Option<HWND>, text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    let t = WideString::new(text);
    // SAFETY: valid strings.
    unsafe { MessageBoxW(owner, t.pcwstr(), w!("explorer.rs"), style) }.0
}

/// Show an error message box (also used before any window exists).
pub fn error_box(text: &str) {
    message_box(None, text, MB_OK | MB_ICONERROR);
}

impl Wizard {
    fn lang(&self) -> Lang {
        self.kind.lang()
    }

    fn s(&self, en: &'static str, es: &'static str) -> &'static str {
        tr(self.lang(), en, es)
    }

    fn create_controls(&mut self) {
        let p = self.hwnd;
        let st = WINDOW_STYLE(SS_LEFT);
        let none = WINDOW_EX_STYLE(0);
        let lang = self.lang();
        let s = |en, es| tr(lang, en, es);
        let all: &'static [Page] = &[
            Page::Welcome,
            Page::Options,
            Page::Confirm,
            Page::Progress,
            Page::Done,
        ];
        let opts: &'static [Page] = &[Page::Options];

        self.title = make(p, w!("STATIC"), "", st, none, 0);
        self.ctls.push(Ctl {
            hwnd: self.title,
            pages: all,
            role: Role::Text,
            title: true,
        });
        self.body = make(p, w!("STATIC"), "", st, none, 0);
        self.ctls.push(Ctl {
            hwnd: self.body,
            pages: &[Page::Welcome, Page::Confirm, Page::Done],
            role: Role::Text,
            title: false,
        });

        // Options page.
        self.mode_label = make(
            p,
            w!("STATIC"),
            s(
                "When should explorer.rs start?",
                "¿Cuándo debe iniciarse explorer.rs?",
            ),
            st,
            none,
            0,
        );
        self.ctls.push(Ctl {
            hwnd: self.mode_label,
            pages: opts,
            role: Role::Text,
            title: false,
        });
        let modes: [(u16, u16, &str, &str, Role); 3] = [
            (
                ID_R_ALONG,
                ID_L_ALONG,
                s(
                    "Run alongside Windows Explorer at sign-in (recommended)",
                    "Ejecutar junto al Explorador de Windows al iniciar sesión (recomendado)",
                ),
                s(
                    "Adds the explorer.rs taskbar and Start menu. Nothing in Windows is replaced.",
                    "Añade la barra de tareas y el menú Inicio de explorer.rs. No se reemplaza nada de Windows.",
                ),
                Role::Sub,
            ),
            (
                ID_R_SHELL,
                ID_L_SHELL,
                s(
                    "Replace Windows Explorer as my shell (advanced)",
                    "Reemplazar el Explorador de Windows como mi shell (avanzado)",
                ),
                s(
                    "Windows Explorer will no longer start when you sign in. To get it back, press Ctrl+Alt+Shift+E or run \"Restore Windows Explorer\" from the Start menu.",
                    "El Explorador de Windows ya no se iniciará al iniciar sesión. Para recuperarlo, pulsa Ctrl+Alt+Mayús+E o ejecuta \"Restaurar el Explorador de Windows\" desde el menú Inicio.",
                ),
                Role::Warn,
            ),
            (
                ID_R_NONE,
                ID_L_NONE,
                s("Don't start automatically", "No iniciar automáticamente"),
                s(
                    "Start explorer.rs yourself from the Start menu.",
                    "Inicia explorer.rs tú mismo desde el menú Inicio.",
                ),
                Role::Sub,
            ),
        ];
        for (i, (rid, lid, label, desc, role)) in modes.into_iter().enumerate() {
            let group = if i == 0 { WS_GROUP } else { WINDOW_STYLE(0) };
            self.radios[i] = make(
                p,
                w!("BUTTON"),
                label,
                WINDOW_STYLE(BS_AUTORADIOBUTTON as u32) | WS_TABSTOP | group,
                none,
                rid,
            );
            self.radio_labels[i] = make(
                p,
                w!("STATIC"),
                label,
                st | WINDOW_STYLE(SS_NOTIFY),
                none,
                lid,
            );
            self.radio_descs[i] = make(p, w!("STATIC"), desc, st, none, 0);
            for (h, r) in [
                (self.radios[i], Role::Text),
                (self.radio_labels[i], Role::Text),
                (self.radio_descs[i], role),
            ] {
                self.ctls.push(Ctl {
                    hwnd: h,
                    pages: opts,
                    role: r,
                    title: false,
                });
            }
        }
        let check = WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP | WS_GROUP;
        let search_label = s("Fast file search", "Búsqueda rápida de archivos");
        self.c_search = make(p, w!("BUTTON"), search_label, check, none, ID_C_SEARCH);
        self.l_search = make(
            p,
            w!("STATIC"),
            search_label,
            st | WINDOW_STYLE(SS_NOTIFY),
            none,
            ID_L_SEARCH,
        );
        self.d_search = make(
            p,
            w!("STATIC"),
            s(
                "Installs a helper that reads the NTFS file table with administrator rights (file names only, never contents) so the Start menu finds files instantly.",
                "Instala un asistente que lee la tabla de archivos NTFS con permisos de administrador (solo nombres, nunca contenidos) para que el menú Inicio encuentre archivos al instante.",
            ),
            st,
            none,
            0,
        );
        let launch_label = s("Start explorer.rs now", "Iniciar explorer.rs ahora");
        self.c_launch = make(p, w!("BUTTON"), launch_label, check, none, ID_C_LAUNCH);
        self.l_launch = make(
            p,
            w!("STATIC"),
            launch_label,
            st | WINDOW_STYLE(SS_NOTIFY),
            none,
            ID_L_LAUNCH,
        );
        self.dir_label = make(
            p,
            w!("STATIC"),
            s("Install folder:", "Carpeta de instalación:"),
            st,
            none,
            0,
        );
        self.dir_edit = make(
            p,
            w!("EDIT"),
            &self.kind.facts().install_dir.clone(),
            WINDOW_STYLE((ES_READONLY | ES_AUTOHSCROLL) as u32) | WS_TABSTOP,
            WS_EX_CLIENTEDGE,
            0,
        );
        self.shell_now = make(
            p,
            w!("STATIC"),
            s(
                "explorer.rs is your shell right now. Setup closes it briefly (your taskbar disappears for a moment) and starts it again at the end.",
                "explorer.rs es tu shell ahora mismo. El instalador lo cerrará un momento (la barra de tareas desaparecerá brevemente) y lo volverá a iniciar al final.",
            ),
            st,
            none,
            0,
        );
        for (h, r) in [
            (self.c_search, Role::Text),
            (self.l_search, Role::Text),
            (self.d_search, Role::Sub),
            (self.c_launch, Role::Text),
            (self.l_launch, Role::Text),
            (self.dir_label, Role::Text),
            (self.dir_edit, Role::Text),
            (self.shell_now, Role::Warn),
        ] {
            self.ctls.push(Ctl {
                hwnd: h,
                pages: opts,
                role: r,
                title: false,
            });
        }

        // Uninstall confirm page.
        let data_label = s(
            "Also remove my settings and logs",
            "Eliminar también mi configuración y mis registros",
        );
        self.c_data = make(p, w!("BUTTON"), data_label, check, none, ID_C_DATA);
        self.l_data = make(
            p,
            w!("STATIC"),
            data_label,
            st | WINDOW_STYLE(SS_NOTIFY),
            none,
            ID_L_DATA,
        );
        for h in [self.c_data, self.l_data] {
            self.ctls.push(Ctl {
                hwnd: h,
                pages: &[Page::Confirm],
                role: Role::Text,
                title: false,
            });
        }

        // Progress page.
        self.status = make(p, w!("STATIC"), "", st, none, 0);
        self.progress = make(p, PROGRESS_CLASSW, "", WINDOW_STYLE(0), none, 0);
        self.log = make(
            p,
            w!("EDIT"),
            "",
            WINDOW_STYLE((ES_MULTILINE | ES_READONLY) as u32) | WS_VSCROLL | WS_TABSTOP,
            WS_EX_CLIENTEDGE,
            0,
        );
        for h in [self.status, self.progress, self.log] {
            self.ctls.push(Ctl {
                hwnd: h,
                pages: &[Page::Progress, Page::Done],
                role: Role::Text,
                title: false,
            });
        }

        // Buttons.
        self.back = make(
            p,
            w!("BUTTON"),
            s("< Back", "< Atrás"),
            WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP | WS_GROUP,
            none,
            ID_BACK,
        );
        self.next = make(
            p,
            w!("BUTTON"),
            "",
            WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP,
            none,
            ID_NEXT,
        );
        self.cancel = make(
            p,
            w!("BUTTON"),
            s("Cancel", "Cancelar"),
            WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP,
            none,
            ID_CANCEL,
        );

        if self.colors.dark {
            for h in [
                self.back,
                self.next,
                self.cancel,
                self.log,
                self.c_search,
                self.c_launch,
                self.c_data,
                self.radios[0],
                self.radios[1],
                self.radios[2],
            ] {
                // SAFETY: valid window; failure leaves the light theme.
                let _ = unsafe { SetWindowTheme(h, w!("DarkMode_Explorer"), PCWSTR::null()) };
            }
        }

        // Initial state.
        if let Kind::Install(i) = &self.kind {
            let idx = match i.choices.mode {
                StartMode::Alongside => 0,
                StartMode::Shell => 1,
                StartMode::None => 2,
            };
            set_checked(self.radios[idx], true);
            set_checked(self.c_search, i.choices.file_search);
            set_checked(self.c_launch, i.choices.launch);
        }
        // SAFETY: plain messages.
        unsafe {
            SendMessageW(
                self.progress,
                PBM_SETRANGE32,
                Some(WPARAM(0)),
                Some(LPARAM(1000)),
            );
        }
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
            for c in &self.ctls {
                let f = if c.title { title_font } else { font };
                SendMessageW(
                    c.hwnd,
                    WM_SETFONT,
                    Some(WPARAM(f.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            for h in [self.back, self.next, self.cancel] {
                SendMessageW(
                    h,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
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

    fn px(&self, v: i32) -> i32 {
        v * self.dpi as i32 / 96
    }

    fn place(&self, h: HWND, x: i32, y: i32, w: i32, hgt: i32) {
        // SAFETY: valid child window.
        let _ = unsafe {
            SetWindowPos(
                h,
                None,
                self.px(x),
                self.px(y),
                self.px(w),
                self.px(hgt),
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }

    /// Glyph + label (+ optional description) row; returns the next y.
    fn place_choice(
        &self,
        glyph: HWND,
        label: HWND,
        desc: Option<(HWND, i32)>,
        x: i32,
        y: i32,
    ) -> i32 {
        let cw = W - M - x;
        self.place(glyph, x, y, GLYPH, 20);
        self.place(label, x + GLYPH + 4, y + 2, cw - GLYPH - 4, 20);
        let mut ny = y + 22;
        if let Some((d, lines)) = desc {
            self.place(d, x + GLYPH + 4, ny, cw - GLYPH - 4, 17 * lines);
            ny += 17 * lines;
        }
        ny
    }

    fn layout(&self) {
        let cw = W - 2 * M;
        self.place(self.title, M, 18, cw, 34);
        // Leaves room for the uninstall checkbox (siblings must not overlap).
        self.place(self.body, M, 64, cw, H - BAR - 64 - 64);

        // Options.
        let mut y = 64;
        self.place(self.mode_label, M, y, cw, 20);
        y += 26;
        let lines = [1, 3, 1];
        for (i, lines) in lines.into_iter().enumerate() {
            y = self.place_choice(
                self.radios[i],
                self.radio_labels[i],
                Some((self.radio_descs[i], lines)),
                M,
                y,
            ) + 8;
        }
        y += 6;
        y = self.place_choice(self.c_search, self.l_search, Some((self.d_search, 2)), M, y) + 10;
        y = self.place_choice(self.c_launch, self.l_launch, None, M, y) + 12;
        self.place(self.dir_label, M, y + 4, 150, 20);
        self.place(self.dir_edit, M + 154, y, cw - 154, 24);
        y += 34;
        self.place(self.shell_now, M, y, cw, 36);

        // Uninstall confirm: body above, checkbox at the bottom of the area.
        self.place_choice(self.c_data, self.l_data, None, M, H - BAR - 48);

        // Progress.
        self.place(self.status, M, 64, cw, 20);
        self.place(self.progress, M, 90, cw, 16);
        self.place(self.log, M, 118, cw, H - BAR - 118 - 16);

        // Buttons, right-aligned in the bottom bar.
        let (bw, bh) = (96, 30);
        let by = H - BAR + (BAR - bh) / 2;
        self.place(self.cancel, W - M - bw, by, bw, bh);
        self.place(self.next, W - M - 2 * bw - 8, by, bw, bh);
        self.place(self.back, W - M - 3 * bw - 16, by, bw, bh);
    }

    fn show_page(&mut self, page: Page) {
        self.page = page;
        for c in &self.ctls {
            let on = c.pages.contains(&page);
            // SAFETY: valid child window.
            let _ = unsafe { ShowWindow(c.hwnd, if on { SW_SHOW } else { SW_HIDE }) };
        }
        let lang = self.lang();
        let shell_warning = self.kind.facts().ours_is_shell();
        // SAFETY: valid child window.
        let _ = unsafe {
            ShowWindow(
                self.shell_now,
                if page == Page::Options && shell_warning {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            )
        };
        let (title, next, back, cancel): (String, &str, bool, bool) = match page {
            Page::Welcome => (
                self.s(
                    "Welcome to explorer.rs",
                    "Te damos la bienvenida a explorer.rs",
                )
                .into(),
                self.s("Next >", "Siguiente >"),
                false,
                true,
            ),
            Page::Options => (
                self.s("Options", "Opciones").into(),
                self.s("Install", "Instalar"),
                true,
                true,
            ),
            Page::Confirm => (
                self.s("Uninstall explorer.rs", "Desinstalar explorer.rs")
                    .into(),
                self.s("Uninstall", "Desinstalar"),
                false,
                true,
            ),
            Page::Progress => (
                match &self.kind {
                    Kind::Install(_) => self.s("Installing…", "Instalando…"),
                    Kind::Uninstall(_) => self.s("Uninstalling…", "Desinstalando…"),
                }
                .into(),
                self.s("Next >", "Siguiente >"),
                false,
                false,
            ),
            Page::Done => (String::new(), self.s("Close", "Cerrar"), false, false),
        };
        if page != Page::Done {
            set_text(self.title, &title);
        }
        set_text(self.next, next);
        enable(self.back, back);
        enable(self.cancel, cancel);
        enable(self.next, page != Page::Progress);
        // SAFETY: valid windows.
        unsafe {
            let _ = ShowWindow(self.back, if back { SW_SHOW } else { SW_HIDE });
            let _ = ShowWindow(
                self.cancel,
                if page == Page::Done { SW_HIDE } else { SW_SHOW },
            );
            let _ = SetFocus(Some(self.next));
        }
        match page {
            Page::Welcome => {
                let body = self.welcome_text();
                set_text(self.body, &body);
            }
            Page::Confirm => {
                let dir = self.kind.facts().install_dir.clone();
                set_text(
                    self.body,
                    &trf(
                        lang,
                        format!(
                            "explorer.rs will be removed from this computer:\n\n\
                             • its files in {dir}\n\
                             • its Start menu shortcuts and Installed apps entry\n\
                             • start at sign-in and the fast file search helper\n\n\
                             If explorer.rs is your shell, Windows Explorer becomes your shell again \
                             (for every signed-in user) and is started now."
                        ),
                        format!(
                            "Se quitará explorer.rs de este equipo:\n\n\
                             • sus archivos en {dir}\n\
                             • sus accesos directos del menú Inicio y su entrada en Aplicaciones instaladas\n\
                             • el inicio al iniciar sesión y el asistente de búsqueda rápida\n\n\
                             Si explorer.rs es tu shell, el Explorador de Windows vuelve a serlo \
                             (para cada usuario con sesión iniciada) y se inicia ahora."
                        ),
                    ),
                );
            }
            _ => {}
        }
    }

    fn welcome_text(&self) -> String {
        let Kind::Install(i) = &self.kind else {
            return String::new();
        };
        let new = &i.payload.product_version;
        let upgrade = match i.previous.as_ref().and_then(|p| p.version.clone()) {
            Some(old) => trf(
                self.lang(),
                format!("\n\nVersion {old} is installed and will be replaced by {new}. Your settings are kept."),
                format!("\n\nLa versión {old} está instalada y se sustituirá por la {new}. Tu configuración se conserva."),
            ),
            None => String::new(),
        };
        trf(
            self.lang(),
            format!(
                "This will install explorer.rs {new}, a fast native taskbar and Start menu for Windows 11.\n\n\
                 License: GNU General Public License, version 3 or later (GPL-3.0-or-later). \
                 explorer.rs is free software and comes with ABSOLUTELY NO WARRANTY. \
                 The full license text is installed as LICENSE.\n\n\
                 explorer.rs is an independent project. It is not affiliated with, endorsed by or \
                 sponsored by Microsoft. Windows is a trademark of Microsoft Corporation.{upgrade}"
            ),
            format!(
                "Se instalará explorer.rs {new}, una barra de tareas y un menú Inicio nativos y rápidos para Windows 11.\n\n\
                 Licencia: Licencia Pública General de GNU, versión 3 o posterior (GPL-3.0-or-later). \
                 explorer.rs es software libre y se ofrece SIN NINGUNA GARANTÍA. \
                 El texto completo de la licencia se instala como LICENSE.\n\n\
                 explorer.rs es un proyecto independiente. No está afiliado a Microsoft ni cuenta con \
                 su respaldo o patrocinio. Windows es una marca comercial de Microsoft Corporation.{upgrade}"
            ),
        )
    }

    fn choices(&self) -> Choices {
        let mode = if is_checked(self.radios[1]) {
            StartMode::Shell
        } else if is_checked(self.radios[2]) {
            StartMode::None
        } else {
            StartMode::Alongside
        };
        Choices {
            mode,
            file_search: is_checked(self.c_search),
            launch: is_checked(self.c_launch),
        }
    }

    fn append_log(&self, line: &str) {
        use explorer_win::windows::Win32::UI::Controls::{EM_REPLACESEL, EM_SETSEL};
        use explorer_win::windows::Win32::UI::WindowsAndMessaging::GetWindowTextLengthW;
        let text = WideString::new(&format!("{line}\r\n"));
        // SAFETY: valid edit control and string.
        unsafe {
            let len = GetWindowTextLengthW(self.log) as usize;
            SendMessageW(
                self.log,
                EM_SETSEL,
                Some(WPARAM(len)),
                Some(LPARAM(len as isize)),
            );
            SendMessageW(
                self.log,
                EM_REPLACESEL,
                Some(WPARAM(0)),
                Some(LPARAM(text.pcwstr().0 as isize)),
            );
        }
    }

    fn start_work(&mut self) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.running = true;
        let hwnd = self.hwnd.0 as isize;
        let lang = self.lang();
        let facts = self.kind.facts().clone();
        enum Work {
            Install(Arc<Payload>, Choices, Vec<String>),
            Uninstall(bool),
        }
        let work = match &self.kind {
            Kind::Install(i) => Work::Install(
                i.payload.clone(),
                self.choices(),
                crate::win::exec::old_manifest(&i.facts.install_dir),
            ),
            Kind::Uninstall(_) => Work::Uninstall(is_checked(self.c_data)),
        };
        self.show_page(Page::Progress);
        std::thread::spawn(move || {
            let _com = explorer_win::util::ComApartment::sta();
            let notify = move || {
                // SAFETY: posting to our window (or a destroyed one, harmless).
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(hwnd as *mut core::ffi::c_void)),
                        WM_APP_EVENT,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            };
            let tx2 = tx.clone();
            let mut report = move |e: Event| {
                let _ = tx2.send(Msg::Event(e));
                notify();
            };
            let result = match work {
                Work::Install(payload, choices, old) => {
                    tracing::info!(?choices, "installing");
                    let steps = plan::install_plan(
                        &facts,
                        &choices,
                        &payload.product_version,
                        &payload.paths(),
                        payload.total_size(),
                        &old,
                        lang,
                    );
                    Exec::new(facts, Some(&payload), lang, &mut report).run(&steps)
                }
                Work::Uninstall(remove_data) => {
                    tracing::info!(remove_data, "uninstalling");
                    let steps = plan::uninstall_plan(remove_data);
                    Exec::new(facts, None, lang, &mut report).run(&steps)
                }
            };
            if let Err(e) = &result {
                tracing::error!(error = %format!("{e:#}"), "setup failed");
            }
            let _ = tx.send(Msg::Done(result.map_err(|e| format!("{e:#}"))));
            notify();
        });
    }

    fn drain(&mut self) {
        let Some(rx) = self.rx.as_ref() else {
            return;
        };
        let msgs: Vec<Msg> = rx.try_iter().collect();
        for m in msgs {
            match m {
                Msg::Event(Event::Step { index, total, text }) => {
                    set_text(self.status, &format!("{text}…"));
                    self.append_log(&text);
                    let pos = index * 1000 / total.max(1);
                    // SAFETY: plain message.
                    unsafe { SendMessageW(self.progress, PBM_SETPOS, Some(WPARAM(pos)), None) };
                }
                Msg::Event(Event::Warning(w)) => {
                    self.append_log(&format!("  ! {w}"));
                }
                Msg::Done(r) => self.finish(r),
            }
        }
    }

    fn finish(&mut self, r: Result<Outcome, String>) {
        self.running = false;
        self.rx = None;
        // SAFETY: plain message.
        unsafe { SendMessageW(self.progress, PBM_SETPOS, Some(WPARAM(1000)), None) };
        let lang = self.lang();
        let log = self
            .kind
            .log_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".into());
        let (title, mut text, code) = match (&r, &self.kind) {
            (Ok(o), Kind::Install(i)) => {
                let c = self.choices();
                let mode_text = match c.mode {
                    StartMode::Alongside => tr(
                        lang,
                        "explorer.rs starts automatically when you sign in, next to Windows Explorer.",
                        "explorer.rs se inicia automáticamente al iniciar sesión, junto al Explorador de Windows.",
                    ),
                    StartMode::Shell => tr(
                        lang,
                        "explorer.rs is your shell from your next sign-in. To go back to Windows Explorer press Ctrl+Alt+Shift+E or run \"Restore Windows Explorer\" from the Start menu.",
                        "explorer.rs será tu shell a partir del próximo inicio de sesión. Para volver al Explorador de Windows pulsa Ctrl+Alt+Mayús+E o ejecuta \"Restaurar el Explorador de Windows\" desde el menú Inicio.",
                    ),
                    StartMode::None => tr(
                        lang,
                        "Start explorer.rs from the Start menu whenever you like.",
                        "Inicia explorer.rs desde el menú Inicio cuando quieras.",
                    ),
                };
                let ver = &i.payload.product_version;
                (
                    tr(lang, "explorer.rs is installed", "explorer.rs está instalado"),
                    trf(
                        lang,
                        format!("explorer.rs {ver} was installed.\n\n{mode_text}\n\nSettings: Start menu → \"explorer.rs settings\".\nUninstall: Settings → Apps → Installed apps."),
                        format!("Se ha instalado explorer.rs {ver}.\n\n{mode_text}\n\nConfiguración: menú Inicio → \"Configuración de explorer.rs\".\nDesinstalar: Configuración → Aplicaciones → Aplicaciones instaladas."),
                    ),
                    if o.reboot_required {
                        exit::REBOOT_REQUIRED
                    } else {
                        exit::OK
                    },
                )
            }
            (Ok(o), Kind::Uninstall(_)) => (
                tr(lang, "explorer.rs was removed", "explorer.rs se ha quitado"),
                tr(
                    lang,
                    "explorer.rs was uninstalled. Thank you for trying it.",
                    "explorer.rs se ha desinstalado. Gracias por probarlo.",
                )
                .to_string(),
                if o.reboot_required {
                    exit::REBOOT_REQUIRED
                } else {
                    exit::OK
                },
            ),
            (Err(e), _) => (
                tr(lang, "Something went wrong", "Algo salió mal"),
                trf(
                    lang,
                    format!("Setup could not finish:\n\n{e}\n\nWindows Explorer remains your shell unless it was already replaced before. You can run setup again."),
                    format!("El instalador no pudo terminar:\n\n{e}\n\nEl Explorador de Windows sigue siendo tu shell salvo que ya se hubiera reemplazado antes. Puedes volver a ejecutar el instalador."),
                ),
                exit::ERROR,
            ),
        };
        if let Ok(o) = &r {
            if o.reboot_required {
                text.push_str(tr(
                    lang,
                    "\n\nRestart Windows to finish removing files that were in use.",
                    "\n\nReinicia Windows para terminar de eliminar los archivos que estaban en uso.",
                ));
            }
            if o.warnings > 0 {
                text.push_str(&trf(
                    lang,
                    "\n\nSome steps reported warnings (see the list above).".to_string(),
                    "\n\nAlgunos pasos generaron advertencias (consulta la lista de arriba)."
                        .to_string(),
                ));
            }
        }
        text.push_str(&trf(
            lang,
            format!("\n\nLog: {log}"),
            format!("\n\nRegistro: {log}"),
        ));
        self.exit_code = code;
        self.show_page(Page::Done);
        set_text(self.title, title);
        // The step list stays visible above; the summary goes in the status
        // line and the log box keeps the details.
        set_text(self.status, title);
        self.append_log("");
        for line in text.lines() {
            self.append_log(line);
        }
    }

    fn on_command(&mut self, id: u16, code: u16) {
        let click = code as u32 == BN_CLICKED || code as u32 == STN_CLICKED;
        if !click {
            return;
        }
        let toggle = |h: HWND| {
            // SAFETY: simulates a click: focus, state and BN_CLICKED.
            unsafe { SendMessageW(h, BM_CLICK, None, None) };
        };
        match id {
            ID_L_ALONG => toggle(self.radios[0]),
            ID_L_SHELL => toggle(self.radios[1]),
            ID_L_NONE => toggle(self.radios[2]),
            ID_L_SEARCH => toggle(self.c_search),
            ID_L_LAUNCH => toggle(self.c_launch),
            ID_L_DATA => toggle(self.c_data),
            ID_BACK => {
                if self.page == Page::Options {
                    self.show_page(Page::Welcome);
                }
            }
            ID_NEXT => match self.page {
                Page::Welcome => self.show_page(Page::Options),
                Page::Options | Page::Confirm => self.start_work(),
                Page::Done => self.close(),
                Page::Progress => {}
            },
            ID_CANCEL => self.cancel(),
            _ => {}
        }
    }

    fn cancel(&mut self) {
        match self.page {
            Page::Progress => {}
            Page::Done => self.close(),
            _ => {
                let q = self.s(
                    "Cancel setup? Nothing has been changed.",
                    "¿Cancelar? No se ha cambiado nada.",
                );
                if message_box(Some(self.hwnd), q, MB_YESNO | MB_ICONQUESTION) != IDNO.0 {
                    self.exit_code = exit::CANCELLED;
                    self.close();
                }
            }
        }
    }

    fn close(&mut self) {
        // SAFETY: our own window.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

impl Handler for Wizard {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                // SAFETY: standard paint cycle.
                unsafe {
                    BeginPaint(hwnd, &mut ps);
                    let _ = EndPaint(hwnd, &ps);
                }
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as u16;
                let code = ((wparam.0 >> 16) & 0xFFFF) as u16;
                self.on_command(id, code);
                Some(LRESULT(0))
            }
            WM_APP_EVENT => {
                self.drain();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.dpi = ((wparam.0 >> 16) & 0xFFFF) as u32;
                // SAFETY: lparam points to the suggested RECT.
                let rc = unsafe { *(lparam.0 as *const RECT) };
                // SAFETY: our own window.
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
                PAINT.with(|p| {
                    if let Some(p) = p.borrow_mut().as_mut() {
                        p.dpi = self.dpi;
                    }
                });
                self.update_fonts();
                self.layout();
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.cancel();
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                // SAFETY: GDI objects created by us.
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(self.font.0));
                    let _ = DeleteObject(HGDIOBJ(self.title_font.0));
                    PAINT.with(|p| p.borrow_mut().take());
                    let _ = DeleteObject(HGDIOBJ(self.bg_brush.0));
                    let _ = DeleteObject(HGDIOBJ(self.bar_brush.0));
                    PostQuitMessage(self.exit_code);
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
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

fn run(kind: Kind) -> i32 {
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES | ICC_PROGRESS_CLASS,
    };
    // SAFETY: valid struct.
    let _ = unsafe { InitCommonControlsEx(&icc) };
    explorer_win::dpi::enable_per_monitor_v2();
    window::register_class(CLASS, None);

    let dark = wants_dark(&kind.facts().user_sid);
    let colors = Colors::new(dark);
    // SAFETY: plain calls.
    let (bg_brush, bar_brush, dpi) = unsafe {
        (
            CreateSolidBrush(colors.bg),
            CreateSolidBrush(colors.bar),
            GetDpiForSystem(),
        )
    };
    let title = match &kind {
        Kind::Install(i) => trf(
            i.lang,
            format!("explorer.rs {} setup", i.payload.product_version),
            format!("Instalación de explorer.rs {}", i.payload.product_version),
        ),
        Kind::Uninstall(u) => tr(u.lang, "Uninstall explorer.rs", "Desinstalar explorer.rs").into(),
    };
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: W * dpi as i32 / 96,
        bottom: H * dpi as i32 / 96,
    };
    // SAFETY: valid RECT.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut rc, style, false, WS_EX_CONTROLPARENT, dpi) };
    let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
    // SAFETY: plain queries.
    let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let first_page = match &kind {
        Kind::Install(_) => Page::Welcome,
        Kind::Uninstall(_) => Page::Confirm,
    };
    let wizard = Wizard {
        kind,
        hwnd: HWND::default(),
        page: first_page,
        dpi,
        colors,
        bg_brush,
        bar_brush,
        font: HFONT::default(),
        title_font: HFONT::default(),
        ctls: Vec::new(),
        title: HWND::default(),
        body: HWND::default(),
        back: HWND::default(),
        next: HWND::default(),
        cancel: HWND::default(),
        radios: [HWND::default(); 3],
        radio_labels: [HWND::default(); 3],
        radio_descs: [HWND::default(); 3],
        shell_now: HWND::default(),
        mode_label: HWND::default(),
        c_search: HWND::default(),
        l_search: HWND::default(),
        d_search: HWND::default(),
        c_launch: HWND::default(),
        l_launch: HWND::default(),
        dir_label: HWND::default(),
        dir_edit: HWND::default(),
        c_data: HWND::default(),
        l_data: HWND::default(),
        status: HWND::default(),
        progress: HWND::default(),
        log: HWND::default(),
        rx: None,
        running: false,
        exit_code: exit::CANCELLED,
    };
    // Controls are created on WM_APP_INIT (posted below), when the handler
    // knows its window handle and no other borrow is active.
    let hwnd = match window::create(
        CreateParams {
            class: CLASS,
            title: &title,
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
        Box::new(Setup(Some(wizard))),
    ) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "cannot create the setup window");
            error_box(&format!("{e:#}"));
            return exit::ERROR;
        }
    };
    if dark {
        let on: i32 = 1;
        // SAFETY: documented attribute (Windows 11 / Windows 10 20H1+).
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(20), // DWMWA_USE_IMMERSIVE_DARK_MODE
                (&on as *const i32).cast(),
                4,
            )
        };
    }
    PAINT.with(|p| {
        *p.borrow_mut() = Some(PaintState {
            colors,
            bg_brush,
            bar_brush,
            dpi,
            roles: Vec::new(),
        })
    });
    // SAFETY: subclassing our own window; the previous procedure is kept
    // for CallWindowProcW.
    unsafe {
        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, paint_proc as *const () as isize);
        PREV_PROC.with(|c| c.set(prev));
    }
    set_icon(hwnd);
    // SAFETY: posting to our own window.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_INIT, WPARAM(0), LPARAM(0)) };
    // SAFETY: our own window.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    let code = window::run_message_loop(|msg| {
        // SAFETY: msg came from GetMessageW; hwnd is our window.
        unsafe { IsDialogMessageW(hwnd, msg) }.as_bool()
    });
    tracing::info!(code, "wizard closed");
    code
}

const WM_APP_INIT: u32 = WM_APP + 2;

/// Handler wrapper: the wizard is initialised on WM_APP_INIT, when its
/// window handle is known and no other borrow is active.
struct Setup(Option<Wizard>);

impl Handler for Setup {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let w = self.0.as_mut()?;
        if msg == WM_APP_INIT && w.hwnd.is_invalid() {
            w.hwnd = hwnd;
            w.create_controls();
            let roles: Vec<(isize, Role)> =
                w.ctls.iter().map(|c| (c.hwnd.0 as isize, c.role)).collect();
            PAINT.with(|p| {
                if let Some(p) = p.borrow_mut().as_mut() {
                    p.roles = roles;
                }
            });
            w.update_fonts();
            w.layout();
            let page = w.page;
            w.show_page(page);
            return Some(LRESULT(0));
        }
        if w.hwnd.is_invalid() {
            // Before init: only the window must be destroyable.
            return match msg {
                WM_DESTROY => {
                    // SAFETY: plain call.
                    unsafe { PostQuitMessage(w.exit_code) };
                    Some(LRESULT(0))
                }
                _ => None,
            };
        }
        w.handle(hwnd, msg, wparam, lparam)
    }
}

pub fn run_install(ui: InstallUi) -> i32 {
    run(Kind::Install(ui))
}

pub fn run_uninstall(ui: UninstallUi) -> i32 {
    run(Kind::Uninstall(ui))
}

/// Colours for WM_CTLCOLOR* and the background. Kept outside the wizard's
/// handler because controls request their colours synchronously while the
/// handler may be borrowed (e.g. a static repainting inside SetWindowTextW);
/// the RefCell model would then fall back to default colours (white boxes
/// in dark mode). A small subclass procedure answers those messages from
/// this thread-local state without touching the handler.
struct PaintState {
    colors: Colors,
    bg_brush: HBRUSH,
    bar_brush: HBRUSH,
    dpi: u32,
    roles: Vec<(isize, Role)>,
}

thread_local! {
    static PAINT: std::cell::RefCell<Option<PaintState>> = const { std::cell::RefCell::new(None) };
    static PREV_PROC: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

impl PaintState {
    fn ctl_color(&self, hdc: HDC, ctl: HWND, in_bar: bool) -> LRESULT {
        let role = self
            .roles
            .iter()
            .find(|(h, _)| *h == ctl.0 as isize)
            .map(|(_, r)| *r)
            .unwrap_or(Role::Text);
        let color = match role {
            Role::Text => self.colors.text,
            Role::Sub => self.colors.sub,
            Role::Warn => self.colors.warn,
        };
        let (bg, brush) = if in_bar {
            (self.colors.bar, self.bar_brush)
        } else {
            (self.colors.bg, self.bg_brush)
        };
        // SAFETY: HDC provided by the control for this message.
        unsafe {
            SetTextColor(hdc, color);
            SetBkColor(hdc, bg);
        }
        LRESULT(brush.0 as isize)
    }

    fn erase(&self, hwnd: HWND, hdc: HDC) {
        let mut rc = RECT::default();
        // SAFETY: valid window and DC.
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
            let split = rc.bottom - BAR * self.dpi as i32 / 96;
            let top = RECT {
                bottom: split,
                ..rc
            };
            let bar = RECT { top: split, ..rc };
            FillRect(hdc, &top, self.bg_brush);
            FillRect(hdc, &bar, self.bar_brush);
        }
    }
}

unsafe extern "system" fn paint_proc(
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
            WM_CTLCOLORSTATIC => Some(p.ctl_color(hdc, ctl, false)),
            WM_CTLCOLORBTN => Some(p.ctl_color(hdc, ctl, true)),
            WM_ERASEBKGND => {
                p.erase(hwnd, hdc);
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
