// SPDX-License-Identifier: GPL-3.0-or-later
//! "Windows key pressed and released alone" detection.
//!
//! Fed by a low-level keyboard hook (`WH_KEYBOARD_LL`) in `explorer-win`. When
//! the Win key is released without any other key in between, we open the
//! launcher. To stop explorer.exe's Start menu from *also* opening in dev
//! mode, the hook injects a "mask" key press (an unassigned virtual key)
//! right before the Win key-up passes through: Windows then sees a Win+key
//! chord, not a lone Win tap. Shortcuts such as Win+E or Win+L are untouched
//! because we never swallow any key.

pub const VK_LWIN: u32 = 0x5B;
pub const VK_RWIN: u32 = 0x5C;
/// Unassigned virtual-key code used as the mask key (0xE8 per WinUser.h).
pub const VK_MASK: u32 = 0xE8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Let the event through unchanged.
    Pass,
    /// Inject the mask key, let the Win key-up through, open the launcher.
    MaskAndTrigger,
}

#[derive(Debug, Default)]
pub struct WinKeyDetector {
    win_down: bool,
    chorded: bool,
}

impl WinKeyDetector {
    pub fn new() -> Self {
        Self::default()
    }

    fn is_win(vk: u32) -> bool {
        vk == VK_LWIN || vk == VK_RWIN
    }

    /// Feed one keyboard event. `injected` events (including our own mask
    /// key) are ignored so they cannot confuse the state machine.
    pub fn on_key(&mut self, vk: u32, down: bool, injected: bool) -> KeyAction {
        if injected {
            return KeyAction::Pass;
        }
        match (Self::is_win(vk), down) {
            (true, true) => {
                if !self.win_down {
                    self.win_down = true;
                    self.chorded = false;
                }
                // Auto-repeat of the Win key keeps the current state.
                KeyAction::Pass
            }
            (true, false) => {
                let alone = self.win_down && !self.chorded;
                self.win_down = false;
                self.chorded = false;
                if alone {
                    KeyAction::MaskAndTrigger
                } else {
                    KeyAction::Pass
                }
            }
            (false, _) => {
                if self.win_down {
                    self.chorded = true;
                }
                KeyAction::Pass
            }
        }
    }

    /// A mouse click while Win is held (Win+click) also cancels the tap.
    pub fn on_mouse_button(&mut self) {
        if self.win_down {
            self.chorded = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_tap_triggers() {
        let mut d = WinKeyDetector::new();
        assert_eq!(d.on_key(VK_LWIN, true, false), KeyAction::Pass);
        assert_eq!(d.on_key(VK_LWIN, true, false), KeyAction::Pass); // repeat
        assert_eq!(d.on_key(VK_LWIN, false, false), KeyAction::MaskAndTrigger);
    }

    #[test]
    fn chord_does_not_trigger() {
        let mut d = WinKeyDetector::new();
        d.on_key(VK_LWIN, true, false);
        d.on_key(0x45, true, false); // E
        d.on_key(0x45, false, false);
        assert_eq!(d.on_key(VK_LWIN, false, false), KeyAction::Pass);
        // Next lone tap works again.
        d.on_key(VK_RWIN, true, false);
        assert_eq!(d.on_key(VK_RWIN, false, false), KeyAction::MaskAndTrigger);
    }

    #[test]
    fn injected_and_mouse() {
        let mut d = WinKeyDetector::new();
        d.on_key(VK_LWIN, true, false);
        d.on_key(VK_MASK, true, true);
        assert_eq!(d.on_key(VK_LWIN, false, false), KeyAction::MaskAndTrigger);
        d.on_key(VK_LWIN, true, false);
        d.on_mouse_button();
        assert_eq!(d.on_key(VK_LWIN, false, false), KeyAction::Pass);
    }

    #[test]
    fn stray_keyup_ignored() {
        let mut d = WinKeyDetector::new();
        assert_eq!(d.on_key(VK_LWIN, false, false), KeyAction::Pass);
    }
}
