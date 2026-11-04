// SPDX-License-Identifier: GPL-3.0-or-later
//! Persistent crash log and boot-loop policy
//! (`%LOCALAPPDATA%\explorer-rs\state\crashlog.toml`).
//!
//! The guardian (`explorer-rs.exe --shell`) is the only writer. It records
//! start/crash events in a bounded ring buffer and one record per shell-mode
//! sign-in. From those records it decides, at every sign-in, whether
//! explorer.rs has failed too often in a row and Windows Explorer must be
//! restored automatically: there must never be an endless boot loop.
//!
//! A shell-mode session counts as **failed** when it fell back to
//! explorer.exe (crash loop), or when it neither became healthy (ran for
//! [`HEALTHY_SESSION_S`]) nor ended cleanly (sign-out / shutdown seen). The
//! second rule catches sessions the user could only end by powering off.
//!
//! All times are Unix seconds passed in explicitly, so everything is testable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;

/// Ring-buffer size.
pub const MAX_EVENTS: usize = 100;
/// After this many consecutive failed shell-mode sign-ins, Windows Explorer
/// is restored permanently without asking.
pub const MAX_FAILED_SIGNINS: u32 = 3;
/// A session whose supervisor ran this long is healthy.
pub const HEALTHY_SESSION_S: u64 = 180;

/// Location of the crash log.
pub fn crashlog_file(paths: &AppPaths) -> PathBuf {
    // `state_dir` is `%LOCALAPPDATA%\explorer-rs` on Windows but `<base>\state`
    // under EXPLORER_RS_HOME; avoid `state\state` in the latter case.
    let dir = if paths.state_dir.file_name().is_some_and(|n| n == "state") {
        paths.state_dir.clone()
    } else {
        paths.state_dir.join("state")
    };
    dir.join("crashlog.toml")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    /// A guardian started (beginning of a dev or shell session).
    SessionStart,
    /// A process was started.
    Start,
    /// A process exited unexpectedly.
    Crash,
    /// A process exited on purpose.
    Exit,
    /// The session became healthy.
    Healthy,
    /// Crash loop: explorer.exe started as a fallback.
    Fallback,
    /// Windows Explorer restored as the shell (by the user or automatically).
    Restored,
    /// Too many failed sign-ins: Windows Explorer restored automatically.
    AutoRestore,
    /// Sign-out / shutdown observed.
    SessionEnd,
}

impl EventKind {
    fn as_str(self) -> &'static str {
        match self {
            EventKind::SessionStart => "session-start",
            EventKind::Start => "start",
            EventKind::Crash => "crash",
            EventKind::Exit => "exit",
            EventKind::Healthy => "healthy",
            EventKind::Fallback => "fallback",
            EventKind::Restored => "restored",
            EventKind::AutoRestore => "auto-restore",
            EventKind::SessionEnd => "session-end",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Unix seconds.
    pub at: u64,
    pub kind: EventKind,
    /// "guardian", "supervisor", "taskbar", ...
    #[serde(default)]
    pub component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl Event {
    pub fn new(at: u64, kind: EventKind, component: &str) -> Self {
        Self {
            at,
            kind,
            component: component.into(),
            code: None,
            detail: String::new(),
        }
    }

    pub fn with_code(mut self, code: Option<i32>) -> Self {
        self.code = code;
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// One human-readable line (UTC time).
    pub fn line(&self) -> String {
        let mut s = format!(
            "{} {:<13} {}",
            format_utc(self.at),
            self.kind.as_str(),
            self.component
        );
        if let Some(c) = self.code {
            s.push_str(&format!(" code={c}"));
        }
        if !self.detail.is_empty() {
            s.push_str(&format!(" ({})", self.detail));
        }
        s
    }
}

/// One shell-mode sign-in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionRecord {
    pub started: u64,
    pub healthy: bool,
    pub ended_clean: bool,
    pub fell_back: bool,
}

impl SessionRecord {
    pub fn failed(&self) -> bool {
        self.fell_back || !(self.healthy || self.ended_clean)
    }
}

/// Contents of crashlog.toml.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrashState {
    /// Consecutive failed shell-mode sign-ins before the current one.
    pub failed_signins: u32,
    /// Set when Windows Explorer was restored automatically; cleared once
    /// the user has been told.
    pub auto_restored_at: Option<u64>,
    /// The current (or last) shell-mode sign-in.
    pub session: Option<SessionRecord>,
    pub events: Vec<Event>,
}

/// What to do at the start of a shell-mode sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigninDecision {
    /// Start explorer.rs. `failed_signins` consecutive sign-ins failed
    /// before this one.
    Normal { failed_signins: u32 },
    /// Too many failures: restore Windows Explorer permanently, start it,
    /// tell the user.
    AutoRestore,
}

