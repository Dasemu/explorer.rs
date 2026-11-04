// SPDX-License-Identifier: GPL-3.0-or-later
//! The taskbar's popup ("flyout") window, reused for every popup:
//!
//! - **window picker**: live DWM thumbnails of an app's windows with
//!   titles and a close button. Shown on hover after a delay ("peek":
//!   never activated, hidden when the pointer leaves the button and the
//!   popup) or on click for multi-window apps ("sticky");
//! - **"…" overflow**: the app buttons that did not fit;
//! - **tray overflow**: notification icons that are not promoted;
//! - **Quick settings**: volume, brightness, network, battery;
//! - **calendar**: full date and a month grid.
//!
//! Sticky flyouts are activated (keyboard focus for Esc) and close when
//! they lose activation. The bar uses `MA_NOACTIVATE`, so clicking the
//! opener while a flyout is open does not deactivate it; the opener then
//! toggles it closed. A short debounce after a deactivation-close still
//! guards against the "click on the opener reopens it" race.
//!
//! State lives in `Shared::flyout` (not in the window handler) so the bar
//! and the controller can open, refresh and close it. Borrows of that
//! state are never held across Win32 calls that dispatch messages to the
//! flyout window.

use std::rc::Rc;
use std::time::{Duration, Instant};

use explorer_core::calendar;
use explorer_core::flyout_layout::{
    self as fl, CalendarLayout, HAnchor, ListLayout, PickerLayout, QuickSettingsLayout, SliderRow,
    FLYOUT_MARGIN,
};
use explorer_core::i18n::{tr, trf};
use explorer_core::layout::Rect;
use explorer_core::system_glyphs as sg;
use explorer_core::taskbar_groups::{self as groups, AppButton, GroupClick};
use explorer_core::theme::{mix, Palette};
use explorer_core::tray::{IconKey, TrayButton};
use explorer_win::gfx::{Align, Color, Painter, Surface};
use explorer_win::thumbnails::Thumbnail;
use explorer_win::util::u64_to_hwnd;
use explorer_win::window::{self, CreateParams, Handler};
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use explorer_win::windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use explorer_win::windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, ScreenToClient, PAINTSTRUCT,
};
use explorer_win::windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_LEFT,
    VK_NEXT, VK_PRIOR, VK_RIGHT,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowRect, IsWindowVisible, SetTimer, SetWindowPos,
    ShowWindow, HWND_TOPMOST, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WM_ACTIVATE,
    WM_CAPTURECHANGED, WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_SIZE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::app::{Shared, TIMER_PEEK};
use crate::bar::net_kind;
use crate::workers::SysCmd;

const FLYOUT_CLASS: &str = "ExplorerRs.TaskbarFlyout";

/// WinUser.h value; not exported by the `windows` crate module we use.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Ignore a toggle of the same opener this soon after a deactivation
/// closed it (same value as the Start menu's TOGGLE_DEBOUNCE).
const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(400);
/// Hover thumbnails stay this long after the pointer left button and popup.
const PEEK_GRACE: Duration = Duration::from_millis(300);
/// A sticky flyout that is not the foreground window this long after
/// opening (foreground lock, missed WM_ACTIVATE) is closed.
const STICKY_FOREGROUND_GRACE: Duration = Duration::from_millis(700);

/// What opened the flyout (used to toggle it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Opener {
    Button(String),
    More,
    Chevron,
    System,
    Clock,
}

/// Where the flyout belongs (all screen coordinates, physical pixels).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Anchor {
    /// The opener's rectangle.
    pub item: Rect,
    pub bar_rect: Rect,
    pub monitor: Rect,
    pub bottom: bool,
    pub scale: f32,
}

pub(crate) enum Request {
    Picker { button: AppButton },
    More { buttons: Vec<AppButton> },
    Tray { icons: Vec<(IconKey, String)> },
    Quick,
    Calendar,
}

struct PickerItem {
    hwnd: u64,
    title: String,
    thumb: Option<Thumbnail>,
}

impl PickerItem {
    fn usable(&self) -> bool {
        self.thumb.as_ref().is_some_and(|t| t.is_usable())
    }
}

