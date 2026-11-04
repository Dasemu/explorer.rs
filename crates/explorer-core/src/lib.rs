// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform-independent core of explorer.rs.
//!
//! Everything in this crate must compile and be unit-testable on Linux. It
//! contains no Win32 calls: the `explorer-win` crate gathers raw data from the
//! OS and hands it to the pure functions defined here.

pub mod calc;
pub mod calendar;
pub mod catalog;
pub mod clock;
pub mod components;
pub mod config;
pub mod crashlog;
pub mod dump;
pub mod flyout_layout;
pub mod guardian;
pub mod hotkey;
pub mod i18n;
pub mod image;
pub mod ipc;
pub mod layout;
pub mod logging;
pub mod paths;
pub mod recent;
pub mod results;
pub mod run_command;
pub mod search;
pub mod settings_pages;
pub mod setup;
pub mod shellpath;
pub mod shellreg;
pub mod start;
pub mod startup;
pub mod supervisor;
pub mod system_glyphs;
pub mod taskbar_groups;
pub mod taskbar_layout;
pub mod taskbar_model;
pub mod theme;
pub mod tray;
pub mod tray_promotion;
pub mod wallpaper;
pub mod window_filter;

/// Human-readable product name.
pub const PRODUCT_NAME: &str = "explorer.rs";

/// Folder name used under `%APPDATA%` / `%LOCALAPPDATA%`.
pub const APP_DIR_NAME: &str = "explorer-rs";
