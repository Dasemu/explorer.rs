// SPDX-License-Identifier: GPL-3.0-or-later
//! Icon extraction into CPU images (`explorer_core::image::Image`).
//!
//! - Apps / files: `IShellItemImageFactory::GetImage` on a shell item
//!   (`shell:AppsFolder\<id>` or a file path). Documented Shell API; gives
//!   the same icons as Start and File Explorer, including packaged apps.
//! - Windows: AppUserModelID from the window's property store (packaged
//!   apps, UWP frames), then `WM_GETICON`, the class icon, and finally the
//!   process image.
//! - Tray: `HICON` handles drawn into a 32-bit DIB with `DrawIconEx`.

use explorer_core::catalog::LaunchTarget;
use explorer_core::image::{Image, SharedImage};
use windows::core::{GUID, PWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Foundation::{HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDIBits, GetObjectW,
    SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetClassLongPtrW, SendMessageTimeoutW, DI_MASK, DI_NORMAL, GCLP_HICON,
    GCLP_HICONSM, HICON, ICON_BIG, ICON_SMALL, ICON_SMALL2, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    WM_GETICON,
};

use crate::util::WideString;

/// PKEY_AppUserModel_ID {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5.
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3),
    pid: 5,
};

/// Icon of a shell item given its parsing name. Requires COM.
pub fn from_parsing_name(parsing: &str, size: u32) -> Option<SharedImage> {
    let p = WideString::new(parsing);
    // SAFETY: documented Shell calls; the HBITMAP is owned and freed.
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(p.pcwstr(), None).ok()?;
        let hbm = factory
            .GetImage(
                SIZE {
                    cx: size as i32,
                    cy: size as i32,
                },
                SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
            )
            .ok()?;
        let img = hbitmap_to_image(hbm);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        img
    }
}

/// Icon for a launcher entry. Requires COM.
pub fn for_target(t: &LaunchTarget, size: u32) -> Option<SharedImage> {
    match t {
        LaunchTarget::AppsFolder(id) => from_parsing_name(&format!("shell:AppsFolder\\{id}"), size),
        LaunchTarget::Path(p) => from_parsing_name(&p.to_string_lossy(), size),
    }
}

/// Copy a 32-bpp (or any) HBITMAP into a top-down BGRA image.
fn hbitmap_to_image(hbm: HBITMAP) -> Option<SharedImage> {
    // SAFETY: GDI calls with properly sized structures; DC released.
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(
            HGDIOBJ(hbm.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut core::ffi::c_void),
        ) == 0
        {
            return None;
        }
        let (w, h) = (bm.bmWidth.max(0) as u32, bm.bmHeight.unsigned_abs());
        if w == 0 || h == 0 || w > 1024 || h > 1024 {
            return None;
        }
        let mut bi = bitmap_info(w, h);
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            hbm,
            0,
            h,
            Some(px.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        if lines == 0 {
            return None;
        }
        Image::from_bgra(w, h, px)
    }
}

fn bitmap_info(w: u32, h: u32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Render an HICON at `size` px. The handle is not destroyed.
pub fn from_hicon(hicon: HICON, size: u32) -> Option<SharedImage> {
    if hicon.is_invalid() || size == 0 {
        return None;
    }
    let draw = |flags| -> Option<Vec<u8>> {
        // SAFETY: DIB section owned locally and selected into a temporary
        // DC; everything is released before returning.
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bi = bitmap_info(size, size);
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let old = SelectObject(dc, HGDIOBJ(dib.0));
            let ok = DrawIconEx(dc, 0, 0, hicon, size as i32, size as i32, 0, None, flags).is_ok();
            let len = (size * size * 4) as usize;
            let out = (ok && !bits.is_null())
                .then(|| std::slice::from_raw_parts(bits as *const u8, len).to_vec());
            SelectObject(dc, old);
            let _ = DeleteObject(HGDIOBJ(dib.0));
            let _ = DeleteDC(dc);
            out
        }
    };
    let mut px = draw(DI_NORMAL)?;
    if px.chunks_exact(4).all(|p| p[3] == 0) {
        // Legacy icon without alpha: derive alpha from the AND mask
        // (black mask pixel = opaque).
        if let Some(mask) = draw(DI_MASK) {
            for (p, m) in px.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                if m[0] == 0 && m[1] == 0 && m[2] == 0 {
                    p[3] = 255;
                } else {
                    p.copy_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
    }
    Image::from_bgra(size, size, px)
}

/// AppUserModelID set on a window (packaged apps, UWP frames, some Win32
/// apps). Requires COM.
pub fn window_app_id(hwnd: HWND) -> Option<String> {
    // SAFETY: documented property-store calls; the string is freed.
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let v = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?;
        let p: PWSTR = PropVariantToStringAlloc(&v).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        s.filter(|s| !s.is_empty())
    }
}

fn get_icon_message(hwnd: HWND, kind: u32) -> Option<HICON> {
    let mut result: usize = 0;
    // SAFETY: bounded wait on a possibly hung window.
    let r = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETICON,
            WPARAM(kind as usize),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            50,
            Some(&mut result),
        )
    };
    (r.0 != 0 && result != 0).then_some(HICON(result as *mut _))
}

/// Taskbar icon for a window. Requires COM. Never blocks more than ~150 ms
/// even for hung windows.
pub fn for_window(hwnd: HWND, size: u32) -> Option<SharedImage> {
    if let Some(id) = window_app_id(hwnd) {
        if let Some(img) = from_parsing_name(&format!("shell:AppsFolder\\{id}"), size) {
            return Some(img);
        }
    }
    let hicon = get_icon_message(hwnd, ICON_BIG)
        .or_else(|| get_icon_message(hwnd, ICON_SMALL2))
        .or_else(|| get_icon_message(hwnd, ICON_SMALL))
        .or_else(|| {
            // SAFETY: plain query.
            let v = unsafe { GetClassLongPtrW(hwnd, GCLP_HICON) };
            (v != 0).then_some(HICON(v as *mut _))
        })
        .or_else(|| {
            // SAFETY: plain query.
            let v = unsafe { GetClassLongPtrW(hwnd, GCLP_HICONSM) };
            (v != 0).then_some(HICON(v as *mut _))
        });
    if let Some(img) = hicon.and_then(|h| from_hicon(h, size)) {
        if !img.is_blank() {
            return Some(img);
        }
    }
    let path = crate::winlist::process_path(crate::winlist::process_id(hwnd))?;
    from_parsing_name(&path, size)
}