enum Content {
    Picker {
        button: AppButton,
        items: Vec<PickerItem>,
        layout: PickerLayout,
    },
    More {
        buttons: Vec<AppButton>,
        layout: ListLayout,
    },
    Tray {
        icons: Vec<(IconKey, String)>,
        layout: ListLayout,
    },
    Quick {
        layout: QuickSettingsLayout,
    },
    Calendar {
        year: i32,
        month: u32,
        week_start: u32,
        layout: CalendarLayout,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    None,
    Card(usize),
    Close(usize),
    Row(usize),
    VolIcon,
    VolTrack,
    Device,
    BriTrack,
    Network,
    Battery,
    Settings,
    Prev,
    Next,
    Month,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    Volume(f32),
    Brightness(f32),
}

pub(crate) struct State {
    content: Content,
    opener: Opener,
    sticky: bool,
    anchor: Anchor,
    size: (f32, f32),
    hover: Part,
    drag: Option<Drag>,
    opened_at: Instant,
    left_at: Option<Instant>,
}

impl State {
    fn scale(&self) -> f32 {
        self.anchor.scale
    }
}

// ---------------------------------------------------------------------------
// Public API (used by the bar and the controller)
// ---------------------------------------------------------------------------

pub(crate) fn register_class() {
    window::register_class(FLYOUT_CLASS, None);
}

pub(crate) fn create_window(shared: &Rc<Shared>) {
    let res = window::create(
        CreateParams {
            class: FLYOUT_CLASS,
            title: "explorer.rs flyout",
            style: WS_POPUP,
            ex_style: WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            rect: RECT {
                left: 0,
                top: 0,
                right: 10,
                bottom: 10,
            },
            parent: None,
        },
        Box::new(FlyoutWin {
            shared: shared.clone(),
            surface: None,
            tracking: false,
            last_sent: None,
        }),
    );
    match res {
        Ok(h) => {
            let corner: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            // SAFETY: valid attribute pointer and size (Windows 11 only;
            // failure on older systems is harmless).
            let _ = unsafe {
                DwmSetWindowAttribute(
                    h,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    &corner as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
                )
            };
            shared.flyout_hwnd.set(h);
        }
        Err(e) => tracing::error!(error = %e, "cannot create the flyout window"),
    }
}

/// The opener of the open flyout, if any.
pub(crate) fn open_opener(shared: &Shared) -> Option<Opener> {
    shared
        .flyout
        .try_borrow()
        .ok()?
        .as_ref()
        .map(|s| s.opener.clone())
}

pub(crate) fn is_open(shared: &Shared) -> bool {
    shared.flyout.try_borrow().is_ok_and(|s| s.is_some())
}

pub(crate) fn is_peek(shared: &Shared) -> bool {
    shared
        .flyout
        .try_borrow()
        .is_ok_and(|s| s.as_ref().is_some_and(|s| !s.sticky))
}

pub(crate) fn is_sticky_open(shared: &Shared) -> bool {
    shared
        .flyout
        .try_borrow()
        .is_ok_and(|s| s.as_ref().is_some_and(|s| s.sticky))
}

pub(crate) fn dragging_volume(shared: &Shared) -> Option<f32> {
    match shared.flyout.try_borrow().ok()?.as_ref()?.drag? {
        Drag::Volume(v) => Some(v),
        Drag::Brightness(_) => None,
    }
}

pub(crate) fn dragging_brightness(shared: &Shared) -> Option<u8> {
    match shared.flyout.try_borrow().ok()?.as_ref()?.drag? {
        Drag::Brightness(v) => Some(sg::percent(v) as u8),
        Drag::Volume(_) => None,
    }
}

pub(crate) fn invalidate(shared: &Shared) {
    let h = shared.flyout_hwnd.get();
    if !h.is_invalid() {
        relayout_quick_if_needed(shared);
        // SAFETY: repaint request on our own window.
        let _ = unsafe { InvalidateRect(Some(h), None, false) };
    }
}

/// Toggle a sticky flyout from a click on its opener.
pub(crate) fn toggle(shared: &Rc<Shared>, opener: Opener, req: Request, anchor: Anchor) {
    let cur = shared
        .flyout
        .borrow()
        .as_ref()
        .map(|s| (s.opener.clone(), s.sticky));
    match cur {
        Some((o, true)) if o == opener => {
            close(shared);
            return;
        }
        Some((o, false)) if o == opener => {
            // Hover thumbnails are up for this button: keep them, sticky.
            make_sticky(shared);
            return;
        }
        _ => {}
    }
    let recently_closed = shared
        .flyout_closed
        .borrow()
        .as_ref()
        .is_some_and(|(o, t)| *o == opener && t.elapsed() < TOGGLE_DEBOUNCE);
    *shared.flyout_closed.borrow_mut() = None;
    if recently_closed {
        // The click that deactivated (and closed) the flyout was on its
        // own opener: do not reopen.
        return;
    }
    open(shared, opener, req, anchor, true);
}

/// Show hover thumbnails for `button` (never activates the flyout).
pub(crate) fn peek(shared: &Rc<Shared>, button: AppButton, anchor: Anchor) {
    if is_sticky_open(shared) {
        return;
    }
    let opener = Opener::Button(button.id.clone());
    if open_opener(shared).as_ref() == Some(&opener) {
        return;
    }
    open(shared, opener, Request::Picker { button }, anchor, false);
}

/// Close (hide) the flyout.
pub(crate) fn close(shared: &Shared) {
    let Ok(mut slot) = shared.flyout.try_borrow_mut() else {
        return;
    };
    let Some(st) = slot.take() else {
        return;
    };
    drop(slot);
    if st.sticky {
        *shared.flyout_closed.borrow_mut() = Some((st.opener.clone(), Instant::now()));
    }
    if matches!(st.content, Content::Quick { .. }) {
        shared.sys_cmd(SysCmd::Fast(false));
    }
    // Unregister thumbnails before hiding.
    drop(st);
    let h = shared.flyout_hwnd.get();
    if !h.is_invalid() {
        // SAFETY: hiding our own window; nested messages are harmless.
        let _ = unsafe { ShowWindow(h, SW_HIDE) };
    }
    shared.invalidate_bars();
}

/// Model or tray changed: update the open picker / tray overflow.
pub(crate) fn refresh(shared: &Rc<Shared>) {
    enum Next {
        Nothing,
        Close,
        Picker(AppButton),
        Tray(Vec<(IconKey, String)>),
    }
    let next = {
        let Ok(st) = shared.flyout.try_borrow() else {
            return;
        };
        let Some(st) = st.as_ref() else { return };
        match &st.content {
            Content::Picker { button, items, .. } => {
                let now = shared.buttons(|_| true);
                match now.into_iter().find(|b| b.id == button.id) {
                    Some(b) if b.is_running() => {
                        let same: Vec<u64> = items.iter().map(|i| i.hwnd).collect();
                        let titles_same = items
                            .iter()
                            .all(|i| shared.window_title(i.hwnd).as_deref() == Some(&i.title));
                        if same == b.windows && titles_same {
                            Next::Nothing
                        } else {
                            Next::Picker(b)
                        }
                    }
                    _ => Next::Close,
                }
            }
            Content::Tray { icons, .. } => {
                let tray = shared.tray.borrow();
                let alive: Vec<(IconKey, String)> = match tray.as_ref() {
                    Some(t) => {
                        let ts = t.state.borrow();
                        icons
                            .iter()
                            .filter_map(|(k, _)| {
                                ts.model
                                    .get(*k)
                                    .filter(|i| !i.hidden)
                                    .map(|i| (*k, i.tip.clone()))
                            })
                            .collect()
                    }
                    None => Vec::new(),
                };
                if alive.is_empty() {
                    Next::Close
                } else if alive.len() != icons.len() {
                    Next::Tray(alive)
                } else {
                    Next::Nothing
                }
            }
            _ => Next::Nothing,
        }
    };
    match next {
        Next::Nothing => invalidate(shared),
        Next::Close => close(shared),
        Next::Picker(button) => rebuild(shared, Request::Picker { button }),
        Next::Tray(icons) => rebuild(shared, Request::Tray { icons }),
    }
}

/// Controller timer while a flyout is open. Returns false to stop it.
pub(crate) fn peek_tick(shared: &Shared) -> bool {
    let h = shared.flyout_hwnd.get();
    let info = {
        let Ok(st) = shared.flyout.try_borrow() else {
            return true;
        };
        st.as_ref().map(|s| {
            (
                s.sticky,
                s.anchor.item,
                s.opened_at,
                s.left_at,
                s.drag.is_some(),
            )
        })
    };
    let Some((sticky, item, opened_at, left_at, dragging)) = info else {
        return false;
    };
    if sticky {
        // SAFETY: plain query.
        let fg = unsafe { GetForegroundWindow() };
        if fg != h && opened_at.elapsed() > STICKY_FOREGROUND_GRACE && !dragging {
            tracing::debug!("sticky flyout lost the foreground; closing");
            close(shared);
            return false;
        }
        return true;
    }
    let mut pt = POINT::default();
    let mut wr = RECT::default();
    // SAFETY: valid out-pointers.
    unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = GetWindowRect(h, &mut wr);
    }
    let (x, y) = (pt.x as f32, pt.y as f32);
    let in_popup = pt.x >= wr.left && pt.x < wr.right && pt.y >= wr.top && pt.y < wr.bottom;
    if item.contains(x, y) || in_popup {
        if left_at.is_some() {
            if let Ok(mut st) = shared.flyout.try_borrow_mut() {
                if let Some(s) = st.as_mut() {
                    s.left_at = None;
                }
            }
        }
        return true;
    }
    match left_at {
        Some(t) if t.elapsed() >= PEEK_GRACE => {
            close(shared);
            false
        }
        Some(_) => true,
        None => {
            if let Ok(mut st) = shared.flyout.try_borrow_mut() {
                if let Some(s) = st.as_mut() {
                    s.left_at = Some(Instant::now());
                }
            }
            true
        }
    }
}

