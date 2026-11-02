// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-side images (icons) shared between threads and render targets.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 32-bit BGRA, premultiplied alpha, top-down rows, tightly packed.
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    /// Unique id, used as a cache key for GPU copies.
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub type SharedImage = Arc<Image>;

impl Image {
    /// Build from BGRA pixels whose alpha may be straight or premultiplied
    /// (Windows APIs are inconsistent); see [`normalize_alpha`].
    pub fn from_bgra(width: u32, height: u32, mut pixels: Vec<u8>) -> Option<SharedImage> {
        if width == 0 || height == 0 || pixels.len() != (width * height * 4) as usize {
            return None;
        }
        normalize_alpha(&mut pixels);
        Some(Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            pixels,
        }))
    }

    /// True if every pixel is fully transparent.
    pub fn is_blank(&self) -> bool {
        self.pixels.chunks_exact(4).all(|p| p[3] == 0)
    }
}

/// Make BGRA pixels premultiplied.
///
/// - All alpha 0 (legacy icons without an alpha channel): treat every pixel
///   with any colour as opaque.
/// - Any colour channel greater than its alpha: the data cannot be
///   premultiplied, so it is straight alpha; premultiply it.
/// - Otherwise assume it is already premultiplied.
pub fn normalize_alpha(px: &mut [u8]) {
    let all_zero_alpha = px.chunks_exact(4).all(|p| p[3] == 0);
    if all_zero_alpha {
        for p in px.chunks_exact_mut(4) {
            if p[0] | p[1] | p[2] != 0 {
                p[3] = 255;
            }
        }
        return;
    }
    let straight = px
        .chunks_exact(4)
        .any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
    if straight {
        for p in px.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_alpha_is_premultiplied() {
        let mut px = vec![200, 100, 50, 128, 0, 0, 0, 0];
        normalize_alpha(&mut px);
        assert_eq!(&px[..4], &[100, 50, 25, 128]);
    }

    #[test]
    fn premultiplied_untouched() {
        let mut px = vec![100, 50, 25, 128, 255, 255, 255, 255];
        let orig = px.clone();
        normalize_alpha(&mut px);
        assert_eq!(px, orig);
    }

    #[test]
    fn no_alpha_channel_becomes_opaque() {
        let mut px = vec![10, 20, 30, 0, 0, 0, 0, 0];
        normalize_alpha(&mut px);
        assert_eq!(px, vec![10, 20, 30, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn from_bgra_validates_size_and_ids_are_unique() {
        assert!(Image::from_bgra(2, 2, vec![0; 3]).is_none());
        let a = Image::from_bgra(1, 1, vec![0, 0, 0, 0]).unwrap();
        let b = Image::from_bgra(1, 1, vec![1, 1, 1, 255]).unwrap();
        assert_ne!(a.id, b.id);
        assert!(a.is_blank() && !b.is_blank());
    }
}
