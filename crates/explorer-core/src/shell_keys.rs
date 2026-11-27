// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard shortcuts that Windows Explorer implements, provided by the
//! taskbar in shell mode.
//!
//! Win+Shift+S, Print Screen, Win+Print Screen, Win+E, Win+I, ... are not
//! handled by Windows itself but by explorer.exe. With explorer.rs as the
//! shell they silently did nothing. Alongside Explorer (dev mode) Explorer
//! still handles them and none of this is registered.

use crate::i18n::Lang;
use crate::ipc::RunMode;

/// `RegisterHotKey` modifier bits (WinUser.h).
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;
/// Do not repeat while the keys are held.
pub const MOD_NOREPEAT: u32 = 0x4000;

/// Virtual-key codes used below.
pub const VK_SNAPSHOT: u32 = 0x2C;

/// What a shell shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAction {
    /// Snipping Tool's screen clip overlay (`ms-screenclip:`).
    ScreenClip,
    /// Whole virtual screen to Pictures\Screenshots and the clipboard.
    ScreenshotToFile,
    /// A File Explorer window.
    FileExplorer,
    /// Win+R: our Start menu, whose search box runs commands.
    Run,
    /// The Settings app.
    Settings,
    /// Toggle "show desktop".
    ShowDesktop,
    /// Quick settings flyout.
    QuickSettings,
    /// Win+N: the calendar flyout (Windows 11 shows notifications and the
    /// calendar there; we have only the calendar).
    Notifications,
}

/// One shortcut for `RegisterHotKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellHotkey {
    /// Hotkey id (`WM_HOTKEY` wParam); unique, below 0xC000.
    pub id: i32,
    /// `MOD_*` bits, without [`MOD_NOREPEAT`].
    pub modifiers: u32,
    pub vk: u32,
    pub action: ShellAction,
    /// For logs.
    pub name: &'static str,
}

const fn key(
    id: i32,
    modifiers: u32,
    vk: u32,
    action: ShellAction,
    name: &'static str,
) -> ShellHotkey {
    ShellHotkey {
        id,
        modifiers,
        vk,
        action,
        name,
    }
}

/// Id of the lone Print Screen hotkey, which comes and goes with Windows'
/// "Use the Print screen key to open screen capture" setting.
pub const PRINT_SCREEN_ID: i32 = 0x5301;

const WIN_KEYS: &[ShellHotkey] = &[
    key(
        0x5300,
        MOD_WIN | MOD_SHIFT,
        b'S' as u32,
        ShellAction::ScreenClip,
        "Win+Shift+S",
    ),
    key(
        0x5302,
        MOD_WIN,
        VK_SNAPSHOT,
        ShellAction::ScreenshotToFile,
        "Win+PrtScn",
    ),
    key(
        0x5303,
        MOD_WIN,
        b'E' as u32,
        ShellAction::FileExplorer,
        "Win+E",
    ),
    key(0x5304, MOD_WIN, b'R' as u32, ShellAction::Run, "Win+R"),
    key(0x5305, MOD_WIN, b'I' as u32, ShellAction::Settings, "Win+I"),
    key(
        0x5306,
        MOD_WIN,
        b'D' as u32,
        ShellAction::ShowDesktop,
        "Win+D",
    ),
    key(
        0x5307,
        MOD_WIN,
        b'A' as u32,
        ShellAction::QuickSettings,
        "Win+A",
    ),
    key(
        0x5308,
        MOD_WIN,
        b'N' as u32,
        ShellAction::Notifications,
        "Win+N",
    ),
];

const PRINT_SCREEN: ShellHotkey = key(
    PRINT_SCREEN_ID,
    0,
    VK_SNAPSHOT,
    ShellAction::ScreenClip,
    "PrtScn",
);

/// Whether the taskbar provides Explorer's shortcuts at all: only when
/// Explorer is not there to do it.
pub fn wanted(mode: RunMode) -> bool {
    mode == RunMode::Shell
}

/// The Win+ shortcuts to register (always, in shell mode).
pub fn win_hotkeys() -> &'static [ShellHotkey] {
    WIN_KEYS
}

/// The lone Print Screen key, when it should open the screen clip overlay.
/// Otherwise it is left alone: copying the screen to the clipboard is done
/// by Windows itself (win32k), not by Explorer.
pub fn print_screen_hotkey(opens_snipping: bool) -> Option<ShellHotkey> {
    opens_snipping.then_some(PRINT_SCREEN)
}

/// Find the shortcut for a `WM_HOTKEY` id.
pub fn by_id(id: i32) -> Option<ShellHotkey> {
    WIN_KEYS
        .iter()
        .copied()
        .chain(std::iter::once(PRINT_SCREEN))
        .find(|k| k.id == id)
}