/// Forward a click to a tray icon at the cursor position.
pub(crate) fn forward_tray_click(shared: &Shared, key: IconKey, button: TrayButton) {
    let mut pt = POINT::default();
    // SAFETY: valid out-pointer.
    let _ = unsafe { GetCursorPos(&mut pt) };
    if let Some(t) = shared.tray.borrow().as_ref() {
        t.click(key, button, pt.x, pt.y);
    }
}

// ---------------------------------------------------------------------------
// Opening and geometry
// ---------------------------------------------------------------------------

fn build(shared: &Shared, req: Request, anchor: &Anchor) -> Content {
    let s = anchor.scale;
    match req {
        Request::Picker { button } => {
            let max_w = anchor.monitor.w - 2.0 * FLYOUT_MARGIN * s;
            let layout = fl::picker(button.windows.len(), s, max_w);
            let items = button
                .windows
                .iter()
                .map(|&h| PickerItem {
                    hwnd: h,
                    title: shared.window_title(h).unwrap_or_default(),
                    thumb: None,
                })
                .collect();
            Content::Picker {
                button,
                items,
                layout,
            }
        }
        Request::More { buttons } => {
            let max_rows = (((anchor.monitor.h * 0.6) / (36.0 * s)).floor() as usize).max(4);
            Content::More {
                layout: fl::list(buttons.len(), s, max_rows),
                buttons,
            }
        }
        Request::Tray { icons } => Content::Tray {
            layout: fl::icon_grid(icons.len(), s),
            icons,
        },
        Request::Quick => {
            let st = shared.sys_status.borrow();
            Content::Quick {
                layout: fl::quick_settings(s, st.brightness.is_some(), st.battery.is_some()),
            }
        }
        Request::Calendar => {
            let now = shared.now.get();
            Content::Calendar {
                year: now.map(|n| n.year as i32).unwrap_or(2026),
                month: now.map(|n| n.month as u32).unwrap_or(1),
                week_start: week_start(),
                layout: fl::calendar(s),
            }
        }
    }
}

fn content_size(c: &Content) -> (f32, f32) {
    match c {
        Content::Picker { layout, .. } => layout.size,
        Content::More { layout, .. } | Content::Tray { layout, .. } => layout.size,
        Content::Quick { layout } => layout.size,
        Content::Calendar { layout, .. } => layout.size,
    }
}

/// First day of the week from the user's locale (0 = Sunday).
fn week_start() -> u32 {
    use explorer_win::windows::core::PCWSTR;
    use explorer_win::windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_IFIRSTDAYOFWEEK};
    let mut buf = [0u16; 8];
    // SAFETY: NULL locale = LOCALE_NAME_USER_DEFAULT; sized buffer.
    let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_IFIRSTDAYOFWEEK, Some(&mut buf)) };
    let v = if n > 0 {
        explorer_win::util::from_wide(&buf)
            .trim()
            .parse::<u32>()
            .unwrap_or(0)
    } else {
        0
    };
    calendar::week_start_from_locale(v)
}

fn place(st: &State) -> (i32, i32, i32, i32) {
    let s = st.scale();
    let h_anchor = match st.opener {
        Opener::System | Opener::Clock => HAnchor::Right,
        _ => HAnchor::Center(st.anchor.item.x + st.anchor.item.w / 2.0),
    };
    let margin = if st.sticky || !matches!(st.content, Content::Picker { .. }) {
        FLYOUT_MARGIN * s
    } else {
        6.0 * s
    };
    let (x, y) = fl::place(
        st.size,
        h_anchor,
        st.anchor.bar_rect,
        st.anchor.monitor,
        st.anchor.bottom,
        margin,
    );
    (
        x.round() as i32,
        y.round() as i32,
        st.size.0.round() as i32,
        st.size.1.round() as i32,
    )
}

/// Register DWM thumbnails for the picker's windows.
fn register_thumbs(shared: &Shared, hwnd: HWND) {
    let Ok(mut st) = shared.flyout.try_borrow_mut() else {
        return;
    };
    let Some(st) = st.as_mut() else { return };
    if let Content::Picker { items, layout, .. } = &mut st.content {
        for (item, card) in items.iter_mut().zip(layout.cards.iter()) {
            if item.thumb.is_none() {
                item.thumb = Thumbnail::register(hwnd, u64_to_hwnd(item.hwnd));
            }
            if let Some(t) = &item.thumb {
                match t.source_size().filter(|_| t.is_usable()) {
                    Some((w, h)) => t.show(Some(fl::fit(w, h, card.thumb))),
                    None => t.show(None),
                }
            }
        }
    }
}

fn show_window(shared: &Shared, activate: bool) {
    let hwnd = shared.flyout_hwnd.get();
    let geom = shared.flyout.borrow().as_ref().map(place);
    let Some((x, y, w, h)) = geom else { return };
    let flags = if activate {
        SWP_SHOWWINDOW
    } else {
        SWP_SHOWWINDOW | SWP_NOACTIVATE
    };
    // SAFETY: positioning our own window; nested messages are handled.
    let _ = unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, flags) };
    register_thumbs(shared, hwnd);
    if activate {
        explorer_win::activate::force_foreground(hwnd);
    }
    // SAFETY: repaint request on our own window.
    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
}

fn open(shared: &Rc<Shared>, opener: Opener, req: Request, anchor: Anchor, sticky: bool) {
    let hwnd = shared.flyout_hwnd.get();
    if hwnd.is_invalid() {
        return;
    }
    // Drop the previous content (and its thumbnails) first.
    let was_quick = shared
        .flyout
        .borrow_mut()
        .take()
        .is_some_and(|s| matches!(s.content, Content::Quick { .. }));
    let content = build(shared, req, &anchor);
    let is_quick = matches!(content, Content::Quick { .. });
    if was_quick && !is_quick {
        shared.sys_cmd(SysCmd::Fast(false));
    }
    if is_quick {
        shared.sys_cmd(SysCmd::Fast(true));
    }
    let size = content_size(&content);
    *shared.flyout.borrow_mut() = Some(State {
        content,
        opener,
        sticky,
        anchor,
        size,
        hover: Part::None,
        drag: None,
        opened_at: Instant::now(),
        left_at: None,
    });
    show_window(shared, sticky);
    // SAFETY: timer on the controller window.
    unsafe { SetTimer(Some(shared.controller.get()), TIMER_PEEK, 100, None) };
    shared.invalidate_bars();
}

