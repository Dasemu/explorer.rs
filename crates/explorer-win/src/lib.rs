// SPDX-License-Identifier: GPL-3.0-or-later
//! Thin Win32/COM wrappers used by the explorer.rs binaries.
//!
//! Everything here is `#[cfg(windows)]`: on other targets the crate is empty
//! so the workspace still builds and the pure logic in `explorer-core` can be
//! tested on Linux. Keep these wrappers thin — decisions belong in
//! `explorer-core` where they can be unit-tested.
//!
//! None of this code is verified on real Windows yet; see docs/STATUS.md.

#[cfg(windows)]
pub mod activate;
#[cfg(windows)]
pub mod appbar;
#[cfg(windows)]
pub mod appident;
#[cfg(windows)]
pub mod apps;
#[cfg(windows)]
pub mod brightness;
#[cfg(windows)]
pub mod desktop;
#[cfg(windows)]
pub mod dialog;
#[cfg(windows)]
pub mod dpi;
#[cfg(windows)]
pub mod gfx;
#[cfg(windows)]
pub mod icons;
#[cfg(windows)]
pub mod ipc;
#[cfg(windows)]
pub mod job;
#[cfg(windows)]
pub mod keyboard;
#[cfg(windows)]
pub mod monitors;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
pub mod sched;
#[cfg(windows)]
pub mod secdir;
#[cfg(windows)]
pub mod security;
#[cfg(windows)]
pub mod session;
#[cfg(windows)]
pub mod shellhook;
#[cfg(windows)]
pub mod shellreg;
#[cfg(windows)]
pub mod shortcut;
#[cfg(windows)]
pub mod single_instance;
#[cfg(windows)]
pub mod startup;
#[cfg(windows)]
pub mod theme;
#[cfg(windows)]
pub mod thumbnails;
#[cfg(windows)]
pub mod time;
#[cfg(windows)]
pub mod tooltip;
#[cfg(windows)]
pub mod util;
#[cfg(windows)]
pub mod wallpaper;
#[cfg(windows)]
pub mod window;
#[cfg(windows)]
pub mod winlist;

#[cfg(windows)]
pub mod console;
#[cfg(windows)]
pub mod quick_settings;
#[cfg(windows)]
pub mod tray;
#[cfg(all(windows, feature = "virtual-desktops"))]
pub mod vdesk;

/// Re-export so binaries share one `windows` feature set.
#[cfg(windows)]
pub use windows;
