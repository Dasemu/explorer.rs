#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate assets/explorer-rs.ico reproducibly (Pillow >= 9).

Design: a rounded square with a deep-blue to teal diagonal gradient and three
white, left-aligned rounded bars (the middle one shorter): an abstract "e" /
list glyph. Original artwork; it intentionally resembles no operating-system
logo or trademark.

Each size is rendered at 16x supersampling and downscaled, so small sizes stay
reasonably crisp without hand-tuned pixels. Output is deterministic for a
given Pillow version.

    python3 assets/make-icon.py             # writes assets/explorer-rs.ico
    python3 assets/make-icon.py --png DIR   # also writes DIR/explorer-rs-<n>.png
"""
import argparse
import os

from PIL import Image, ImageDraw

SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
TOP_LEFT = (0x1E, 0x3A, 0x8A)  # deep blue
BOTTOM_RIGHT = (0x0F, 0x9E, 0xA8)  # teal
SS = 16  # supersampling factor


def gradient(n):
    """Diagonal gradient, n x n, RGBA."""
    img = Image.new("RGBA", (n, n))
    px = img.load()
    for y in range(n):
        for x in range(n):
            t = (x + y) / (2 * (n - 1))
            px[x, y] = tuple(
                round(a + (b - a) * t) for a, b in zip(TOP_LEFT, BOTTOM_RIGHT)
            ) + (255,)
    return img


def render(size):
    n = size * SS
    # Gradient computed at low resolution and upscaled: smooth and fast.
    bg = gradient(64).resize((n, n), Image.BILINEAR)
    small = size <= 24
    margin = round(n * (0.03 if small else 0.06))
    radius = round(n * 0.22)
    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (margin, margin, n - 1 - margin, n - 1 - margin), radius=radius, fill=255
    )
    tile = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    tile.paste(bg, (0, 0), mask)

    # Small sizes use a slightly heavier glyph relative to the tile.
    d = ImageDraw.Draw(tile)
    inner = n - 2 * margin
    bar_h = inner * (0.15 if small else 0.12)
    gap = inner * (0.11 if small else 0.10)
    left = margin + inner * 0.22
    full = inner * 0.56
    total_h = 3 * bar_h + 2 * gap
    top = margin + (inner - total_h) / 2
    for i, frac in enumerate((1.0, 0.68, 1.0)):
        y0 = top + i * (bar_h + gap)
        d.rounded_rectangle(
            (round(left), round(y0), round(left + full * frac), round(y0 + bar_h)),
            radius=round(bar_h / 2),
            fill=(255, 255, 255, 255),
        )
    return tile.resize((size, size), Image.LANCZOS)


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=os.path.join(here, "explorer-rs.ico"))
    ap.add_argument("--png", help="also write PNG previews to this folder")
    a = ap.parse_args()
    images = [render(s) for s in SIZES]
    images[-1].save(
        a.out,
        format="ICO",
        sizes=[(s, s) for s in SIZES],
        append_images=images[:-1],
    )
    if a.png:
        os.makedirs(a.png, exist_ok=True)
        for s, img in zip(SIZES, images):
            img.save(os.path.join(a.png, f"explorer-rs-{s}.png"))
    print(f"wrote {a.out}")


if __name__ == "__main__":
    main()
