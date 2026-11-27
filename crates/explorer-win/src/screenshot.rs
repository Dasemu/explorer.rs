// SPDX-License-Identifier: GPL-3.0-or-later
//! Win+Print Screen: the whole virtual screen to a PNG in the Screenshots
//! folder and to the clipboard, as Explorer does (explorer.rs as the shell
//! only; alongside, Explorer still does it).
//!
//! Naming follows Windows: "Screenshot (N).png" ("Captura de pantalla (N)
//! .png" in Spanish), with N from Explorer's own counter
//! (`HKCU\...\Explorer\ScreenshotIndex`), so captures continue the user's
//! numbering. Windows dims the screen for a moment; we do not.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use explorer_core::i18n::Lang;
use explorer_core::shell_keys::{next_screenshot_index, screenshot_file_name};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, GENERIC_WRITE, HANDLE};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
    SRCCOPY,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
    IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_DIB;
use windows::Win32::UI::Shell::{
    FOLDERID_Pictures, FOLDERID_Screenshots, SHGetKnownFolderPath, KF_FLAG_CREATE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::registry::{self, Root};
use crate::util::WideString;

const EXPLORER_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer";

/// A capture: top-down rows of BGRA pixels (alpha 255).
struct Capture {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Capture the whole virtual screen (all monitors, physical pixels: the
/// process is per-monitor DPI aware).
fn capture() -> Result<Capture> {
    // SAFETY: plain metric queries.
    let (x, y, w, h) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if w <= 0 || h <= 0 {
        bail!("empty virtual screen {w}x{h}");
    }
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative: top-down rows, as WIC wants them.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    // SAFETY: GDI objects created here are released on every path below;
    // `pixels` holds exactly h rows of w 32-bit pixels.
    let lines = unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            bail!("GetDC(screen) failed");
        }
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        // CAPTUREBLT includes layered windows (menus, tooltips, overlays).
        let blt = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT);
        SelectObject(mem, old);
        let lines = if blt.is_ok() {
            GetDIBits(
                mem,
                bmp,
                0,
                h as u32,
                Some(pixels.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        blt.context("BitBlt")?;
        lines
    };
    if lines != h {
        bail!("GetDIBits copied {lines} of {h} lines");
    }
    // The screen has no alpha channel: GDI leaves it 0 (fully transparent
    // in a PNG).
    for px in pixels.chunks_exact_mut(4) {
        px[3] = 0xFF;
    }
    Ok(Capture {
        width: w as u32,
        height: h as u32,
        pixels,
    })
}

fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    // SAFETY: the returned buffer is freed with CoTaskMemFree.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_CREATE, None).ok()?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        (!s.is_empty()).then(|| PathBuf::from(s))
    }
}

/// The Screenshots known folder (created if missing), or Pictures\Screenshots.
fn screenshots_folder() -> Result<PathBuf> {
    if let Some(p) = known_folder(&FOLDERID_Screenshots) {
        return Ok(p);
    }
    let pictures = known_folder(&FOLDERID_Pictures).ok_or_else(|| anyhow!("no Pictures folder"))?;
    let p = pictures.join("Screenshots");
    std::fs::create_dir_all(&p).with_context(|| format!("create {}", p.display()))?;
    Ok(p)
}

fn encode_png(c: &Capture, path: &Path) -> Result<()> {
    let file = WideString::new(&path.to_string_lossy());
    // SAFETY: documented WIC calls with valid arguments; COM is initialized
    // on this thread by the caller.
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .context("WIC factory")?;
        let stream = factory.CreateStream().context("WIC stream")?;
        stream
            .InitializeFromFilename(PCWSTR(file.pcwstr().0), GENERIC_WRITE.0)
            .with_context(|| format!("create {}", path.display()))?;
        let encoder = factory
            .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
            .context("PNG encoder")?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(|| anyhow!("no PNG frame"))?;
        frame.Initialize(None)?;
        frame.SetSize(c.width, c.height)?;
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format)?;
        if format != GUID_WICPixelFormat32bppBGRA {
            bail!("the PNG encoder does not take 32-bit BGRA");
        }
        frame.WritePixels(c.height, c.width * 4, &c.pixels)?;
        frame.Commit()?;
        encoder.Commit()?;
    }
    Ok(())
}

/// Put the capture on the clipboard as a device-independent bitmap.
fn copy_to_clipboard(c: &Capture) -> Result<()> {
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: c.width as i32,
        // Positive: bottom-up rows, which every program reads correctly
        // (top-down CF_DIBs are mishandled by some).
        biHeight: c.height as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: c.width * c.height * 4,
        ..Default::default()
    };
    let head = std::mem::size_of::<BITMAPINFOHEADER>();
    let row = c.width as usize * 4;
    let bytes = head + c.pixels.len();
    // SAFETY: standard clipboard protocol; the global block is handed to the
    // clipboard on success and freed by us on failure. Writes stay within
    // the `bytes` allocated.
    unsafe {
        OpenClipboard(None).context("OpenClipboard")?;
        let _ = EmptyClipboard();
        let res = (|| -> Result<()> {
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes).context("GlobalAlloc")?;
            let p = GlobalLock(h) as *mut u8;
            if p.is_null() {
                let _ = GlobalFree(Some(h));
                bail!("GlobalLock failed");
            }
            std::ptr::copy_nonoverlapping(
                (&header as *const BITMAPINFOHEADER).cast::<u8>(),
                p,
                head,
            );
            for (i, src) in c.pixels.chunks_exact(row).rev().enumerate() {
                std::ptr::copy_nonoverlapping(src.as_ptr(), p.add(head + i * row), row);
            }
            let _ = GlobalUnlock(h);
            if let Err(e) = SetClipboardData(CF_DIB.0 as u32, Some(HANDLE(h.0))) {
                let _ = GlobalFree(Some(h));
                return Err(e).context("SetClipboardData");
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        res
    }
}

/// Take a Win+Print Screen capture. Requires COM on the calling thread
/// (run it off the UI thread: encoding a multi-monitor capture takes a
/// moment). Returns the file written.
pub fn save_screen(lang: Lang) -> Result<PathBuf> {
    let c = capture()?;
    if let Err(e) = copy_to_clipboard(&c) {
        tracing::warn!(error = %e, "screenshot: cannot copy to the clipboard");
    }
    let dir = screenshots_folder()?;
    let counter = registry::read_dword(Root::CurrentUser, EXPLORER_KEY, "ScreenshotIndex");
    let index = next_screenshot_index(counter, |i| {
        dir.join(screenshot_file_name(lang, i)).exists()
    });
    let path = dir.join(screenshot_file_name(lang, index));
    encode_png(&c, &path)?;
    if !registry::write_dword(
        Root::CurrentUser,
        EXPLORER_KEY,
        "ScreenshotIndex",
        index.saturating_add(1),
    ) {
        tracing::debug!("screenshot: cannot update ScreenshotIndex");
    }
    Ok(path)
}
