// SPDX-License-Identifier: GPL-3.0-or-later
//! One taskbar ("bar") per monitor: an AppBar that paints the start button,
//! app buttons (pinned + running, grouped per app), the "…" overflow
//! button, the tray (promoted icons + chevron), the system glyphs, the
//! clock and the show-desktop corner, and turns mouse input into actions.

use std::rc::Rc;

use anyhow::Result;
use explorer_core::config::{Combine, Edge};
use explorer_core::flyout_layout::step_level;
use explorer_core::i18n::{tr, trf};
use explorer_core::ipc::{LauncherCommand, RunMode, TASKBAR_WINDOW_CLASS};
use explorer_core::layout::Rect;
use explorer_core::supervisor::{EXIT_RESTART, EXIT_SHUTDOWN_ALL};
use explorer_core::system_glyphs::{self as sg, NetKind};
use explorer_core::taskbar_groups::{self as groups, button_label, AppButton, GroupClick};
use explorer_core::taskbar_layout::{self, Hit, TaskbarLayout, TaskbarSpec};
use explorer_core::tray::{IconKey, TrayButton};
use explorer_core::tray_promotion;
use explorer_win::appbar::{AppBar, ABN_FULLSCREENAPP, ABN_POSCHANGED};
use explorer_win::gfx::{Align, Color, Surface};
use explorer_win::monitors::{self, Monitor};
use explorer_win::quick_settings::{Connectivity, Network};
use explorer_win::tooltip::Tooltip;
use explorer_win::util::{u64_to_hwnd, wide};
use explorer_win::window::{self, CreateParams, Handler};
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use explorer_win::windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, InvalidateRect, MonitorFromWindow, ScreenToClient,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use explorer_win::windows::Win32::System::Shutdown::{ExitWindowsEx, EWX_LOGOFF, SHUTDOWN_REASON};
use explorer_win::windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_SHIFT,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetClientRect, GetCursorPos, GetWindowRect,
    KillTimer, PostMessageW, SetTimer, SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, MA_NOACTIVATE,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_ACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_CLOSE,
    WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_SIZE,
    WM_TIMER, WM_WINDOWPOSCHANGED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::app::Shared;
use crate::flyout::{self, Anchor, Opener, Request};
use crate::workers::SysCmd;

const WM_APP_APPBAR: u32 = WM_APP + 2;
const WM_APP_POSITION: u32 = WM_APP + 3;

/// WinUser.h value; not exported by the `windows` crate module we use.
const WM_MOUSELEAVE: u32 = 0x02A3;

const TIMER_HOVER: usize = 10;
/// Hover delay before thumbnails appear (Windows uses ~400 ms).
const HOVER_MS: u32 = 400;

/// Logical pixels the pointer must move before a press becomes a drag.
const DRAG_THRESHOLD: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SysKind {
    Network,
    Volume,
    Battery,
}

#[derive(Debug, Clone)]
pub(crate) struct SysItem {
    pub kind: SysKind,
    pub glyph: &'static str,
    pub tip: String,
}

pub(crate) fn net_kind(n: Option<&Network>) -> NetKind {
    match n {
        None => NetKind::Disconnected,
        Some(n) => match n.connectivity {
            Connectivity::None => NetKind::Disconnected,
            Connectivity::Local => NetKind::NoInternet { wifi: n.wifi },
            Connectivity::Internet => NetKind::Internet { wifi: n.wifi },
        },
    }
}

/// Glyphs of the system area, in display order.
pub(crate) fn system_items(shared: &Shared) -> Vec<SysItem> {
    let lang = shared.lang;
    let st = shared.sys_status.borrow();
    let k = net_kind(st.network.as_ref());
    let names: &[String] = st
        .network
        .as_ref()
        .map(|n| n.names.as_slice())
        .unwrap_or(&[]);
    let mut v = vec![SysItem {
        kind: SysKind::Network,
        glyph: sg::network_glyph(k),
        tip: sg::network_tooltip(lang, k, names),
    }];
    v.push(match &st.audio {
        Some(a) => SysItem {
            kind: SysKind::Volume,
            glyph: sg::volume_glyph(a.level, a.muted),
            tip: sg::volume_tooltip(lang, &a.device, a.level, a.muted),
        },
        None => SysItem {
            kind: SysKind::Volume,
            glyph: sg::VOLUME_MUTE,
            tip: tr(
                lang,
                "No audio output device is installed",
                "No hay ningún dispositivo de salida de audio instalado",
            )
            .to_string(),
        },
    });
    if let Some(b) = st.battery {
        v.push(SysItem {
            kind: SysKind::Battery,
            glyph: sg::battery_glyph(b.percent, b.charging),
            tip: sg::battery_tooltip(lang, b.percent, b.charging, b.on_ac),
        });
    }
    v
}

