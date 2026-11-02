// SPDX-License-Identifier: GPL-3.0-or-later
//! Fixed drives and their file systems.

use explorer_win::util::{from_wide, wide};
use explorer_win::windows::core::PCWSTR;
use explorer_win::windows::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
};

const DRIVE_FIXED: u32 = 3;

/// Roots of all fixed local drives, e.g. `["C:\\", "D:\\"]`.
pub fn fixed_drives() -> Vec<String> {
    let mut buf = vec![0u16; 512];
    // SAFETY: buffer is valid for its length.
    let n = unsafe { GetLogicalDriveStringsW(Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return Vec::new();
    }
    buf[..n]
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .filter(|root| {
            let w = wide(root);
            // SAFETY: valid NUL-terminated string.
            unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) == DRIVE_FIXED }
        })
        .collect()
}

/// File-system name (`NTFS`, `ReFS`, `FAT32`, ...) of a volume root.
pub fn filesystem_name(root: &str) -> Option<String> {
    let mut r = root.to_string();
    if !r.ends_with('\\') {
        r.push('\\');
    }
    let w = wide(&r);
    let mut fs = vec![0u16; 64];
    // SAFETY: valid string and buffers.
    unsafe {
        GetVolumeInformationW(PCWSTR(w.as_ptr()), None, None, None, None, Some(&mut fs)).ok()?;
    }
    Some(from_wide(&fs))
}

/// Drive letter of a volume root such as `C:\` (None for folders or UNC).
pub fn drive_letter(root: &str) -> Option<u8> {
    let b = root.trim_end_matches(['\\', '/']).as_bytes();
    (b.len() == 2 && b[1] == b':' && b[0].is_ascii_alphabetic()).then(|| b[0].to_ascii_uppercase())
}
