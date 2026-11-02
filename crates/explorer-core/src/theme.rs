// SPDX-License-Identifier: GPL-3.0-or-later
//! Colour palette derived from the Windows personalisation settings.
//!
//! `explorer-win::theme` reads the registry (light/dark, accent colour,
//! "show accent colour on Start and taskbar"); this module turns those
//! inputs into concrete colours. Colours are 0xRRGGBB.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemTheme {
    /// "Choose your Windows mode" = light (taskbar, Start).
    pub light: bool,
    /// Accent colour, 0xRRGGBB.
    pub accent: u32,
    /// "Show accent colour on Start and taskbar".
    pub accent_on_taskbar: bool,
}

impl Default for SystemTheme {
    fn default() -> Self {
        Self {
            light: false,
            accent: 0x0078D4,
            accent_on_taskbar: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub bar_bg: u32,
    pub button_hover: u32,
    pub button_active: u32,
    pub panel_bg: u32,
    pub input_bg: u32,
    pub selection: u32,
    pub text: u32,
    pub text_dim: u32,
    pub accent: u32,
    /// Accent variant readable on `panel_bg` / `bar_bg`.
    pub accent_text: u32,
    pub running_indicator: u32,
    pub flash: u32,
    pub border: u32,
    pub light: bool,
}

fn rgb(c: u32) -> (f32, f32, f32) {
    (
        ((c >> 16) & 0xff) as f32,
        ((c >> 8) & 0xff) as f32,
        (c & 0xff) as f32,
    )
}

fn pack(r: f32, g: f32, b: f32) -> u32 {
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u32;
    (c(r) << 16) | (c(g) << 8) | c(b)
}

/// Linear blend: t=0 -> a, t=1 -> b.
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    pack(ar + (br - ar) * t, ag + (bg - ag) * t, ab + (bb - ab) * t)
}

/// Relative luminance (sRGB approximation), 0..1.
pub fn luminance(c: u32) -> f32 {
    let (r, g, b) = rgb(c);
    (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0
}

/// Theme override from the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    System,
    Dark,
    Light,
}

impl Palette {
    pub fn from_system(t: &SystemTheme, mode: Mode) -> Self {
        let light = match mode {
            Mode::System => t.light,
            Mode::Dark => false,
            Mode::Light => true,
        };
        let (bar, panel, text, text_dim) = if light {
            (0xEEEEEE, 0xF3F3F3, 0x1B1B1B, 0x5F5F5F)
        } else {
            (0x1C1C1C, 0x242424, 0xFFFFFF, 0xC5C5C5)
        };
        // Accent on taskbar only applies in dark mode on Windows 11.
        let bar_bg = if t.accent_on_taskbar && !light {
            mix(t.accent, 0x000000, 0.55)
        } else {
            bar
        };
        let panel_bg = if t.accent_on_taskbar && !light {
            mix(t.accent, 0x000000, 0.6)
        } else {
            panel
        };
        let overlay = if light { 0x000000 } else { 0xFFFFFF };
        let accent_text = if light {
            mix(t.accent, 0x000000, 0.25)
        } else if luminance(t.accent) < 0.45 {
            mix(t.accent, 0xFFFFFF, 0.45)
        } else {
            t.accent
        };
        Self {
            bar_bg,
            button_hover: mix(bar_bg, overlay, 0.08),
            button_active: mix(bar_bg, overlay, 0.13),
            panel_bg,
            input_bg: if light {
                0xFFFFFF
            } else {
                mix(panel_bg, 0x000000, 0.25)
            },
            selection: mix(panel_bg, overlay, 0.09),
            text,
            text_dim,
            accent: t.accent,
            accent_text,
            running_indicator: mix(bar_bg, overlay, 0.45),
            flash: 0xC27C0E,
            border: mix(panel_bg, overlay, 0.12),
            light,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_endpoints() {
        assert_eq!(mix(0x000000, 0xFFFFFF, 0.0), 0x000000);
        assert_eq!(mix(0x000000, 0xFFFFFF, 1.0), 0xFFFFFF);
        assert_eq!(mix(0x000000, 0xFFFFFF, 0.5), 0x808080);
    }

    #[test]
    fn dark_and_light() {
        let t = SystemTheme::default();
        let d = Palette::from_system(&t, Mode::System);
        assert!(!d.light && luminance(d.bar_bg) < 0.2 && luminance(d.text) > 0.9);
        let l = Palette::from_system(&t, Mode::Light);
        assert!(l.light && luminance(l.bar_bg) > 0.8 && luminance(l.text) < 0.2);
        let lt = SystemTheme { light: true, ..t };
        assert!(Palette::from_system(&lt, Mode::System).light);
        assert!(!Palette::from_system(&lt, Mode::Dark).light);
    }

    #[test]
    fn accent_on_taskbar_tints_dark_bar() {
        let t = SystemTheme {
            light: false,
            accent: 0x0078D4,
            accent_on_taskbar: true,
        };
        let p = Palette::from_system(&t, Mode::System);
        assert_ne!(p.bar_bg, 0x1C1C1C);
        // Dark accent text is lightened for contrast on dark panels.
        assert!(luminance(p.accent_text) > luminance(0x0078D4));
    }
}