/// Tray icons with their tooltips.
type TrayList = Vec<(IconKey, String)>;

/// What was painted, for hit-testing, tooltips and flyouts.
struct Snapshot {
    layout: TaskbarLayout,
    /// All buttons; the first `layout.buttons.len()` are on the bar, the
    /// rest are in the "…" flyout.
    buttons: Vec<AppButton>,
    tray: Vec<(IconKey, String)>,
    tray_hidden: Vec<(IconKey, String)>,
    system: Vec<SysItem>,
}

struct Press {
    index: usize,
    start_x: f32,
    cur_x: f32,
    dragging: bool,
}

pub(crate) struct Bar {
    shared: Rc<Shared>,
    monitor: Monitor,
    primary: bool,
    appbar: Option<AppBar>,
    surface: Option<Surface>,
    snap: Option<Snapshot>,
    hover: Hit,
    tracking: bool,
    tooltip: Option<Tooltip>,
    press: Option<Press>,
}

const CMD_LAUNCH_NEW: u32 = 1;
const CMD_PIN: u32 = 2;
const CMD_UNPIN: u32 = 3;
const CMD_CLOSE_WINDOW: u32 = 4;
const CMD_CLOSE_ALL: u32 = 5;
const CMD_TASK_MANAGER: u32 = 10;
const CMD_TASKBAR_SETTINGS: u32 = 11;
const CMD_RESTART_TASKBAR: u32 = 12;
const CMD_QUIT: u32 = 13;
const CMD_SIGN_OUT: u32 = 14;
const CMD_DATETIME: u32 = 20;
const CMD_SOUND: u32 = 21;
const CMD_MIXER: u32 = 22;
const CMD_NETWORK: u32 = 23;
const CMD_POWER: u32 = 24;

fn thickness(shared: &Shared, dpi: u32) -> i32 {
    (shared.config.taskbar.height as f32 * explorer_win::dpi::scale(dpi)).round() as i32
}

fn lparam_xy(lparam: LPARAM) -> (f32, f32) {
    (
        (lparam.0 & 0xffff) as i16 as f32,
        ((lparam.0 >> 16) & 0xffff) as i16 as f32,
    )
}

fn invalidate(hwnd: HWND) {
    // SAFETY: repaint request on our own window.
    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
}

fn shift_down() -> bool {
    // SAFETY: plain query.
    (unsafe { GetKeyState(VK_SHIFT.0 as i32) } as u16 & 0x8000) != 0
}

impl Bar {
    pub fn create(shared: Rc<Shared>, m: Monitor) -> Result<HWND> {
        let rect = explorer_win::appbar::bar_rect(
            m.rect,
            shared.config.taskbar.position,
            thickness(&shared, m.dpi),
        );
        window::create(
            CreateParams {
                class: TASKBAR_WINDOW_CLASS,
                title: "explorer.rs taskbar",
                style: WS_POPUP,
                ex_style: WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                rect,
                parent: None,
            },
            Box::new(Bar {
                shared,
                primary: m.primary,
                monitor: m,
                appbar: None,
                surface: None,
                snap: None,
                hover: Hit::Empty,
                tracking: false,
                tooltip: None,
                press: None,
            }),
        )
    }

    fn scale(&self) -> f32 {
        explorer_win::dpi::scale(self.monitor.dpi)
    }

    fn compute(&self, hwnd: HWND) -> Snapshot {
        let mut rc = RECT::default();
        // SAFETY: valid out-pointer.
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let own_only = self.shared.config.taskbar.windows_on_own_monitor_only;
        let mon = self.monitor.handle;
        let buttons = self.shared.buttons(|h| {
            !own_only || {
                // SAFETY: MonitorFromWindow accepts any handle.
                unsafe { MonitorFromWindow(u64_to_hwnd(h), MONITOR_DEFAULTTONEAREST) == mon }
            }
        });
        let (tray, tray_hidden) = self.tray_items();
        let system = system_items(&self.shared);
        let t = &self.shared.config.taskbar;
        let layout = taskbar_layout::layout(&TaskbarSpec {
            width: (rc.right - rc.left) as f32,
            height: (rc.bottom - rc.top) as f32,
            scale: self.scale(),
            n_buttons: buttons.len(),
            labels: t.button_labels,
            button_width: t.button_width as f32,
            centered: self.shared.centered.get(),
            tray_icons: tray.len(),
            tray_chevron: !tray_hidden.is_empty(),
            system_glyphs: if self.primary { system.len() } else { 0 },
            show_clock: t.show_clock,
            show_desktop: true,
        });
        Snapshot {
            layout,
            buttons,
            tray,
            tray_hidden,
            system: if self.primary { system } else { Vec::new() },
        }
    }

