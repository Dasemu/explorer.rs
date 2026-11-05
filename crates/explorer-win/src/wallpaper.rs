// SPDX-License-Identifier: GPL-3.0-or-later
//! Wallpaper settings (documented `SPI_GETDESKWALLPAPER`, the user-visible
//! `HKCU\Control Panel\Desktop` values and the desktop colour) and image
//! decoding with WIC. Geometry lives in `explorer_core::wallpaper`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use explorer_core::wallpaper::{parse_rgb, Style};
use windows::core::Interface;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Gdi::{GetSysColor, COLOR_DESKTOP};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::registry::{self, Root};
use crate::util::{from_wide, WideString};

const DESKTOP_KEY: &str = r"Control Panel\Desktop";
const COLORS_KEY: &str = r"Control Panel\Colors";

/// The user's wallpaper settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// `None`: solid colour only.
    pub path: Option<PathBuf>,
    pub style: Style,
    /// 0xRRGGBB.
    pub background: u32,
}

/// Read the current settings.
pub fn settings() -> Settings {
    let mut buf = [0u16; 1024];
    // SAFETY: buffer of `buf.len()` characters.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            Some(buf.as_mut_ptr().cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    let path = ok
        .then(|| from_wide(&buf))
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    let style = Style::from_registry(
        registry::read_string(Root::CurrentUser, DESKTOP_KEY, "WallpaperStyle").as_deref(),
        registry::read_string(Root::CurrentUser, DESKTOP_KEY, "TileWallpaper").as_deref(),
    );
    let background = registry::read_string(Root::CurrentUser, COLORS_KEY, "Background")
        .and_then(|s| parse_rgb(&s))
        .unwrap_or_else(|| {
            // SAFETY: plain query. COLORREF is 0x00BBGGRR.
            let c = unsafe { GetSysColor(COLOR_DESKTOP) };
            ((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff)
        });
    Settings {
        path,
        style,
        background,
    }
}

/// Decoded image: premultiplied BGRA, top-down, tightly packed.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Decode `path` with WIC. `target` maps the natural size to the size to
/// decode at (downscaling with a high-quality filter). COM must be
/// initialised on this thread.
pub fn decode(
    path: &std::path::Path,
    target: impl FnOnce(u32, u32) -> (u32, u32),
) -> Result<Decoded> {
    // SAFETY: WIC calls with valid arguments; COM initialised by the caller.
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .context("create WIC factory")?;
        let name = WideString::new(&path.to_string_lossy());
        let decoder = factory
            .CreateDecoderFromFilename(
                name.pcwstr(),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .with_context(|| format!("open {}", path.display()))?;
        let frame = decoder.GetFrame(0).context("GetFrame")?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).context("GetSize")?;
        if w == 0 || h == 0 {
            anyhow::bail!("empty image");
        }
        let (tw, th) = target(w, h);
        let mut source: IWICBitmapSource = frame.cast()?;
        if (tw, th) != (w, h) && tw > 0 && th > 0 {
            let scaler = factory.CreateBitmapScaler().context("scaler")?;
            scaler
                .Initialize(&source, tw, th, WICBitmapInterpolationModeFant)
                .context("scale")?;
            source = scaler.cast()?;
            w = tw;
            h = th;
        }
        let conv = factory.CreateFormatConverter().context("converter")?;
        conv.Initialize(
            &source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .context("convert")?;
        let stride = w.checked_mul(4).context("image too wide")?;
        let len = (stride as usize)
            .checked_mul(h as usize)
            .context("image too large")?;
        let mut pixels = vec![0u8; len];
        conv.CopyPixels(std::ptr::null(), stride, &mut pixels)
            .context("CopyPixels")?;
        Ok(Decoded {
            width: w,
            height: h,
            pixels,
        })
    }
}
