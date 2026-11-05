// SPDX-License-Identifier: GPL-3.0-or-later
//! Decisions for registering / restoring the per-user shell
//! (`HKCU\Software\Microsoft\Windows NT\CurrentVersion\Winlogon\Shell`).
//!
//! The registry I/O lives in `explorer-win::shellreg`; this module decides
//! what to write, so installer, uninstaller, recovery code and scripts all
//! follow the same rules (mirrors scripts/install-shell.ps1 and
//! scripts/restore-explorer.ps1).

/// Winlogon key, relative to a user hive.
pub const WINLOGON_SUBKEY: &str = r"Software\Microsoft\Windows NT\CurrentVersion\Winlogon";
/// Backup key, relative to a user hive.
pub const BACKUP_SUBKEY: &str = r"Software\explorer-rs";
pub const BACKUP_VALUE: &str = "PreviousShell";
pub const BACKUP_EXISTED_VALUE: &str = "PreviousShellExisted";

/// Command line registered as the shell.
pub fn shell_command(exe: &str) -> String {
    format!("\"{exe}\" --shell")
}

/// True if a Shell value points at explorer.rs.
pub fn is_ours(shell: &str) -> bool {
    shell.to_ascii_lowercase().contains("explorer-rs.exe")
}

/// What to do with the backup when registering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupAction {
    /// Keep the existing backup (re-install / upgrade).
    Keep,
    /// Record the current value (`None` = there was no per-user value).
    Record(Option<String>),
}

/// `existing_backup`: `Some(prev)` if a backup is recorded (`prev` = value
/// or `None` for "did not exist"), `None` if there is no usable backup.
pub fn backup_on_register(
    current: Option<&str>,
    existing_backup: Option<Option<String>>,
) -> BackupAction {
    match existing_backup {
        Some(_) => BackupAction::Keep,
        // Never back up our own value as "previous".
        None => BackupAction::Record(current.filter(|c| !is_ours(c)).map(str::to_string)),
    }
}

/// What to write when restoring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreAction {
    /// Nothing to do: the per-user shell is not ours.
    Nothing,
    /// Delete the per-user value: the machine default (explorer.exe) applies.
    DeleteValue,
    /// Write back the previous value.
    SetValue(String),
}

pub fn restore(current: Option<&str>, backup: Option<Option<String>>) -> RestoreAction {
    match current {
        Some(c) if is_ours(c) => match backup.flatten() {
            Some(prev) if !prev.trim().is_empty() && !is_ours(&prev) => {
                RestoreAction::SetValue(prev)
            }
            _ => RestoreAction::DeleteValue,
        },
        _ => RestoreAction::Nothing,
    }
}

/// Preference of a process image as evidence of "who is signed in at the
/// console": the Windows Explorer shell first, then explorer.rs. `None` for
/// anything else.
pub fn shell_process_rank(image: &str) -> Option<u8> {
    let name = image.rsplit(['\\', '/']).next().unwrap_or(image);
    if name.eq_ignore_ascii_case("explorer.exe") {
        Some(0)
    } else if name.eq_ignore_ascii_case("explorer-rs.exe") {
        Some(1)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_user_evidence() {
        assert_eq!(shell_process_rank("explorer.exe"), Some(0));
        assert_eq!(shell_process_rank(r"C:\Windows\EXPLORER.EXE"), Some(0));
        assert_eq!(shell_process_rank("explorer-rs.exe"), Some(1));
        assert_eq!(shell_process_rank("explorer-rs-taskbar.exe"), None);
        assert_eq!(shell_process_rank("svchost.exe"), None);
    }

    #[test]
    fn command_and_detection() {
        let c = shell_command(r"C:\Program Files\explorer-rs\explorer-rs.exe");
        assert_eq!(
            c,
            r#""C:\Program Files\explorer-rs\explorer-rs.exe" --shell"#
        );
        assert!(is_ours(&c));
        assert!(!is_ours("explorer.exe"));
    }

    #[test]
    fn backup_rules() {
        assert_eq!(
            backup_on_register(Some("custom.exe"), None),
            BackupAction::Record(Some("custom.exe".into()))
        );
        assert_eq!(backup_on_register(None, None), BackupAction::Record(None));
        assert_eq!(
            backup_on_register(Some("x"), Some(Some("y".into()))),
            BackupAction::Keep
        );
        // Our own value is never recorded as the previous shell.
        assert_eq!(
            backup_on_register(Some(r#""C:\a\explorer-rs.exe" --shell"#), None),
            BackupAction::Record(None)
        );
    }

    #[test]
    fn restore_rules() {
        let ours = r#""C:\a\explorer-rs.exe" --shell"#;
        assert_eq!(restore(Some("explorer.exe"), None), RestoreAction::Nothing);
        assert_eq!(restore(None, None), RestoreAction::Nothing);
        assert_eq!(restore(Some(ours), None), RestoreAction::DeleteValue);
        assert_eq!(restore(Some(ours), Some(None)), RestoreAction::DeleteValue);
        assert_eq!(
            restore(Some(ours), Some(Some("custom.exe".into()))),
            RestoreAction::SetValue("custom.exe".into())
        );
        assert_eq!(
            restore(Some(ours), Some(Some(ours.into()))),
            RestoreAction::DeleteValue
        );
    }
}
