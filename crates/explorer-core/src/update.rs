// SPDX-License-Identifier: GPL-3.0-or-later
//! Self-update: the portable decisions.
//!
//! explorer.rs can check GitHub for a newer release and install it with the
//! release's own setup program (`--silent`, which keeps the installed start
//! mode and never touches the user's settings). Everything that can be
//! decided without Windows lives here and is unit tested:
//!
//! - [`Version`]: `X.Y.Z` with an optional `v`; anything else (pre-release
//!   suffixes included) is not a version we offer;
//! - [`parse_release`]: GitHub's "latest release" JSON -> the assets to
//!   download, accepting only this repository's HTTPS download URLs;
//! - [`parse_sha256sums`] / [`verify_setup`]: the installer must match the
//!   release's `SHA256SUMS.txt`;
//! - [`UpdateState`]: when the last check ran, what it found, and when the
//!   user was last reminded (or asked not to be) — the rules
//!   [`should_check`] and [`should_remind`] decide from it.
//!
//! The network (WinHTTP), the elevation prompt and the user interface are in
//! `explorer_win::update`, the settings window and the taskbar.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::i18n::{tr, trf, Lang};

/// The only repository updates are taken from.
pub const REPO: &str = "FerroApps/explorer.rs";
/// GitHub API: the newest published, non-draft, non-prerelease release.
pub const LATEST_URL: &str = "https://api.github.com/repos/FerroApps/explorer.rs/releases/latest";
/// Human-readable list of releases (fallback when installing fails).
pub const RELEASES_PAGE: &str = "https://github.com/FerroApps/explorer.rs/releases";
/// Every accepted asset URL starts with one of these, followed by
/// `<tag>/<name>`. The repository moved from Dasemu to FerroApps; GitHub
/// redirects the old name, and release JSON may name either owner.
const DOWNLOAD_PREFIXES: &[&str] = &[
    "https://github.com/FerroApps/explorer.rs/releases/download/",
    "https://github.com/Dasemu/explorer.rs/releases/download/",
];
pub const SUMS_NAME: &str = "SHA256SUMS.txt";

/// Size limits for what is downloaded. The installer is a few tens of MB;
/// the limits only stop a broken or hostile response from filling the disk.
pub const MAX_RELEASE_JSON_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_SUMS_BYTES: u64 = 64 * 1024;
pub const MAX_SETUP_BYTES: u64 = 300 * 1024 * 1024;

/// Automatic checks run at most this often.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// A failed check is retried after this long.
pub const RETRY_INTERVAL_SECS: u64 = 60 * 60;
/// While an update is available and not dismissed, the reminder comes back
/// at most this often.
pub const REMIND_INTERVAL_SECS: u64 = 3 * 24 * 60 * 60;

/// A release version, `major.minor.patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// `1.2.3` or `v1.2.3`. Pre-release (`-rc1`) and build (`+x`) suffixes
    /// are rejected: only final releases are offered.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s
            .strip_prefix('v')
            .or_else(|| s.strip_prefix('V'))
            .unwrap_or(s);
        let mut parts = s.split('.');
        let mut num = || -> Option<u32> {
            let p = parts.next()?;
            if p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let v = Self {
            major: num()?,
            minor: num()?,
            patch: num()?,
        };
        parts.next().is_none().then_some(v)
    }

    /// The version of this build.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self {
            major: 0,
            minor: 0,
            patch: 0,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// File name of the installer of `v`, as `.github/workflows/release.yml`
/// (via scripts/build-windows.sh) publishes it.
pub fn setup_asset_name(v: Version) -> String {
    format!("explorer-rs-setup-{v}-x64.exe")
}

