// SPDX-License-Identifier: GPL-3.0-or-later
//! Startup apps ("Run" keys and Startup folders) in shell mode: the pure
//! rules. Registry and file I/O live in `explorer-win::startup`.
//!
//! What explorer.exe runs at sign-in (documented under "Run and RunOnce
//! Registry Keys" and the Startup folder known folders):
//! - `HKCU\...\RunOnce` (each value is deleted *before* it runs, unless its
//!   name starts with `!`, in which case it is deleted after it ran);
//! - `HKLM\...\Run`, `HKLM\Software\WOW6432Node\...\Run`, `HKCU\...\Run`;
//! - the common and per-user Startup folders.
//!
//! Entries switched off in Settings > Apps > Startup or Task Manager are
//! recorded under `...\Explorer\StartupApproved\{Run,Run32,StartupFolder}`
//! as 12-byte binary values: a 4-byte little-endian flag word followed by
//! the FILETIME of the change. Observed flag values: `02`/`06` = enabled,
//! `03`/`07` = disabled, i.e. bit 0 set means disabled. A missing value
//! means enabled. (Not formally documented; see ADR 0014.)

/// Delay between two launches so dozens of tray apps do not start at once.
pub const STAGGER_MS: u64 = 300;

pub const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const RUN_ONCE_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\RunOnce";
pub const RUN_WOW64_SUBKEY: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
pub const APPROVED_SUBKEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";

/// Where a startup entry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    UserRunOnce,
    MachineRun,
    MachineRun32,
    UserRun,
    CommonStartupFolder,
    UserStartupFolder,
}

impl Source {
    /// Launch order (RunOnce first, like explorer).
    pub const ORDER: [Source; 6] = [
        Source::UserRunOnce,
        Source::MachineRun,
        Source::MachineRun32,
        Source::UserRun,
        Source::CommonStartupFolder,
        Source::UserStartupFolder,
    ];

    /// Name of the `StartupApproved` subkey that can disable this entry
    /// (looked up under HKCU and HKLM). RunOnce entries cannot be disabled.
    pub fn approval_subkey(self) -> Option<&'static str> {
        match self {
            Source::UserRunOnce => None,
            Source::MachineRun | Source::UserRun => Some("Run"),
            Source::MachineRun32 => Some("Run32"),
            Source::CommonStartupFolder | Source::UserStartupFolder => Some("StartupFolder"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Source::UserRunOnce => "HKCU RunOnce",
            Source::MachineRun => "HKLM Run",
            Source::MachineRun32 => "HKLM Run (32-bit)",
            Source::UserRun => "HKCU Run",
            Source::CommonStartupFolder => "common Startup folder",
            Source::UserStartupFolder => "Startup folder",
        }
    }
}

/// Is a `StartupApproved` value "enabled"? `None` (no value) = enabled.
/// Malformed (empty) data is treated as enabled, like explorer.
pub fn approved_enabled(data: Option<&[u8]>) -> bool {
    match data.and_then(|d| d.first()) {
        Some(flags) => flags & 1 == 0,
        None => true,
    }
}

/// RunOnce value name handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOnceName {
    /// Delete the value only after the command was started (`!` prefix).
    pub delete_after: bool,
}

pub fn run_once_name(name: &str) -> RunOnceName {
    // A `*` prefix only means "also in safe mode"; prefixes may combine.
    let n = name.trim_start_matches('*');
    RunOnceName {
        delete_after: n.starts_with('!'),
    }
}

/// Expand `%VAR%` references (REG_EXPAND_SZ semantics). Unknown variables
/// and lone `%` are left untouched, like `ExpandEnvironmentStrings`.
pub fn expand_env(s: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match lookup(name) {
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            Some(_) => {
                // "%%": the first one is literal; the second may open a name.
                out.push('%');
                rest = after;
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A command line split into program and arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    pub program: String,
    pub args: String,
}

impl CommandLine {
    /// Rebuilt command line with the program quoted (for CreateProcess).
    pub fn to_command_line(&self) -> String {
        if self.args.is_empty() {
            format!("\"{}\"", self.program)
        } else {
            format!("\"{}\" {}", self.program, self.args)
        }
    }
}

/// Split a Run-key command line.
///
/// - `"C:\Program Files\App\app.exe" --tray` → quoted program.
/// - `C:\Program Files\App\app.exe --tray` (unquoted, with spaces): like
///   CreateProcess, try ever longer space-separated prefixes and take the
///   first one that `exists` (as written or with `.exe` appended).
/// - Otherwise the first whitespace-separated token is the program.
///
/// Returns `None` for an empty command.
pub fn split_command(cmd: &str, exists: impl Fn(&str) -> bool) -> Option<CommandLine> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return None;
    }
    if let Some(rest) = cmd.strip_prefix('"') {
        let (program, args) = match rest.find('"') {
            Some(end) => (&rest[..end], rest[end + 1..].trim()),
            None => (rest, ""),
        };
        if program.trim().is_empty() {
            return None;
        }
        return Some(CommandLine {
            program: program.to_string(),
            args: args.to_string(),
        });
    }
    let spaces: Vec<usize> = cmd
        .char_indices()
        .filter(|(_, c)| *c == ' ')
        .map(|(i, _)| i)
        .collect();
    if !spaces.is_empty() {
        let ends = spaces.iter().copied().chain(std::iter::once(cmd.len()));
        for end in ends {
            let candidate = &cmd[..end];
            let has_ext = candidate
                .rsplit(['\\', '/'])
                .next()
                .is_some_and(|f| f.contains('.'));
            if exists(candidate) || (!has_ext && exists(&format!("{candidate}.exe"))) {
                return Some(CommandLine {
                    program: candidate.to_string(),
                    args: cmd[end..].trim().to_string(),
                });
            }
        }
    }
    let (program, args) = match cmd.find(char::is_whitespace) {
        Some(i) => (&cmd[..i], cmd[i..].trim()),
        None => (cmd, ""),
    };
    Some(CommandLine {
        program: program.to_string(),
        args: args.to_string(),
    })
}

