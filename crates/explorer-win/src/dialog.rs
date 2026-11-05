// SPDX-License-Identifier: GPL-3.0-or-later
//! Plain message boxes for recovery prompts. `MessageBoxW` is used instead
//! of task dialogs because it needs no common-controls manifest and works
//! even on an otherwise empty desktop.

use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND,
    MB_TOPMOST, MB_YESNO,
};

use crate::util::WideString;

/// Yes/No question; true = Yes. `default_no` focuses "No".
pub fn ask_yes_no(caption: &str, text: &str, default_no: bool) -> bool {
    let c = WideString::new(caption);
    let t = WideString::new(text);
    let mut style = MB_YESNO | MB_ICONWARNING | MB_TOPMOST | MB_SETFOREGROUND;
    if default_no {
        style |= MB_DEFBUTTON2;
    }
    // SAFETY: valid strings; no owner window.
    unsafe { MessageBoxW(None, t.pcwstr(), c.pcwstr(), style) == IDYES }
}

/// Information box with an OK button.
pub fn info(caption: &str, text: &str) {
    let c = WideString::new(caption);
    let t = WideString::new(text);
    // SAFETY: valid strings; no owner window.
    unsafe {
        MessageBoxW(
            None,
            t.pcwstr(),
            c.pcwstr(),
            MB_OK | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND,
        )
    };
}
