// SPDX-License-Identifier: GPL-3.0-or-later
//! explorer.rs installer (`explorer-rs-setup.exe`).
//!
//! Portable, unit-tested modules ([`payload`], [`cli`], [`plan`],
//! [`task_xml`]) decide everything; the `win` module only executes the plan
//! and shows the wizard. See docs/decisions/0013-installer.md.

pub mod cli;
pub mod payload;
pub mod plan;
pub mod task_xml;

#[cfg(windows)]
pub mod win;