/// What to do when a crash loop forces the fallback to explorer.exe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackDecision {
    /// Ask the user (restore permanently / try again next sign-in).
    Ask { failures_including_this: u32 },
    /// This is the last allowed failure: restore without asking.
    AutoRestore,
}

impl CrashState {
    /// Read the file; any error yields an empty state (and is returned for
    /// logging). A corrupt crash log must never stop the shell.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(s) => match toml::from_str::<CrashState>(&s) {
                Ok(mut st) => {
                    st.trim();
                    (st, None)
                }
                Err(e) => (
                    Self::default(),
                    Some(format!("parse {}: {e}", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (
                Self::default(),
                Some(format!("read {}: {e}", path.display())),
            ),
        }
    }

    /// Write atomically (temporary file + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    fn trim(&mut self) {
        if self.events.len() > MAX_EVENTS {
            let extra = self.events.len() - MAX_EVENTS;
            self.events.drain(..extra);
        }
    }

    /// Append to the ring buffer.
    pub fn record(&mut self, ev: Event) {
        self.events.push(ev);
        self.trim();
    }

    /// Start of a shell-mode sign-in: account for the previous session and
    /// decide whether to run at all.
    pub fn begin_shell_session(&mut self, now: u64) -> SigninDecision {
        if let Some(prev) = self.session.take() {
            if prev.failed() {
                self.failed_signins = self.failed_signins.saturating_add(1);
            } else {
                self.failed_signins = 0;
            }
        }
        self.record(Event::new(now, EventKind::SessionStart, "guardian").with_detail("shell"));
        self.session = Some(SessionRecord {
            started: now,
            ..SessionRecord::default()
        });
        if self.failed_signins >= MAX_FAILED_SIGNINS {
            SigninDecision::AutoRestore
        } else {
            SigninDecision::Normal {
                failed_signins: self.failed_signins,
            }
        }
    }

    /// A crash loop happened in this session.
    pub fn on_fallback(&mut self, now: u64, reason: &str) -> FallbackDecision {
        if let Some(s) = self.session.as_mut() {
            s.fell_back = true;
        }
        self.record(Event::new(now, EventKind::Fallback, "guardian").with_detail(reason));
        let failures = self.failed_signins.saturating_add(1);
        if failures >= MAX_FAILED_SIGNINS {
            FallbackDecision::AutoRestore
        } else {
            FallbackDecision::Ask {
                failures_including_this: failures,
            }
        }
    }

    /// Windows Explorer is the shell again: start counting from zero.
    pub fn on_restored(&mut self, now: u64, automatic: bool) {
        self.failed_signins = 0;
        self.session = None;
        if automatic {
            self.auto_restored_at = Some(now);
            self.record(Event::new(now, EventKind::AutoRestore, "guardian"));
        } else {
            self.record(Event::new(now, EventKind::Restored, "guardian"));
        }
    }

    /// Mark the current session healthy. Returns true if this changed it.
    pub fn mark_healthy(&mut self, now: u64) -> bool {
        match self.session.as_mut() {
            Some(s) if !s.healthy => {
                s.healthy = true;
                self.record(Event::new(now, EventKind::Healthy, "guardian"));
                true
            }
            _ => false,
        }
    }

    /// Sign-out / shutdown observed.
    pub fn mark_session_end(&mut self, now: u64) {
        if let Some(s) = self.session.as_mut() {
            s.ended_clean = true;
        }
        self.record(Event::new(now, EventKind::SessionEnd, "guardian"));
    }

    /// Last `n` events as text lines, oldest first.
    pub fn last_lines(&self, n: usize) -> Vec<String> {
        let skip = self.events.len().saturating_sub(n);
        self.events[skip..].iter().map(Event::line).collect()
    }
}

