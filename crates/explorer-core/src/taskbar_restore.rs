// SPDX-License-Identifier: GPL-3.0-or-later
//! Decisions for giving Windows' own taskbar back after explorer.rs hid it
//! (`[taskbar] hide_windows_taskbar`).
//!
//! The previous "Automatically hide the taskbar" setting is saved in the
//! state file before anything changes. Restoring it has two routes:
//!
//! - Explorer's taskbar window exists: ask it (`ABM_SETSTATE`), which also
//!   persists the setting.
//! - Explorer is not running (shell mode, or Explorer is about to start
//!   after `--restore-explorer` / uninstall): write the setting where
//!   Explorer reads it when it starts (`StuckRects3`, byte 8 bit 0). Without
//!   this, an `explorer.exe` started later would come up on auto-hide with
//!   no visible taskbar.
//!
//! This module only decides; `explorer-win::win_taskbar` does the I/O.
//! The state file is kept until the restore is confirmed, so a failed
//! attempt can be retried by whoever runs next.

/// Byte of the `StuckRects3\Settings` value holding the taskbar flags.
pub const SETTINGS_FLAGS_INDEX: usize = 8;
/// Bit 0 of that byte: "Automatically hide the taskbar".
const AUTOHIDE_BIT: u8 = 0x01;

/// The auto-hide flag in a `StuckRects3\Settings` value (`None` if the
/// value is too short to contain it).
pub fn settings_autohide(settings: &[u8]) -> Option<bool> {
    settings
        .get(SETTINGS_FLAGS_INDEX)
        .map(|b| b & AUTOHIDE_BIT != 0)
}

/// `settings` with the auto-hide flag set to `autohide`, every other bit
/// untouched. `None` if the value is too short to contain the flag.
pub fn settings_with_autohide(settings: &[u8], autohide: bool) -> Option<Vec<u8>> {
    let mut out = settings.to_vec();
    let b = out.get_mut(SETTINGS_FLAGS_INDEX)?;
    if autohide {
        *b |= AUTOHIDE_BIT;
    } else {
        *b &= !AUTOHIDE_BIT;
    }
    Some(out)
}

/// What is known when a restore is attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreInput {
    /// Auto-hide setting saved in the state file (`None`: file missing or
    /// unreadable, so we did not hide anything).
    pub saved_autohide: Option<bool>,
    /// A taskbar window owned by `explorer.exe` exists. Our own tray host
    /// also uses the class `Shell_TrayWnd`; it must not count.
    pub explorer_taskbar_present: bool,
    /// Auto-hide flag currently in `StuckRects3\Settings` (`None`: value
    /// missing or malformed).
    pub registry_autohide: Option<bool>,
}

/// What to do to restore the setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreAction {
    /// We did not hide Windows' taskbar: leave everything alone.
    Nothing,
    /// Ask Explorer's taskbar to switch to this auto-hide state and show
    /// itself.
    ViaExplorer { autohide: bool },
    /// Explorer is not running: write the flag into `StuckRects3`.
    WriteRegistry { autohide: bool },
    /// Explorer is not running and the registry already holds the saved
    /// value: nothing to write, the restore is complete.
    AlreadyRestored,
    /// Explorer is not running and the registry cannot be used: keep the
    /// state file so a later restore (with Explorer running) can finish.
    Defer,
}

pub fn plan_restore(i: RestoreInput) -> RestoreAction {
    let Some(was) = i.saved_autohide else {
        return RestoreAction::Nothing;
    };
    if i.explorer_taskbar_present {
        return RestoreAction::ViaExplorer { autohide: was };
    }
    match i.registry_autohide {
        Some(now) if now == was => RestoreAction::AlreadyRestored,
        Some(_) => RestoreAction::WriteRegistry { autohide: was },
        None => RestoreAction::Defer,
    }
}

