// SPDX-License-Identifier: GPL-3.0-or-later
//! Glyphs and tooltips of the taskbar's system area (network, volume,
//! battery). Code points are from "Segoe Fluent Icons" (Windows 11), which
//! shares them with "Segoe MDL2 Assets" (Windows 10).

use crate::i18n::{tr, trf, Lang};

pub const WIFI: &str = "\u{E701}";
pub const ETHERNET: &str = "\u{E839}";
/// "Network offline" (globe with a slash).
pub const NETWORK_OFFLINE: &str = "\u{F384}";
pub const VOLUME_MUTE: &str = "\u{E74F}";
pub const VOLUME_0: &str = "\u{E992}";
pub const VOLUME_1: &str = "\u{E993}";
pub const VOLUME_2: &str = "\u{E994}";
pub const VOLUME_3: &str = "\u{E995}";
pub const BRIGHTNESS: &str = "\u{E706}";
pub const SETTINGS: &str = "\u{E713}";
pub const CHEVRON_UP: &str = "\u{E70E}";
pub const CHEVRON_DOWN: &str = "\u{E70D}";
pub const CHEVRON_RIGHT: &str = "\u{E76C}";
pub const MORE: &str = "\u{E712}";
pub const CLOSE: &str = "\u{E8BB}";
pub const BATTERY_SAVER: &str = "\u{EBA0}";

/// Network state as shown by the glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetKind {
    Disconnected,
    /// Connected without internet access.
    NoInternet {
        wifi: bool,
    },
    Internet {
        wifi: bool,
    },
}

pub fn network_glyph(k: NetKind) -> &'static str {
    match k {
        NetKind::Disconnected => NETWORK_OFFLINE,
        NetKind::NoInternet { wifi } | NetKind::Internet { wifi } => {
            if wifi {
                WIFI
            } else {
                ETHERNET
            }
        }
    }
}

/// Status line ("Connected, internet access" ...).
pub fn network_status(lang: Lang, k: NetKind) -> &'static str {
    match k {
        NetKind::Disconnected => tr(lang, "Not connected", "Sin conexión"),
        NetKind::NoInternet { .. } => tr(lang, "No internet", "Sin internet"),
        NetKind::Internet { .. } => tr(lang, "Internet access", "Acceso a Internet"),
    }
}

/// Tooltip: network name(s) and status.
pub fn network_tooltip(lang: Lang, k: NetKind, names: &[String]) -> String {
    let status = network_status(lang, k);
    if names.is_empty() || k == NetKind::Disconnected {
        status.to_string()
    } else {
        format!("{}\n{status}", names.join(", "))
    }
}

/// Speaker glyph for a level in 0..=1.
pub fn volume_glyph(level: f32, muted: bool) -> &'static str {
    if muted {
        VOLUME_MUTE
    } else if level <= 0.0 {
        VOLUME_0
    } else if level < 0.34 {
        VOLUME_1
    } else if level < 0.67 {
        VOLUME_2
    } else {
        VOLUME_3
    }
}

pub fn percent(level: f32) -> u32 {
    (level.clamp(0.0, 1.0) * 100.0).round() as u32
}

pub fn volume_tooltip(lang: Lang, device: &str, level: f32, muted: bool) -> String {
    let p = percent(level);
    let state = if muted {
        trf(lang, format!("{p}% (muted)"), format!("{p}% (silenciado)"))
    } else {
        format!("{p}%")
    };
    if device.is_empty() {
        trf(
            lang,
            format!("Volume: {state}"),
            format!("Volumen: {state}"),
        )
    } else {
        format!("{device}: {state}")
    }
}

/// Battery glyph: Battery0..Battery10 (E850..E859, E83F) and
/// BatteryCharging0..10 (E85A..E862, E83E, EA93).
pub fn battery_glyph(percent: Option<u8>, charging: bool) -> &'static str {
    const PLAIN: [&str; 11] = [
        "\u{E850}", "\u{E851}", "\u{E852}", "\u{E853}", "\u{E854}", "\u{E855}", "\u{E856}",
        "\u{E857}", "\u{E858}", "\u{E859}", "\u{E83F}",
    ];
    const CHARGING: [&str; 11] = [
        "\u{E85A}", "\u{E85B}", "\u{E85C}", "\u{E85D}", "\u{E85E}", "\u{E85F}", "\u{E860}",
        "\u{E861}", "\u{E862}", "\u{E83E}", "\u{EA93}",
    ];
    let step = (percent.unwrap_or(100).min(100) as usize + 5) / 10;
    if charging {
        CHARGING[step]
    } else {
        PLAIN[step]
    }
}

pub fn battery_tooltip(lang: Lang, percent: Option<u8>, charging: bool, on_ac: bool) -> String {
    let p = percent
        .map(|p| format!("{p}%"))
        .unwrap_or_else(|| "?".into());
    let state = match (charging, on_ac) {
        (true, _) => tr(lang, "charging", "cargando"),
        (false, true) => tr(lang, "plugged in", "conectado"),
        (false, false) => tr(lang, "on battery", "con batería"),
    };
    trf(
        lang,
        format!("Battery: {p} ({state})"),
        format!("Batería: {p} ({state})"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_levels() {
        assert_eq!(volume_glyph(0.5, true), VOLUME_MUTE);
        assert_eq!(volume_glyph(0.0, false), VOLUME_0);
        assert_eq!(volume_glyph(0.2, false), VOLUME_1);
        assert_eq!(volume_glyph(0.5, false), VOLUME_2);
        assert_eq!(volume_glyph(1.0, false), VOLUME_3);
        assert_eq!(percent(0.456), 46);
        assert_eq!(
            volume_tooltip(Lang::Es, "Altavoces", 0.5, true),
            "Altavoces: 50% (silenciado)"
        );
        assert_eq!(volume_tooltip(Lang::En, "", 0.25, false), "Volume: 25%");
    }

    #[test]
    fn battery_levels() {
        assert_eq!(battery_glyph(Some(100), false), "\u{E83F}");
        assert_eq!(battery_glyph(Some(0), false), "\u{E850}");
        assert_eq!(battery_glyph(Some(94), true), "\u{E83E}");
        assert_eq!(battery_glyph(Some(96), true), "\u{EA93}");
        assert_eq!(battery_glyph(None, false), "\u{E83F}");
        assert_eq!(battery_glyph(Some(250), false), "\u{E83F}");
        assert_eq!(
            battery_tooltip(Lang::Es, Some(80), true, true),
            "Batería: 80% (cargando)"
        );
    }

    #[test]
    fn network() {
        assert_eq!(network_glyph(NetKind::Disconnected), NETWORK_OFFLINE);
        assert_eq!(network_glyph(NetKind::Internet { wifi: true }), WIFI);
        assert_eq!(network_glyph(NetKind::NoInternet { wifi: false }), ETHERNET);
        assert_eq!(
            network_tooltip(Lang::En, NetKind::Internet { wifi: true }, &["Home".into()]),
            "Home\nInternet access"
        );
        assert_eq!(
            network_tooltip(Lang::Es, NetKind::Disconnected, &["x".into()]),
            "Sin conexión"
        );
    }
}
