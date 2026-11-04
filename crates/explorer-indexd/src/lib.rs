// SPDX-License-Identifier: GPL-3.0-or-later
//! The explorer.rs file-name index daemon as a library, so that the daemon
//! loop and the IPC server can be tested in-process (on Linux with walked
//! roots and Unix sockets). The binaries are thin wrappers.

pub mod daemon;
pub mod server;

pub use daemon::Daemon;
