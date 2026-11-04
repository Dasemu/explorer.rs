// SPDX-License-Identifier: GPL-3.0-or-later
//! Notification-area (tray) protocol: payload parsing, icon model, and the
//! callback messages sent back to applications. Pure and unit-tested; the
//! Win32 side lives in `explorer-win::tray`.
//!
//! `Shell_NotifyIconW` finds the `Shell_TrayWnd` window and sends it
//! `WM_COPYDATA` with `dwData == 1`. The payload is not documented. Its
//! layout, as publicly described and observed, is:
//!
//! ```text
//! u32 signature      (not validated, only logged)
//! u32 message        NIM_ADD / NIM_MODIFY / NIM_DELETE / NIM_SETFOCUS / NIM_SETVERSION
//! NOTIFYICONDATAW with every handle stored as 32 bits (so the same layout
//! works for 32- and 64-bit callers):
//!   +0   u32 cbSize
//!   +4   u32 hWnd
//!   +8   u32 uID
//!   +12  u32 uFlags
//!   +16  u32 uCallbackMessage
//!   +20  u32 hIcon
//!   +24  u16 szTip[64]   (V1, cbSize < 936) | u16 szTip[128] (V2+)
//!   V2+: +280 u32 dwState, +284 u32 dwStateMask, +288 u16 szInfo[256],
//!        +800 u32 uTimeout/uVersion, +804 u16 szInfoTitle[64],
//!        +932 u32 dwInfoFlags, V3+: +936 GUID guidItem, V4: +952 u32 hBalloonIcon
//! ```
//!
//! UNVERIFIED on Windows: confirm with a real capture before relying on the
//! V2+ offsets. Parsing never panics on short or malformed input.

pub const NIM_ADD: u32 = 0;
pub const NIM_MODIFY: u32 = 1;
pub const NIM_DELETE: u32 = 2;
pub const NIM_SETFOCUS: u32 = 3;
pub const NIM_SETVERSION: u32 = 4;

pub const NIF_MESSAGE: u32 = 0x01;
pub const NIF_ICON: u32 = 0x02;
pub const NIF_TIP: u32 = 0x04;
pub const NIF_STATE: u32 = 0x08;
pub const NIF_GUID: u32 = 0x20;

pub const NIS_HIDDEN: u32 = 0x01;

pub const NOTIFYICON_VERSION_4: u32 = 4;

/// `WM_COPYDATA` `dwData` values sent to `Shell_TrayWnd`.
pub const COPYDATA_APPBAR: usize = 0;
pub const COPYDATA_NOTIFYICON: usize = 1;

