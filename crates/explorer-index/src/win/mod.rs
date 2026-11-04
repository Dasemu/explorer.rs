// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows-only sources. Unverified on real Windows; see docs/STATUS.md.
//!
//! - [`ntfs`]: direct `FSCTL_*` access to a volume handle (needs admin);
//! - [`watch`]: walk + `ReadDirectoryChangesW` fallback source;
//! - [`volumes`]: fixed drive and file-system discovery.

pub mod ntfs;
pub mod volumes;
pub mod watch;