/// One downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// What a release offers, already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// The release's page on github.com (built from the tag, not taken
    /// from the response, so it can only point at this repository).
    pub page: String,
    /// A few plain-text lines from the release notes.
    pub notes: String,
    pub setup: Asset,
    pub sums: Asset,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReleaseError {
    #[error("unreadable release information: {0}")]
    Json(String),
    #[error("the latest release is a draft or a pre-release")]
    NotFinal,
    #[error("unexpected release tag {0:?}")]
    BadTag(String),
    #[error("the release has no {0}")]
    MissingAsset(String),
    #[error("unexpected download address for {0}")]
    BadUrl(String),
    #[error("{0} has an unexpected size")]
    BadSize(String),
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    #[serde(default)]
    size: u64,
    browser_download_url: String,
}

/// Parse GitHub's release JSON and pick the installer and the checksum
/// file. Asset URLs must be exactly this repository's HTTPS download URL
/// for the release's tag and the asset's name (GitHub then redirects to
/// its storage host, which the HTTP layer follows over HTTPS only).
pub fn parse_release(json: &str) -> Result<Release, ReleaseError> {
    let raw: RawRelease =
        serde_json::from_str(json).map_err(|e| ReleaseError::Json(e.to_string()))?;
    if raw.draft || raw.prerelease {
        return Err(ReleaseError::NotFinal);
    }
    let version =
        Version::parse(&raw.tag_name).ok_or(ReleaseError::BadTag(raw.tag_name.clone()))?;
    // Tags are `vX.Y.Z` (release.yml); the tag is part of the URLs below.
    let tag = format!("v{version}");
    if raw.tag_name != tag {
        return Err(ReleaseError::BadTag(raw.tag_name));
    }
    let pick = |name: &str, max: u64| -> Result<Asset, ReleaseError> {
        let a = raw
            .assets
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| ReleaseError::MissingAsset(name.to_string()))?;
        let suffix = format!("{tag}/{name}");
        let url_ok = DOWNLOAD_PREFIXES.iter().any(|p| {
            a.browser_download_url
                .strip_prefix(p)
                .is_some_and(|rest| rest == suffix)
        });
        if !url_ok {
            return Err(ReleaseError::BadUrl(name.to_string()));
        }
        if a.size == 0 || a.size > max {
            return Err(ReleaseError::BadSize(name.to_string()));
        }
        Ok(Asset {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
            size: a.size,
        })
    };
    let setup = pick(&setup_asset_name(version), MAX_SETUP_BYTES)?;
    let sums = pick(SUMS_NAME, MAX_SUMS_BYTES)?;
    Ok(Release {
        version,
        page: format!("{RELEASES_PAGE}/tag/{tag}"),
        tag,
        notes: notes_excerpt(raw.body.as_deref().unwrap_or("")),
        setup,
        sums,
    })
}

/// Most lines and characters of release notes shown to the user.
const NOTES_MAX_LINES: usize = 6;
const NOTES_MAX_CHARS: usize = 480;

/// A short plain-text excerpt of release notes (Markdown): the bullet
/// points, without the fixed "Install" instructions every release carries,
/// the "by @user in <link>" credits GitHub generates and Markdown marks.
pub fn notes_excerpt(body: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut skip_section = false;
    let mut chars = 0;
    for line in body.lines() {
        let l = line.trim();
        if let Some(h) = l.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim().to_ascii_lowercase();
            skip_section = h == "install" || h == "instalar";
            continue;
        }
        if skip_section || l.is_empty() || l.starts_with("**Full Changelog**") {
            continue;
        }
        let bullet = l
            .strip_prefix("* ")
            .or_else(|| l.strip_prefix("- "))
            .or_else(|| l.strip_prefix("+ "));
        let Some(item) = bullet else { continue };
        let mut item = item.to_string();
        if let Some(i) = item.find(" by @") {
            item.truncate(i);
        }
        let item: String = item.replace("**", "").replace('`', "").trim().to_string();
        if item.is_empty() {
            continue;
        }
        let line = format!("• {item}");
        chars += line.chars().count();
        if out.len() == NOTES_MAX_LINES || chars > NOTES_MAX_CHARS {
            out.push("…".into());
            break;
        }
        out.push(line);
    }
    out.join("\n")
}

