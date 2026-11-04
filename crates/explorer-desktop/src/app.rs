// SPDX-License-Identifier: GPL-3.0-or-later
//! The desktop window: bottom-most, non-activating, spanning the virtual
//! screen; paints the wallpaper per monitor with the user's settings.
//!
//! Drawing uses its own small Direct2D target (the shared `gfx::Painter`
//! only draws square icons); geometry comes from
//! `explorer_core::wallpaper`.

use explorer_core::config::Config;
use explorer_core::i18n::{tr, Lang};
use explorer_core::ipc::RunMode;
use explorer_core::layout::Rect;
use explorer_core::paths::AppPaths;
use explorer_core::wallpaper::{decode_size, place, virtual_screen, Placement};
use explorer_win::util::{ComApartment, WideString};
use explorer_win::wallpaper::{self, Decoded, Settings};
use explorer_win::window::{self, CreateParams, Handler};
use explorer_win::windows::core::PCWSTR;
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use explorer_win::windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
    D2D_RECT_F, D2D_SIZE_U,
};
use explorer_win::windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    D2D1_BITMAP_BRUSH_PROPERTIES, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
    D2D1_EXTEND_MODE_WRAP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
};
use explorer_win::windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use explorer_win::windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT,
};
use explorer_win::windows::Win32::UI::Shell::ShellExecuteW;
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, SetForegroundWindow, SetWindowPos,
    ShowWindow, TrackPopupMenu, HWND_BOTTOM, MA_NOACTIVATE, MF_SEPARATOR, MF_STRING,
    SPI_SETDESKWALLPAPER, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWNOACTIVATE, SW_SHOWNORMAL,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOWPOS, WM_CONTEXTMENU, WM_CREATE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_PAINT, WM_SETTINGCHANGE,
    WM_SIZE, WM_SYSCOLORCHANGE, WM_WINDOWPOSCHANGING, WS_CLIPCHILDREN, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::Args;

const CMD_PERSONALIZE: usize = 1;
const CMD_DISPLAY: usize = 2;
const CMD_REFRESH: usize = 3;

fn d2r(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.x + r.w,
        bottom: r.y + r.h,
    }
}