const HEADER: usize = 8;
const V1_SIZE: usize = 152;
const V2_SIZE: usize = 936;
const V3_SIZE: usize = 952;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyIconData {
    pub hwnd: u32,
    pub uid: u32,
    pub flags: u32,
    pub callback_message: u32,
    pub hicon: u32,
    pub tip: String,
    pub state: u32,
    pub state_mask: u32,
    /// `uTimeout` / `uVersion` union.
    pub version: u32,
    pub guid: Option<[u8; 16]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayCommand {
    pub signature: u32,
    pub message: u32,
    pub data: NotifyIconData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    TooShort(usize),
    BadSize(u32),
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .unwrap_or(0)
}

fn wstr_at(b: &[u8], off: usize, chars: usize) -> String {
    let Some(s) = b.get(off..(off + chars * 2).min(b.len())) else {
        return String::new();
    };
    let units: Vec<u16> = s
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Parse a `WM_COPYDATA` payload with `dwData == 1`.
pub fn parse(payload: &[u8]) -> Result<TrayCommand, ParseError> {
    if payload.len() < HEADER + V1_SIZE {
        return Err(ParseError::TooShort(payload.len()));
    }
    let signature = u32_at(payload, 0);
    let message = u32_at(payload, 4);
    let n = &payload[HEADER..];
    let cb = u32_at(n, 0);
    if (cb as usize) < V1_SIZE || cb > 4096 {
        return Err(ParseError::BadSize(cb));
    }
    // Trust the smaller of the declared and actual sizes.
    let size = (cb as usize).min(n.len());
    let n = &n[..size];
    let v2 = size >= V2_SIZE;
    let tip_chars = if v2 { 128 } else { 64 };
    let flags = u32_at(n, 12);
    let guid = (size >= V3_SIZE && flags & NIF_GUID != 0).then(|| {
        let mut g = [0u8; 16];
        g.copy_from_slice(&n[936..952]);
        g
    });
    Ok(TrayCommand {
        signature,
        message,
        data: NotifyIconData {
            hwnd: u32_at(n, 4),
            uid: u32_at(n, 8),
            flags,
            callback_message: u32_at(n, 16),
            hicon: u32_at(n, 20),
            tip: wstr_at(n, 24, tip_chars),
            state: if v2 { u32_at(n, 280) } else { 0 },
            state_mask: if v2 { u32_at(n, 284) } else { 0 },
            version: if v2 { u32_at(n, 800) } else { 0 },
            guid,
        },
    })
}

/// Identity of an icon: GUID if the app supplied one, else (hwnd, uid).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconKey {
    Guid([u8; 16]),
    Window(u32, u32),
}

impl NotifyIconData {
    pub fn key(&self) -> IconKey {
        match self.guid {
            Some(g) => IconKey::Guid(g),
            None => IconKey::Window(self.hwnd, self.uid),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayIcon {
    pub key: IconKey,
    pub hwnd: u32,
    pub uid: u32,
    pub callback_message: u32,
    pub hicon: u32,
    pub tip: String,
    pub hidden: bool,
    pub version: u32,
    /// Incremented when the icon image changes (cache invalidation).
    pub icon_generation: u32,
}

/// Effect of applying a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// Return value for the sender (TRUE/FALSE).
    pub ok: bool,
    /// Visible state changed; repaint.
    pub changed: bool,
    /// The icon image must be (re)loaded.
    pub icon_changed: bool,
}

#[derive(Debug, Default)]
pub struct TrayModel {
    icons: Vec<TrayIcon>,
}

impl TrayModel {
    pub fn icons(&self) -> &[TrayIcon] {
        &self.icons
    }

    pub fn visible(&self) -> impl Iterator<Item = &TrayIcon> {
        self.icons.iter().filter(|i| !i.hidden)
    }

    fn find(&self, key: IconKey) -> Option<usize> {
        self.icons.iter().position(|i| i.key == key)
    }

    pub fn get(&self, key: IconKey) -> Option<&TrayIcon> {
        self.find(key).map(|i| &self.icons[i])
    }

    fn update(icon: &mut TrayIcon, d: &NotifyIconData) -> (bool, bool) {
        let mut changed = false;
        let mut icon_changed = false;
        if d.flags & NIF_MESSAGE != 0 {
            icon.callback_message = d.callback_message;
        }
        if d.flags & NIF_ICON != 0 && icon.hicon != d.hicon {
            icon.hicon = d.hicon;
            icon.icon_generation = icon.icon_generation.wrapping_add(1);
            changed = true;
            icon_changed = true;
        } else if d.flags & NIF_ICON != 0 {
            // Same handle value, but apps reuse handles for new images.
            icon.icon_generation = icon.icon_generation.wrapping_add(1);
            icon_changed = true;
            changed = true;
        }
        if d.flags & NIF_TIP != 0 && icon.tip != d.tip {
            icon.tip = d.tip.clone();
        }
        if d.flags & NIF_STATE != 0 && d.state_mask & NIS_HIDDEN != 0 {
            let hidden = d.state & NIS_HIDDEN != 0;
            if hidden != icon.hidden {
                icon.hidden = hidden;
                changed = true;
            }
        }
        // Keep the window handle current (it can change with NIF_GUID).
        icon.hwnd = d.hwnd;
        icon.uid = d.uid;
        (changed, icon_changed)
    }

    pub fn apply(&mut self, cmd: &TrayCommand) -> Applied {
        let d = &cmd.data;
        let key = d.key();
        let none = Applied {
            ok: false,
            changed: false,
            icon_changed: false,
        };
        match cmd.message {
            NIM_ADD => {
                if self.find(key).is_some() {
                    return none;
                }
                let mut icon = TrayIcon {
                    key,
                    hwnd: d.hwnd,
                    uid: d.uid,
                    callback_message: 0,
                    hicon: 0,
                    tip: String::new(),
                    hidden: false,
                    version: 0,
                    icon_generation: 0,
                };
                let (_, icon_changed) = Self::update(&mut icon, d);
                let visible = !icon.hidden;
                self.icons.push(icon);
                Applied {
                    ok: true,
                    changed: visible,
                    icon_changed,
                }
            }
            NIM_MODIFY => match self.find(key) {
                Some(i) => {
                    let (changed, icon_changed) = Self::update(&mut self.icons[i], d);
                    Applied {
                        ok: true,
                        changed,
                        icon_changed,
                    }
                }
                None => none,
            },
            NIM_DELETE => match self.find(key) {
                Some(i) => {
                    let was_visible = !self.icons[i].hidden;
                    self.icons.remove(i);
                    Applied {
                        ok: true,
                        changed: was_visible,
                        icon_changed: false,
                    }
                }
                None => none,
            },
            NIM_SETVERSION => match self.find(key) {
                Some(i) => {
                    self.icons[i].version = d.version;
                    Applied {
                        ok: true,
                        changed: false,
                        icon_changed: false,
                    }
                }
                None => none,
            },
            NIM_SETFOCUS => Applied {
                ok: self.find(key).is_some(),
                changed: false,
                icon_changed: false,
            },
            _ => none,
        }
    }

    /// Drop icons whose owner window no longer exists (apps that crashed
    /// without NIM_DELETE). Returns true if anything was removed.
    pub fn retain_alive(&mut self, alive: impl Fn(u32) -> bool) -> bool {
        let before = self.icons.len();
        self.icons.retain(|i| alive(i.hwnd));
        before != self.icons.len()
    }
}

/// Mouse buttons for [`click_messages`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayButton {
    Left,
    Right,
    LeftDouble,
}

pub const WM_CONTEXTMENU: u32 = 0x007B;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
/// `NIN_SELECT` (WM_USER + 0).
pub const NIN_SELECT: u32 = 0x0400;

/// (wParam, lParam) pairs to post with the icon's callback message for a
/// click, following the documented `NOTIFYICON_VERSION_4` rules (lParam =
/// event | uID << 16, wParam = x | y << 16) or the legacy ones (wParam =
/// uID, lParam = event).
pub fn click_messages(icon: &TrayIcon, button: TrayButton, x: i16, y: i16) -> Vec<(usize, isize)> {
    let events: &[u32] = match (button, icon.version >= NOTIFYICON_VERSION_4) {
        (TrayButton::Left, true) => &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
        (TrayButton::Right, true) => &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
        (TrayButton::LeftDouble, true) => &[WM_LBUTTONDBLCLK, WM_LBUTTONUP],
        (TrayButton::Left, false) => &[WM_LBUTTONDOWN, WM_LBUTTONUP],
        (TrayButton::Right, false) => &[WM_RBUTTONDOWN, WM_RBUTTONUP],
        (TrayButton::LeftDouble, false) => &[WM_LBUTTONDBLCLK, WM_LBUTTONUP],
    };
    events
        .iter()
        .map(|&e| {
            if icon.version >= NOTIFYICON_VERSION_4 {
                let wparam = (x as u16 as usize) | ((y as u16 as usize) << 16);
                let lparam = (e as isize & 0xffff) | (((icon.uid & 0xffff) as isize) << 16);
                (wparam, lparam)
            } else {
                (icon.uid as usize, e as isize)
            }
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build a payload as Shell_NotifyIconW would (V4 size).
    pub fn payload(message: u32, d: &NotifyIconData, size: usize) -> Vec<u8> {
        let mut b = vec![0u8; HEADER + size];
        b[0..4].copy_from_slice(&0x3475_3423u32.to_le_bytes());
        b[4..8].copy_from_slice(&message.to_le_bytes());
        let n = &mut b[HEADER..];
        let put =
            |n: &mut [u8], off: usize, v: u32| n[off..off + 4].copy_from_slice(&v.to_le_bytes());
        put(n, 0, size as u32);
        put(n, 4, d.hwnd);
        put(n, 8, d.uid);
        put(n, 12, d.flags);
        put(n, 16, d.callback_message);
        put(n, 20, d.hicon);
        let max = if size >= V2_SIZE { 127 } else { 63 };
        for (i, u) in d.tip.encode_utf16().take(max).enumerate() {
            n[24 + i * 2..26 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        if size >= V2_SIZE {
            put(n, 280, d.state);
            put(n, 284, d.state_mask);
            put(n, 800, d.version);
        }
        if let (Some(g), true) = (d.guid, size >= V3_SIZE) {
            n[936..952].copy_from_slice(&g);
        }
        b
    }

    fn nid(hwnd: u32, uid: u32, flags: u32) -> NotifyIconData {
        NotifyIconData {
            hwnd,
            uid,
            flags,
            callback_message: 0x8001,
            hicon: 0x1234,
            tip: "Volume".into(),
            state: 0,
            state_mask: 0,
            version: 0,
            guid: None,
        }
    }

    #[test]
    fn parse_v4_roundtrip() {
        let mut d = nid(0xABCD, 7, NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_GUID);
        d.guid = Some([9; 16]);
        d.version = 4;
        let c = parse(&payload(NIM_ADD, &d, 956)).unwrap();
        assert_eq!(c.message, NIM_ADD);
        assert_eq!(c.data, d);
        assert_eq!(c.data.key(), IconKey::Guid([9; 16]));
    }

    #[test]
    fn parse_v1() {
        let d = nid(1, 2, NIF_TIP);
        let c = parse(&payload(NIM_MODIFY, &d, V1_SIZE)).unwrap();
        assert_eq!(c.data.tip, "Volume");
        assert_eq!(c.data.version, 0);
        assert_eq!(c.data.guid, None);
    }

    #[test]
    fn parse_rejects_garbage_without_panicking() {
        assert!(parse(&[]).is_err());
        assert!(parse(&[0u8; 50]).is_err());
        let mut b = payload(NIM_ADD, &nid(1, 1, 0), 956);
        b[HEADER..HEADER + 4].copy_from_slice(&5u32.to_le_bytes());
        assert_eq!(parse(&b), Err(ParseError::BadSize(5)));
        // Declared size larger than the buffer: parsed from what exists.
        let mut b = payload(NIM_ADD, &nid(1, 1, NIF_TIP), V2_SIZE);
        b.truncate(HEADER + 300);
        b[HEADER..HEADER + 4].copy_from_slice(&956u32.to_le_bytes());
        assert!(parse(&b).is_ok());
        // Fuzz-ish: every truncation of a valid payload must not panic.
        let full = payload(NIM_ADD, &nid(1, 1, 0xff), 956);
        for len in 0..full.len() {
            let _ = parse(&full[..len]);
        }
    }

    fn cmd(message: u32, d: NotifyIconData) -> TrayCommand {
        TrayCommand {
            signature: 0,
            message,
            data: d,
        }
    }

    #[test]
    fn model_lifecycle() {
        let mut m = TrayModel::default();
        let d = nid(10, 1, NIF_MESSAGE | NIF_ICON | NIF_TIP);
        let a = m.apply(&cmd(NIM_ADD, d.clone()));
        assert!(a.ok && a.changed && a.icon_changed);
        assert!(!m.apply(&cmd(NIM_ADD, d.clone())).ok, "duplicate add fails");
        let mut hide = nid(10, 1, NIF_STATE);
        hide.state = NIS_HIDDEN;
        hide.state_mask = NIS_HIDDEN;
        assert!(m.apply(&cmd(NIM_MODIFY, hide)).changed);
        assert_eq!(m.visible().count(), 0);
        let mut ver = nid(10, 1, 0);
        ver.version = 4;
        assert!(m.apply(&cmd(NIM_SETVERSION, ver)).ok);
        assert_eq!(m.icons()[0].version, 4);
        assert!(m.apply(&cmd(NIM_DELETE, nid(10, 1, 0))).ok);
        assert!(!m.apply(&cmd(NIM_DELETE, nid(10, 1, 0))).ok);
        assert!(!m.apply(&cmd(NIM_MODIFY, nid(10, 1, NIF_TIP))).ok);
    }

    #[test]
    fn retain_alive_drops_dead_owners() {
        let mut m = TrayModel::default();
        m.apply(&cmd(NIM_ADD, nid(10, 1, NIF_ICON)));
        m.apply(&cmd(NIM_ADD, nid(11, 1, NIF_ICON)));
        assert!(m.retain_alive(|h| h == 11));
        assert_eq!(m.icons().len(), 1);
    }

    #[test]
    fn click_messages_v4_and_legacy() {
        let mut m = TrayModel::default();
        m.apply(&cmd(NIM_ADD, nid(10, 3, NIF_ICON)));
        let mut icon = m.icons()[0].clone();
        let legacy = click_messages(&icon, TrayButton::Left, 100, 200);
        assert_eq!(
            legacy,
            vec![(3, WM_LBUTTONDOWN as isize), (3, WM_LBUTTONUP as isize)]
        );
        icon.version = 4;
        let v4 = click_messages(&icon, TrayButton::Right, 100, 200);
        assert_eq!(v4.len(), 3);
        assert_eq!(v4[2].1, (WM_CONTEXTMENU as isize) | (3 << 16));
        assert_eq!(v4[0].0, 100 | (200 << 16));
    }
}
