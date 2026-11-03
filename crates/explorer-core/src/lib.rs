// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform-independent core of explorer.rs.
//!
//! Everything in this crate must compile and be unit-testable on Linux. It
//! contains no Win32 calls: the `explorer-win` crate gathers raw data from the
//! OS and hands it to the pure functions defined here.

pub mod catalog;
pub mod clock;
pub mod components;
pub mod config;
pub mod crashlog;
pub mod dump;
pub mod guardian;
pub mod hotkey;
pub mod i18n;
pub mod image;
pub mod ipc;
pub mod layout;
pub mod logging;
pub mod paths;
pub mod search;
pub mod shellreg;
pub mod start;
pub mod startup;
pub mod supervisor;
pub mod taskbar_model;
pub mod theme;
pub mod tray;
pub mod wallpaper;
pub mod window_filter;

/// Human-readable product name.
pub const PRODUCT_NAME: &str = "explorer.rs";

/// Folder name used under `%APPDATA%` / `%LOCALAPPDATA%`.
pub const APP_DIR_NAME: &str = "explorer-rs";