fn color(c: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((c >> 16) & 0xff) as f32 / 255.0,
        g: ((c >> 8) & 0xff) as f32 / 255.0,
        b: (c & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

struct Desktop {
    hwnd: HWND,
    lang: Lang,
    factory: ID2D1Factory,
    target: Option<(ID2D1HwndRenderTarget, ID2D1SolidColorBrush)>,
    bitmap: Option<ID2D1Bitmap>,
    settings: Settings,
    image: Option<Decoded>,
    /// Virtual screen in screen coordinates.
    screen: RECT,
    /// Monitor rects relative to the window origin.
    monitors: Vec<Rect>,
}

impl Desktop {
    fn refresh_geometry(&mut self) {
        self.screen = explorer_win::desktop::virtual_screen();
        let (ox, oy) = (self.screen.left as f32, self.screen.top as f32);
        self.monitors = explorer_win::monitors::all()
            .iter()
            .map(|m| {
                Rect::new(
                    m.rect.left as f32 - ox,
                    m.rect.top as f32 - oy,
                    (m.rect.right - m.rect.left) as f32,
                    (m.rect.bottom - m.rect.top) as f32,
                )
            })
            .collect();
        let r = self.screen;
        // SAFETY: our own window.
        let _ = unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_BOTTOM),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOACTIVATE,
            )
        };
        if let Some((rt, _)) = &self.target {
            let size = D2D_SIZE_U {
                width: (r.right - r.left).max(1) as u32,
                height: (r.bottom - r.top).max(1) as u32,
            };
            // SAFETY: valid size.
            if unsafe { rt.Resize(&size) }.is_err() {
                self.drop_target();
            }
        }
    }

    fn drop_target(&mut self) {
        self.target = None;
        self.bitmap = None;
    }

    fn ensure_target(&mut self) -> Option<(ID2D1HwndRenderTarget, ID2D1SolidColorBrush)> {
        if self.target.is_none() {
            let r = self.screen;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U {
                    width: (r.right - r.left).max(1) as u32,
                    height: (r.bottom - r.top).max(1) as u32,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            // SAFETY: valid property structs.
            let created = unsafe {
                self.factory
                    .CreateHwndRenderTarget(&props, &hprops)
                    .and_then(|rt| {
                        let b = rt.CreateSolidColorBrush(&color(0), None)?;
                        Ok((rt, b))
                    })
            };
            match created {
                Ok(t) => self.target = Some(t),
                Err(e) => {
                    tracing::error!(error = %e, "CreateHwndRenderTarget failed");
                    return None;
                }
            }
            self.bitmap = None;
        }
        self.target.clone()
    }

    /// Re-read the settings; decode the image again if needed.
    fn reload(&mut self, force_decode: bool) {
        let new = wallpaper::settings();
        let decode = force_decode
            || self.image.is_none()
            || new.path != self.settings.path
            || new.style != self.settings.style;
        self.settings = new;
        if decode {
            self.decode();
        }
    }

    fn decode(&mut self) {
        self.image = None;
        self.bitmap = None;
        let Some(path) = self.settings.path.clone() else {
            tracing::info!("no wallpaper image; solid colour");
            return;
        };
        let max_side = self
            .ensure_target()
            // SAFETY: plain query on a live target.
            .map(|(rt, _)| unsafe { rt.GetMaximumBitmapSize() })
            .unwrap_or(8192)
            .max(1024);
        let style = self.settings.style;
        let monitors = self.monitors.clone();
        match wallpaper::decode(&path, |w, h| decode_size(style, w, h, &monitors, max_side)) {
            Ok(img) => {
                tracing::info!(path = %path.display(), w = img.width, h = img.height, ?style, "wallpaper decoded");
                self.image = Some(img);
            }
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "cannot decode wallpaper"),
        }
    }

    fn ensure_bitmap(&mut self, rt: &ID2D1HwndRenderTarget) -> Option<ID2D1Bitmap> {
        if self.bitmap.is_none() {
            let img = self.image.as_ref()?;
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            // SAFETY: the pixel buffer holds width*height*4 bytes.
            let b = unsafe {
                rt.CreateBitmap(
                    D2D_SIZE_U {
                        width: img.width,
                        height: img.height,
                    },
                    Some(img.pixels.as_ptr().cast()),
                    img.width * 4,
                    &props,
                )
            };
            match b {
                Ok(b) => self.bitmap = Some(b),
                Err(e) => {
                    tracing::warn!(error = %e, "CreateBitmap failed");
                    return None;
                }
            }
        }
        self.bitmap.clone()
    }

    fn paint(&mut self) {
        let Some((rt, brush)) = self.ensure_target() else {
            return;
        };
        let bmp = self.ensure_bitmap(&rt);
        let size = self.image.as_ref().map(|i| (i.width, i.height));
        let vs = virtual_screen(&self.monitors);
        // SAFETY: BeginDraw/EndDraw pairing on a live target.
        let result = unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&color(self.settings.background)));
            if let (Some(bmp), Some((w, h))) = (&bmp, size) {
                for m in &self.monitors {
                    match place(self.settings.style, w, h, *m, vs) {
                        Placement::Draw { src, dst } => rt.DrawBitmap(
                            bmp,
                            Some(&d2r(dst)),
                            1.0,
                            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                            Some(&d2r(src)),
                        ),
                        Placement::Tile => {
                            let props = D2D1_BITMAP_BRUSH_PROPERTIES {
                                extendModeX: D2D1_EXTEND_MODE_WRAP,
                                extendModeY: D2D1_EXTEND_MODE_WRAP,
                                interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                            };
                            if let Ok(tb) = rt.CreateBitmapBrush(bmp, Some(&props), None) {
                                // Translation matrix (Matrix3x2 layout).
                                let m3: [f32; 6] = [1.0, 0.0, 0.0, 1.0, m.x, m.y];
                                tb.SetTransform(m3.as_ptr().cast());
                                rt.FillRectangle(&d2r(*m), &tb);
                            }
                        }
                        Placement::None => {}
                    }
                }
            }
            let _ = &brush;
            rt.EndDraw(None, None)
        };
        if let Err(e) = result {
            tracing::debug!(error = %e, "EndDraw failed; recreating render target");
            self.drop_target();
        }
    }

    fn context_menu(&mut self, lparam: LPARAM) {
        let (mut x, mut y) = (
            (lparam.0 & 0xffff) as i16 as i32,
            ((lparam.0 >> 16) & 0xffff) as i16 as i32,
        );
        if lparam.0 == -1 {
            let mut p = POINT::default();
            // SAFETY: valid out-pointer.
            let _ = unsafe { GetCursorPos(&mut p) };
            x = p.x;
            y = p.y;
        }
        let lang = self.lang;
        let items = [
            (CMD_PERSONALIZE, tr(lang, "Personalize", "Personalizar")),
            (
                CMD_DISPLAY,
                tr(lang, "Display settings", "Configuración de pantalla"),
            ),
            (0, ""),
            (CMD_REFRESH, tr(lang, "Refresh", "Actualizar")),
        ];
        // SAFETY: menu created, used and destroyed here.
        let cmd = unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            for (id, text) in items {
                if id == 0 {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                } else {
                    let w = WideString::new(text);
                    let _ = AppendMenuW(menu, MF_STRING, id, w.pcwstr());
                }
            }
            let _ = SetForegroundWindow(self.hwnd);
            let r = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                x,
                y,
                None,
                self.hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
            r.0 as usize
        };
        match cmd {
            CMD_PERSONALIZE => open_uri("ms-settings:personalization-background"),
            CMD_DISPLAY => open_uri("ms-settings:display"),
            CMD_REFRESH => {
                self.refresh_geometry();
                self.reload(true);
                self.invalidate();
            }
            _ => {}
        }
    }

    fn invalidate(&self) {
        // SAFETY: our own window.
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
    }
}

