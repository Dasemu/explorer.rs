// SPDX-License-Identifier: GPL-3.0-or-later
//! Selects the back-end of the "Files" results section.
//!
//! - feature `file-index`: the explorer.rs index daemon (`file_index.rs`);
//! - feature `everything` (without `file-index`): the Everything SDK.
//!
//! Both workers have the same interface (`start`, `query`, `results`).

use explorer_core::config::Config;

#[cfg(feature = "file-index")]
pub use crate::file_index::IndexWorker as FileWorker;

#[cfg(all(feature = "everything", not(feature = "file-index")))]
pub use crate::everything::EverythingWorker as FileWorker;

/// Whether file results are wanted for this configuration.
pub fn enabled(config: &Config) -> bool {
    if cfg!(feature = "file-index") {
        config.index.launcher_results > 0
    } else {
        config.launcher.everything.enabled
    }
}

/// Hits asked from the back-end per wanted row: many are dropped as
/// system or application files (`explorer_core::filerank`).
const OVERFETCH: u32 = 6;

/// Filter and order raw back-end hits for the Start menu.
pub fn rank(
    hits: Vec<explorer_core::filerank::FileHit>,
    limit: u32,
) -> Vec<explorer_core::filerank::FileHit> {
    let home = std::env::var("USERPROFILE").ok();
    explorer_core::filerank::rank(hits, home.as_deref(), limit as usize)
}

/// How many hits to ask the back-end for `rows` results.
pub fn fetch_count(rows: u32) -> u32 {
    rows.saturating_mul(OVERFETCH).clamp(rows, 100)
}

/// Maximum number of file rows.
pub fn max_results(config: &Config) -> u32 {
    if cfg!(feature = "file-index") {
        config.index.launcher_results as u32
    } else {
        config.launcher.everything.max_results as u32
    }
}
