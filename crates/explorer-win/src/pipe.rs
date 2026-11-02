// SPDX-License-Identifier: GPL-3.0-or-later
//! Named pipes with explicit security descriptors and overlapped I/O.
//!
//! Handles are opened with `FILE_FLAG_OVERLAPPED` so that one thread can
//! block in `read` while another thread writes on the same pipe (on a
//! synchronous handle the I/O manager would serialize the two, and a pending
//! read would block every write). Each operation waits for its own event, so
//! the API stays blocking and simple. `cancel_io` aborts pending operations
//! from any thread.
//!
//! Server pipes are created from an SDDL string (see
//! docs/decisions/0010-index-privilege-model.md for the strings used) and
//! always with `PIPE_REJECT_REMOTE_CLIENTS`.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::util::wide;

fn win_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

fn is(e: &windows::core::Error, code: windows::Win32::Foundation::WIN32_ERROR) -> bool {
    e.code() == code.to_hresult()
}

struct OwnedHandle(HANDLE);
// SAFETY: kernel handles may be used from any thread.
unsafe impl Send for OwnedHandle {}
// SAFETY: all operations on the handle are thread-safe kernel calls.
unsafe impl Sync for OwnedHandle {}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn event() -> io::Result<OwnedHandle> {
    // SAFETY: anonymous manual-reset event.
    let h = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(win_err)?;
    Ok(OwnedHandle(h))
}

/// A connected pipe end. Clones share the handle.
#[derive(Clone)]
pub struct PipeStream {
    h: Arc<OwnedHandle>,
}

impl PipeStream {
    /// Connect to `name` (`\\.\pipe\...`), waiting up to `timeout` while all
    /// instances are busy. A missing pipe fails immediately (`NotFound`).
    pub fn connect(name: &str, timeout: Duration) -> io::Result<Self> {
        let w = wide(name);
        let deadline = Instant::now() + timeout;
        loop {
            // SAFETY: valid NUL-terminated name. SECURITY_IDENTIFICATION
            // prevents a malicious server from impersonating us.
            let r = unsafe {
                CreateFileW(
                    PCWSTR(w.as_ptr()),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    None,
                )
            };
            match r {
                Ok(h) => {
                    return Ok(Self {
                        h: Arc::new(OwnedHandle(h)),
                    })
                }
                Err(e) if is(&e, ERROR_PIPE_BUSY) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    // SAFETY: valid name.
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(w.as_ptr()), left.as_millis() as u32) };
                }
                Err(e) => return Err(win_err(e)),
            }
        }
    }

    fn overlapped(
        &self,
        op: impl FnOnce(HANDLE, *mut OVERLAPPED) -> windows::core::Result<()>,
    ) -> io::Result<usize> {
        let ev = event()?;
        let mut ov = OVERLAPPED {
            hEvent: ev.0,
            ..Default::default()
        };
        match op(self.h.0, &mut ov) {
            Ok(()) => {}
            Err(e) if is(&e, ERROR_IO_PENDING) => {}
            Err(e) => return Err(win_err(e)),
        }
        let mut n = 0u32;
        // SAFETY: `ov` stays alive until the operation completed (bWait).
        unsafe { GetOverlappedResult(self.h.0, &ov, &mut n, true) }.map_err(win_err)?;
        Ok(n as usize)
    }

    /// Abort pending reads/writes on this pipe (any thread).
    pub fn cancel_io(&self) {
        // SAFETY: valid handle.
        let _ = unsafe { CancelIoEx(self.h.0, None) };
    }

    /// Process id of the server end (client side only).
    pub fn server_pid(&self) -> io::Result<u32> {
        let mut pid = 0;
        // SAFETY: valid handle and out pointer.
        unsafe { GetNamedPipeServerProcessId(self.h.0, &mut pid) }.map_err(win_err)?;
        Ok(pid)
    }

    /// Process id of the client end (server side only).
    pub fn client_pid(&self) -> io::Result<u32> {
        let mut pid = 0;
        // SAFETY: valid handle and out pointer.
        unsafe { GetNamedPipeClientProcessId(self.h.0, &mut pid) }.map_err(win_err)?;
        Ok(pid)
    }
}