/// Delete the state file only once the setting is confirmed to be back:
/// `confirmed_autohide` is the value read back after acting (from
/// Explorer's taskbar or from the registry). A missing or different value
/// keeps the file.
pub fn state_may_be_removed(action: RestoreAction, confirmed_autohide: Option<bool>) -> bool {
    match action {
        RestoreAction::Nothing | RestoreAction::Defer => false,
        RestoreAction::AlreadyRestored => true,
        RestoreAction::ViaExplorer { autohide } | RestoreAction::WriteRegistry { autohide } => {
            confirmed_autohide == Some(autohide)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(saved: Option<bool>, explorer: bool, reg: Option<bool>) -> RestoreInput {
        RestoreInput {
            saved_autohide: saved,
            explorer_taskbar_present: explorer,
            registry_autohide: reg,
        }
    }

    #[test]
    fn nothing_saved_nothing_done() {
        for explorer in [false, true] {
            for reg in [None, Some(false), Some(true)] {
                assert_eq!(
                    plan_restore(input(None, explorer, reg)),
                    RestoreAction::Nothing
                );
            }
        }
    }

    #[test]
    fn explorer_running_is_asked_directly() {
        assert_eq!(
            plan_restore(input(Some(false), true, Some(true))),
            RestoreAction::ViaExplorer { autohide: false }
        );
        assert_eq!(
            plan_restore(input(Some(true), true, Some(true))),
            RestoreAction::ViaExplorer { autohide: true }
        );
    }

    #[test]
    fn shell_mode_writes_the_registry() {
        // The B2 case: explorer.rs hid Windows' taskbar (registry has the
        // auto-hide flag) and Explorer is not running.
        assert_eq!(
            plan_restore(input(Some(false), false, Some(true))),
            RestoreAction::WriteRegistry { autohide: false }
        );
        assert_eq!(
            plan_restore(input(Some(true), false, Some(false))),
            RestoreAction::WriteRegistry { autohide: true }
        );
    }

    #[test]
    fn registry_already_correct_needs_no_write() {
        assert_eq!(
            plan_restore(input(Some(false), false, Some(false))),
            RestoreAction::AlreadyRestored
        );
    }

    #[test]
    fn unusable_registry_defers() {
        assert_eq!(
            plan_restore(input(Some(false), false, None)),
            RestoreAction::Defer
        );
    }

    #[test]
    fn state_removed_only_when_confirmed() {
        let via = RestoreAction::ViaExplorer { autohide: false };
        assert!(state_may_be_removed(via, Some(false)));
        assert!(!state_may_be_removed(via, Some(true)));
        assert!(!state_may_be_removed(via, None));
        let reg = RestoreAction::WriteRegistry { autohide: true };
        assert!(state_may_be_removed(reg, Some(true)));
        assert!(!state_may_be_removed(reg, Some(false)));
        assert!(state_may_be_removed(RestoreAction::AlreadyRestored, None));
        assert!(!state_may_be_removed(RestoreAction::Defer, None));
        assert!(!state_may_be_removed(RestoreAction::Nothing, None));
    }

    #[test]
    fn settings_flag_roundtrip_keeps_other_bits() {
        let mut s = vec![0u8; 40];
        s[SETTINGS_FLAGS_INDEX] = 0x03;
        assert_eq!(settings_autohide(&s), Some(true));
        let off = settings_with_autohide(&s, false).unwrap();
        assert_eq!(off[SETTINGS_FLAGS_INDEX], 0x02);
        assert_eq!(settings_autohide(&off), Some(false));
        let on = settings_with_autohide(&off, true).unwrap();
        assert_eq!(on, s);
        // Other bytes are untouched.
        s[0] = 0x28;
        s[20] = 0x7f;
        let off = settings_with_autohide(&s, false).unwrap();
        assert_eq!((off[0], off[20]), (0x28, 0x7f));
    }

    #[test]
    fn short_settings_are_rejected() {
        let s = [0u8; SETTINGS_FLAGS_INDEX];
        assert_eq!(settings_autohide(&s), None);
        assert_eq!(settings_with_autohide(&s, true), None);
    }
}
