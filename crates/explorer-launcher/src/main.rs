// SPDX-License-Identifier: GPL-3.0-or-later
//! `explorer-rs-launcher`: the explorer.rs Start menu. Pinned apps,
//! recommended files, All apps, and a keyboard-first search over apps
//! (Start Menu shortcuts + `shell:AppsFolder`, including packaged apps),
//! Windows Settings pages, files, commands and a calculator.
//! No web results, ever. See docs/START-MENU.md.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

use clap::Parser;

#[cfg(windows)]
mod app;
#[cfg_attr(not(windows), allow(dead_code))]
mod avatar;
#[cfg(windows)]
mod cue;
#[cfg(all(windows, feature = "everything"))]
// With both features the index back-end wins; keep Everything compiled.
#[cfg_attr(feature = "file-index", allow(dead_code))]
mod everything;
#[cfg(all(windows, feature = "file-index"))]
mod file_index;
#[cfg(all(windows, any(feature = "everything", feature = "file-index")))]
mod files;
#[cfg_attr(not(windows), allow(dead_code))]
mod model;
#[cfg(windows)]
mod sys;
#[cfg(windows)]
mod worker;

#[derive(Parser, Debug)]
#[command(name = "explorer-rs-launcher", version, about = "explorer.rs launcher")]
pub struct Args {
    /// Index apps, print the catalog, then exit.
    #[arg(long)]
    pub dump_apps: bool,
    /// Show the launcher immediately after start.
    #[arg(long)]
    pub show: bool,
    /// With --dump-apps: write the output to this file instead of stdout
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
        eprintln!("explorer-rs-launcher only runs on Windows");
        std::process::exit(1);
    }
}
