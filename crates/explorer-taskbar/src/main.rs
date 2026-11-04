// SPDX-License-Identifier: GPL-3.0-or-later
//! `explorer-rs-taskbar`: one AppBar per monitor with the window list,
//! a start button that toggles the launcher, and a clock.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

use clap::Parser;

#[cfg(windows)]
mod app;

#[derive(Parser, Debug)]
#[command(name = "explorer-rs-taskbar", version, about = "explorer.rs taskbar")]
pub struct Args {
    /// Print every top-level window and whether/why it gets a button, then exit.
    #[arg(long)]
    pub dump_windows: bool,
    /// Print monitors (rects, work areas, DPI), then exit.
    #[arg(long)]
    pub dump_monitors: bool,
    /// With --dump-*: write the output to this file instead of stdout
    /// (recommended: GUI-subsystem programs may not print to a console).
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
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
        eprintln!("explorer-rs-taskbar only runs on Windows");
        std::process::exit(1);
    }
}