fn make_sticky(shared: &Shared) {
    if let Some(st) = shared.flyout.borrow_mut().as_mut() {
        st.sticky = true;
        st.opened_at = Instant::now();
    }
    show_window(shared, true);
    shared.invalidate_bars();
}

/// Replace the content of the open flyout, keeping opener/anchor/mode.
fn rebuild(shared: &Shared, req: Request) {
    let prev = shared.flyout.borrow_mut().take();
    let Some(prev) = prev else { return };
    let content = build(shared, req, &prev.anchor);
    let size = content_size(&content);
    let st = State {
        content,
        size,
        hover: Part::None,
        drag: None,
        ..prev
    };
    *shared.flyout.borrow_mut() = Some(st);
    show_window(shared, false);
}

/// Quick settings rows depend on what the system reports (brightness,
/// battery); re-layout when that changed since opening.
fn relayout_quick_if_needed(shared: &Shared) {
    let needed = {
        let Ok(st) = shared.flyout.try_borrow() else {
            return;
        };
        let sys = shared.sys_status.borrow();
        match st.as_ref().map(|s| &s.content) {
            Some(Content::Quick { layout }) => {
                layout.brightness.is_some() != sys.brightness.is_some()
                    || layout.battery.is_some() != sys.battery.is_some()
            }
            _ => false,
        }
    };
    if needed {
        rebuild(shared, Request::Quick);
    }
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

struct FlyoutWin {
    shared: Rc<Shared>,
    surface: Option<Surface>,
    tracking: bool,
    /// Last level sent while dragging (avoid flooding the worker).
    last_sent: Option<u32>,
}

fn lparam_xy(lparam: LPARAM) -> (f32, f32) {
    (
        (lparam.0 & 0xffff) as i16 as f32,
        ((lparam.0 >> 16) & 0xffff) as i16 as f32,
    )
}

fn hit(st: &State, x: f32, y: f32) -> Part {
    match &st.content {
        Content::Picker { layout, .. } => {
            for (i, c) in layout.cards.iter().enumerate() {
                if c.close.contains(x, y) {
                    return Part::Close(i);
                }
                if c.card.contains(x, y) {
                    return Part::Card(i);
                }
            }
            Part::None
        }
        Content::More { layout, .. } | Content::Tray { layout, .. } => layout
            .rows
            .iter()
            .position(|r| r.contains(x, y))
            .map(Part::Row)
            .unwrap_or(Part::None),
        Content::Quick { layout } => {
            let track_hit = |row: &SliderRow| {
                let t = row.track;
                Rect::new(t.x - 8.0, t.y, t.w + 16.0, t.h).contains(x, y)
            };
            if layout.volume.icon.contains(x, y) {
                Part::VolIcon
            } else if track_hit(&layout.volume) {
                Part::VolTrack
            } else if layout.device.contains(x, y) {
                Part::Device
            } else if layout.brightness.as_ref().is_some_and(track_hit) {
                Part::BriTrack
            } else if layout.network.row.contains(x, y) || layout.network.link.contains(x, y) {
                Part::Network
            } else if layout
                .battery
                .as_ref()
                .is_some_and(|b| b.row.contains(x, y) || b.link.contains(x, y))
            {
                Part::Battery
            } else if layout.settings.contains(x, y) {
                Part::Settings
            } else {
                Part::None
            }
        }
        Content::Calendar { layout, .. } => {
            if layout.prev.contains(x, y) {
                Part::Prev
            } else if layout.next.contains(x, y) {
                Part::Next
            } else if layout.month.contains(x, y) || layout.date.contains(x, y) {
                Part::Month
            } else {
                Part::None
            }
        }
    }
}

/// Text colour on an accent-coloured background.
fn on_accent(pal: &Palette) -> u32 {
    if explorer_core::theme::luminance(pal.accent_text) > 0.5 {
        0x000000
    } else {
        0xFFFFFF
    }
}

fn draw_slider(p: &Painter<'_>, pal: &Palette, row: &SliderRow, s: f32, level: f32, enabled: bool) {
    let t = row.track;
    let cy = t.y + t.h / 2.0;
    let rail = Rect::new(t.x, cy - 2.0 * s, t.w, 4.0 * s);
    let rail_bg = mix(pal.panel_bg, pal.text, 0.35);
    p.fill_rounded(rail, 2.0 * s, Color(rail_bg));
    let fill_c = if enabled {
        pal.accent_text
    } else {
        pal.text_dim
    };
    let fx = t.x + t.w * level.clamp(0.0, 1.0);
    p.fill_rounded(
        Rect::new(t.x, rail.y, fx - t.x, rail.h),
        2.0 * s,
        Color(fill_c),
    );
    let knob = 18.0 * s;
    let kr = Rect::new(fx - knob / 2.0, cy - knob / 2.0, knob, knob);
    p.fill_rounded(kr, knob / 2.0, Color(mix(pal.panel_bg, pal.text, 0.15)));
    let inner = 10.0 * s;
    p.fill_rounded(
        Rect::new(fx - inner / 2.0, cy - inner / 2.0, inner, inner),
        inner / 2.0,
        Color(fill_c),
    );
}

impl FlyoutWin {
    fn paint(&mut self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: BeginPaint/EndPaint pairing in WM_PAINT.
        unsafe { BeginPaint(hwnd, &mut ps) };
        let shared = self.shared.clone();
        let pal = shared.palette.get();
        let lang = shared.lang;
        let surface = self.surface.get_or_insert_with(|| Surface::new(hwnd));
        if let Ok(st) = shared.flyout.try_borrow() {
            if let Some(st) = st.as_ref() {
                let s = st.scale();
                let sys = shared.sys_status.borrow().clone();
                let active = shared.model.borrow().active();
                surface.draw(&shared.gfx, Color(pal.panel_bg), |p| {
                    let (w, h) = st.size;
                    p.stroke_rounded(
                        Rect::new(0.5, 0.5, w - 1.0, h - 1.0),
                        8.0 * s,
                        1.0,
                        Color(pal.border),
                    );
                    let hover = st.hover;
                    match &st.content {
                        Content::Picker {
                            button,
                            items,
                            layout,
                        } => {
                            let fallback_icon = shared.button_icon(button);
                            for (i, (c, item)) in layout.cards.iter().zip(items.iter()).enumerate()
                            {
                                let hov = matches!(hover, Part::Card(j) | Part::Close(j) if j == i);
                                if hov {
                                    p.fill_rounded(c.card, 6.0 * s, Color(pal.selection));
                                }
                                if active == Some(item.hwnd) {
                                    p.stroke_rounded(
                                        c.card,
                                        6.0 * s,
                                        1.5 * s,
                                        Color(pal.accent_text),
                                    );
                                }
                                let icon = shared.window_icon(item.hwnd).or(fallback_icon.clone());
                                if let Some(img) = &icon {
                                    p.image(img, c.icon);
                                }
                                p.text(
                                    &item.title,
                                    c.title,
                                    12.0 * s,
                                    Color(pal.text),
                                    Align::Left,
                                    false,
                                );
                                if hov {
                                    if hover == Part::Close(i) {
                                        p.fill_rounded(c.close, 4.0 * s, Color(0xC42B1C));
                                        p.glyph(sg::CLOSE, c.close, 10.0 * s, Color(0xFFFFFF));
                                    } else {
                                        p.glyph(sg::CLOSE, c.close, 10.0 * s, Color(pal.text));
                                    }
                                }
                                if !item.usable() {
                                    // Minimized or no DWM content: icon + title.
                                    p.fill_rounded(c.thumb, 4.0 * s, Color(pal.input_bg));
                                    let sz = 48.0 * s;
                                    let r = Rect::new(
                                        c.thumb.x + (c.thumb.w - sz) / 2.0,
                                        c.thumb.y + (c.thumb.h - sz) / 2.0 - 8.0 * s,
                                        sz,
                                        sz,
                                    );
                                    if let Some(img) = &icon {
                                        p.image(img, r);
                                    }
                                    p.text(
                                        &item.title,
                                        Rect::new(
                                            c.thumb.x + 6.0 * s,
                                            r.bottom() + 2.0 * s,
                                            c.thumb.w - 12.0 * s,
                                            20.0 * s,
                                        ),
                                        11.0 * s,
                                        Color(pal.text_dim),
                                        Align::Center,
                                        false,
                                    );
                                }
                            }
                        }
                        Content::More { buttons, layout } => {
                            for (i, (r, b)) in layout.rows.iter().zip(buttons.iter()).enumerate() {
                                if hover == Part::Row(i) {
                                    p.fill_rounded(*r, 4.0 * s, Color(pal.selection));
                                }
                                if b.is_running() {
                                    let c = if b.active {
                                        pal.accent_text
                                    } else {
                                        pal.running_indicator
                                    };
                                    p.fill_rounded(
                                        Rect::new(
                                            r.x + 2.0 * s,
                                            r.y + r.h / 2.0 - 6.0 * s,
                                            3.0 * s,
                                            12.0 * s,
                                        ),
                                        1.5 * s,
                                        Color(c),
                                    );
                                }
                                let isz = 24.0 * s;
                                let ir =
                                    Rect::new(r.x + 10.0 * s, r.y + (r.h - isz) / 2.0, isz, isz);
                                if let Some(img) = shared.button_icon(b) {
                                    p.image(&img, ir);
                                }
                                let name = match b.windows.as_slice() {
                                    [one] => {
                                        shared.window_title(*one).unwrap_or_else(|| b.name.clone())
                                    }
                                    _ => b.name.clone(),
                                };
                                p.text(
                                    &name,
                                    Rect::new(
                                        ir.right() + 10.0 * s,
                                        r.y,
                                        r.right() - ir.right() - 16.0 * s,
                                        r.h,
                                    ),
                                    12.0 * s,
                                    Color(pal.text),
                                    Align::Left,
                                    false,
                                );
                            }
                        }
                        Content::Tray { icons, layout } => {
                            let tray = shared.tray.borrow();
                            let images = tray.as_ref().map(|t| t.state.borrow().images.clone());
                            for (i, (r, (k, _))) in layout.rows.iter().zip(icons.iter()).enumerate()
                            {
                                if hover == Part::Row(i) {
                                    p.fill_rounded(*r, 4.0 * s, Color(pal.selection));
                                }
                                if let Some(img) = images.as_ref().and_then(|m| m.get(k)) {
                                    let sz = 16.0 * s;
                                    p.image(
                                        img,
                                        Rect::new(
                                            r.x + (r.w - sz) / 2.0,
                                            r.y + (r.h - sz) / 2.0,
                                            sz,
                                            sz,
                                        ),
                                    );
                                }
                            }
                        }
                        Content::Quick { layout } => {
                            paint_quick(p, &pal, lang, s, layout, hover, &sys);
                        }
                        Content::Calendar {
                            year,
                            month,
                            week_start,
                            layout,
                        } => {
                            let today = shared.now.get();
                            let date = today
                                .map(|n| calendar::long_date(lang, &n))
                                .unwrap_or_default();
                            let mut first = date.chars();
                            let date: String = first
                                .next()
                                .map(|c| c.to_uppercase().chain(first).collect())
                                .unwrap_or_default();
                            p.text(
                                &date,
                                layout.date,
                                14.0 * s,
                                Color(pal.text),
                                Align::Left,
                                true,
                            );
                            if hover == Part::Month {
                                p.fill_rounded(layout.month, 4.0 * s, Color(pal.selection));
                            }
                            p.text(
                                &calendar::month_title(lang, *year, *month),
                                Rect::new(
                                    layout.month.x + 6.0 * s,
                                    layout.month.y,
                                    layout.month.w - 6.0 * s,
                                    layout.month.h,
                                ),
                                14.0 * s,
                                Color(pal.text),
                                Align::Left,
                                true,
                            );
                            for (part, r, g) in [
                                (Part::Prev, layout.prev, sg::CHEVRON_UP),
                                (Part::Next, layout.next, sg::CHEVRON_DOWN),
                            ] {
                                if hover == part {
                                    p.fill_rounded(r, 4.0 * s, Color(pal.selection));
                                }
                                p.glyph(g, r, 12.0 * s, Color(pal.text));
                            }
                            for (r, wd) in layout
                                .weekdays
                                .iter()
                                .zip(calendar::weekday_order(*week_start))
                            {
                                p.text(
                                    calendar::weekday_short(lang, wd),
                                    *r,
                                    12.0 * s,
                                    Color(pal.text_dim),
                                    Align::Center,
                                    false,
                                );
                            }
                            let grid = calendar::month_grid(*year, *month, *week_start);
                            for (r, d) in layout.days.iter().zip(grid.iter()) {
                                let is_today = today.is_some_and(|t| {
                                    t.year as i32 == d.year
                                        && t.month as u32 == d.month
                                        && t.day as u32 == d.day
                                });
                                let sz = r.w.min(r.h) - 6.0 * s;
                                let circle = Rect::new(
                                    r.x + (r.w - sz) / 2.0,
                                    r.y + (r.h - sz) / 2.0,
                                    sz,
                                    sz,
                                );
                                let color = if is_today {
                                    p.fill_rounded(circle, sz / 2.0, Color(pal.accent_text));
                                    on_accent(&pal)
                                } else if d.in_month {
                                    pal.text
                                } else {
                                    pal.text_dim
                                };
                                p.text(
                                    &d.day.to_string(),
                                    *r,
                                    13.0 * s,
                                    Color(color),
                                    Align::Center,
                                    is_today,
                                );
                            }
                        }
                    }
                });
            }
        }
        // SAFETY: matches BeginPaint above.
        let _ = unsafe { EndPaint(hwnd, &ps) };
    }

    fn set_hover(&mut self, hwnd: HWND, part: Part) {
        let changed = match self.shared.flyout.try_borrow_mut() {
            Ok(mut st) => match st.as_mut() {
                Some(st) if st.hover != part => {
                    st.hover = part;
                    true
                }
                _ => false,
            },
            Err(_) => false,
        };
        if changed {
            // SAFETY: repaint request on our own window.
            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
        }
    }

    fn part_at(&self, x: f32, y: f32) -> Part {
        match self.shared.flyout.try_borrow() {
            Ok(st) => st.as_ref().map(|s| hit(s, x, y)).unwrap_or(Part::None),
            Err(_) => Part::None,
        }
    }

    /// Slider value for a pointer at client x, per the open layout.
    fn slider_value(&self, which: Part, x: f32) -> Option<f32> {
        let st = self.shared.flyout.try_borrow().ok()?;
        let Content::Quick { layout } = &st.as_ref()?.content else {
            return None;
        };
        let track = match which {
            Part::VolTrack => layout.volume.track,
            Part::BriTrack => layout.brightness?.track,
            _ => return None,
        };
        Some(fl::slider_value(track, x))
    }

    fn apply_drag(&mut self, hwnd: HWND, drag: Drag) {
        if let Ok(mut st) = self.shared.flyout.try_borrow_mut() {
            if let Some(st) = st.as_mut() {
                st.drag = Some(drag);
            }
        }
        match drag {
            Drag::Volume(v) => {
                let pct = sg::percent(v);
                if let Some(a) = self.shared.sys_status.borrow_mut().audio.as_mut() {
                    a.level = v;
                }
                if self.last_sent != Some(pct) {
                    self.last_sent = Some(pct);
                    self.shared.sys_cmd(SysCmd::SetVolume(v));
                }
            }
            Drag::Brightness(v) => {
                let pct = sg::percent(v);
                self.shared.sys_status.borrow_mut().brightness = Some(pct as u8);
                if self.last_sent != Some(pct) {
                    self.last_sent = Some(pct);
                    self.shared.sys_cmd(SysCmd::SetBrightness(pct as u8));
                }
            }
        }
        self.shared.invalidate_bars();
        // SAFETY: repaint request on our own window.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }

    fn end_drag(&mut self) -> bool {
        self.last_sent = None;
        match self.shared.flyout.try_borrow_mut() {
            Ok(mut st) => st.as_mut().and_then(|s| s.drag.take()).is_some(),
            Err(_) => false,
        }
    }

    fn on_down(&mut self, hwnd: HWND, x: f32, y: f32) {
        let part = self.part_at(x, y);
        let drag = match part {
            Part::VolTrack if self.shared.sys_status.borrow().audio.is_some() => {
                self.slider_value(part, x).map(Drag::Volume)
            }
            Part::BriTrack => self.slider_value(part, x).map(Drag::Brightness),
            _ => None,
        };
        if let Some(d) = drag {
            // SAFETY: capture for our own window.
            unsafe { SetCapture(hwnd) };
            self.apply_drag(hwnd, d);
        }
    }

    fn on_click(&mut self, hwnd: HWND, x: f32, y: f32) {
        let part = self.part_at(x, y);
        let shared = self.shared.clone();
        enum Action {
            None,
            Activate(u64),
            CloseWindow(u64),
            Button(AppButton),
            Tray(IconKey),
            ToggleMute,
            Open(&'static str),
            Month(i32),
            Today,
        }
        let action = {
            let Ok(st) = shared.flyout.try_borrow() else {
                return;
            };
            let Some(st) = st.as_ref() else { return };
            match (&st.content, part) {
                (Content::Picker { items, .. }, Part::Card(i)) => items
                    .get(i)
                    .map(|it| Action::Activate(it.hwnd))
                    .unwrap_or(Action::None),
                (Content::Picker { items, .. }, Part::Close(i)) => items
                    .get(i)
                    .map(|it| Action::CloseWindow(it.hwnd))
                    .unwrap_or(Action::None),
                (Content::More { buttons, .. }, Part::Row(i)) => buttons
                    .get(i)
                    .cloned()
                    .map(Action::Button)
                    .unwrap_or(Action::None),
                (Content::Tray { icons, .. }, Part::Row(i)) => icons
                    .get(i)
                    .map(|t| Action::Tray(t.0))
                    .unwrap_or(Action::None),
                (Content::Quick { .. }, Part::VolIcon) => Action::ToggleMute,
                (Content::Quick { .. }, Part::Device) => Action::Open("ms-settings:sound"),
                (Content::Quick { .. }, Part::Network) => Action::Open("ms-settings:network"),
                (Content::Quick { .. }, Part::Battery) => Action::Open("ms-settings:powersleep"),
                (Content::Quick { .. }, Part::Settings) => Action::Open("ms-settings:"),
                (Content::Calendar { .. }, Part::Prev) => Action::Month(-1),
                (Content::Calendar { .. }, Part::Next) => Action::Month(1),
                (Content::Calendar { .. }, Part::Month) => Action::Today,
                _ => Action::None,
            }
        };
        match action {
            Action::None => {}
            Action::Activate(w) => {
                close(&shared);
                explorer_win::activate::activate(u64_to_hwnd(w));
            }
            Action::CloseWindow(w) => explorer_win::activate::close(u64_to_hwnd(w)),
            Action::Button(b) => {
                let active = shared.model.borrow().active();
                match groups::click(&b, active, |h| {
                    explorer_win::winlist::is_minimized(u64_to_hwnd(h))
                }) {
                    GroupClick::Launch => {
                        close(&shared);
                        shared.launch_new(&b);
                    }
                    GroupClick::Activate(w) => {
                        close(&shared);
                        explorer_win::activate::activate(u64_to_hwnd(w));
                    }
                    GroupClick::Minimize(w) => {
                        close(&shared);
                        explorer_win::activate::minimize(u64_to_hwnd(w));
                    }
                    GroupClick::Picker => {
                        // Same anchor ("…"), now showing that app's windows.
                        if let Some(st) = shared.flyout.borrow_mut().as_mut() {
                            st.opener = Opener::Button(b.id.clone());
                        }
                        rebuild(&shared, Request::Picker { button: b });
                    }
                }
            }
            Action::Tray(k) => forward_tray_click(&shared, k, TrayButton::Left),
            Action::ToggleMute => {
                let muted = {
                    let mut sys = shared.sys_status.borrow_mut();
                    match sys.audio.as_mut() {
                        Some(a) => {
                            a.muted = !a.muted;
                            Some(a.muted)
                        }
                        None => None,
                    }
                };
                if let Some(m) = muted {
                    shared.sys_cmd(SysCmd::SetMute(m));
                    shared.invalidate_bars();
                }
            }
            Action::Open(uri) => {
                close(&shared);
                crate::workers::open_uri(uri);
            }
            Action::Month(d) => self.shift_month(d),
            Action::Today => {
                let now = shared.now.get();
                if let (Some(n), Ok(mut st)) = (now, shared.flyout.try_borrow_mut()) {
                    if let Some(Content::Calendar { year, month, .. }) =
                        st.as_mut().map(|s| &mut s.content)
                    {
                        *year = n.year as i32;
                        *month = n.month as u32;
                    }
                }
            }
        }
        // SAFETY: repaint request on our own window.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }

    fn shift_month(&mut self, delta: i32) {
        if let Ok(mut st) = self.shared.flyout.try_borrow_mut() {
            if let Some(Content::Calendar { year, month, .. }) = st.as_mut().map(|s| &mut s.content)
            {
                let (y, m) = calendar::add_months(*year, *month, delta);
                *year = y;
                *month = m;
            }
        }
    }

    fn on_wheel(&mut self, hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
        let notches = ((wparam.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
        let mut pt = POINT {
            x: (lparam.0 & 0xffff) as i16 as i32,
            y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
        };
        // SAFETY: valid in/out pointer.
        let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
        let (x, y) = (pt.x as f32, pt.y as f32);
        enum Target {
            None,
            Volume,
            Brightness,
            Month,
        }
        let target = {
            let Ok(st) = self.shared.flyout.try_borrow() else {
                return;
            };
            match st.as_ref().map(|s| &s.content) {
                Some(Content::Quick { layout }) => {
                    let row = |r: &SliderRow| {
                        Rect::new(0.0, r.track.y - 6.0, f32::MAX, r.track.h + 12.0).contains(x, y)
                    };
                    if layout.brightness.as_ref().is_some_and(row) {
                        Target::Brightness
                    } else {
                        Target::Volume
                    }
                }
                Some(Content::Calendar { .. }) => Target::Month,
                _ => Target::None,
            }
        };
        match target {
            Target::None => {}
            Target::Volume => {
                let level = {
                    let mut sys = self.shared.sys_status.borrow_mut();
                    sys.audio.as_mut().map(|a| {
                        a.level = fl::step_level(a.level, 0.02 * notches);
                        a.level
                    })
                };
                if let Some(l) = level {
                    self.shared.sys_cmd(SysCmd::SetVolume(l));
                    self.shared.invalidate_bars();
                }
            }
            Target::Brightness => {
                let level = {
                    let mut sys = self.shared.sys_status.borrow_mut();
                    sys.brightness.map(|b| {
                        let v = sg::percent(fl::step_level(b as f32 / 100.0, 0.02 * notches)) as u8;
                        sys.brightness = Some(v);
                        v
                    })
                };
                if let Some(v) = level {
                    self.shared.sys_cmd(SysCmd::SetBrightness(v));
                }
            }
            Target::Month => self.shift_month(if notches > 0.0 { -1 } else { 1 }),
        }
        // SAFETY: repaint request on our own window.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }

    fn sticky(&self) -> bool {
        is_sticky_open(&self.shared)
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_quick(
    p: &Painter<'_>,
    pal: &Palette,
    lang: explorer_core::i18n::Lang,
    s: f32,
    layout: &QuickSettingsLayout,
    hover: Part,
    sys: &crate::workers::SysStatus,
) {
    let w = layout.size.0;
    // Volume.
    let (level, muted, device) = match &sys.audio {
        Some(a) => (a.level, a.muted, a.device.clone()),
        None => (
            0.0,
            true,
            tr(
                lang,
                "No audio output device",
                "No hay dispositivo de salida de audio",
            )
            .into(),
        ),
    };
    let v = &layout.volume;
    if hover == Part::VolIcon {
        p.fill_rounded(v.icon, 4.0 * s, Color(pal.selection));
    }
    p.glyph(
        sg::volume_glyph(level, muted),
        v.icon,
        16.0 * s,
        Color(pal.text),
    );
    draw_slider(p, pal, v, s, level, sys.audio.is_some() && !muted);
    p.text(
        &format!("{}", sg::percent(level)),
        v.value,
        13.0 * s,
        Color(pal.text),
        Align::Right,
        false,
    );
    if hover == Part::Device {
        p.fill_rounded(layout.device, 4.0 * s, Color(pal.selection));
    }
    p.text(
        &device,
        Rect::new(
            layout.device.x + 4.0 * s,
            layout.device.y,
            layout.device.w - 24.0 * s,
            layout.device.h,
        ),
        11.0 * s,
        Color(pal.text_dim),
        Align::Left,
        false,
    );
    p.glyph(
        sg::CHEVRON_RIGHT,
        Rect::new(
            layout.device.right() - 20.0 * s,
            layout.device.y,
            20.0 * s,
            layout.device.h,
        ),
        9.0 * s,
        Color(pal.text_dim),
    );
    // Brightness.
    if let (Some(b), Some(level)) = (&layout.brightness, sys.brightness) {
        p.glyph(sg::BRIGHTNESS, b.icon, 16.0 * s, Color(pal.text));
        let l = level as f32 / 100.0;
        draw_slider(p, pal, b, s, l, true);
        p.text(
            &format!("{level}"),
            b.value,
            13.0 * s,
            Color(pal.text),
            Align::Right,
            false,
        );
    }
    // Separator above the status rows.
    p.fill(
        Rect::new(16.0 * s, layout.network.row.y - 8.0 * s, w - 32.0 * s, 1.0),
        Color(pal.border),
    );
    // Network.
    let k = net_kind(sys.network.as_ref());
    let n = &layout.network;
    if hover == Part::Network {
        p.fill_rounded(
            Rect::new(
                n.row.x - 6.0 * s,
                n.row.y,
                n.row.w + 12.0 * s,
                n.link.bottom() - n.row.y,
            ),
            6.0 * s,
            Color(pal.selection),
        );
    }
    p.glyph(sg::network_glyph(k), n.icon, 18.0 * s, Color(pal.text));
    let names = sys
        .network
        .as_ref()
        .map(|n| n.names.join(", "))
        .filter(|s| !s.is_empty() && k != sg::NetKind::Disconnected)
        .unwrap_or_else(|| tr(lang, "Network", "Red").into());
    p.text(
        &names,
        n.title,
        13.0 * s,
        Color(pal.text),
        Align::Left,
        true,
    );
    p.text(
        sg::network_status(lang, k),
        n.detail,
        12.0 * s,
        Color(pal.text_dim),
        Align::Left,
        false,
    );
    p.text(
        tr(
            lang,
            "Network & internet settings",
            "Configuración de red e Internet",
        ),
        n.link,
        12.0 * s,
        Color(pal.accent_text),
        Align::Left,
        false,
    );
    // Battery.
    if let (Some(b), Some(bat)) = (&layout.battery, sys.battery) {
        if hover == Part::Battery {
            p.fill_rounded(
                Rect::new(
                    b.row.x - 6.0 * s,
                    b.row.y,
                    b.row.w + 12.0 * s,
                    b.link.bottom() - b.row.y,
                ),
                6.0 * s,
                Color(pal.selection),
            );
        }
        p.glyph(
            sg::battery_glyph(bat.percent, bat.charging),
            b.icon,
            18.0 * s,
            Color(pal.text),
        );
        let pct = bat
            .percent
            .map(|p| format!("{p}%"))
            .unwrap_or_else(|| "?".into());
        p.text(&pct, b.title, 13.0 * s, Color(pal.text), Align::Left, true);
        let state = match (bat.charging, bat.on_ac) {
            (true, _) => tr(lang, "Charging", "Cargando"),
            (false, true) => tr(lang, "Plugged in", "Conectado"),
            (false, false) => tr(lang, "On battery", "Con batería"),
        };
        p.text(
            state,
            b.detail,
            12.0 * s,
            Color(pal.text_dim),
            Align::Left,
            false,
        );
        p.text(
            tr(
                lang,
                "Power & battery settings",
                "Configuración de energía y batería",
            ),
            b.link,
            12.0 * s,
            Color(pal.accent_text),
            Align::Left,
            false,
        );
    }
    // Footer.
    let footer_bg = mix(
        pal.panel_bg,
        if pal.light { 0xFFFFFF } else { 0x000000 },
        0.12,
    );
    p.fill(layout.footer, Color(footer_bg));
    p.fill(Rect::new(0.0, layout.footer.y, w, 1.0), Color(pal.border));
    let label = trf(
        lang,
        "Quick settings".to_string(),
        "Configuración rápida".to_string(),
    );
    p.text(
        &label,
        Rect::new(16.0 * s, layout.footer.y, w / 2.0, layout.footer.h),
        12.0 * s,
        Color(pal.text_dim),
        Align::Left,
        false,
    );
    if hover == Part::Settings {
        p.fill_rounded(layout.settings, 4.0 * s, Color(pal.selection));
    }
    p.glyph(sg::SETTINGS, layout.settings, 16.0 * s, Color(pal.text));
}

impl Handler for FlyoutWin {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
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
            WM_DPICHANGED => {
                // We size and place ourselves for the bar's monitor; ignore
                // the suggested rectangle.
                Some(LRESULT(0))
            }
            WM_MOUSEACTIVATE => {
                if self.sticky() {
                    None
                } else {
                    // Hover thumbnails never take activation.
                    Some(LRESULT(MA_NOACTIVATE as isize))
                }
            }
            WM_ACTIVATE => {
                // Lost activation (click elsewhere): close a sticky flyout.
                // SAFETY: plain query on our own window.
                let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
                if wparam.0 & 0xffff == 0 && visible && self.sticky() {
                    close(&self.shared);
                }
                None
            }
            WM_KEYDOWN => {
                let vk = wparam.0 as u16;
                if vk == VK_ESCAPE.0 {
                    close(&self.shared);
                } else if vk == VK_PRIOR.0 || vk == VK_LEFT.0 {
                    self.shift_month(-1);
                    // SAFETY: repaint request on our own window.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                } else if vk == VK_NEXT.0 || vk == VK_RIGHT.0 {
                    self.shift_month(1);
                    // SAFETY: as above.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
                Some(LRESULT(0))
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
                let (x, y) = lparam_xy(lparam);
                let drag = self
                    .shared
                    .flyout
                    .try_borrow()
                    .ok()
                    .and_then(|s| s.as_ref().and_then(|s| s.drag));
                match drag {
                    Some(Drag::Volume(_)) => {
                        if let Some(v) = self.slider_value(Part::VolTrack, x) {
                            self.apply_drag(hwnd, Drag::Volume(v));
                        }
                    }
                    Some(Drag::Brightness(_)) => {
                        if let Some(v) = self.slider_value(Part::BriTrack, x) {
                            self.apply_drag(hwnd, Drag::Brightness(v));
                        }
                    }
                    None => {
                        let part = self.part_at(x, y);
                        self.set_hover(hwnd, part);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.set_hover(hwnd, Part::None);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let (x, y) = lparam_xy(lparam);
                self.on_down(hwnd, x, y);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lparam);
                if self.end_drag() {
                    // SAFETY: releasing our own capture.
                    let _ = unsafe { ReleaseCapture() };
                    self.shared.invalidate_bars();
                } else {
                    self.on_click(hwnd, x, y);
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.end_drag();
                Some(LRESULT(0))
            }
            WM_LBUTTONDBLCLK => {
                let (x, y) = lparam_xy(lparam);
                let key = match self.part_at(x, y) {
                    Part::Row(i) => self.shared.flyout.try_borrow().ok().and_then(|st| {
                        match st.as_ref().map(|s| &s.content) {
                            Some(Content::Tray { icons, .. }) => icons.get(i).map(|t| t.0),
                            _ => None,
                        }
                    }),
                    _ => None,
                };
                if let Some(k) = key {
                    forward_tray_click(&self.shared, k, TrayButton::LeftDouble);
                } else {
                    self.on_click(hwnd, x, y);
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                let (x, y) = lparam_xy(lparam);
                let key = match self.part_at(x, y) {
                    Part::Row(i) => self.shared.flyout.try_borrow().ok().and_then(|st| {
                        match st.as_ref().map(|s| &s.content) {
                            Some(Content::Tray { icons, .. }) => icons.get(i).map(|t| t.0),
                            _ => None,
                        }
                    }),
                    _ => None,
                };
                if let Some(k) = key {
                    // The app shows its menu and takes the foreground; we
                    // close first so the menu is not covered.
                    close(&self.shared);
                    forward_tray_click(&self.shared, k, TrayButton::Right);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.on_wheel(hwnd, wparam, lparam);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}
