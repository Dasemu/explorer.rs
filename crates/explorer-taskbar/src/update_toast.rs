// SPDX-License-Identifier: GPL-3.0-or-later
//! The "update available" reminder: a small card above the clock corner,
//! drawn like the taskbar's flyouts, with three choices:
//!
//! - **Update now**: opens the settings window's Updates page, which
//!   downloads, verifies and runs the installer (`explorer-rs --settings
//!   --update-now`). That process is not one of the supervised components,
//!   so the installer can stop all of them, this taskbar included;
//! - **Later** (and the ×): closes it; it comes back after
//!   `explorer_core::update::REMIND_INTERVAL_SECS`;
//! - **Don't remind me again**: no more reminders for this version.
//!
//! When it appears is decided by `explorer_core::update::should_remind`
//! (controller, `app.rs`). It never takes the focus (no activation), so it
//! cannot swallow typing; it is hidden when a full-screen app comes up.
//! A Windows toast notification was not used: it needs an
//! AppUserModelID registered through a Start menu shortcut, which a
//! portable copy or a replaced shell does not reliably have.

use std::rc::Rc;

use explorer_core::config::Edge;
use explorer_core::i18n::{tr, trf};
use explorer_core::layout::Rect;
use explorer_core::system_glyphs as sg;
use explorer_core::update::{UpdateState, Version};
use explorer_win::gfx::{Align, Color, Surface};
use explorer_win::window::{self, CreateParams, Handler};
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use explorer_win::windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use explorer_win::windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT,
};
use explorer_win::windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, SetWindowPos, HWND_TOPMOST, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    WM_DESTROY, WM_DPICHANGED, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_SIZE,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::app::Shared;

const CLASS: &str = "ExplorerRs.UpdateReminder";
/// WinUser.h value; not exported by the `windows` crate module we use.
const WM_MOUSELEAVE: u32 = 0x02A3;

// Layout, logical pixels.
const W: f32 = 400.0;
const H: f32 = 136.0;
const PAD: f32 = 16.0;
const MARGIN: f32 = 12.0;
const BUTTON_H: f32 = 32.0;
const GAP: f32 = 4.0;
/// Button widths: Update now, Later, Don't remind me again.
const BUTTON_W: [f32; 3] = [120.0, 96.0, 144.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    None,
    Close,
    Button(usize),
}

pub(crate) fn register_class() {
    window::register_class(CLASS, None);
}

/// Show the reminder for `version` (replacing one already shown).
pub(crate) fn show(shared: &Rc<Shared>, version: Version) {
    hide(shared);
    let Some(mon) = explorer_win::monitors::all()
        .into_iter()
        .find(|m| m.primary)
    else {
        return;
    };
    let scale = mon.dpi.max(96) as f32 / 96.0;
    let (w, h) = ((W * scale).round() as i32, (H * scale).round() as i32);
    let margin = (MARGIN * scale).round() as i32;
    // The work area leaves out the taskbar's reserved space.
    let wa = mon.work;
    let x = wa.right - w - margin;
    let y = match shared.config.taskbar.position {
        Edge::Bottom => wa.bottom - h - margin,
        Edge::Top => wa.top + margin,
    };
    let res = window::create(
        CreateParams {
            class: CLASS,
            title: "explorer.rs update",
            style: WS_POPUP,
            ex_style: WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            rect: RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            },
            parent: None,
        },
        Box::new(Toast {
            shared: shared.clone(),
            version,
            scale,
            surface: None,
            hover: Part::None,
            tracking: false,
        }),
    );
    match res {
        Ok(hwnd) => {
            let corner: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            // SAFETY: valid attribute pointer and size (Windows 11 only).
            let _ = unsafe {
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    &corner as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
                )
            };
            // Created without WS_VISIBLE: show it explicitly, unactivated.
            // SAFETY: our own window.
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    w,
                    h,
                    SWP_SHOWWINDOW | SWP_NOACTIVATE,
                )
            };
            shared.update_toast.set(hwnd);
            tracing::info!(%version, "update reminder shown");
        }
        Err(e) => tracing::warn!(error = %e, "cannot create the update reminder"),
    }
}

/// Close the reminder if it is shown.
pub(crate) fn hide(shared: &Shared) {
    let h = shared.update_toast.replace(HWND::default());
    if !h.is_invalid() {
        // SAFETY: our own window, destroyed from outside its handler (the
        // handler posts nothing that re-enters here).
        let _ = unsafe { DestroyWindow(h) };
    }
}

pub(crate) fn is_shown(shared: &Shared) -> bool {
    !shared.update_toast.get().is_invalid()
}

struct Toast {
    shared: Rc<Shared>,
    version: Version,
    scale: f32,
    surface: Option<Surface>,
    hover: Part,
    tracking: bool,
}

impl Toast {
    fn close_rect(&self) -> Rect {
        let s = self.scale;
        Rect::new((W - 8.0 - 28.0) * s, 8.0 * s, 28.0 * s, 28.0 * s)
    }

    fn button_rect(&self, i: usize) -> Rect {
        let s = self.scale;
        let x: f32 = PAD + BUTTON_W[..i].iter().map(|w| w + GAP).sum::<f32>();
        Rect::new(
            x * s,
            (H - PAD - BUTTON_H) * s,
            BUTTON_W[i] * s,
            BUTTON_H * s,
        )
    }

    fn part_at(&self, x: f32, y: f32) -> Part {
        let inside = |r: Rect| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
        if inside(self.close_rect()) {
            return Part::Close;
        }
        (0..BUTTON_W.len())
            .find(|&i| inside(self.button_rect(i)))
            .map_or(Part::None, Part::Button)
    }

