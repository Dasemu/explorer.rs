// SPDX-License-Identifier: GPL-3.0-or-later
//! Small helpers: UTF-16 strings, HWND <-> integer, COM apartments.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    COINIT_MULTITHREADED,
};

/// NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Convert a (possibly NUL-terminated) UTF-16 buffer to a `String`.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn hwnd_to_u64(h: HWND) -> u64 {
    h.0 as usize as u64
}

pub fn u64_to_hwnd(v: u64) -> HWND {
    HWND(v as usize as *mut core::ffi::c_void)
}

/// Owned wide string usable as a `PCWSTR` while alive.
pub struct WideString(HSTRING);

impl WideString {
    pub fn new(s: &str) -> Self {
        Self(HSTRING::from(s))
    }

    pub fn pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }
}

/// RAII COM initialisation for the current thread.
pub struct ComApartment {
    initialized: bool,
}

impl ComApartment {
    /// Single-threaded apartment: required on UI threads and for most shell
    /// objects.
    pub fn sta() -> Self {
        // SAFETY: plain FFI call; balanced by CoUninitialize in Drop.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        Self {
            initialized: hr.is_ok(),
        }
    }

    /// Multi-threaded apartment, for worker threads.
    pub fn mta() -> Self {
        // SAFETY: plain FFI call; balanced by CoUninitialize in Drop.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self {
            initialized: hr.is_ok(),
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: matches a successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Path of a known folder (e.g. `FOLDERID_ProgramData`), without a
/// trailing backslash.
pub fn known_folder(id: &windows::core::GUID) -> anyhow::Result<String> {
    use anyhow::Context;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    // SAFETY: the returned buffer is freed with CoTaskMemFree.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).context("SHGetKnownFolderPath")?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        if s.is_empty() {
            anyhow::bail!("empty known folder path");
        }
        Ok(s.trim_end_matches('\\').to_string())
    }
}
