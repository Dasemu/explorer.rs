// SPDX-License-Identifier: GPL-3.0-or-later
//! `explorer-rs.exe --status`: a short plain-text diagnostics report for
//! bug reports. Data is gathered by the binary; formatting is here.

use std::fmt::Write;

/// Everything shown by `--status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusInfo {
    pub version: String,
    pub guardian_running: bool,
    pub supervisor_running: bool,
    pub explorer_running: bool,
    /// `(component, running)`.
    pub components: Vec<(String, bool)>,
    /// Per-user `Winlogon\Shell` value (`None` = not set: explorer.exe).
    pub shell_value: Option<String>,
    pub config_path: String,
    pub config_exists: bool,
    /// Error message if the config file does not parse/validate.
    pub config_error: Option<String>,
    pub crashlog_path: String,
    pub failed_signins: u32,
    pub auto_restored_at: Option<String>,
    pub crash_lines: Vec<String>,
}

impl StatusInfo {
    /// "shell", "dev" or "not running" (inferred: in shell mode
    /// explorer.exe is not running).
    pub fn mode(&self) -> &'static str {
        match (
            self.guardian_running || self.supervisor_running,
            self.explorer_running,
        ) {
            (false, _) => "not running",
            (true, true) => "dev (explorer.exe is running)",
            (true, false) => "shell (explorer.exe is not running)",
        }
    }
}

fn yes(b: bool) -> &'static str {
    if b {
        "running"
    } else {
        "not running"
    }
}

/// Render the report.
pub fn render(s: &StatusInfo) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "explorer.rs {}", s.version);
    let _ = writeln!(o, "mode:        {}", s.mode());
    let _ = writeln!(o, "guardian:    {}", yes(s.guardian_running));
    let _ = writeln!(o, "supervisor:  {}", yes(s.supervisor_running));
    let _ = writeln!(o, "explorer.exe: {}", yes(s.explorer_running));
    let _ = writeln!(o, "components:");
    for (name, running) in &s.components {
        let _ = writeln!(o, "  {name:<10} {}", yes(*running));
    }
    let shell = match &s.shell_value {
        Some(v) if explorer_core::shellreg::is_ours(v) => format!("explorer.rs ({v})"),
        Some(v) => format!("other ({v})"),
        None => "Windows default (explorer.exe)".to_string(),
    };
    let _ = writeln!(o, "shell registration (HKCU): {shell}");
    let cfg = match (&s.config_error, s.config_exists) {
        (_, false) => "not present (defaults)".to_string(),
        (None, true) => "valid".to_string(),
        (Some(e), true) => format!("INVALID: {e}"),
    };
    let _ = writeln!(o, "config:      {} [{cfg}]", s.config_path);
    let _ = writeln!(o, "crash log:   {}", s.crashlog_path);
    let _ = writeln!(o, "failed shell sign-ins in a row: {}", s.failed_signins);
    if let Some(t) = &s.auto_restored_at {
        let _ = writeln!(o, "Windows Explorer was restored automatically at {t}");
    }
    if s.crash_lines.is_empty() {
        let _ = writeln!(o, "recent events: none");
    } else {
        let _ = writeln!(o, "recent events (UTC):");
        for l in &s.crash_lines {
            let _ = writeln!(o, "  {l}");
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_contents() {
        let mut s = StatusInfo {
            version: "0.1.0".into(),
            guardian_running: true,
            supervisor_running: true,
            components: vec![("taskbar".into(), true), ("launcher".into(), false)],
            shell_value: Some(r#""C:\x\explorer-rs.exe" --shell"#.into()),
            config_path: "C:/c.toml".into(),
            config_exists: true,
            config_error: Some("bad height".into()),
            crash_lines: vec!["line one".into()],
            ..Default::default()
        };
        let r = render(&s);
        assert!(r.contains("shell (explorer.exe is not running)"));
        assert!(r.contains("taskbar    running"));
        assert!(r.contains("launcher   not running"));
        assert!(r.contains("explorer.rs (\""));
        assert!(r.contains("INVALID: bad height"));
        assert!(r.contains("  line one"));
        s.guardian_running = false;
        s.supervisor_running = false;
        s.shell_value = None;
        s.config_exists = false;
        let r = render(&s);
        assert!(r.contains("mode:        not running"));
        assert!(r.contains("Windows default"));
        assert!(r.contains("not present"));
    }
}