/// Unix seconds now.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM:SSZ` for Unix seconds.
pub fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (H. Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_layout() {
        let p = AppPaths::resolve(|k| {
            (k == "LOCALAPPDATA").then(|| PathBuf::from("C:/Users/u/AppData/Local"))
        });
        assert_eq!(
            crashlog_file(&p),
            PathBuf::from("C:/Users/u/AppData/Local/explorer-rs/state/crashlog.toml")
        );
        let p = AppPaths::under(PathBuf::from("/tmp/x"));
        assert_eq!(
            crashlog_file(&p),
            PathBuf::from("/tmp/x/state/crashlog.toml")
        );
    }

    #[test]
    fn ring_buffer_bounded() {
        let mut s = CrashState::default();
        for i in 0..(MAX_EVENTS as u64 + 10) {
            s.record(Event::new(i, EventKind::Start, "supervisor"));
        }
        assert_eq!(s.events.len(), MAX_EVENTS);
        assert_eq!(s.events[0].at, 10);
        assert_eq!(s.last_lines(2).len(), 2);
    }

    #[test]
    fn roundtrip_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state").join("crashlog.toml");
        let mut s = CrashState::default();
        s.begin_shell_session(100);
        s.record(Event::new(101, EventKind::Crash, "supervisor").with_code(Some(3)));
        s.save(&p).unwrap();
        let (back, err) = CrashState::load(&p);
        assert!(err.is_none());
        assert_eq!(back, s);
        std::fs::write(&p, "garbage = = =").unwrap();
        let (back, err) = CrashState::load(&p);
        assert_eq!(back, CrashState::default());
        assert!(err.is_some());
        let (back, err) = CrashState::load(&dir.path().join("missing.toml"));
        assert_eq!(back, CrashState::default());
        assert!(err.is_none());
    }

    #[test]
    fn healthy_or_clean_sessions_reset_the_counter() {
        let mut s = CrashState {
            failed_signins: 2,
            ..Default::default()
        };
        s.begin_shell_session(0);
        s.mark_healthy(200);
        assert_eq!(
            s.begin_shell_session(1000),
            SigninDecision::Normal { failed_signins: 0 }
        );
        s.failed_signins = 2;
        s.mark_session_end(1010);
        assert_eq!(
            s.begin_shell_session(2000),
            SigninDecision::Normal { failed_signins: 0 }
        );
    }

    #[test]
    fn three_failed_signins_auto_restore() {
        let mut s = CrashState::default();
        // 1st sign-in: crash loop, user chose "try again".
        assert_eq!(
            s.begin_shell_session(0),
            SigninDecision::Normal { failed_signins: 0 }
        );
        assert_eq!(
            s.on_fallback(10, "boot loop"),
            FallbackDecision::Ask {
                failures_including_this: 1
            }
        );
        // 2nd sign-in: the machine was powered off before anything was
        // recorded (neither healthy nor clean) -> also a failure.
        assert_eq!(
            s.begin_shell_session(100),
            SigninDecision::Normal { failed_signins: 1 }
        );
        // 3rd sign-in: crash loop again; this is the last allowed failure.
        assert_eq!(
            s.begin_shell_session(200),
            SigninDecision::Normal { failed_signins: 2 }
        );
        assert_eq!(s.on_fallback(210, "x"), FallbackDecision::AutoRestore);
        // Had the restore failed, the next sign-in restores.
        assert_eq!(s.begin_shell_session(300), SigninDecision::AutoRestore);
        s.on_restored(301, true);
        assert_eq!(s.failed_signins, 0);
        assert_eq!(s.auto_restored_at, Some(301));
    }

    #[test]
    fn fallback_after_healthy_start_still_counts() {
        let mut s = CrashState::default();
        s.begin_shell_session(0);
        s.mark_healthy(500);
        s.on_fallback(900, "taskbar");
        s.mark_session_end(1000);
        assert_eq!(
            s.begin_shell_session(2000),
            SigninDecision::Normal { failed_signins: 1 }
        );
    }

    #[test]
    fn utc_formatting() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14 22:13:20Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29 00:00:00Z");
        let line = Event::new(0, EventKind::Crash, "taskbar")
            .with_code(Some(5))
            .with_detail("x")
            .line();
        assert!(line.contains("crash") && line.contains("code=5") && line.contains("(x)"));
    }
}
