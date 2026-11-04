// SPDX-License-Identifier: GPL-3.0-or-later
//! Names and small decisions shared between the installer
//! (`explorer-rs-setup.exe`, crate `explorer-setup`) and the programs it
//! installs. The installer's full planning logic lives in
//! `explorer_setup::plan`; only what the other binaries need is here.

/// Machine-wide installer state: `HKLM\Software\explorer-rs` (64-bit view).
pub const MACHINE_KEY: &str = r"Software\explorer-rs";
/// `REG_SZ`: install folder, e.g. `C:\Program Files\explorer-rs`.
pub const VALUE_INSTALL_DIR: &str = "InstallDir";
/// `REG_SZ`: installed version.
pub const VALUE_VERSION: &str = "Version";
/// `REG_SZ`: start mode chosen in the installer (`alongside`, `shell`, `none`).
pub const VALUE_MODE: &str = "Mode";
/// `REG_DWORD`: 1 if "Fast file search" was chosen (index helper task
/// registered), 0 otherwise.
pub const VALUE_FILE_SEARCH: &str = "FileSearch";

/// Per-user `Run` value name used for "run alongside Windows Explorer".
pub const RUN_VALUE_NAME: &str = "explorer-rs";
/// `Run` key, relative to a user hive.
pub const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