fn eof(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(|c| c as u32),
        Some(c) if c == ERROR_BROKEN_PIPE.0 || c == ERROR_PIPE_NOT_CONNECTED.0 || c == ERROR_NO_DATA.0
    )
}

impl io::Read for &PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(1 << 20);
        let ptr = buf.as_mut_ptr();
        // SAFETY: the buffer outlives the (awaited) operation.
        let r = self.overlapped(|h, ov| unsafe {
            ReadFile(
                h,
                Some(std::slice::from_raw_parts_mut(ptr, len)),
                None,
                Some(ov),
            )
        });
        match r {
            Err(e) if eof(&e) => Ok(0),
            r => r,
        }
    }
}

impl io::Write for &PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(1 << 20);
        let ptr = buf.as_ptr();
        // SAFETY: the buffer outlives the (awaited) operation.
        self.overlapped(|h, ov| unsafe {
            WriteFile(
                h,
                Some(std::slice::from_raw_parts(ptr, len)),
                None,
                Some(ov),
            )
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl io::Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
}

impl io::Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);
// SAFETY: the descriptor is immutable LocalAlloc'ed memory.
unsafe impl Send for SecurityDescriptor {}
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        let _ = unsafe { LocalFree(Some(HLOCAL(self.0 .0))) };
    }
}

/// Listening side of a named pipe.
pub struct PipeServer {
    name: Vec<u16>,
    sd: SecurityDescriptor,
    first: bool,
}

impl PipeServer {
    /// Prepare a server for `name` secured by `sddl`. No pipe instance is
    /// created until [`PipeServer::accept`].
    pub fn new(name: &str, sddl: &str) -> io::Result<Self> {
        let w = wide(sddl);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: valid string; `sd` receives a LocalAlloc'ed descriptor.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(w.as_ptr()),
                1,
                &mut sd,
                None,
            )
        }
        .map_err(win_err)?;
        Ok(Self {
            name: wide(name),
            sd: SecurityDescriptor(sd),
            first: true,
        })
    }

    /// Wait for a client. Polls `stop` every 200 ms; returns `Ok(None)` when
    /// it returns true. The first instance is created with
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE`, so the call fails if another process
    /// already owns the name (pipe squatting).
    pub fn accept(&mut self, stop: &dyn Fn() -> bool) -> io::Result<Option<PipeStream>> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.sd.0 .0,
            bInheritHandle: false.into(),
        };
        let mut open = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if self.first {
            open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: valid name and security attributes.
        let h = unsafe {
            CreateNamedPipeW(
                PCWSTR(self.name.as_ptr()),
                open,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                Some(&sa),
            )
        };
        if h == INVALID_HANDLE_VALUE || h.is_invalid() {
            return Err(io::Error::last_os_error());
        }
        self.first = false;
        let pipe = PipeStream {
            h: Arc::new(OwnedHandle(h)),
        };
        let ev = event()?;
        let mut ov = OVERLAPPED {
            hEvent: ev.0,
            ..Default::default()
        };
        // SAFETY: `ov` outlives the operation: we either wait for it or
        // cancel it and wait for the cancellation below.
        match unsafe { ConnectNamedPipe(h, Some(&mut ov)) } {
            Ok(()) => return Ok(Some(pipe)),
            Err(e) if is(&e, ERROR_PIPE_CONNECTED) => return Ok(Some(pipe)),
            Err(e) if is(&e, ERROR_IO_PENDING) => {}
            Err(e) => return Err(win_err(e)),
        }
        loop {
            // SAFETY: valid event handle.
            if unsafe { WaitForSingleObject(ev.0, 200) } == WAIT_OBJECT_0 {
                let mut n = 0;
                // SAFETY: operation completed.
                unsafe { GetOverlappedResult(h, &ov, &mut n, false) }.map_err(win_err)?;
                return Ok(Some(pipe));
            }
            if stop() {
                pipe.cancel_io();
                let mut n = 0;
                // SAFETY: wait for the cancelled operation before `ov` dies.
                let _ = unsafe { GetOverlappedResult(h, &ov, &mut n, true) };
                return Ok(None);
            }
        }
    }
}