    /// (promoted, hidden) tray icons; only on the primary monitor.
    fn tray_items(&self) -> (TrayList, TrayList) {
        if !self.primary {
            return (Vec::new(), Vec::new());
        }
        let icons: Vec<(IconKey, String, u32, u32)> = {
            let tray = self.shared.tray.borrow();
            let Some(t) = tray.as_ref() else {
                return (Vec::new(), Vec::new());
            };
            let st = t.state.borrow();
            st.model
                .visible()
                .map(|i| (i.key, i.tip.clone(), i.hwnd, i.uid))
                .collect()
        };
        let show_all = self.shared.config.tray.show_all;
        let promoted: Vec<bool> = {
            let settings = self.shared.tray_settings.borrow();
            icons
                .iter()
                .map(|(_, _, hwnd, uid)| {
                    show_all
                        || tray_promotion::is_promoted(
                            &settings,
                            self.shared.tray_owner_exe(*hwnd).as_deref(),
                            *uid,
                            false,
                        )
                })
                .collect()
        };
        let (bar, hidden) = tray_promotion::partition(&promoted);
        let pick = |ix: Vec<usize>| -> Vec<(IconKey, String)> {
            ix.into_iter()
                .map(|i| (icons[i].0, icons[i].1.clone()))
                .collect()
        };
        (pick(bar), pick(hidden))
    }

    fn position(&mut self, hwnd: HWND) {
        // Refresh monitor info: DPI or work area may have changed.
        if let Some(m) = monitors::query(self.monitor.handle) {
            self.monitor = m;
        }
        let thickness = thickness(&self.shared, self.monitor.dpi);
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
        invalidate(hwnd);
    }

