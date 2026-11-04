// SPDX-License-Identifier: GPL-3.0-or-later
//! Tiny Direct2D/DirectWrite renderer: filled rectangles and single-line
//! text. That is all the taskbar and launcher draw.
//!
//! Render targets use 96 DPI so one DIP equals one physical pixel; layout
//! code in `explorer-core` works in physical pixels with an explicit scale.

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::Result;
use explorer_core::image::SharedImage;
use explorer_core::layout::Rect;
use windows::core::w;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
    D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

/// 0xRRGGBB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: ((self.0 >> 16) & 0xff) as f32 / 255.0,
            g: ((self.0 >> 8) & 0xff) as f32 / 255.0,
            b: (self.0 & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Font used for UI glyphs (power, search, chevrons...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Font {
    Text,
    Icons,
}

/// Well-known glyphs of "Segoe Fluent Icons" / "Segoe MDL2 Assets"
/// (same code points in both).
pub mod glyph {
    pub const POWER: &str = "\u{E7E8}";
    pub const SEARCH: &str = "\u{E721}";
    pub const SETTINGS: &str = "\u{E713}";
    pub const USER: &str = "\u{E77B}";
    pub const CHEVRON_RIGHT: &str = "\u{E76C}";
    pub const CHEVRON_LEFT: &str = "\u{E76B}";
    pub const CHEVRON_UP: &str = "\u{E70E}";
    pub const APP: &str = "\u{ECAA}";
}

pub struct Gfx {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    icon_family: Vec<u16>,
    formats: RefCell<HashMap<(Font, u32, bool, Align), IDWriteTextFormat>>,
}

impl Gfx {
    pub fn new() -> Result<Self> {
        // SAFETY: factory creation with valid enum values.
        unsafe {
            Ok(Self {
                d2d: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                dwrite: {
                    let f: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
                    f
                },
                icon_family: Vec::new(),
                formats: RefCell::new(HashMap::new()),
            })
            .map(|mut g: Self| {
                g.icon_family = g.pick_icon_family();
                g
            })
        }
    }

    /// "Segoe Fluent Icons" (Windows 11) or "Segoe MDL2 Assets" (Windows 10).
    fn pick_icon_family(&self) -> Vec<u16> {
        let fluent = crate::util::wide("Segoe Fluent Icons");
        // SAFETY: DirectWrite queries with valid out-pointers.
        let has_fluent = unsafe {
            let mut coll = None;
            self.dwrite
                .GetSystemFontCollection(&mut coll, false)
                .is_ok()
                && coll.is_some_and(|c| {
                    let mut idx = 0u32;
                    let mut exists = windows::core::BOOL(0);
                    c.FindFamilyName(
                        windows::core::PCWSTR(fluent.as_ptr()),
                        &mut idx,
                        &mut exists,
                    )
                    .is_ok()
                        && exists.as_bool()
                })
        };
        if has_fluent {
            fluent
        } else {
            crate::util::wide("Segoe MDL2 Assets")
        }
    }

    fn format(&self, font: Font, size: f32, bold: bool, align: Align) -> Option<IDWriteTextFormat> {
        let key = (font, (size * 4.0) as u32, bold, align);
        if let Some(f) = self.formats.borrow().get(&key) {
            return Some(f.clone());
        }
        // SAFETY: DirectWrite calls with valid arguments.
        let f = unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(
                    match font {
                        Font::Text => w!("Segoe UI Variable Text"),
                        Font::Icons => windows::core::PCWSTR(self.icon_family.as_ptr()),
                    },
                    None,
                    if bold {
                        DWRITE_FONT_WEIGHT_SEMI_BOLD
                    } else {
                        DWRITE_FONT_WEIGHT_NORMAL
                    },
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!(""),
                )
                .ok()?;
            let _ = f.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(&f) {
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    ..Default::default()
                };
                let _ = f.SetTrimming(&trimming, &sign);
            }
            f
        };
        self.formats.borrow_mut().insert(key, f.clone());
        Some(f)
    }
}

/// Per-window render target, recreated lazily after device loss.
pub struct Surface {
    hwnd: HWND,
    target: Option<(ID2D1HwndRenderTarget, ID2D1SolidColorBrush)>,
    /// GPU copies of CPU images, keyed by `Image::id`. Cleared with the
    /// render target.
    bitmaps: RefCell<HashMap<u64, ID2D1Bitmap>>,
}

fn client_size(hwnd: HWND) -> D2D_SIZE_U {
    let mut rc = RECT::default();
    // SAFETY: valid out-pointer.
    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
    D2D_SIZE_U {
        width: (rc.right - rc.left).max(1) as u32,
        height: (rc.bottom - rc.top).max(1) as u32,
    }
}

