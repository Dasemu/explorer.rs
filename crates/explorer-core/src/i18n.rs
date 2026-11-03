// SPDX-License-Identifier: GPL-3.0-or-later
//! User-interface language.
//!
//! There is deliberately no central string table: each call site passes
//! both translations (`tr(lang, "Pinned", "Anclado")`). Every string is
//! therefore translated where it is written, features never conflict over
//! one shared file, and a missing translation is impossible by construction.
//!
//! Supported: English and Spanish. Any other Windows UI language falls back
//! to English.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    Es,
}

/// Config value: follow Windows or force a language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguageSetting {
    #[default]
    System,
    En,
    Es,
}

/// Primary language id of a Windows LANGID (low 10 bits).
const LANG_SPANISH: u16 = 0x0A;

impl Lang {
    /// From a Windows LANGID as returned by `GetUserDefaultUILanguage`.
    pub fn from_langid(langid: u16) -> Self {
        match langid & 0x3FF {
            LANG_SPANISH => Lang::Es,
            _ => Lang::En,
        }
    }

    /// Resolve the config setting; `system_langid` is only consulted for
    /// [`LanguageSetting::System`].
    pub fn resolve(setting: LanguageSetting, system_langid: impl FnOnce() -> u16) -> Self {
        match setting {
            LanguageSetting::System => Self::from_langid(system_langid()),
            LanguageSetting::En => Lang::En,
            LanguageSetting::Es => Lang::Es,
        }
    }
}

/// Pick the string for `lang`.
#[inline]
pub fn tr(lang: Lang, en: &'static str, es: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Es => es,
    }
}

/// Owned variant for formatted strings: `trf(lang, format!(..), format!(..))`.
pub fn trf(lang: Lang, en: String, es: String) -> String {
    match lang {
        Lang::En => en,
        Lang::Es => es,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn langid_detection() {
        assert_eq!(Lang::from_langid(0x0C0A), Lang::Es); // es-ES
        assert_eq!(Lang::from_langid(0x080A), Lang::Es); // es-MX
        assert_eq!(Lang::from_langid(0x0409), Lang::En); // en-US
        assert_eq!(Lang::from_langid(0x040C), Lang::En); // fr-FR -> fallback
    }

    #[test]
    fn resolve_and_tr() {
        assert_eq!(Lang::resolve(LanguageSetting::Es, || 0x0409), Lang::Es);
        assert_eq!(Lang::resolve(LanguageSetting::System, || 0x0C0A), Lang::Es);
        assert_eq!(tr(Lang::Es, "Pinned", "Anclado"), "Anclado");
        assert_eq!(trf(Lang::En, "a".into(), "b".into()), "a");
    }
}
