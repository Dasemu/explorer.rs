// SPDX-License-Identifier: GPL-3.0-or-later
//! "Run" results of the Start menu search box (like Win+R).
//!
//! [`classify`] decides, without touching the file system, whether a query
//! could be a command:
//! - `>` prefix: run the rest in a Command Prompt (`cmd.exe /k`);
//! - a path: `C:\...`, `\\server\share`, `%VAR%\...`, `shell:...`;
//! - a bare program name (`notepad`, `regedit file.reg`): only offered when
//!   the caller resolves it (SearchPathW / App Paths) on a worker thread.
//!
//! Web addresses are never offered: no web, ever. A query that names a URL
//! anywhere (scheme `http:`, `https:`, `ftp:`, ..., or `www.`) is rejected,
//! including as an argument (`msedge https://...`).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunKind {
    /// `>cmd`: run through `cmd.exe /k`.
    Shell,
    /// A file-system or shell path, opened with ShellExecute.
    Path,
    /// A program name that must be resolved on PATH / App Paths first.
    Program,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCommand {
    pub kind: RunKind,
    /// Program, path or (for `Shell`) the whole command line.
    pub program: String,
    /// Arguments (may be empty).
    pub args: String,
}

impl RunCommand {
    /// The text shown in the result row.
    pub fn display(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args)
        }
    }
}

/// Maximum length of a query considered for "Run".
pub const MAX_COMMAND: usize = 1024;

/// URI schemes that are allowed to run (local shell locations only).
const LOCAL_SCHEMES: &[&str] = &["shell", "ms-settings"];

/// True if `s` contains anything that looks like a web address.
pub fn mentions_url(s: &str) -> bool {
    let lower = s.to_lowercase();
    if lower.contains("://") {
        return true;
    }
    for word in lower.split(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        if word.starts_with("www.") {
            return true;
        }
        if let Some((scheme, _)) = word.split_once(':') {
            let valid_scheme = scheme.len() > 1
                && scheme
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
            if valid_scheme && !LOCAL_SCHEMES.contains(&scheme) {
                return true;
            }
        }
    }
    false
}

/// Split a command line into program and arguments, honouring a quoted
/// program (`"C:\Program Files\x.exe" -a`).
pub fn split_command(s: &str) -> (String, String) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_string(), rest[end + 1..].trim().to_string());
        }
        return (rest.to_string(), String::new());
    }
    match s.split_once(char::is_whitespace) {
        Some((p, a)) => (p.to_string(), a.trim().to_string()),
        None => (s.to_string(), String::new()),
    }
}

fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/')
}

fn is_env_path(s: &str) -> bool {
    // %VAR% or %VAR%\rest
    s.strip_prefix('%')
        .and_then(|r| r.split_once('%'))
        .is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        })
}

/// Characters allowed in a bare program name.
fn is_program_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().any(|c| c.is_alphabetic())
        && s.chars().all(|c| c.is_alphanumeric() || "._-+".contains(c))
        && !s.starts_with('.')
        && !s.starts_with('-')
}

/// Classify a search query. `None` = not a command (or not allowed).
pub fn classify(query: &str) -> Option<RunCommand> {
    let q = query.trim();
    if q.is_empty() || q.len() > MAX_COMMAND || q.contains('\0') || mentions_url(q) {
        return None;
    }
    if let Some(rest) = q.strip_prefix('>') {
        let rest = rest.trim();
        if rest.is_empty() {
            return None;
        }
        return Some(RunCommand {
            kind: RunKind::Shell,
            program: rest.to_string(),
            args: String::new(),
        });
    }
    let (program, args) = split_command(q);
    let lower = program.to_lowercase();
    let is_path = is_drive_path(&program)
        || program.starts_with("\\\\")
        || is_env_path(&program)
        || lower.starts_with("shell:")
        || lower.starts_with("ms-settings:");
    if is_path {
        // A path with spaces but no quotes: the whole query is the path
        // ("C:\Program Files"), as in the Run dialog.
        let (program, args) = if !q.starts_with('"') && !args.is_empty() {
            (q.to_string(), String::new())
        } else {
            (program, args)
        };
        return Some(RunCommand {
            kind: RunKind::Path,
            program,
            args,
        });
    }
    if is_program_name(&program) {
        return Some(RunCommand {
            kind: RunKind::Program,
            program,
            args,
        });
    }
    None
}