impl Surface {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            target: None,
            bitmaps: RefCell::new(HashMap::new()),
        }
    }

    /// Call from WM_SIZE.
    pub fn resize(&mut self) {
        if let Some((rt, _)) = &self.target {
            // SAFETY: valid size struct.
            if unsafe { rt.Resize(&client_size(self.hwnd)) }.is_err() {
                self.target = None;
            }
        }
    }

    fn ensure(&mut self, gfx: &Gfx) -> Option<(ID2D1HwndRenderTarget, ID2D1SolidColorBrush)> {
        if self.target.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: client_size(self.hwnd),
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            // SAFETY: valid property structs.
            let created = unsafe {
                gfx.d2d
                    .CreateHwndRenderTarget(&props, &hprops)
                    .and_then(|rt| {
                        let white = D2D1_COLOR_F {
                            r: 1.0,
                            g: 1.0,
                            b: 1.0,
                            a: 1.0,
                        };
                        let b = rt.CreateSolidColorBrush(&white, None)?;
                        Ok((rt, b))
                    })
            };
            self.bitmaps.borrow_mut().clear();
            match created {
                Ok(t) => self.target = Some(t),
                Err(e) => {
                    tracing::error!(error = %e, "CreateHwndRenderTarget failed");
                    return None;
                }
            }
        }
        self.target.clone()
    }

    /// Draw a frame. Call from WM_PAINT (after BeginPaint).
    pub fn draw(&mut self, gfx: &Gfx, bg: Color, f: impl FnOnce(&Painter<'_>)) {
        let Some((rt, brush)) = self.ensure(gfx) else {
            return;
        };
        // SAFETY: standard BeginDraw/EndDraw pairing on a live target.
        let result = unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&bg.d2d()));
            if self.bitmaps.borrow().len() > 512 {
                self.bitmaps.borrow_mut().clear();
            }
            f(&Painter {
                rt: &rt,
                brush: &brush,
                gfx,
                bitmaps: &self.bitmaps,
            });
            rt.EndDraw(None, None)
        };
        if let Err(e) = result {
            // D2DERR_RECREATE_TARGET and friends: rebuild next frame.
            tracing::debug!(error = %e, "EndDraw failed; recreating render target");
            self.target = None;
        }
    }
}

pub struct Painter<'a> {
    rt: &'a ID2D1HwndRenderTarget,
    brush: &'a ID2D1SolidColorBrush,
    gfx: &'a Gfx,
    bitmaps: &'a RefCell<HashMap<u64, ID2D1Bitmap>>,
}

fn d2r(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.x + r.w,
        bottom: r.y + r.h,
    }
}

impl Painter<'_> {
    pub fn fill(&self, r: Rect, c: Color) {
        // SAFETY: drawing on a target inside BeginDraw.
        unsafe {
            self.brush.SetColor(&c.d2d());
            self.rt.FillRectangle(&d2r(r), self.brush);
        }
    }

    pub fn fill_rounded(&self, r: Rect, radius: f32, c: Color) {
        // SAFETY: drawing on a target inside BeginDraw.
        unsafe {
            self.brush.SetColor(&c.d2d());
            self.rt.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: d2r(r),
                    radiusX: radius,
                    radiusY: radius,
                },
                self.brush,
            );
        }
    }

    /// 1-px (scaled) outline of a rounded rectangle.
    pub fn stroke_rounded(&self, r: Rect, radius: f32, width: f32, c: Color) {
        // SAFETY: drawing on a target inside BeginDraw.
        unsafe {
            self.brush.SetColor(&c.d2d());
            self.rt.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: d2r(r),
                    radiusX: radius,
                    radiusY: radius,
                },
                self.brush,
                width,
                None,
            );
        }
    }

    /// Run `f` with drawing clipped to `r`.
    pub fn clipped(&self, r: Rect, f: impl FnOnce(&Self)) {
        // SAFETY: balanced push/pop on a target inside BeginDraw.
        unsafe {
            self.rt
                .PushAxisAlignedClip(&d2r(r), D2D1_ANTIALIAS_MODE_ALIASED);
        }
        f(self);
        // SAFETY: matches the push above.
        unsafe { self.rt.PopAxisAlignedClip() };
    }

    /// Draw an image scaled into `r` (aspect preserved, centred).
    pub fn image(&self, img: &SharedImage, r: Rect) {
        let bmp = {
            let cached = self.bitmaps.borrow().get(&img.id).cloned();
            match cached {
                Some(b) => b,
                None => {
                    let props = D2D1_BITMAP_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                        },
                        dpiX: 96.0,
                        dpiY: 96.0,
                    };
                    // SAFETY: pixel buffer matches width*height*4 with the
                    // given pitch.
                    let created = unsafe {
                        self.rt.CreateBitmap(
                            D2D_SIZE_U {
                                width: img.width,
                                height: img.height,
                            },
                            Some(img.pixels.as_ptr() as *const core::ffi::c_void),
                            img.width * 4,
                            &props,
                        )
                    };
                    match created {
                        Ok(b) => {
                            self.bitmaps.borrow_mut().insert(img.id, b.clone());
                            b
                        }
                        Err(_) => return,
                    }
                }
            }
        };
        let side = r.w.min(r.h);
        let dst = Rect::new(
            r.x + (r.w - side) / 2.0,
            r.y + (r.h - side) / 2.0,
            side,
            side,
        );
        // SAFETY: drawing on a target inside BeginDraw.
        unsafe {
            self.rt.DrawBitmap(
                &bmp,
                Some(&d2r(dst)),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
    }

    /// Glyph from the system icon font.
    pub fn glyph(&self, g: &str, r: Rect, size: f32, c: Color) {
        self.text_with(Font::Icons, g, r, size, c, Align::Center, false);
    }

    /// Single line of text, vertically centred in `r`, ellipsised.
    pub fn text(&self, s: &str, r: Rect, size: f32, c: Color, align: Align, bold: bool) {
        self.text_with(Font::Text, s, r, size, c, align, bold);
    }

    #[allow(clippy::too_many_arguments)]
    fn text_with(
        &self,
        font: Font,
        s: &str,
        r: Rect,
        size: f32,
        c: Color,
        align: Align,
        bold: bool,
    ) {
        let Some(fmt) = self.gfx.format(font, size, bold, align) else {
            return;
        };
        let utf16: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: drawing on a target inside BeginDraw.
        unsafe {
            self.brush.SetColor(&c.d2d());
            self.rt.DrawText(
                &utf16,
                &fmt,
                &d2r(r),
                self.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}
