// SPDX-License-Identifier: GPL-3.0-or-later
//! Live window thumbnails through the documented DWM thumbnail API
//! (`DwmRegisterThumbnail`, `DwmUpdateThumbnailProperties`,
//! `DwmQueryThumbnailSourceSize`, `DwmUnregisterThumbnail`).
//!
//! DWM composes the source window's image into a rectangle of one of our
//! top-level windows; we never copy pixels. For minimized windows DWM has no
//! current content to show (Windows' own taskbar asks the app for an
//! "iconic" bitmap); the source size then reports the tiny minimized frame,
//! which callers detect with [`Thumbnail::is_usable`] and replace with the
//! window icon and title.

use explorer_core::layout::Rect;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY,
    DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};

/// A registered thumbnail; unregistered on drop.
pub struct Thumbnail {
    id: isize,
}

impl Thumbnail {
    /// Register `source` to be drawn into `destination` (one of our own
    /// top-level windows).
    pub fn register(destination: HWND, source: HWND) -> Option<Self> {
        // SAFETY: plain DWM call; both handles are validated by DWM.
        match unsafe { DwmRegisterThumbnail(destination, source) } {
            Ok(id) if id != 0 => Some(Self { id }),
            Ok(_) => None,
            Err(e) => {
                tracing::debug!(error = %e, "DwmRegisterThumbnail failed");
                None
            }
        }
    }

    /// Size of the source window in pixels.
    pub fn source_size(&self) -> Option<(f32, f32)> {
        // SAFETY: id is a live registration.
        let s = unsafe { DwmQueryThumbnailSourceSize(self.id) }.ok()?;
        Some((s.cx as f32, s.cy as f32))
    }

    /// False if DWM has nothing meaningful to draw (minimized window: only
    /// a ~160x30 caption remains).
    pub fn is_usable(&self) -> bool {
        self.source_size()
            .is_some_and(|(w, h)| w >= 64.0 && h >= 48.0)
    }

    /// Show the thumbnail in `dest` (client coordinates of the destination
    /// window), or hide it.
    pub fn show(&self, dest: Option<Rect>) {
        let r = dest.unwrap_or_default();
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_VISIBLE
                | DWM_TNP_OPACITY
                | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: RECT {
                left: r.x.round() as i32,
                top: r.y.round() as i32,
                right: (r.x + r.w).round() as i32,
                bottom: (r.y + r.h).round() as i32,
            },
            opacity: 255,
            fVisible: dest.is_some().into(),
            fSourceClientAreaOnly: false.into(),
            ..Default::default()
        };
        // SAFETY: id is a live registration; props valid for the call.
        if let Err(e) = unsafe { DwmUpdateThumbnailProperties(self.id, &props) } {
            tracing::debug!(error = %e, "DwmUpdateThumbnailProperties failed");
        }
    }
}

impl Drop for Thumbnail {
    fn drop(&mut self) {
        // SAFETY: id came from DwmRegisterThumbnail and is released once.
        let _ = unsafe { DwmUnregisterThumbnail(self.id) };
    }
}