fn open_uri(uri: &str) {
    let u = WideString::new(uri);
    let open = WideString::new("open");
    // SAFETY: valid strings.
    let r = unsafe {
        ShellExecuteW(
            None,
            open.pcwstr(),
            u.pcwstr(),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (r.0 as isize) <= 32 {
        tracing::warn!(uri, code = r.0 as isize, "ShellExecuteW failed");
    }
}

impl Handler for Desktop {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if self.hwnd.is_invalid() {
            self.hwnd = hwnd;
        }
        match msg {
            WM_CREATE => {
                self.refresh_geometry();
                self.reload(true);
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                // SAFETY: BeginPaint/EndPaint pairing.
                unsafe { BeginPaint(hwnd, &mut ps) };
                self.paint();
                // SAFETY: matches BeginPaint.
                let _ = unsafe { EndPaint(hwnd, &ps) };
                Some(LRESULT(0))
            }
            WM_SIZE => {
                self.invalidate();
                None
            }
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_WINDOWPOSCHANGING => {
                // Always stay at the bottom of the z-order.
                // SAFETY: lParam points to a WINDOWPOS for this message.
                if let Some(wp) = unsafe { (lparam.0 as *mut WINDOWPOS).as_mut() } {
                    wp.hwndInsertAfter = HWND_BOTTOM;
                    wp.flags &= !SWP_NOZORDER;
                }
                None
            }
            WM_SETTINGCHANGE => {
                let wallpaper_changed = wparam.0 as u32 == SPI_SETDESKWALLPAPER.0;
                self.reload(wallpaper_changed);
                self.invalidate();
                None
            }
            WM_SYSCOLORCHANGE => {
                self.reload(false);
                self.invalidate();
                None
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                // Ignore the suggested rect: we always cover the virtual screen.
                self.refresh_geometry();
                self.reload(true);
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_CONTEXTMENU => {
                self.context_menu(lparam);
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                window::quit(0);
                None
            }
            _ => None,
        }
    }
}

pub fn run(args: Args) -> i32 {
    let paths = AppPaths::from_env();
    let _ = paths.ensure();
    let config_path = args.config.clone().unwrap_or_else(|| paths.config_file());
    let (config, config_err) = Config::load_or_default(&config_path);
    let mode = RunMode::from_env();
    if !config.desktop.enabled.enabled_in(mode) {
        // Not wanted in this mode: exit 0 so the supervisor does not restart.
        return 0;
    }
    let _log = explorer_core::logging::init(
        "desktop",
        &paths.log_dir,
        &config.general.log_level,
        config.general.log_keep_files,
    );
    if let Some(e) = config_err {
        tracing::error!(error = %e, "config error, using defaults");
    }
    let Some(_instance) = explorer_win::single_instance::acquire("ExplorerRs.Desktop") else {
        tracing::warn!("another desktop is already running; exiting");
        return 0;
    };
    tracing::info!(mode = mode.as_str(), "desktop starting");
    explorer_win::dpi::enable_per_monitor_v2();
    let _com = ComApartment::sta();

    // SAFETY: factory creation with valid enum values.
    let factory: ID2D1Factory =
        match unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) } {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = %e, "D2D1CreateFactory failed");
                return 1;
            }
        };
    let shell = mode == RunMode::Shell;
    // Some programs look for the desktop as "Progman"/"Program Manager";
    // only claim that name when explorer.exe is not around.
    let (class, title) = if shell {
        ("Progman", "Program Manager")
    } else {
        ("ExplorerRs.Desktop", "explorer.rs desktop")
    };
    window::register_class(class, None);
    let screen = explorer_win::desktop::virtual_screen();
    let lang = explorer_win::theme::ui_lang(config.general.language);
    let hwnd = match window::create(
        CreateParams {
            class,
            title,
            style: WS_POPUP | WS_CLIPCHILDREN,
            ex_style: WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            rect: screen,
            parent: None,
        },
        Box::new(Desktop {
            hwnd: HWND::default(),
            lang,
            factory,
            target: None,
            bitmap: None,
            settings: wallpaper::settings(),
            image: None,
            screen,
            monitors: Vec::new(),
        }),
    ) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "cannot create desktop window");
            return 1;
        }
    };
    // SAFETY: our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    if shell {
        explorer_win::desktop::set_shell_window(hwnd);
    }
    window::run_message_loop(|_| false)
}