/// First Windows 11 build.
pub const WINDOWS_11_BUILD: u32 = 22000;

/// Windows' "Use the Print screen key to open screen capture" setting
/// (`HKCU\Control Panel\Keyboard\PrintScreenKeyForSnippingEnabled`): 1 on,
/// 0 off. A missing value means the default, which is on in Windows 11 (an
/// update in 2023 turned it on) and off before.
pub fn print_screen_opens_snipping(flag: Option<u32>, build: Option<u32>) -> bool {
    match flag {
        Some(v) => v != 0,
        None => build.is_some_and(|b| b >= WINDOWS_11_BUILD),
    }
}

/// Name Windows gives the `index`th Win+Print Screen capture, in the UI
/// language ("Screenshot (3).png", "Captura de pantalla (3).png").
pub fn screenshot_file_name(lang: Lang, index: u32) -> String {
    let base = crate::i18n::tr(lang, "Screenshot", "Captura de pantalla");
    format!("{base} ({index}).png")
}

/// The index for the next capture: Windows' counter (`ScreenshotIndex`,
/// next index to use; missing or 0 means 1), moved past files that already
/// exist (`taken`), so a capture never overwrites one.
pub fn next_screenshot_index(counter: Option<u32>, taken: impl Fn(u32) -> bool) -> u32 {
    let mut i = counter.unwrap_or(1).max(1);
    // Bounded: a folder full of captures must not hang the taskbar.
    for _ in 0..100_000 {
        if !taken(i) {
            return i;
        }
        i = i.saturating_add(1);
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_shell_mode_takes_over_explorers_shortcuts() {
        assert!(wanted(RunMode::Shell));
        assert!(!wanted(RunMode::Dev));
    }

    #[test]
    fn hotkey_table_is_consistent() {
        let all: Vec<ShellHotkey> = win_hotkeys()
            .iter()
            .copied()
            .chain(print_screen_hotkey(true))
            .collect();
        for (i, a) in all.iter().enumerate() {
            assert!(a.id > 0 && a.id < 0xC000, "{}", a.name);
            assert_eq!(a.modifiers & MOD_NOREPEAT, 0, "{}", a.name);
            assert_eq!(by_id(a.id), Some(*a));
            for b in &all[i + 1..] {
                assert_ne!(a.id, b.id);
                assert_ne!((a.modifiers, a.vk), (b.modifiers, b.vk), "{}", a.name);
            }
        }
        // Every Win+ entry really uses the Win key.
        assert!(win_hotkeys().iter().all(|k| k.modifiers & MOD_WIN != 0));
        assert_eq!(by_id(1), None);
    }

    #[test]
    fn screenshot_shortcuts() {
        let clip = by_id(0x5300).unwrap();
        assert_eq!(
            (clip.modifiers, clip.vk),
            (MOD_WIN | MOD_SHIFT, b'S' as u32)
        );
        assert_eq!(clip.action, ShellAction::ScreenClip);
        let file = win_hotkeys()
            .iter()
            .find(|k| k.action == ShellAction::ScreenshotToFile)
            .unwrap();
        assert_eq!((file.modifiers, file.vk), (MOD_WIN, VK_SNAPSHOT));
        let prt = print_screen_hotkey(true).unwrap();
        assert_eq!((prt.modifiers, prt.vk), (0, VK_SNAPSHOT));
        assert_eq!(prt.action, ShellAction::ScreenClip);
        assert_eq!(print_screen_hotkey(false), None);
    }

    #[test]
    fn print_screen_setting() {
        // Explicit values win.
        assert!(print_screen_opens_snipping(Some(1), Some(19045)));
        assert!(!print_screen_opens_snipping(Some(0), Some(26100)));
        // Missing: the Windows 11 default is on, Windows 10's is off.
        assert!(print_screen_opens_snipping(None, Some(22631)));
        assert!(print_screen_opens_snipping(None, Some(22000)));
        assert!(!print_screen_opens_snipping(None, Some(19045)));
        assert!(!print_screen_opens_snipping(None, None));
    }

    #[test]
    fn screenshot_names_follow_windows() {
        assert_eq!(screenshot_file_name(Lang::En, 1), "Screenshot (1).png");
        assert_eq!(
            screenshot_file_name(Lang::Es, 12),
            "Captura de pantalla (12).png"
        );
    }

    #[test]
    fn screenshot_index_skips_existing_files() {
        assert_eq!(next_screenshot_index(None, |_| false), 1);
        assert_eq!(next_screenshot_index(Some(0), |_| false), 1);
        assert_eq!(next_screenshot_index(Some(7), |_| false), 7);
        assert_eq!(next_screenshot_index(Some(2), |i| i < 5), 5);
        assert_eq!(next_screenshot_index(None, |i| i == 1 || i == 2), 3);
    }
}