/// The SHA-256 listed for `name` in a `sha256sum` output (`<hex>  <name>`
/// or `<hex> *<name>`).
pub fn parse_sha256sums(text: &str, name: &str) -> Option<[u8; 32]> {
    text.lines().find_map(|line| {
        let (hex, rest) = line.trim().split_once(char::is_whitespace)?;
        let file = rest
            .trim_start()
            .strip_prefix('*')
            .unwrap_or(rest.trim_start());
        (file.trim_end() == name)
            .then(|| decode_hex32(hex))
            .flatten()
    })
}

fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        let hi = (b[2 * i] as char).to_digit(16)?;
        let lo = (b[2 * i + 1] as char).to_digit(16)?;
        *o = (hi * 16 + lo) as u8;
    }
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    #[error("SHA256SUMS.txt has no entry for {0}")]
    NotListed(String),
    #[error("the download has {got} bytes, the release says {expected}")]
    Size { got: u64, expected: u64 },
    #[error("the download does not match its SHA-256 checksum")]
    Mismatch,
}

/// The downloaded installer must have the size the release announced and
/// the SHA-256 the release's checksum file lists for its exact name.
pub fn verify_setup(release: &Release, sums_text: &str, setup: &[u8]) -> Result<(), VerifyError> {
    let expected = parse_sha256sums(sums_text, &release.setup.name)
        .ok_or_else(|| VerifyError::NotListed(release.setup.name.clone()))?;
    let got = setup.len() as u64;
    if got != release.setup.size {
        return Err(VerifyError::Size {
            got,
            expected: release.setup.size,
        });
    }
    let digest: [u8; 32] = Sha256::digest(setup).into();
    if digest != expected {
        return Err(VerifyError::Mismatch);
    }
    Ok(())
}

/// A release seen by the last successful check.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownRelease {
    pub version: String,
    pub page: String,
    pub notes: String,
}

impl KnownRelease {
    pub fn from_release(r: &Release) -> Self {
        Self {
            version: r.version.to_string(),
            page: r.page.clone(),
            notes: r.notes.clone(),
        }
    }

    pub fn parsed(&self) -> Option<Version> {
        Version::parse(&self.version)
    }
}

/// `%LOCALAPPDATA%\explorer-rs\update.toml`, written by the taskbar (daily
/// check, reminder) and the settings window ("Check now"). Times are Unix
/// seconds.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    /// Last check attempt (successful or not).
    pub last_check: Option<u64>,
    /// Last successful check.
    pub last_success: Option<u64>,
    /// Why the last check failed (cleared by a successful one).
    pub last_error: Option<String>,
    /// The latest release the last successful check found.
    pub latest: Option<KnownRelease>,
    /// The version the last reminder was about, and when it was shown.
    pub reminded_version: Option<String>,
    pub reminded_at: Option<u64>,
    /// "Don't remind me again" was chosen for this version.
    pub dismissed_version: Option<String>,
}

impl UpdateState {
    /// Missing or unreadable file: a fresh state (the next check rebuilds
    /// it; nothing in it is precious).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        crate::fsutil::write_atomic(path, text.as_bytes())
    }

    pub fn record_success(&mut self, now: u64, latest: &Release) {
        self.last_check = Some(now);
        self.last_success = Some(now);
        self.last_error = None;
        self.latest = Some(KnownRelease::from_release(latest));
    }

    pub fn record_failure(&mut self, now: u64, error: &str) {
        self.last_check = Some(now);
        self.last_error = Some(error.chars().take(300).collect());
    }

    /// The release to offer: the latest known one if newer than `current`.
    pub fn available(&self, current: Version) -> Option<(&KnownRelease, Version)> {
        let k = self.latest.as_ref()?;
        let v = k.parsed()?;
        (v > current).then_some((k, v))
    }

    pub fn mark_reminded(&mut self, version: Version, now: u64) {
        self.reminded_version = Some(version.to_string());
        self.reminded_at = Some(now);
    }

    pub fn dismiss(&mut self, version: Version) {
        self.dismissed_version = Some(version.to_string());
    }
}

