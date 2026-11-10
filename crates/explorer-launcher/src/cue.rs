// SPDX-License-Identifier: GPL-3.0-or-later
//! Placeholder ("cue banner") text for the search box.
//!
//! `EM_SETCUEBANNER` needs comctl32 v6 (an application manifest) and draws
//! nothing with the classic EDIT control, so the placeholder is painted by
//! subclassing the EDIT window: after the control paints itself, if it is
//! empty, the text is drawn in the dim colour into its formatting
//! rectangle. This works with and without the v6 manifest and looks the
//! same in both. The state lives in a thread-local: the control belongs to
//! the UI thread.

use std::cell::RefCell;

use explorer_win::windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use explorer_win::windows::Win32::Graphics::Gdi::{
    DrawTextW, GetDC, InvalidateRect, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_TOP, HFONT, HGDIOBJ, TRANSPARENT,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, GetWindowTextLengthW, SendMessageW, SetWindowLongPtrW, GWLP_WNDPROC, WM_PAINT,
    WNDPROC,
};

const WM_GETFONT: u32 = 0x0031;
const EM_GETRECT: u32 = 0x00B2;

/// Messages that may change the text: WM_KEYDOWN, WM_CHAR, WM_SETTEXT,
/// WM_CUT, WM_PASTE, WM_CLEAR, WM_UNDO, EM_REPLACESEL, EM_UNDO,
/// WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION.
const EDITING_MESSAGES: &[u32] = &[
    0x0100, 0x0102, 0x000C, 0x0300, 0x0302, 0x0303, 0x0304, 0x00C2, 0x00C7, 0x010F, 0x010E,
];

#[derive(Default)]
struct Cue {
    orig: isize,
    text: Vec<u16>,
    /// COLORREF (0x00BBGGRR).
    color: u32,
}

thread_local! {
    static CUE: RefCell<Cue> = RefCell::new(Cue::default());
}

/// Subclass `edit` (once).
pub fn install(edit: HWND) {
    if edit.is_invalid() {
        return;
    }
    let proc_ptr = cue_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
    // SAFETY: replacing the window procedure of our own child control; the
    // previous one is stored and always called first.
    let orig = unsafe { SetWindowLongPtrW(edit, GWLP_WNDPROC, proc_ptr as usize as isize) };
    CUE.with(|c| c.borrow_mut().orig = orig);
}

/// Set the placeholder text and colour (0xRRGGBB); repaints the control.
pub fn set(edit: HWND, text: &str, rgb: u32) {
    let w: Vec<u16> = text.encode_utf16().collect();
    let color = ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff);
    let changed = CUE.with(|c| {
        let mut c = c.borrow_mut();
        let changed = c.text != w || c.color != color;
        c.text = w;
        c.color = color;
        changed
    });
    if changed {
        // SAFETY: repaint request on our own control.
        let _ = unsafe { InvalidateRect(Some(edit), None, true) };
    }
}

unsafe extern "system" fn cue_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Copy what we need out of the thread-local before calling the original
    // procedure, which may re-enter this function.
    let orig = CUE.with(|c| c.try_borrow().map(|c| c.orig).unwrap_or(0));
    if orig == 0 {
        return LRESULT(0);
    }
    // SAFETY: `orig` is the EDIT class procedure returned by
    // SetWindowLongPtrW; WNDPROC is a nullable function pointer.
    let orig_proc: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(orig) };
    // The classic EDIT draws typed characters directly, without a full
    // repaint: when the text goes from empty to non-empty (or back), force
    // one so the placeholder appears or disappears completely.
    let edits_text = EDITING_MESSAGES.contains(&msg);
    // SAFETY: plain query on our own control (WM_GETTEXTLENGTH is not in
    // EDITING_MESSAGES, so this does not recurse).
    let was_empty = edits_text && unsafe { GetWindowTextLengthW(hwnd) } == 0;
    // SAFETY: forwarding the message unchanged.
    let r = unsafe { CallWindowProcW(orig_proc, hwnd, msg, wp, lp) };
    if edits_text {
        // SAFETY: as above.
        let is_empty = unsafe { GetWindowTextLengthW(hwnd) } == 0;
        if was_empty != is_empty {
            // SAFETY: repaint request on our own control.
            let _ = unsafe { InvalidateRect(Some(hwnd), None, true) };
        }
    }
    if msg == WM_PAINT {
        // SAFETY: drawing on our own control after it painted itself.
        unsafe { draw_cue(hwnd) };
    }
    r
}

unsafe fn draw_cue(hwnd: HWND) {
    // SAFETY: plain query on our own control.
    if unsafe { GetWindowTextLengthW(hwnd) } != 0 {
        return;
    }
    let Some((mut text, color)) = CUE.with(|c| {
        c.try_borrow()
            .ok()
            .filter(|c| !c.text.is_empty())
            .map(|c| (c.text.clone(), c.color))
    }) else {
        return;
    };
    // SAFETY: DC acquired and released here; the font is the control's own
    // and is deselected before release.
    unsafe {
        let mut rc = RECT::default();
        SendMessageW(
            hwnd,
            EM_GETRECT,
            None,
            Some(LPARAM(&mut rc as *mut RECT as isize)),
        );
        let hdc = GetDC(Some(hwnd));
        if hdc.is_invalid() {
            return;
        }
        let font = SendMessageW(hwnd, WM_GETFONT, None, None);
        let old = (font.0 != 0).then(|| SelectObject(hdc, HGDIOBJ(HFONT(font.0 as *mut _).0)));
        SetTextColor(hdc, COLORREF(color));
        SetBkMode(hdc, TRANSPARENT);
        DrawTextW(
            hdc,
            &mut text,
            &mut rc,
            DT_LEFT | DT_TOP | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
        if let Some(old) = old {
            SelectObject(hdc, old);
        }
        ReleaseDC(Some(hwnd), hdc);
    }
}
