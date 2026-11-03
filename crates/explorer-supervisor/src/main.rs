// SPDX-License-Identifier: GPL-3.0-or-later
//! `explorer-rs`: guardian, supervisor and recovery commands.
//!
//! - `explorer-rs --dev|--shell`: the **guardian**. A tiny process that runs
//!   the supervisor as a child, restarts it with backoff, records crashes,
//!   owns the emergency hotkeys and handles crash loops by falling back to
//!   explorer.exe (see docs/RECOVERY.md, ADR 0014).
//! - `explorer-rs --supervise --dev|--shell`: the **supervisor** (started by
//!   the guardian). Starts each component (taskbar, launcher, desktop,
//!   indexd) as its own process and restarts it independently according to
//!   `explorer_core::supervisor`; reloads the config file when it changes;
//!   in shell mode runs the startup apps once per sign-in.
//! - `--restore-explorer`, `--edit-config`, `--status`: one-shot commands.
//!
//! Safety rule: without an explicit mode or command the program only prints
//! help. Registering as the Windows shell is never done here; see
//! scripts/install-shell.ps1.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

use clap::Parser;
use explorer_core::ipc::RunMode;

mod cli;
mod supervise;
#[cfg(windows)]
mod win_host;

#[derive(Parser, Debug)]
#[command(
    name = "explorer-rs",
    version,
    about = "explorer.rs: native Windows shell replacement",
    after_help = "Use --dev to try explorer.rs alongside explorer.exe.\n\
                  --shell is only meant to be used by the per-user shell registration\n\
                  done by the installer or scripts/install-shell.ps1. See docs/RECOVERY.md.\n\
                  Emergency keys: Ctrl+Alt+Shift+E starts explorer.exe,\n\
                  Ctrl+Alt+Shift+R restarts explorer.rs."
)]
pub struct Args {
    /// Windowed dev mode: run next to explorer.exe without replacing it.
    #[arg(long, conflicts_with = "shell")]
    pub dev: bool,
    /// Shell mode: explorer.rs is the user's shell (no explorer.exe).
    #[arg(long)]
    pub shell: bool,
    /// Internal: run the supervisor itself (started by the guardian).
    #[arg(long, hide = true)]
    pub supervise: bool,
    /// Use this config file instead of %APPDATA%\explorer-rs\config.toml.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Run only one component.
    #[arg(long, value_name = "COMPONENT", value_parser = ["taskbar", "launcher", "indexd", "desktop"])]
    pub only: Option<String>,
    /// Restore Windows Explorer as this user's shell, stop explorer.rs and
    /// start explorer.exe if it is not running.
    #[arg(long, conflicts_with_all = ["dev", "shell", "supervise", "edit_config", "status"])]
    pub restore_explorer: bool,
    /// Create the config file from the default if missing and open it in
    /// Notepad.
    #[arg(long, conflicts_with_all = ["dev", "shell", "supervise", "status"])]
    pub edit_config: bool,
    /// Print a diagnostics report (version, mode, components, shell
    /// registration, config validity, recent crash-log events).
    #[arg(long, conflicts_with_all = ["dev", "shell", "supervise"])]
    pub status: bool,
    /// With --status: write the report to this file instead of stdout.
    #[arg(long, value_name = "PATH", requires = "status")]
    pub output: Option<PathBuf>,
}

fn main() {
    #[cfg(windows)]
    let console = explorer_win::console::attach_parent();
    #[cfg(not(windows))]
    let console = true;

    let args = Args::parse();
    if args.restore_explorer {
        std::process::exit(cli::restore_explorer(console));
    }
    if args.edit_config {
        std::process::exit(cli::edit_config(args.config.as_deref()));
    }
    if args.status {
        std::process::exit(cli::status(args.config.as_deref(), args.output.as_deref()));
    }
    let mode = match (args.dev, args.shell) {
        (true, _) => RunMode::Dev,
        (_, true) => RunMode::Shell,
        _ => {
            use clap::CommandFactory;
            let _ = Args::command().print_help();
            eprintln!("\nRefusing to start without --dev or --shell.");
            std::process::exit(2);
        }
    };
    let code = if args.supervise {
        supervise::main(mode, args.config.as_deref(), args.only.as_deref())
    } else {
        cli::guardian_main(mode, &args)
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_explicit_mode_flag() {
        let a = Args::try_parse_from(["explorer-rs"]).unwrap();
        assert!(!a.dev && !a.shell);
        assert!(Args::try_parse_from(["explorer-rs", "--dev", "--shell"]).is_err());
        assert!(Args::try_parse_from(["explorer-rs", "--dev", "--only", "tray"]).is_err());
        assert!(Args::try_parse_from(["explorer-rs", "--dev", "--only", "desktop"]).is_ok());
    }

    #[test]
    fn commands_are_exclusive() {
        assert!(Args::try_parse_from(["explorer-rs", "--restore-explorer"]).is_ok());
        assert!(Args::try_parse_from(["explorer-rs", "--restore-explorer", "--shell"]).is_err());
        assert!(Args::try_parse_from(["explorer-rs", "--status", "--output", "x.txt"]).is_ok());
        assert!(Args::try_parse_from(["explorer-rs", "--output", "x.txt"]).is_err());
        assert!(Args::try_parse_from(["explorer-rs", "--edit-config", "--status"]).is_err());
        let a = Args::try_parse_from([
            "explorer-rs",
            "--supervise",
            "--shell",
            "--config",
            "c.toml",
            "--only",
            "taskbar",
        ])
        .unwrap();
        assert!(a.supervise && a.shell);
    }

    #[test]
    fn guardian_passes_flags_to_supervisor() {
        let a = Args::try_parse_from([
            "explorer-rs",
            "--dev",
            "--config",
            "C:/c.toml",
            "--only",
            "launcher",
        ])
        .unwrap();
        let child = cli::supervisor_args(RunMode::Dev, &a);
        let child: Vec<String> = child
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            child,
            [
                "--supervise",
                "--dev",
                "--config",
                "C:/c.toml",
                "--only",
                "launcher"
            ]
        );
        // The supervisor accepts what the guardian passes.
        let mut full = vec!["explorer-rs".to_string()];
        full.extend(child);
        assert!(Args::try_parse_from(full).unwrap().supervise);
    }
}