    fn paint(&mut self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: BeginPaint/EndPaint pairing in WM_PAINT.
        unsafe { BeginPaint(hwnd, &mut ps) };
        let snap = self.compute(hwnd);
        let s = self.scale();
        let pal = self.shared.palette.get();
        let clock_text = self.shared.clock_text.borrow().clone();
        let mut hover = self.hover;
        let labels = self.shared.config.taskbar.button_labels;
        let open = flyout::open_opener(&self.shared);
        let pressed = |h: Hit| -> bool {
            match (&open, h) {
                (Some(Opener::Clock), Hit::Clock)
                | (Some(Opener::Chevron), Hit::Chevron)
                | (Some(Opener::More), Hit::More)
                | (Some(Opener::System), Hit::System(_)) => true,
                (Some(Opener::Button(id)), Hit::Button(i)) => {
                    snap.buttons.get(i).is_some_and(|b| &b.id == id)
                }
                _ => false,
            }
        };
        let n = snap.layout.buttons.len();
        let icons: Vec<Option<_>> = snap.buttons[..n]
            .iter()
            .map(|b| self.shared.button_icon(b))
            .collect();
        let label_texts: Vec<String> = if labels {
            snap.buttons[..n]
                .iter()
                .map(|b| button_label(b, |h| self.shared.window_title(h)))
                .collect()
        } else {
            Vec::new()
        };
        let tray_images: Vec<Option<_>> = {
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
        // Drag: which button moves and where it would land.
        let drag = self.press.as_ref().filter(|p| p.dragging).map(|p| {
            let centers = self.pinned_centers(&snap);
            (
                p.index,
                p.cur_x,
                groups::drop_slot(&centers, p.cur_x),
                centers.len(),
            )
        });
        if drag.is_some() {
            hover = Hit::Empty;
        }
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
            let hl = |r: Rect, h: Hit| {
                if pressed(h) {
                    p.fill_rounded(inset(r), radius, Color(pal.button_active));
                } else if hover == h {
                    p.fill_rounded(inset(r), radius, Color(pal.button_hover));
                }
            };
            // Hairline separating the bar from the desktop.
            p.fill(Rect::new(0.0, 0.0, full_width, 1.0), Color(pal.border));

            // Start button: four rounded tiles in the accent colour.
            hl(l.start, Hit::Start);
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

            // App buttons.
            let draw_button = |r: Rect, i: usize, b: &AppButton, ghost: bool| {
                let ir = inset(r);
                let bg = if ghost {
                    None
                } else if b.flashing {
                    Some(pal.flash)
                } else if b.active || pressed(Hit::Button(i)) {
                    Some(pal.button_active)
                } else if hover == Hit::Button(i) {
                    Some(pal.button_hover)
                } else {
                    None
                };
                if let Some(bg) = bg {
                    p.fill_rounded(ir, radius, Color(bg));
                }
                let icon_size = 24.0 * s;
                let icon = icons.get(i).cloned().flatten();
                let icon_r = if labels {
                    Rect::new(
                        ir.x + 8.0 * s,
                        ir.y + (ir.h - icon_size) / 2.0,
                        icon_size,
                        icon_size,
                    )
                } else {
                    Rect::new(
                        ir.x + (ir.w - icon_size) / 2.0,
                        ir.y + (ir.h - icon_size) / 2.0 - s,
                        icon_size,
                        icon_size,
                    )
                };
                match &icon {
                    Some(img) => p.image(img, icon_r),
                    None => {
                        let letter: String = b.name.chars().take(1).collect();
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
                if labels {
                    let tx = icon_r.right() + 8.0 * s;
                    p.text(
                        label_texts.get(i).map(String::as_str).unwrap_or(""),
                        Rect::new(tx, ir.y, (ir.right() - tx - 6.0 * s).max(0.0), ir.h),
                        12.0 * s,
                        Color(pal.text),
                        Align::Left,
                        false,
                    );
                }
                // Running indicator: short and dim; long and accent if active.
                if b.is_running() && !ghost {
                    let (w, c) = if b.active {
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
            };
            for (i, (r, b)) in l.buttons.iter().zip(snap.buttons.iter()).enumerate() {
                let dragged = drag.is_some_and(|(di, ..)| di == i);
                draw_button(*r, i, b, dragged);
            }
            if let Some((di, x, slot, npinned)) = drag {
                // Insertion caret, then the dragged icon under the pointer.
                let caret_x = if slot < npinned {
                    l.buttons[slot].x - 1.0 * s
                } else if npinned > 0 {
                    l.buttons[npinned - 1].right() + 1.0 * s
                } else {
                    l.start.right()
                };
                p.fill_rounded(
                    Rect::new(caret_x - 1.0 * s, 8.0 * s, 2.0 * s, l.start.h - 16.0 * s),
                    1.0 * s,
                    Color(pal.accent_text),
                );
                if let (Some(r), Some(b)) = (l.buttons.get(di), snap.buttons.get(di)) {
                    let moved = Rect::new(x - r.w / 2.0, r.y, r.w, r.h);
                    p.fill_rounded(inset(moved), radius, Color(pal.button_hover));
                    draw_button(moved, di, b, true);
                }
            }
            if let Some(m) = l.more {
                hl(m, Hit::More);
                p.glyph(sg::MORE, m, 16.0 * s, Color(pal.text));
            }

            // Tray.
            if let Some(c) = l.chevron {
                hl(c, Hit::Chevron);
                let g = match (self.shared.config.taskbar.position, pressed(Hit::Chevron)) {
                    (Edge::Bottom, false) | (Edge::Top, true) => sg::CHEVRON_UP,
                    _ => sg::CHEVRON_DOWN,
                };
                p.glyph(g, c, 12.0 * s, Color(pal.text));
            }
            for (i, r) in l.tray.iter().enumerate() {
                hl(*r, Hit::Tray(i));
                if let Some(Some(img)) = tray_images.get(i) {
                    let sz = 16.0 * s;
                    p.image(
                        img,
                        Rect::new(r.x + (r.w - sz) / 2.0, r.y + (r.h - sz) / 2.0, sz, sz),
                    );
                }
            }

            // System glyphs: one pill, like Windows 11's Quick settings button.
            if let Some(area) = l.system_area() {
                let any = matches!(hover, Hit::System(_));
                if pressed(Hit::System(0)) {
                    p.fill_rounded(inset(area), radius, Color(pal.button_active));
                } else if any {
                    p.fill_rounded(inset(area), radius, Color(pal.button_hover));
                }
                for (r, item) in l.system.iter().zip(snap.system.iter()) {
                    p.glyph(item.glyph, *r, 16.0 * s, Color(pal.text));
                }
            }

            // Clock.
            if let Some(c) = l.clock {
                hl(c, Hit::Clock);
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

            // Show desktop: a thin separator; highlighted on hover.
            if let Some(sd) = l.show_desktop {
                if hover == Hit::ShowDesktop {
                    p.fill(
                        Rect::new(sd.x, sd.y + 1.0, sd.w, sd.h - 1.0),
                        Color(pal.button_hover),
                    );
                }
                p.fill(
                    Rect::new(sd.x, sd.y + 12.0 * s, 1.0, sd.h - 24.0 * s),
                    Color(pal.border),
                );
            }
        });
        self.snap = Some(snap);
        // SAFETY: matches BeginPaint above.
        let _ = unsafe { EndPaint(hwnd, &ps) };
    }

    /// Centres of the pinned buttons on the bar (pinned buttons come first
    /// when apps are combined).
    fn pinned_centers(&self, snap: &Snapshot) -> Vec<f32> {
        snap.layout
            .buttons
            .iter()
            .zip(snap.buttons.iter())
            .take_while(|(_, b)| b.pin.is_some())
            .map(|(r, _)| r.x + r.w / 2.0)
            .collect()
    }

    fn hit_xy(&self, x: f32, y: f32) -> Hit {
        match &self.snap {
            Some(s) => s.layout.hit(x, y),
            None => Hit::Empty,
        }
    }

    fn hit(&self, lparam: LPARAM) -> Hit {
        let (x, y) = lparam_xy(lparam);
        self.hit_xy(x, y)
    }

    fn button(&self, i: usize) -> Option<AppButton> {
        self.snap.as_ref().and_then(|s| s.buttons.get(i).cloned())
    }

    fn rect_of(&self, h: Hit) -> Option<Rect> {
        let l = &self.snap.as_ref()?.layout;
        match h {
            Hit::Start => Some(l.start),
            Hit::Button(i) => l.buttons.get(i).copied(),
            Hit::More => l.more,
            Hit::Chevron => l.chevron,
            Hit::Tray(i) => l.tray.get(i).copied(),
            Hit::System(i) => l.system.get(i).copied(),
            Hit::Clock => l.clock,
            Hit::ShowDesktop => l.show_desktop,
            Hit::Empty => None,
        }
    }

    fn anchor(&self, hwnd: HWND, item: Rect) -> Anchor {
        let mut wr = RECT::default();
        // SAFETY: valid out-pointer.
        let _ = unsafe { GetWindowRect(hwnd, &mut wr) };
        let m = self.monitor.rect;
        Anchor {
            item: Rect::new(
                wr.left as f32 + item.x,
                wr.top as f32 + item.y,
                item.w,
                item.h,
            ),
            bar_rect: Rect::new(
                wr.left as f32,
                wr.top as f32,
                (wr.right - wr.left) as f32,
                (wr.bottom - wr.top) as f32,
            ),
            monitor: Rect::new(
                m.left as f32,
                m.top as f32,
                (m.right - m.left) as f32,
                (m.bottom - m.top) as f32,
            ),
            bottom: self.shared.config.taskbar.position == Edge::Bottom,
            scale: self.scale(),
        }
    }

    fn hide_tooltip(&mut self) {
        if let Some(t) = self.tooltip.as_mut() {
            t.hide();
        }
    }

    fn tooltip_text(&self, h: Hit) -> Option<String> {
        let lang = self.shared.lang;
        let s = self.snap.as_ref()?;
        match h {
            Hit::Start => Some(tr(lang, "Start", "Inicio").into()),
            Hit::Button(i) => {
                let b = s.buttons.get(i)?;
                Some(match b.windows.as_slice() {
                    [one] => self
                        .shared
                        .window_title(*one)
                        .unwrap_or_else(|| b.name.clone()),
                    _ => b.name.clone(),
                })
            }
            Hit::More => Some(tr(lang, "Show more apps", "Mostrar más aplicaciones").into()),
            Hit::Chevron => Some(tr(lang, "Show hidden icons", "Mostrar iconos ocultos").into()),
            Hit::Tray(i) => s.tray.get(i).map(|t| t.1.clone()),
            Hit::System(i) => s.system.get(i).map(|t| t.tip.clone()),
            Hit::Clock => Some(self.shared.long_date()),
            Hit::ShowDesktop => Some(tr(lang, "Show desktop", "Mostrar escritorio").into()),
            Hit::Empty => None,
        }
    }

    fn update_tooltip(&mut self, hwnd: HWND) {
        // Thumbnails replace the tooltip of running apps while visible.
        let peek_for_button = matches!(self.hover, Hit::Button(_)) && flyout::is_open(&self.shared);
        let text = if peek_for_button || self.press.as_ref().is_some_and(|p| p.dragging) {
            None
        } else {
            self.tooltip_text(self.hover)
        };
        match (text.filter(|t| !t.is_empty()), self.rect_of(self.hover)) {
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
                let lines = text.lines().count().max(1) as f32;
                let y = match self.shared.config.taskbar.position {
                    Edge::Bottom => pt.y - ((14.0 + 20.0 * lines) * s) as i32,
                    Edge::Top => pt.y + r.h as i32 + (6.0 * s) as i32,
                };
                if let Some(t) = self.tooltip.as_mut() {
                    t.show(&text, pt.x, y);
                }
            }
            _ => self.hide_tooltip(),
        }
    }

    fn set_hover(&mut self, hwnd: HWND, h: Hit) {
        if h == self.hover {
            return;
        }
        self.hover = h;
        // SAFETY: timers on our own window.
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_HOVER);
        }
        if let Hit::Button(i) = h {
            let running = self.button(i).is_some_and(|b| b.is_running());
            if running && flyout::is_peek(&self.shared) {
                // Moving between buttons while thumbnails are up: switch now.
                self.open_peek(hwnd, i);
            } else if running && !flyout::is_sticky_open(&self.shared) {
                // SAFETY: as above.
                unsafe { SetTimer(Some(hwnd), TIMER_HOVER, HOVER_MS, None) };
            }
        }
        self.update_tooltip(hwnd);
        invalidate(hwnd);
    }

    fn open_peek(&mut self, hwnd: HWND, i: usize) {
        let (Some(b), Some(r)) = (self.button(i), self.rect_of(Hit::Button(i))) else {
            return;
        };
        if !b.is_running() {
            return;
        }
        let anchor = self.anchor(hwnd, r);
        flyout::peek(&self.shared, b, anchor);
        self.hide_tooltip();
    }

    fn toggle(&mut self, hwnd: HWND, h: Hit, opener: Opener, req: Request) {
        let Some(r) = self.rect_of(h) else { return };
        let r = match h {
            Hit::System(_) => self
                .snap
                .as_ref()
                .and_then(|s| s.layout.system_area())
                .unwrap_or(r),
            _ => r,
        };
        let anchor = self.anchor(hwnd, r);
        self.hide_tooltip();
        flyout::toggle(&self.shared, opener, req, anchor);
        invalidate(hwnd);
    }

    fn click_button(&mut self, hwnd: HWND, i: usize, new_instance: bool) {
        let Some(b) = self.button(i) else { return };
        if new_instance {
            flyout::close(&self.shared);
            self.shared.launch_new(&b);
            return;
        }
        let active = self.shared.model.borrow().active();
        let action = groups::click(&b, active, |h| {
            explorer_win::winlist::is_minimized(u64_to_hwnd(h))
        });
        match action {
            GroupClick::Launch => {
                flyout::close(&self.shared);
                self.shared.launch_new(&b);
            }
            GroupClick::Activate(w) => {
                flyout::close(&self.shared);
                explorer_win::activate::activate(u64_to_hwnd(w));
            }
            GroupClick::Minimize(w) => {
                flyout::close(&self.shared);
                explorer_win::activate::minimize(u64_to_hwnd(w));
            }
            GroupClick::Picker => {
                let id = b.id.clone();
                self.toggle(
                    hwnd,
                    Hit::Button(i),
                    Opener::Button(id),
                    Request::Picker { button: b },
                );
            }
        }
    }

    fn on_click(&mut self, hwnd: HWND, lparam: LPARAM, double: bool) {
        let hit = self.hit(lparam);
        match hit {
            Hit::Start if !double => {
                flyout::close(&self.shared);
                explorer_win::ipc::send_launcher_command(LauncherCommand::Toggle);
            }
            Hit::Button(i) if !double => self.click_button(hwnd, i, shift_down()),
            Hit::More if !double => {
                let hidden: Vec<AppButton> = self
                    .snap
                    .as_ref()
                    .map(|s| s.buttons[s.layout.buttons.len()..].to_vec())
                    .unwrap_or_default();
                self.toggle(hwnd, hit, Opener::More, Request::More { buttons: hidden });
            }
            Hit::Chevron if !double => {
                let icons = self
                    .snap
                    .as_ref()
                    .map(|s| s.tray_hidden.clone())
                    .unwrap_or_default();
                self.toggle(hwnd, hit, Opener::Chevron, Request::Tray { icons });
            }
            Hit::Tray(i) => {
                flyout::close(&self.shared);
                let button = if double {
                    TrayButton::LeftDouble
                } else {
                    TrayButton::Left
                };
                self.click_tray(i, button);
            }
            Hit::System(_) if !double => self.toggle(hwnd, hit, Opener::System, Request::Quick),
            Hit::Clock if !double => self.toggle(hwnd, hit, Opener::Clock, Request::Calendar),
            Hit::ShowDesktop if !double => {
                flyout::close(&self.shared);
                self.shared.toggle_show_desktop();
            }
            Hit::Empty => flyout::close(&self.shared),
            _ => {}
        }
    }

    fn click_tray(&self, i: usize, button: TrayButton) {
        let Some(key) = self.snap.as_ref().and_then(|s| s.tray.get(i).map(|t| t.0)) else {
            return;
        };
        crate::flyout::forward_tray_click(&self.shared, key, button);
    }

    fn on_wheel(&mut self, hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
        let mut pt = POINT {
            x: (lparam.0 & 0xffff) as i16 as i32,
            y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
        };
        // SAFETY: valid in/out pointer.
        let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
        let Hit::System(i) = self.hit_xy(pt.x as f32, pt.y as f32) else {
            return;
        };
        let is_volume = self
            .snap
            .as_ref()
            .and_then(|s| s.system.get(i))
            .is_some_and(|it| it.kind == SysKind::Volume);
        if !is_volume {
            return;
        }
        let notches = ((wparam.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
        let level = {
            let mut st = self.shared.sys_status.borrow_mut();
            let Some(a) = st.audio.as_mut() else { return };
            a.level = step_level(a.level, 0.02 * notches);
            a.level
        };
        self.shared.sys_cmd(SysCmd::SetVolume(level));
        self.update_tooltip_text(hwnd);
        invalidate(hwnd);
        flyout::invalidate(&self.shared);
    }

    /// Refresh the tooltip of the hovered item (its text changed).
    fn update_tooltip_text(&mut self, hwnd: HWND) {
        if let Some(s) = self.snap.as_mut() {
            s.system = system_items(&self.shared);
        }
        self.update_tooltip(hwnd);
    }

    fn on_right_click(&mut self, hwnd: HWND, lparam: LPARAM) {
        let hit = self.hit(lparam);
        if let Hit::Tray(i) = hit {
            flyout::close(&self.shared);
            self.click_tray(i, TrayButton::Right);
            return;
        }
        flyout::close(&self.shared);
        self.hide_tooltip();
        let lang = self.shared.lang;
        let b = match hit {
            Hit::Button(i) => self.button(i),
            _ => None,
        };
        let sys_kind = match hit {
            Hit::System(i) => self
                .snap
                .as_ref()
                .and_then(|s| s.system.get(i))
                .map(|it| it.kind),
            _ => None,
        };
        let mut items: Vec<(u32, String)> = Vec::new();
        if let Some(b) = &b {
            let can_launch = self.shared.launch_target(b).is_some();
            items.push((
                if can_launch {
                    CMD_LAUNCH_NEW
                } else {
                    0x8000 | CMD_LAUNCH_NEW
                },
                b.name.clone(),
            ));
            if b.pin.is_some() {
                items.push((
                    CMD_UNPIN,
                    tr(
                        lang,
                        "Unpin from taskbar",
                        "Desanclar de la barra de tareas",
                    )
                    .into(),
                ));
            } else if can_launch {
                items.push((
                    CMD_PIN,
                    tr(lang, "Pin to taskbar", "Anclar a la barra de tareas").into(),
                ));
            }
            match b.windows.len() {
                0 => {}
                1 => {
                    items.push((0, String::new()));
                    items.push((
                        CMD_CLOSE_WINDOW,
                        tr(lang, "Close window", "Cerrar ventana").into(),
                    ));
                }
                n => {
                    items.push((0, String::new()));
                    items.push((
                        CMD_CLOSE_ALL,
                        trf(
                            lang,
                            format!("Close all windows ({n})"),
                            format!("Cerrar todas las ventanas ({n})"),
                        ),
                    ));
                }
            }
        } else {
            match (hit, sys_kind) {
                (Hit::Clock, _) => {
                    items.push((
                        CMD_DATETIME,
                        tr(lang, "Adjust date and time", "Ajustar fecha y hora").into(),
                    ));
                    items.push((0, String::new()));
                }
                (_, Some(SysKind::Volume)) => {
                    items.push((
                        CMD_MIXER,
                        tr(lang, "Open volume mixer", "Abrir el mezclador de volumen").into(),
                    ));
                    items.push((
                        CMD_SOUND,
                        tr(lang, "Sound settings", "Configuración de sonido").into(),
                    ));
                    items.push((0, String::new()));
                }
                (_, Some(SysKind::Network)) => {
                    items.push((
                        CMD_NETWORK,
                        tr(
                            lang,
                            "Network and Internet settings",
                            "Configuración de red e Internet",
                        )
                        .into(),
                    ));
                    items.push((0, String::new()));
                }
                (_, Some(SysKind::Battery)) => {
                    items.push((
                        CMD_POWER,
                        tr(
                            lang,
                            "Power and battery settings",
                            "Configuración de energía y batería",
                        )
                        .into(),
                    ));
                    items.push((0, String::new()));
                }
                _ => {}
            }
            items.push((
                CMD_TASK_MANAGER,
                tr(lang, "Task Manager", "Administrador de tareas").into(),
            ));
            items.push((
                CMD_TASKBAR_SETTINGS,
                tr(
                    lang,
                    "Taskbar settings",
                    "Configuración de la barra de tareas",
                )
                .into(),
            ));
            items.push((0, String::new()));
            items.push((
                CMD_RESTART_TASKBAR,
                tr(lang, "Restart taskbar", "Reiniciar la barra de tareas").into(),
            ));
            match self.shared.mode {
                RunMode::Dev => items.push((
                    CMD_QUIT,
                    tr(lang, "Quit explorer.rs", "Salir de explorer.rs").into(),
                )),
                RunMode::Shell => {
                    items.push((CMD_SIGN_OUT, tr(lang, "Sign out", "Cerrar sesión").into()))
                }
            }
        }
        let cmd = popup_menu(hwnd, &items);
        let open = |uri: &'static str| crate::workers::open_uri(uri);
        match cmd {
            CMD_LAUNCH_NEW => {
                if let Some(b) = &b {
                    self.shared.launch_new(b);
                }
            }
            CMD_PIN | CMD_UNPIN => {
                if let Some(b) = &b {
                    self.shared.set_pinned(b, cmd == CMD_PIN);
                }
            }
            CMD_CLOSE_WINDOW | CMD_CLOSE_ALL => {
                if let Some(b) = &b {
                    for &w in &b.windows {
                        explorer_win::activate::close(u64_to_hwnd(w));
                    }
                }
            }
            CMD_TASK_MANAGER => open("taskmgr.exe"),
            CMD_TASKBAR_SETTINGS => open("ms-settings:taskbar"),
            CMD_DATETIME => open("ms-settings:dateandtime"),
            CMD_SOUND => open("ms-settings:sound"),
            CMD_MIXER => open("ms-settings:apps-volume"),
            CMD_NETWORK => open("ms-settings:network"),
            CMD_POWER => open("ms-settings:powersleep"),
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

    fn on_button_down(&mut self, hwnd: HWND, lparam: LPARAM) {
        let (x, _) = lparam_xy(lparam);
        if self.shared.config.taskbar.combine != Combine::Always {
            return;
        }
        if let Hit::Button(i) = self.hit(lparam) {
            if self.button(i).is_some_and(|b| b.pin.is_some()) {
                self.press = Some(Press {
                    index: i,
                    start_x: x,
                    cur_x: x,
                    dragging: false,
                });
                // SAFETY: capture for our own window.
                unsafe { SetCapture(hwnd) };
            }
        }
    }

    /// Returns true if the button-up ended a drag (no click).
    fn finish_drag(&mut self, hwnd: HWND, lparam: LPARAM) -> bool {
        let Some(p) = self.press.take() else {
            return false;
        };
        // SAFETY: releasing our own capture (WM_CAPTURECHANGED re-enters
        // and is ignored while we are borrowed).
        let _ = unsafe { ReleaseCapture() };
        if !p.dragging {
            return false;
        }
        let (x, _) = lparam_xy(lparam);
        if let Some(snap) = self.snap.as_ref() {
            let centers = self.pinned_centers(snap);
            let slot = groups::drop_slot(&centers, x);
            let pinned = self.shared.pinned.borrow().clone();
            let visible: Vec<String> = snap.buttons[..centers.len()]
                .iter()
                .filter_map(|b| b.pin.and_then(|i| pinned.get(i)).map(|p| p.key.clone()))
                .collect();
            if visible.len() == centers.len() {
                self.shared.reorder_pins(&visible, p.index, slot);
            }
        }
        invalidate(hwnd);
        true
    }
}

fn popup_menu(owner: HWND, items: &[(u32, String)]) -> u32 {
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
                let flags = if id & 0x8000 != 0 {
                    MF_STRING | MF_GRAYED
                } else {
                    MF_STRING
                };
                let _ = AppendMenuW(
                    menu,
                    flags,
                    (*id & 0x7fff) as usize,
                    explorer_win::windows::core::PCWSTR(w.as_ptr()),
                );
            }
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let r = window::track_popup_menu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            owner,
        );
        let _ = DestroyMenu(menu);
        r
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
                flyout::close(&self.shared);
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
                // window the user is about to minimise (and keeps an open
                // flyout active, so its opener can toggle it).
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
                let (x, _) = lparam_xy(lparam);
                let s = self.scale();
                let mut started = false;
                let mut dragging = false;
                if let Some(p) = self.press.as_mut() {
                    p.cur_x = x;
                    if !p.dragging && (x - p.start_x).abs() > DRAG_THRESHOLD * s {
                        p.dragging = true;
                        started = true;
                    }
                    dragging = p.dragging;
                }
                if started {
                    flyout::close(&self.shared);
                    self.hide_tooltip();
                }
                if dragging {
                    invalidate(hwnd);
                    return Some(LRESULT(0));
                }
                let h = self.hit(lparam);
                self.set_hover(hwnd, h);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.press.is_none() {
                    self.set_hover(hwnd, Hit::Empty);
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_HOVER => {
                // SAFETY: our own timer.
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_HOVER) };
                if let Hit::Button(i) = self.hover {
                    if self.press.is_none() && !flyout::is_sticky_open(&self.shared) {
                        self.open_peek(hwnd, i);
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                // SAFETY: our own timer.
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_HOVER) };
                self.on_button_down(hwnd, lparam);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if !self.finish_drag(hwnd, lparam) {
                    self.on_click(hwnd, lparam, false);
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                if self.press.take().is_some_and(|p| p.dragging) {
                    invalidate(hwnd);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDBLCLK => {
                self.on_click(hwnd, lparam, true);
                Some(LRESULT(0))
            }
            WM_MBUTTONUP => {
                if let Hit::Button(i) = self.hit(lparam) {
                    self.click_button(hwnd, i, true);
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                self.on_right_click(hwnd, lparam);
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.on_wheel(hwnd, wparam, lparam);
                Some(LRESULT(0))
            }
            // A bar is only destroyed by the taskbar itself (monitor
            // changes); ignore WM_CLOSE from other programs.
            WM_CLOSE => Some(LRESULT(0)),
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
