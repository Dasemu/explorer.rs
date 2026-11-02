// SPDX-License-Identifier: GPL-3.0-or-later
//! Direct MFT / USN journal access through a volume handle.
//!
//! Opening `\\.\C:` requires administrator rights, so this backend is used
//! by the elevated helper (and by the daemon itself only when it happens to
//! run elevated). Output buffers are handed to the pure parser in
//! [`crate::usn`] unchanged, minus the leading 8-byte FRN/USN.

use explorer_win::util::wide;
use explorer_win::windows::core::{Error as WinError, PCWSTR};
use explorer_win::windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_HANDLE_EOF, ERROR_INVALID_PARAMETER,
    ERROR_JOURNAL_DELETE_IN_PROGRESS, ERROR_JOURNAL_ENTRY_DELETED, ERROR_JOURNAL_NOT_ACTIVE,
    GENERIC_READ, HANDLE, WIN32_ERROR,
};
use explorer_win::windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use explorer_win::windows::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V1,
    READ_USN_JOURNAL_DATA_V1, USN_JOURNAL_DATA_V0,
};
use explorer_win::windows::Win32::System::IO::DeviceIoControl;

use crate::usn::{JournalInfo, UsnBackend, UsnError};

const BUF: usize = 1 << 20;

pub struct DirectBackend {
    h: HANDLE,
    buf: Vec<u64>,
}

// SAFETY: a volume handle may be used from any thread; we use it from one.
unsafe impl Send for DirectBackend {}

impl Drop for DirectBackend {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        let _ = unsafe { CloseHandle(self.h) };
    }
}

fn is(e: &WinError, c: WIN32_ERROR) -> bool {
    e.code() == c.to_hresult()
}

fn map(e: WinError) -> UsnError {
    if is(&e, ERROR_JOURNAL_ENTRY_DELETED) {
        UsnError::EntryDeleted
    } else if is(&e, ERROR_JOURNAL_NOT_ACTIVE) || is(&e, ERROR_JOURNAL_DELETE_IN_PROGRESS) {
        UsnError::NotActive
    } else if is(&e, ERROR_INVALID_PARAMETER) {
        // Most likely a stale journal id; the caller rescans.
        UsnError::IdMismatch
    } else if is(&e, ERROR_ACCESS_DENIED) {
        UsnError::AccessDenied
    } else {
        UsnError::Io(e.to_string())
    }
}

impl DirectBackend {
    /// Open drive `letter` (e.g. `b'C'`). Fails with `AccessDenied` when
    /// not elevated.
    pub fn open(letter: u8) -> Result<Self, UsnError> {
        let path = wide(&format!(r"\\.\{}:", letter.to_ascii_uppercase() as char));
        // SAFETY: valid path; handle owned by the returned value.
        let h = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
        .map_err(map)?;
        Ok(Self {
            h,
            buf: vec![0u64; BUF / 8],
        })
    }

    fn ioctl<T>(&mut self, code: u32, input: Option<&T>) -> Result<usize, WinError> {
        let mut n = 0u32;
        // SAFETY: input points to a properly sized struct, output buffer is
        // valid and 8-byte aligned.
        unsafe {
            DeviceIoControl(
                self.h,
                code,
                input.map(|i| i as *const T as *const core::ffi::c_void),
                input.map_or(0, |_| std::mem::size_of::<T>() as u32),
                Some(self.buf.as_mut_ptr().cast()),
                (self.buf.len() * 8) as u32,
                Some(&mut n),
                None,
            )
        }?;
        Ok(n as usize)
    }

    fn bytes(&self, n: usize) -> &[u8] {
        // SAFETY: reinterpreting initialized u64s as bytes.
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), n.min(BUF)) }
    }
}

impl UsnBackend for DirectBackend {
    fn query(&mut self) -> Result<JournalInfo, UsnError> {
        let n = self
            .ioctl::<u8>(FSCTL_QUERY_USN_JOURNAL, None)
            .map_err(map)?;
        if n < std::mem::size_of::<USN_JOURNAL_DATA_V0>() {
            return Err(UsnError::Io("short FSCTL_QUERY_USN_JOURNAL reply".into()));
        }
        // SAFETY: buffer holds at least a USN_JOURNAL_DATA_V0 and is aligned.
        let d = unsafe { &*(self.buf.as_ptr() as *const USN_JOURNAL_DATA_V0) };
        Ok(JournalInfo {
            journal_id: d.UsnJournalID,
            first_usn: d.FirstUsn,
            next_usn: d.NextUsn,
        })
    }

    fn enum_chunk(
        &mut self,
        start_frn: u64,
        high_usn: i64,
    ) -> Result<Option<(u64, Vec<u8>)>, UsnError> {
        let input = MFT_ENUM_DATA_V1 {
            StartFileReferenceNumber: start_frn,
            LowUsn: 0,
            HighUsn: high_usn,
            MinMajorVersion: 2,
            MaxMajorVersion: 3,
        };
        match self.ioctl(FSCTL_ENUM_USN_DATA, Some(&input)) {
            Ok(n) if n >= 8 => {
                let b = self.bytes(n);
                let next = u64::from_le_bytes(b[..8].try_into().unwrap_or_default());
                Ok(Some((next, b[8..].to_vec())))
            }
            Ok(_) => Ok(None),
            // The end of the enumeration is reported as ERROR_HANDLE_EOF.
            Err(e) if is(&e, ERROR_HANDLE_EOF) => Ok(None),
            Err(e) => Err(map(e)),
        }
    }

    fn read_journal(
        &mut self,
        journal_id: u64,
        start_usn: i64,
    ) -> Result<(i64, Vec<u8>), UsnError> {
        let input = READ_USN_JOURNAL_DATA_V1 {
            StartUsn: start_usn,
            ReasonMask: 0xFFFF_FFFF,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: journal_id,
            MinMajorVersion: 2,
            MaxMajorVersion: 3,
        };
        let n = self
            .ioctl(FSCTL_READ_USN_JOURNAL, Some(&input))
            .map_err(map)?;
        if n < 8 {
            return Ok((start_usn, Vec::new()));
        }
        let b = self.bytes(n);
        let next = i64::from_le_bytes(b[..8].try_into().unwrap_or_default());
        Ok((next, b[8..].to_vec()))
    }
}
