// SPDX-License-Identifier: GPL-3.0-or-later
//! Console attachment for GUI-subsystem binaries.
//!
//! All binaries use the Windows GUI subsystem so that nothing flashes a
//! console window when launched by Winlogon or the supervisor. When started
//! from a terminal (dev mode), we attach to the parent console so that log
//! output on stderr remains visible there.

use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};

/// Attach to the parent's console if there is one. Harmless otherwise.
pub fn attach_parent() -> bool {
    // SAFETY: plain FFI call.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok()
}
