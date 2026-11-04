// SPDX-License-Identifier: GPL-3.0-or-later
//! Fallback source: directory walk + `ReadDirectoryChangesW`.
//!
//! The watch is armed *before* the initial walk so that changes made while
//! walking are not lost (they are replayed idempotently afterwards). A
//! notification buffer overflow (zero bytes returned or
//! `ERROR_NOTIFY_ENUM_DIR`) triggers a full rewalk. Parsing and translation
//! are pure functions in [`crate::notify`].

use std::path::Path;

use explorer_win::util::wide;
use explorer_win::windows::core::PCWSTR;
use explorer_win::windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_NOTIFY_ENUM_DIR, HANDLE, WAIT_OBJECT_0,
};
use explorer_win::windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use explorer_win::windows::Win32::System::Threading::{
    CreateEventW, ResetEvent, WaitForSingleObject,
};
use explorer_win::windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::index::VolId;
use crate::notify::{parse_notify, translate};
use crate::rules::Rules;
use crate::source::{Change, ChangeSink, SourceState, Stop};
use crate::walker::{walk, walk_into_changes};

struct H(HANDLE);
impl Drop for H {
    fn drop(&mut self) {
        // SAFETY: owned handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

const BUF_WORDS: usize = 8 * 1024; // 64 KiB, the documented maximum for network shares

/// Watch `root` until stopped. Returns an error if the folder cannot be
/// opened; the caller then falls back to periodic rescans.
pub fn run_watch(
    vol: VolId,
    root: &Path,
    rules: &Rules,
    sink: &dyn ChangeSink,
    stop: &Stop,
) -> anyhow::Result<()> {
    let w = wide(&root.to_string_lossy());
    // SAFETY: valid path; handle owned by `dir`.
    let dir = H(unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    }?);
    // SAFETY: anonymous manual-reset event.
    let ev = H(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }?);
    let mut buf = vec![0u64; BUF_WORDS];
    let mut ov = OVERLAPPED::default();

    let arm = |buf: &mut [u64], ov: &mut OVERLAPPED| -> anyhow::Result<()> {
        *ov = OVERLAPPED {
            hEvent: ev.0,
            ..Default::default()
        };
        // SAFETY: event handle valid.
        unsafe { ResetEvent(ev.0) }?;
        // SAFETY: buffer and OVERLAPPED stay alive and unmoved until the
        // operation completes or is cancelled below.
        let r = unsafe {
            ReadDirectoryChangesW(
                dir.0,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                true,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_SIZE,
                None,
                Some(ov),
                None,
            )
        };
        match r {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => Ok(()),
            Err(e) => Err(e.into()),
        }
    };
    let rewalk = |stop: &Stop| -> bool {
        sink.set_state(vol, SourceState::Scanning);
        match walk(root, rules, stop) {
            Some((tree, _)) => {
                sink.apply(vec![Change::ReplaceTree { vol, tree }]);
                sink.set_state(vol, SourceState::Live);
                true
            }
            None => false,
        }
    };

    arm(&mut buf, &mut ov)?;
    if !rewalk(stop) {
        // SAFETY: cancel our pending read and wait for it before returning.
        unsafe {
            let _ = CancelIoEx(dir.0, Some(&ov));
            let mut n = 0;
            let _ = GetOverlappedResult(dir.0, &ov, &mut n, true);
        }
        return Ok(());
    }
    loop {
        // SAFETY: valid event.
        if unsafe { WaitForSingleObject(ev.0, 500) } != WAIT_OBJECT_0 {
            if stop.is_stopped() {
                // SAFETY: as above.
                unsafe {
                    let _ = CancelIoEx(dir.0, Some(&ov));
                    let mut n = 0;
                    let _ = GetOverlappedResult(dir.0, &ov, &mut n, true);
                }
                sink.set_state(vol, SourceState::Stopped);
                return Ok(());
            }
            continue;
        }
        let mut n = 0u32;
        // SAFETY: the operation completed.
        let res = unsafe { GetOverlappedResult(dir.0, &ov, &mut n, false) };
        let overflow = match res {
            Ok(()) => n == 0,
            Err(e) if e.code() == ERROR_NOTIFY_ENUM_DIR.to_hresult() => true,
            Err(e) => return Err(e.into()),
        };
        // Copy out before re-arming: the kernel writes into `buf`.
        // SAFETY: reinterpreting initialized u64s as bytes.
        let data: Vec<u8> = unsafe {
            std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), (n as usize).min(BUF_WORDS * 8))
        }
        .to_vec();
        arm(&mut buf, &mut ov)?;
        if overflow {
            tracing::warn!(root = %root.display(), "change notification overflow; rewalking");
            if !rewalk(stop) {
                return Ok(());
            }
            continue;
        }
        let events = match parse_notify(&data) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "bad notification buffer; rewalking");
                rewalk(stop);
                continue;
            }
        };
        let t = translate(vol, &events, |rel| {
            let p = rel.iter().fold(root.to_path_buf(), |p, c| p.join(c));
            std::fs::symlink_metadata(p)
                .ok()
                .map(|m| (m.is_dir(), m.is_file().then_some(m.len())))
        });
        let mut changes = t.changes;
        for d in &t.walk {
            changes.extend(walk_into_changes(vol, root, d, rules));
        }
        if !changes.is_empty() {
            sink.apply(changes);
        }
    }
}
