// SPDX-License-Identifier: GPL-3.0-or-later
//! `explorer-rs-setup.exe`: installer, and (copied as `uninstall.exe`)
//! uninstaller. See docs/INSTALL.md for the command line.

#![cfg_attr(windows, windows_subsystem = "windows")]

/// Payload selected by build.rs (empty unless built by
/// scripts/build-windows.sh).
#[cfg(windows)]
static PAYLOAD: &[u8] = include_bytes!(env!("EXPLORER_RS_PAYLOAD_FILE"));

#[cfg(windows)]
fn main() {
    std::process::exit(explorer_setup::win::run(PAYLOAD));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("explorer-rs-setup only runs on Windows.");
    eprintln!("{}", explorer_setup::cli::USAGE);
    std::process::exit(explorer_setup::cli::exit::ERROR);
}