/// Is an automatic check due? Once a day after a successful check; after
/// a failed one (offline at sign-in, say) again an hour later instead of
/// waiting a whole day. A clock that went backwards does not block checks.
pub fn should_check(state: &UpdateState, now: u64) -> bool {
    let since = |t: u64| (now >= t).then(|| now - t);
    if let Some(t) = state.last_check {
        if since(t).is_some_and(|d| d < RETRY_INTERVAL_SECS) {
            return false;
        }
    }
    match state.last_success {
        None => true,
        Some(t) => since(t).is_none_or(|d| d >= CHECK_INTERVAL_SECS),
    }
}

/// What the reminder rule needs besides the stored state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemindContext {
    /// `general.check_updates`.
    pub enabled: bool,
    /// A reminder was already shown in this sign-in session.
    pub shown_this_session: bool,
    /// A full-screen app (game, video, presentation) is in front.
    pub fullscreen: bool,
}

/// Show the "update available" reminder now?
///
/// - never when updates are off, during a full-screen app, or twice in a
///   sign-in session;
/// - never for a version the user said not to be reminded about (a newer
///   version reminds again);
/// - right away for a version not reminded about yet;
/// - otherwise again once [`REMIND_INTERVAL_SECS`] passed since the last
///   reminder ("Later" just closes it).
pub fn should_remind(state: &UpdateState, current: Version, now: u64, ctx: RemindContext) -> bool {
    if !ctx.enabled || ctx.shown_this_session || ctx.fullscreen {
        return false;
    }
    let Some((_, v)) = state.available(current) else {
        return false;
    };
    let same = |s: &Option<String>| s.as_deref().and_then(Version::parse) == Some(v);
    if same(&state.dismissed_version) {
        return false;
    }
    if !same(&state.reminded_version) {
        return true;
    }
    match state.reminded_at {
        None => true,
        Some(t) => now < t || now - t >= REMIND_INTERVAL_SECS,
    }
}

