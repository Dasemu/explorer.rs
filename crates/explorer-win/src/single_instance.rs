// SPDX-License-Identifier: GPL-3.0-or-later
//! Named-mutex single-instance guard.

use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;

use crate::util::WideString;

pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: handle owned by us.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Returns `None` if another process already holds `name` (session-local).
pub fn acquire(name: &str) -> Option<InstanceGuard> {
    let full = WideString::new(&format!("Local\\{name}"));
    // SAFETY: valid name string; error checked via GetLastError right after.
    unsafe {
        let h = CreateMutexW(None, true, full.pcwstr()).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(h);
            return None;
        }
        Some(InstanceGuard(h))
    }
}
