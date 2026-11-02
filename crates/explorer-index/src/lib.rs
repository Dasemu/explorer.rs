// SPDX-License-Identifier: GPL-3.0-or-later
//! File-name index engine for explorer.rs.
//!
//! Portable parts (compiled and tested everywhere):
//! - [`index`]: the compact in-memory index;
//! - [`query`]: query language, evaluation and ranking;
//! - [`snapshot`]: versioned, checksummed on-disk snapshots;
//! - [`rules`]: exclusion rules;
//! - [`usn`]: pure USN record parsing, MFT tree building and journal replay,
//!   plus the NTFS source state machine over an abstract backend;
//! - [`notify`]: pure `FILE_NOTIFY_INFORMATION` parsing;
//! - [`walker`]: directory-walk source (std::fs);
//! - [`source`]: change events shared by all sources;
//! - [`protocol`]: IPC messages and length-prefixed framing;
//! - [`transport`]: named pipes (Windows) / Unix sockets (elsewhere);
//! - [`access`]: result filtering by the caller's access rights.
//! - [`helper`]: RPC with the elevated MFT/USN helper.
//!
//! Windows-only parts live in [`win`]. See docs/INDEX.md.

pub mod access;
pub mod helper;
pub mod index;
pub mod notify;
pub mod protocol;
pub mod query;
pub mod rules;
pub mod snapshot;
pub mod source;
pub mod synth;
pub mod transport;
pub mod usn;
pub mod walker;

#[cfg(windows)]
pub mod win;

pub use index::{EntryId, Index, VolId, VolumeKind, NONE};
pub use query::{search, Query, QueryOptions};
pub use rules::Rules;