/// File names tried when resolving a bare program name, in order (the
/// PATHEXT subset that makes sense to start from a search box).
pub fn program_candidates(name: &str) -> Vec<String> {
    let lower = name.to_lowercase();
    const EXTS: &[&str] = &[".exe", ".com", ".bat", ".cmd", ".msc", ".cpl"];
    if EXTS.iter().any(|e| lower.ends_with(e)) {
        return vec![name.to_string()];
    }
    EXTS.iter().map(|e| format!("{name}{e}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(q: &str) -> Option<RunKind> {
        classify(q).map(|c| c.kind)
    }

    #[test]
    fn urls_are_never_offered() {
        for q in [
            "http://example.com",
            "https://example.com",
            "www.example.com",
            "ftp://x",
            "msedge https://example.com",
            "chrome www.example.com",
            ">start https://x",
            "mailto:a@b.c",
            "\\\\server\\share http://x",
        ] {
            assert_eq!(classify(q), None, "{q}");
        }
        assert!(!mentions_url("C:\\Windows"));
        assert!(!mentions_url("shell:startup"));
        assert!(!mentions_url("ms-settings:display"));
    }

    #[test]
    fn paths() {
        assert_eq!(kind("C:\\Windows"), Some(RunKind::Path));
        assert_eq!(kind("c:/users"), Some(RunKind::Path));
        assert_eq!(kind("D:"), Some(RunKind::Path));
        assert_eq!(kind("\\\\nas\\share"), Some(RunKind::Path));
        assert_eq!(kind("%APPDATA%"), Some(RunKind::Path));
        assert_eq!(kind("%USERPROFILE%\\Downloads"), Some(RunKind::Path));
        assert_eq!(kind("shell:startup"), Some(RunKind::Path));
        let c = classify("C:\\Program Files").unwrap();
        assert_eq!(c.program, "C:\\Program Files");
        assert_eq!(c.args, "");
        let c = classify("\"C:\\Program Files\\a b.exe\" -x").unwrap();
        assert_eq!(c.program, "C:\\Program Files\\a b.exe");
        assert_eq!(c.args, "-x");
        assert_eq!(kind("%%"), None);
    }

    #[test]
    fn programs_and_shell() {
        let c = classify("notepad C:\\a.txt").unwrap();
        assert_eq!(c.kind, RunKind::Program);
        assert_eq!(c.program, "notepad");
        assert_eq!(c.args, "C:\\a.txt");
        assert_eq!(c.display(), "notepad C:\\a.txt");
        assert_eq!(kind("cmd"), Some(RunKind::Program));
        assert_eq!(kind("devmgmt.msc"), Some(RunKind::Program));
        let c = classify(">ipconfig /all").unwrap();
        assert_eq!(c.kind, RunKind::Shell);
        assert_eq!(c.program, "ipconfig /all");
        assert_eq!(classify(">"), None);
        assert_eq!(classify("   "), None);
        assert_eq!(kind("2+2"), None);
        assert_eq!(kind("123"), None);
        assert_eq!(kind("-rf"), None);
        assert_eq!(kind("hello?"), None);
        assert_eq!(classify(&"a".repeat(2000)), None);
    }

    #[test]
    fn candidates() {
        assert_eq!(program_candidates("devmgmt.msc"), vec!["devmgmt.msc"]);
        let c = program_candidates("cmd");
        assert_eq!(c[0], "cmd.exe");
        assert!(c.contains(&"cmd.bat".to_string()));
    }

    #[test]
    fn split() {
        assert_eq!(split_command("a b  c"), ("a".into(), "b  c".into()));
        assert_eq!(split_command("\"x y"), ("x y".into(), String::new()));
        assert_eq!(split_command("solo"), ("solo".into(), String::new()));
    }
}
