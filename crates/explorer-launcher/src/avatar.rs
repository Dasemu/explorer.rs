// SPDX-License-Identifier: GPL-3.0-or-later
//! Round account picture for the Start menu footer.
//!
//! The renderer only draws rectangles, so the picture is cropped to a
//! centred square and masked to a circle (with a one-pixel anti-aliased
//! edge) on the CPU, once, on the worker thread.

use explorer_core::image::{Image, SharedImage};

/// Crop `img` to a centred square and make everything outside the inscribed
/// circle transparent. Pixels are premultiplied, so every channel is scaled
/// by the coverage.
pub fn circle(img: &Image) -> Option<SharedImage> {
    let side = img.width.min(img.height);
    if side == 0 || img.pixels.len() != (img.width * img.height * 4) as usize {
        return None;
    }
    let x0 = (img.width - side) / 2;
    let y0 = (img.height - side) / 2;
    let r = side as f32 / 2.0;
    let mut out = vec![0u8; (side * side * 4) as usize];
    for y in 0..side {
        for x in 0..side {
            let dx = x as f32 + 0.5 - r;
            let dy = y as f32 + 0.5 - r;
            let d = (dx * dx + dy * dy).sqrt();
            let coverage = (r - d + 0.5).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let src = (((y + y0) * img.width + (x + x0)) * 4) as usize;
            let dst = ((y * side + x) * 4) as usize;
            for c in 0..4 {
                out[dst + c] = (img.pixels[src + c] as f32 * coverage).round() as u8;
            }
        }
    }
    Image::from_bgra(side, side, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_and_masks() {
        // 24x20 opaque white image -> 20x20 circle.
        let img = Image::from_bgra(24, 20, vec![255; 24 * 20 * 4]).unwrap();
        let c = circle(&img).unwrap();
        assert_eq!((c.width, c.height), (20, 20));
        // Corner transparent, centre opaque.
        assert_eq!(&c.pixels[0..4], &[0, 0, 0, 0]);
        let centre = ((10 * 20 + 10) * 4) as usize;
        assert_eq!(c.pixels[centre + 3], 255);
        // Premultiplied: no channel exceeds alpha.
        for p in c.pixels.chunks_exact(4) {
            assert!(p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]);
        }
    }

    #[test]
    fn rejects_bad_images() {
        let bad = Image {
            id: 0,
            width: 2,
            height: 2,
            pixels: vec![0; 3],
        };
        assert!(circle(&bad).is_none());
    }
}