/// Should a file found in a Startup folder be launched?
pub fn is_startup_folder_entry(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    !(lower == "desktop.ini" || lower.starts_with('.') || lower.ends_with(".tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_parsing() {
        let en = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let dis = [0x03, 0, 0, 0, 0x10, 0x20, 0, 0, 0, 0, 0, 0];
        assert!(approved_enabled(None));
        assert!(approved_enabled(Some(&[])));
        assert!(approved_enabled(Some(&en)));
        assert!(approved_enabled(Some(&[0x06, 0, 0, 0])));
        assert!(!approved_enabled(Some(&dis)));
        assert!(!approved_enabled(Some(&[0x07, 0, 0, 0])));
        assert!(!approved_enabled(Some(&[0x01])));
    }

    #[test]
    fn run_once_prefixes() {
        assert!(!run_once_name("Setup").delete_after);
        assert!(run_once_name("!Setup").delete_after);
        assert!(run_once_name("*!Setup").delete_after);
        assert!(!run_once_name("*Setup").delete_after);
    }

    #[test]
    fn env_expansion() {
        let env = |k: &str| match k.to_ascii_uppercase().as_str() {
            "PROGRAMFILES" => Some(r"C:\Program Files".to_string()),
            "USERPROFILE" => Some(r"C:\Users\ana".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_env(r"%ProgramFiles%\App\a.exe", env),
            r"C:\Program Files\App\a.exe"
        );
        assert_eq!(
            expand_env(r"%USERPROFILE%\x %NOPE% 50%", env),
            r"C:\Users\ana\x %NOPE% 50%"
        );
        assert_eq!(expand_env("a %% b", env), "a %% b");
        assert_eq!(expand_env("%%USERPROFILE%", env), r"%C:\Users\ana");
        assert_eq!(expand_env("plain", env), "plain");
    }

    #[test]
    fn split_quoted() {
        let none = |_: &str| false;
        assert_eq!(
            split_command(r#""C:\Program Files\App\app.exe" --tray -x"#, none),
            Some(CommandLine {
                program: r"C:\Program Files\App\app.exe".into(),
                args: "--tray -x".into()
            })
        );
        assert_eq!(split_command(r#""C:\a b\app.exe""#, none).unwrap().args, "");
        assert_eq!(
            split_command(r#""C:\unterminated\app.exe"#, none)
                .unwrap()
                .program,
            r"C:\unterminated\app.exe"
        );
        assert_eq!(split_command("   ", none), None);
        assert_eq!(split_command(r#""" x"#, none), None);
    }

    #[test]
    fn split_unquoted_with_spaces() {
        let fs = |p: &str| {
            p.eq_ignore_ascii_case(r"C:\Program Files\App\app.exe") || p == r"C:\Tools\run.exe"
        };
        assert_eq!(
            split_command(r"C:\Program Files\App\app.exe /background", fs),
            Some(CommandLine {
                program: r"C:\Program Files\App\app.exe".into(),
                args: "/background".into()
            })
        );
        // Extension-less program: `.exe` is tried.
        assert_eq!(
            split_command(r"C:\Tools\run -a", fs).unwrap().program,
            r"C:\Tools\run"
        );
        // Nothing exists: first token.
        assert_eq!(
            split_command("rundll32.exe shell32.dll,Control_RunDLL", |_| false),
            Some(CommandLine {
                program: "rundll32.exe".into(),
                args: "shell32.dll,Control_RunDLL".into()
            })
        );
        assert_eq!(split_command(r"C:\x\tool.exe", |_| false).unwrap().args, "");
    }

    #[test]
    fn rebuild_and_folder_filter() {
        let c = CommandLine {
            program: r"C:\a b\c.exe".into(),
            args: "-q".into(),
        };
        assert_eq!(c.to_command_line(), r#""C:\a b\c.exe" -q"#);
        assert!(is_startup_folder_entry("OneNote.lnk"));
        assert!(!is_startup_folder_entry("desktop.ini"));
        assert!(!is_startup_folder_entry("Desktop.INI"));
        assert_eq!(Source::MachineRun32.approval_subkey(), Some("Run32"));
        assert_eq!(Source::UserRunOnce.approval_subkey(), None);
    }
}
