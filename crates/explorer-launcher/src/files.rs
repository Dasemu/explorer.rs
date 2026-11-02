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

/// Maximum number of file rows.
pub fn max_results(config: &Config) -> u32 {
    if cfg!(feature = "file-index") {
        config.index.launcher_results as u32
    } else {
        config.launcher.everything.max_results as u32
    }
}
