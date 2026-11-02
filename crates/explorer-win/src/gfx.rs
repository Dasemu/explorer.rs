// SPDX-License-Identifier: GPL-3.0-or-later
//! Tiny Direct2D/DirectWrite renderer: filled rectangles and single-line
//! text. That is all the taskbar and launcher draw.
//!
//! Render targets use 96 DPI so one DIP equals one physical pixel; layout
//! code in `explorer-core` works in physical pixels with an explicit scale.

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::Result;
use explorer_core::layout::Rect;
use windows::core::w;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
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

/// Neutral dark palette, close to Windows 11 dark mode.
pub mod theme {
    use super::Color;
    pub const BAR_BG: Color = Color(0x202020);
    pub const BUTTON_BG: Color = Color(0x2b2b2b);
    pub const BUTTON_HOVER: Color = Color(0x383838);
    pub const BUTTON_ACTIVE: Color = Color(0x454545);
    pub const ACCENT: Color = Color(0x4cc2ff);
    pub const FLASH: Color = Color(0xc27c0e);
    pub const TEXT: Color = Color(0xffffff);
    pub const TEXT_DIM: Color = Color(0xa0a0a0);
    pub const PANEL_BG: Color = Color(0x2c2c2c);
    pub const SELECTION: Color = Color(0x3d3d3d);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub struct Gfx {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    formats: RefCell<HashMap<(u32, bool, Align), IDWriteTextFormat>>,
}

impl Gfx {
    pub fn new() -> Result<Self> {
        // SAFETY: factory creation with valid enum values.
        unsafe {
            Ok(Self {
                d2d: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                dwrite: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
                formats: RefCell::new(HashMap::new()),
            })
        }
    }

    fn format(&self, size: f32, bold: bool, align: Align) -> Option<IDWriteTextFormat> {
        let key = ((size * 4.0) as u32, bold, align);
        if let Some(f) = self.formats.borrow().get(&key) {
            return Some(f.clone());
        }
        // SAFETY: DirectWrite calls with valid arguments.
        let f = unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(
                    w!("Segoe UI"),
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
        Self { hwnd, target: None }
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
                        let b = rt.CreateSolidColorBrush(&theme::TEXT.d2d(), None)?;
                        Ok((rt, b))
                    })
            };
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
            f(&Painter {
                rt: &rt,
                brush: &brush,
                gfx,
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

    /// Single line of text, vertically centred in `r`, ellipsised.
    pub fn text(&self, s: &str, r: Rect, size: f32, c: Color, align: Align, bold: bool) {
        let Some(fmt) = self.gfx.format(size, bold, align) else {
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
