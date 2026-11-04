// SPDX-License-Identifier: GPL-3.0-or-later
//! `explorer-rs-desktop`: the desktop background (wallpaper) window.
//!
//! Runs only when `[desktop] enabled` allows it for the current mode
//! (default: shell mode only); otherwise it exits at once with code 0 so
//! the supervisor does not restart it. There are no desktop icons (see
//! docs/SHELL-MODE.md).

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

use clap::Parser;

#[cfg(windows)]
mod app;

#[derive(Parser, Debug)]
#[command(
    name = "explorer-rs-desktop",
    version,
    about = "explorer.rs desktop (wallpaper)"
)]
pub struct Args {
    /// Use this config file instead of %APPDATA%\explorer-rs\config.toml.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();
    #[cfg(windows)]
    {
        let code = app::run(args);
        std::process::exit(code);
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        eprintln!("explorer-rs-desktop only runs on Windows");
        // Exit 0 (= "stop, do not restart") so a supervisor never loops.
        std::process::exit(0);
    }
}
