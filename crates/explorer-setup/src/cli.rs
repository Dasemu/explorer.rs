// SPDX-License-Identifier: GPL-3.0-or-later
//! Command line of `explorer-rs-setup.exe` / `uninstall.exe`.
//!
//! Hand-written (no clap) so the accepted syntax is exactly what is
//! documented in docs/INSTALL.md, including the `--key=value` and
//! `--key value` forms Windows tooling tends to produce.

use std::path::PathBuf;

use explorer_core::i18n::Lang;

use crate::plan::StartMode;

/// Process exit codes (documented in docs/INSTALL.md).
pub mod exit {
    pub const OK: i32 = 0;
    pub const ERROR: i32 = 1;
    pub const CANCELLED: i32 = 2;
    pub const REBOOT_REQUIRED: i32 = 3;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstallArgs {
    pub silent: bool,
    pub mode: Option<StartMode>,
    pub file_search: Option<bool>,
    pub no_launch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UninstallArgs {
    pub silent: bool,
    /// Also delete the user's settings and logs (silent uninstall; the UI
    /// asks with a checkbox).
    pub remove_user_data: bool,
    /// Internal: running from a %TEMP% copy; the install folder to remove.
    pub from_temp: Option<PathBuf>,
    /// Internal: wait for this process (the original uninstaller) to exit.
    pub wait_pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Install(InstallArgs),
    Uninstall(UninstallArgs),
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub command: Command,
    /// UI language override (`--lang=en|es`); default follows Windows.
    pub lang: Option<Lang>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    #[error("unknown option {0:?}")]
    Unknown(String),
    #[error("option {0} needs a value")]
    MissingValue(String),
    #[error("invalid value {1:?} for {0}")]
    BadValue(String, String),
    #[error("{0} cannot be used with {1}")]
    Conflict(String, String),
}

pub const USAGE: &str = "\
explorer-rs-setup.exe [options]

Install (default):
  --silent                  no user interface; use defaults or previous choices
  --mode=alongside|shell|none
                            start mode (default: alongside, or the previous choice)
  --file-search=on|off      fast file search helper (default: on, or previous)
  --no-launch               do not start explorer.rs at the end

Uninstall:
  --uninstall               remove explorer.rs (asks first unless --silent)
  --silent                  no user interface
  --remove-user-data        with --silent: also delete settings and logs

Other:
  --lang=en|es              user-interface language (default: Windows language)
  --help, /?                this text

Exit codes: 0 ok, 1 error, 2 cancelled, 3 ok but a reboot is required.";

/// Parse arguments (without the program name).
pub fn parse<I, S>(args: I) -> Result<Cli, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut it = args.into_iter().map(Into::into).peekable();
    let mut install = InstallArgs::default();
    let mut un = UninstallArgs::default();
    let mut uninstall = false;
    let mut help = false;
    let mut lang = None;
    let mut install_only: Option<String> = None;
    let mut uninstall_only: Option<String> = None;

    while let Some(raw) = it.next() {
        // Accept /? and /silent-style switches too (common on Windows).
        let arg = if raw == "/?" || raw == "-h" {
            "--help".to_string()
        } else if let Some(rest) = raw.strip_prefix('/') {
            format!("--{rest}")
        } else {
            raw.clone()
        };
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) => (k.to_ascii_lowercase(), Some(v.to_string())),
            None => (arg.to_ascii_lowercase(), None),
        };
        let mut value = |k: &str| -> Result<String, CliError> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => match it.peek() {
                    Some(v) if !v.starts_with("--") => Ok(it.next().unwrap()),
                    _ => Err(CliError::MissingValue(k.to_string())),
                },
            }
        };
        match key.as_str() {
            "--help" => help = true,
            "--silent" | "--quiet" | "-s" => {
                install.silent = true;
                un.silent = true;
            }
            "--mode" => {
                let v = value("--mode")?;
                install.mode = Some(
                    StartMode::parse(&v).ok_or_else(|| CliError::BadValue("--mode".into(), v))?,
                );
                install_only.get_or_insert(key.clone());
            }
            "--file-search" => {
                let v = value("--file-search")?;
                install.file_search = Some(match v.to_ascii_lowercase().as_str() {
                    "on" | "1" | "true" | "yes" => true,
                    "off" | "0" | "false" | "no" => false,
                    _ => return Err(CliError::BadValue("--file-search".into(), v)),
                });
                install_only.get_or_insert(key.clone());
            }
            "--no-launch" => {
                install.no_launch = true;
                install_only.get_or_insert(key.clone());
            }
            "--uninstall" => uninstall = true,
            "--remove-user-data" => {
                un.remove_user_data = true;
                uninstall_only.get_or_insert(key.clone());
            }
            "--uninstall-from-temp" => {
                uninstall = true;
                un.from_temp = Some(PathBuf::from(value("--uninstall-from-temp")?));
            }
            "--wait-pid" => {
                let v = value("--wait-pid")?;
                un.wait_pid = Some(
                    v.parse()
                        .map_err(|_| CliError::BadValue("--wait-pid".into(), v))?,
                );
                uninstall_only.get_or_insert(key.clone());
            }
            "--lang" => {
                let v = value("--lang")?;
                lang = Some(match v.to_ascii_lowercase().as_str() {
                    "en" => Lang::En,
                    "es" => Lang::Es,
                    _ => return Err(CliError::BadValue("--lang".into(), v)),
                });
            }
            _ => return Err(CliError::Unknown(raw)),
        }
    }

    let command = if help {
        Command::Help
    } else if uninstall {
        if let Some(opt) = install_only {
            return Err(CliError::Conflict(opt, "--uninstall".into()));
        }
        Command::Uninstall(un)
    } else {
        if let Some(opt) = uninstall_only {
            return Err(CliError::Conflict(opt, "an install".into()));
        }
        Command::Install(install)
    };
    Ok(Cli { command, lang })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Cli, CliError> {
        parse(args.iter().copied())
    }

    #[test]
    fn default_is_interactive_install() {
        assert_eq!(
            p(&[]).unwrap(),
            Cli {
                command: Command::Install(InstallArgs::default()),
                lang: None
            }
        );
    }

    #[test]
    fn silent_install_with_options() {
        let c = p(&[
            "--silent",
            "--mode=shell",
            "--file-search",
            "off",
            "--no-launch",
        ])
        .unwrap();
        assert_eq!(
            c.command,
            Command::Install(InstallArgs {
                silent: true,
                mode: Some(StartMode::Shell),
                file_search: Some(false),
                no_launch: true,
            })
        );
        let c = p(&["/SILENT", "--MODE=None", "--file-search=ON"]).unwrap();
        assert_eq!(
            c.command,
            Command::Install(InstallArgs {
                silent: true,
                mode: Some(StartMode::None),
                file_search: Some(true),
                no_launch: false,
            })
        );
    }

    #[test]
    fn uninstall_forms() {
        assert_eq!(
            p(&["--uninstall", "--silent"]).unwrap().command,
            Command::Uninstall(UninstallArgs {
                silent: true,
                ..Default::default()
            })
        );
        assert_eq!(
            p(&[
                "--uninstall-from-temp",
                r"C:\Program Files\explorer-rs",
                "--wait-pid=42",
                "--remove-user-data"
            ])
            .unwrap()
            .command,
            Command::Uninstall(UninstallArgs {
                silent: false,
                remove_user_data: true,
                from_temp: Some(PathBuf::from(r"C:\Program Files\explorer-rs")),
                wait_pid: Some(42),
            })
        );
    }

    #[test]
    fn help_and_lang() {
        assert_eq!(p(&["/?"]).unwrap().command, Command::Help);
        assert_eq!(
            p(&["--help", "--uninstall"]).unwrap().command,
            Command::Help
        );
        assert_eq!(p(&["--lang=es"]).unwrap().lang, Some(Lang::Es));
    }

    #[test]
    fn errors() {
        assert_eq!(p(&["--bogus"]), Err(CliError::Unknown("--bogus".into())));
        assert_eq!(
            p(&["--mode=desktop"]),
            Err(CliError::BadValue("--mode".into(), "desktop".into()))
        );
        assert_eq!(p(&["--mode"]), Err(CliError::MissingValue("--mode".into())));
        assert_eq!(
            p(&["--mode", "--silent"]),
            Err(CliError::MissingValue("--mode".into()))
        );
        assert!(matches!(
            p(&["--uninstall", "--mode=shell"]),
            Err(CliError::Conflict(..))
        ));
        assert!(matches!(
            p(&["--remove-user-data"]),
            Err(CliError::Conflict(..))
        ));
        assert!(matches!(
            p(&["--uninstall", "--wait-pid=x"]),
            Err(CliError::BadValue(..))
        ));
    }
}