    fn paint(&mut self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: BeginPaint/EndPaint pairing in WM_PAINT.
        unsafe { BeginPaint(hwnd, &mut ps) };
        let shared = self.shared.clone();
        let pal = shared.palette.get();
        let lang = shared.lang;
        let s = self.scale;
        let v = self.version;
        let hover = self.hover;
        let close = self.close_rect();
        let buttons: Vec<Rect> = (0..BUTTON_W.len()).map(|i| self.button_rect(i)).collect();
        let surface = self.surface.get_or_insert_with(|| Surface::new(hwnd));
        surface.draw(&shared.gfx, Color(pal.panel_bg), |p| {
            let (w, h) = (W * s, H * s);
            p.stroke_rounded(
                Rect::new(0.5, 0.5, w - 1.0, h - 1.0),
                8.0 * s,
                1.0,
                Color(pal.border),
            );
            let text_w = w - 2.0 * PAD * s - 32.0 * s;
            p.text(
                &trf(
                    lang,
                    format!("explorer.rs {v} is available"),
                    format!("Ya está disponible explorer.rs {v}"),
                ),
                Rect::new(PAD * s, PAD * s, text_w, 20.0 * s),
                14.0 * s,
                Color(pal.text),
                Align::Left,
                true,
            );
            let lines = [
                tr(
                    lang,
                    "It includes fixes and improvements.",
                    "Incluye correcciones y mejoras.",
                ),
                tr(
                    lang,
                    "Installing it is recommended.",
                    "Se recomienda instalarla.",
                ),
            ];
            for (i, l) in lines.iter().enumerate() {
                p.text(
                    l,
                    Rect::new(
                        PAD * s,
                        (PAD + 26.0 + 18.0 * i as f32) * s,
                        w - 2.0 * PAD * s,
                        18.0 * s,
                    ),
                    12.0 * s,
                    Color(pal.text_dim),
                    Align::Left,
                    false,
                );
            }
            if hover == Part::Close {
                p.fill_rounded(close, 4.0 * s, Color(pal.button_hover));
            }
            p.glyph(sg::CLOSE, close, 10.0 * s, Color(pal.text));
            let labels = [
                tr(lang, "Update now", "Actualizar ahora"),
                tr(lang, "Later", "Más tarde"),
                tr(lang, "Don't remind me again", "No volver a recordar"),
            ];
            for (i, (r, label)) in buttons.iter().zip(labels).enumerate() {
                let hov = hover == Part::Button(i);
                let fg = if i == 0 {
                    // The recommended choice, in the accent colour.
                    let bg = if hov {
                        explorer_core::theme::mix(pal.accent_text, pal.panel_bg, 0.15)
                    } else {
                        pal.accent_text
                    };
                    p.fill_rounded(*r, 4.0 * s, Color(bg));
                    crate::flyout::on_accent(&pal)
                } else {
                    let bg = if hov { pal.button_hover } else { pal.input_bg };
                    p.fill_rounded(*r, 4.0 * s, Color(bg));
                    pal.text
                };
                p.text(label, *r, 12.0 * s, Color(fg), Align::Center, false);
            }
        });
        // SAFETY: pairs BeginPaint.
        let _ = unsafe { EndPaint(hwnd, &ps) };
    }

    fn set_hover(&mut self, hwnd: HWND, part: Part) {
        if part != self.hover {
            self.hover = part;
            // SAFETY: repaint request on our own window.
            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
        }
    }

    fn on_click(&mut self, part: Part) {
        let shared = self.shared.clone();
        match part {
            Part::Button(0) => {
                tracing::info!(version = %self.version, "update reminder: update now");
                run_update_now();
            }
            Part::Button(2) => {
                tracing::info!(version = %self.version, "update reminder: don't remind again");
                let file = explorer_core::paths::AppPaths::from_env().update_state_file();
                let mut st = UpdateState::load(&file);
                st.dismiss(self.version);
                if let Err(e) = st.save(&file) {
                    tracing::warn!(error = %e, "cannot save the update state");
                }
            }
            Part::Button(_) | Part::Close => {
                tracing::info!(version = %self.version, "update reminder: later");
            }
            Part::None => return,
        }
        // Destroy outside this handler (see `window::create`'s ownership
        // notes): post it to the controller.
        crate::app::post_hide_update_toast(&shared);
    }
}

/// `explorer-rs.exe --settings --update-now`, next to this program.
fn run_update_now() {
    let exe = std::env::current_exe()
        .map(|p| p.with_file_name("explorer-rs.exe"))
        .unwrap_or_else(|_| "explorer-rs.exe".into());
    if let Err(e) = std::process::Command::new(&exe)
        .args(["--settings", "--update-now"])
        .spawn()
    {
        tracing::error!(exe = %exe.display(), error = %e, "cannot start the updater");
    }
}

fn lparam_xy(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
    (x, y)
}

impl Handler for Toast {
    fn handle(&mut self, hwnd: HWND, msg: u32, _wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint(hwnd);
                Some(LRESULT(0))
            }
            WM_SIZE => {
                if let Some(s) = self.surface.as_mut() {
                    s.resize();
                }
                None
            }
            // Sized for the primary monitor when shown; a later DPI change
            // only affects the next reminder.
            WM_DPICHANGED => Some(LRESULT(0)),
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
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
                let (x, y) = lparam_xy(lparam);
                let part = self.part_at(x, y);
                self.set_hover(hwnd, part);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.set_hover(hwnd, Part::None);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lparam);
                let part = self.part_at(x, y);
                self.on_click(part);
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                self.surface = None;
                None
            }
            _ => None,
        }
    }
}