/// Current time as Unix seconds.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "5 minutes ago", "3 hours ago", "2 days ago".
pub fn ago(lang: Lang, then: u64, now: u64) -> String {
    let d = now.saturating_sub(then);
    let (n, en, es, en1, es1) = if d < 60 {
        return tr(lang, "just now", "hace un momento").into();
    } else if d < 3600 {
        (d / 60, "minutes", "minutos", "minute", "minuto")
    } else if d < 86_400 {
        (d / 3600, "hours", "horas", "hour", "hora")
    } else {
        (d / 86_400, "days", "días", "day", "día")
    };
    let (en, es) = if n == 1 { (en1, es1) } else { (en, es) };
    trf(lang, format!("{n} {en} ago"), format!("hace {n} {es}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions() {
        assert_eq!(
            v("v0.1.6"),
            Version {
                major: 0,
                minor: 1,
                patch: 6
            }
        );
        assert_eq!(v("0.1.6"), v("V0.1.6"));
        assert!(v("0.1.10") > v("0.1.9"));
        assert!(v("0.2.0") > v("0.1.99"));
        assert!(v("1.0.0") > v("0.99.99"));
        for bad in [
            "",
            "v",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.3-rc1",
            "1.2.3+b",
            "a.b.c",
            "1..3",
            " v1.2.x",
            "-1.2.3",
            "01234567890.0.0",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(v(" v1.2.3 ").to_string(), "1.2.3");
        assert!(Version::current() > v("0.0.0"));
    }

    fn release_json(tag: &str, setup_name: &str, url_tag: &str, extra: &str) -> String {
        format!(
            r###"{{
  "tag_name": "{tag}",
  "html_url": "https://evil.example/x",
  "draft": false,
  "prerelease": false,
  "body": "## Install\n\nDownload `explorer-rs-setup-*-x64.exe` and run it.\n* not a change\n\n## What's Changed\n* Taskbar: **faster** start by @someone in https://github.com/Dasemu/explorer.rs/pull/12\n* Fix `tray` icons\n\n**Full Changelog**: https://github.com/x",
  "assets": [
    {{ "name": "{setup_name}", "size": 1234,
       "browser_download_url": "https://github.com/Dasemu/explorer.rs/releases/download/{url_tag}/{setup_name}" }},
    {{ "name": "SHA256SUMS.txt", "size": 200,
       "browser_download_url": "https://github.com/Dasemu/explorer.rs/releases/download/{url_tag}/SHA256SUMS.txt" }}
    {extra}
  ]
}}"###
        )
    }

    #[test]
    fn release_is_parsed_and_validated() {
        let r = parse_release(&release_json(
            "v0.1.7",
            "explorer-rs-setup-0.1.7-x64.exe",
            "v0.1.7",
            r#", { "name": "explorer-rs-0.1.7-x86_64-pc-windows-msvc.zip", "size": 5, "browser_download_url": "https://github.com/Dasemu/explorer.rs/releases/download/v0.1.7/explorer-rs-0.1.7-x86_64-pc-windows-msvc.zip" }"#,
        ))
        .unwrap();
        assert_eq!(r.version, v("0.1.7"));
        assert_eq!(r.tag, "v0.1.7");
        assert_eq!(
            r.page,
            "https://github.com/FerroApps/explorer.rs/releases/tag/v0.1.7"
        );
        assert_eq!(r.setup.name, "explorer-rs-setup-0.1.7-x64.exe");
        assert_eq!(r.setup.size, 1234);
        assert_eq!(r.sums.name, SUMS_NAME);
        assert_eq!(r.notes, "• Taskbar: faster start\n• Fix tray icons");
    }

    #[test]
    fn assets_from_the_moved_repository_are_accepted() {
        // The repository moved from Dasemu to FerroApps; both owners' URLs
        // are the project's own.
        let moved = release_json("v0.1.7", "explorer-rs-setup-0.1.7-x64.exe", "v0.1.7", "")
            .replace("Dasemu/explorer.rs", "FerroApps/explorer.rs");
        assert!(parse_release(&moved).is_ok());
    }

    #[test]
    fn release_rejections() {
        // Asset from another repository or another tag.
        let bad_url = release_json("v0.1.7", "explorer-rs-setup-0.1.7-x64.exe", "v0.1.6", "");
        assert!(matches!(
            parse_release(&bad_url),
            Err(ReleaseError::BadUrl(_))
        ));
        let other_repo = release_json("v0.1.7", "explorer-rs-setup-0.1.7-x64.exe", "v0.1.7", "")
            .replace(
                "Dasemu/explorer.rs/releases/download/v0.1.7/explorer-rs-setup",
                "Evil/explorer.rs/releases/download/v0.1.7/explorer-rs-setup",
            );
        assert!(matches!(
            parse_release(&other_repo),
            Err(ReleaseError::BadUrl(_))
        ));
        let http = release_json("v0.1.7", "explorer-rs-setup-0.1.7-x64.exe", "v0.1.7", "")
            .replace("https://github.com", "http://github.com");
        assert!(matches!(parse_release(&http), Err(ReleaseError::BadUrl(_))));
        // Installer for another version.
        let wrong_name = release_json("v0.1.7", "explorer-rs-setup-0.1.6-x64.exe", "v0.1.7", "");
        assert!(matches!(
            parse_release(&wrong_name),
            Err(ReleaseError::MissingAsset(_))
        ));
        for tag in ["v0.1.7-rc1", "0.1.7", "latest"] {
            let j = release_json(tag, "explorer-rs-setup-0.1.7-x64.exe", tag, "");
            assert!(
                matches!(parse_release(&j), Err(ReleaseError::BadTag(_))),
                "{tag}"
            );
        }
        let pre = release_json("v0.1.7", "explorer-rs-setup-0.1.7-x64.exe", "v0.1.7", "")
            .replace("\"prerelease\": false", "\"prerelease\": true");
        assert_eq!(parse_release(&pre), Err(ReleaseError::NotFinal));
        let draft = release_json("v0.1.7", "explorer-rs-setup-0.1.7-x64.exe", "v0.1.7", "")
            .replace("\"draft\": false", "\"draft\": true");
        assert_eq!(parse_release(&draft), Err(ReleaseError::NotFinal));
        let huge = release_json("v0.1.7", "explorer-rs-setup-0.1.7-x64.exe", "v0.1.7", "")
            .replace("\"size\": 1234", "\"size\": 999999999999");
        assert!(matches!(
            parse_release(&huge),
            Err(ReleaseError::BadSize(_))
        ));
        assert!(matches!(
            parse_release("{not json"),
            Err(ReleaseError::Json(_))
        ));
        assert!(matches!(
            parse_release(r#"{"message":"Not Found"}"#),
            Err(ReleaseError::Json(_))
        ));
    }

    #[test]
    fn notes_are_short_plain_text() {
        assert_eq!(notes_excerpt(""), "");
        let many: String = (0..20).map(|i| format!("- item {i}\n")).collect();
        let e = notes_excerpt(&many);
        assert_eq!(e.lines().count(), NOTES_MAX_LINES + 1);
        assert!(e.ends_with('…'));
        let long = format!("* {}\n* b\n", "x".repeat(1000));
        assert_eq!(notes_excerpt(&long), "…");
    }

    const HASH_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn sums_file() {
        let text = format!(
            "{HASH_ABC}  explorer-rs-setup-0.1.7-x64.exe\n{}  explorer-rs-0.1.7-x86_64-pc-windows-msvc.zip\n",
            "0".repeat(64)
        );
        let h = parse_sha256sums(&text, "explorer-rs-setup-0.1.7-x64.exe").unwrap();
        assert_eq!(h[0], 0xba);
        assert_eq!(h[31], 0xad);
        let star = format!("{}  *a.exe\r\n", HASH_ABC.to_uppercase());
        assert!(parse_sha256sums(&star, "a.exe").is_some());
        assert!(parse_sha256sums(&text, "explorer-rs-setup-0.1.6-x64.exe").is_none());
        assert!(parse_sha256sums("zz  a.exe", "a.exe").is_none());
        assert!(parse_sha256sums(&format!("{HASH_ABC}  a.exe.bak"), "a.exe").is_none());
    }

    fn release_for(data_len: u64) -> Release {
        parse_release(
            &release_json("v0.1.7", "explorer-rs-setup-0.1.7-x64.exe", "v0.1.7", "")
                .replace("\"size\": 1234", &format!("\"size\": {data_len}")),
        )
        .unwrap()
    }

    #[test]
    fn setup_is_verified() {
        let r = release_for(3);
        let sums = format!("{HASH_ABC}  explorer-rs-setup-0.1.7-x64.exe\n");
        assert_eq!(verify_setup(&r, &sums, b"abc"), Ok(()));
        assert_eq!(verify_setup(&r, &sums, b"abd"), Err(VerifyError::Mismatch));
        assert!(matches!(
            verify_setup(&r, &sums, b"abcd"),
            Err(VerifyError::Size { .. })
        ));
        assert!(matches!(
            verify_setup(&r, &format!("{HASH_ABC}  other.exe\n"), b"abc"),
            Err(VerifyError::NotListed(_))
        ));
    }

    fn state_with(latest: &str) -> UpdateState {
        UpdateState {
            latest: Some(KnownRelease {
                version: latest.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    const ON: RemindContext = RemindContext {
        enabled: true,
        shown_this_session: false,
        fullscreen: false,
    };

    #[test]
    fn check_timing() {
        let mut s = UpdateState::default();
        assert!(should_check(&s, 1000));
        s.last_check = Some(1000);
        s.last_success = Some(1000);
        assert!(!should_check(&s, 1000 + CHECK_INTERVAL_SECS - 1));
        assert!(should_check(&s, 1000 + CHECK_INTERVAL_SECS));
        // Clock went backwards.
        assert!(should_check(&s, 500));
        // A failure is retried after an hour, not a day.
        let due = 1000 + CHECK_INTERVAL_SECS;
        s.record_failure(due, "offline");
        assert!(!should_check(&s, due + RETRY_INTERVAL_SECS - 1));
        assert!(should_check(&s, due + RETRY_INTERVAL_SECS));
        let mut never_ok = UpdateState::default();
        never_ok.record_failure(5000, "offline");
        assert!(!should_check(&never_ok, 5001));
        assert!(should_check(&never_ok, 5000 + RETRY_INTERVAL_SECS));
    }

    #[test]
    fn reminder_rules() {
        let cur = v("0.1.6");
        let now = 10_000_000;
        // Nothing newer.
        assert!(!should_remind(&UpdateState::default(), cur, now, ON));
        assert!(!should_remind(&state_with("0.1.6"), cur, now, ON));
        assert!(!should_remind(&state_with("0.1.5"), cur, now, ON));
        // Newly found: right away, unless off, full screen or already shown.
        let mut s = state_with("0.1.7");
        assert!(should_remind(&s, cur, now, ON));
        for ctx in [
            RemindContext {
                enabled: false,
                ..ON
            },
            RemindContext {
                fullscreen: true,
                ..ON
            },
            RemindContext {
                shown_this_session: true,
                ..ON
            },
        ] {
            assert!(!should_remind(&s, cur, now, ctx));
        }
        // Shown: quiet for three days ("Later"), then again.
        s.mark_reminded(v("0.1.7"), now);
        assert!(!should_remind(&s, cur, now + 60, ON));
        assert!(!should_remind(&s, cur, now + REMIND_INTERVAL_SECS - 1, ON));
        assert!(should_remind(&s, cur, now + REMIND_INTERVAL_SECS, ON));
        // A newer version than the one reminded about: right away.
        s.latest = Some(KnownRelease {
            version: "0.1.8".into(),
            ..Default::default()
        });
        assert!(should_remind(&s, cur, now + 60, ON));
        // "Don't remind me again" holds for that version only.
        s.dismiss(v("0.1.8"));
        assert!(!should_remind(&s, cur, now + 10 * REMIND_INTERVAL_SECS, ON));
        s.latest = Some(KnownRelease {
            version: "0.2.0".into(),
            ..Default::default()
        });
        assert!(should_remind(&s, cur, now + 60, ON));
        // Once installed, nothing.
        assert!(!should_remind(&s, v("0.2.0"), now + 60, ON));
    }

    #[test]
    fn state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("update.toml");
        assert_eq!(UpdateState::load(&p), UpdateState::default());
        let mut s = state_with("0.1.7");
        s.record_failure(5, "offline");
        s.mark_reminded(v("0.1.7"), 7);
        s.save(&p).unwrap();
        assert_eq!(UpdateState::load(&p), s);
        std::fs::write(&p, "garbage = [").unwrap();
        assert_eq!(UpdateState::load(&p), UpdateState::default());
        let r = release_for(3);
        s.record_success(9, &r);
        assert_eq!(s.last_error, None);
        assert_eq!(s.last_success, Some(9));
        assert_eq!(s.available(v("0.1.6")).unwrap().1, v("0.1.7"));
        assert!(s.available(v("0.1.7")).is_none());
    }

    #[test]
    fn relative_times() {
        assert_eq!(ago(Lang::En, 100, 110), "just now");
        assert_eq!(ago(Lang::En, 0, 60), "1 minute ago");
        assert_eq!(ago(Lang::Es, 0, 7200), "hace 2 horas");
        assert_eq!(ago(Lang::En, 0, 3 * 86_400 + 5), "3 days ago");
        assert_eq!(ago(Lang::Es, 0, 86_400), "hace 1 día");
        assert_eq!(ago(Lang::En, 100, 0), "just now");
    }
}
