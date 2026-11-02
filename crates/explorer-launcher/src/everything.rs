// SPDX-License-Identifier: GPL-3.0-or-later
//! Optional file search through the Everything SDK (feature `everything`).
//!
//! `Everything64.dll` (from the voidtools SDK) is loaded at runtime from the
//! launcher's folder or the DLL search path; it is not linked or shipped.
//! Everything itself must be running. Queries run on a dedicated worker
//! thread so a slow or hung Everything never blocks the launcher UI.
//! Unverified on Windows.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use explorer_win::util::WideString;
use explorer_win::windows::core::{BOOL, PCSTR, PCWSTR, PWSTR};
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use explorer_win::windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::PostMessageW;

type SetSearchW = unsafe extern "system" fn(PCWSTR);
type SetMax = unsafe extern "system" fn(u32);
type QueryW = unsafe extern "system" fn(BOOL) -> BOOL;
type GetNumResults = unsafe extern "system" fn() -> u32;
type GetResultFullPathNameW = unsafe extern "system" fn(u32, PWSTR, u32) -> u32;

struct Api {
    set_search: SetSearchW,
    set_max: SetMax,
    query: QueryW,
    num: GetNumResults,
    full_path: GetResultFullPathNameW,
}

fn load() -> Option<Api> {
    let name = WideString::new("Everything64.dll");
    // SAFETY: loading a DLL by name and resolving documented exports; the
    // function pointer types match the SDK's Everything.h declarations.
    unsafe {
        let lib = LoadLibraryW(name.pcwstr()).ok()?;
        macro_rules! sym {
            ($n:literal, $t:ty) => {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, $t>(GetProcAddress(
                    lib,
                    PCSTR(concat!($n, "\0").as_ptr()),
                )?)
            };
        }
        Some(Api {
            set_search: sym!("Everything_SetSearchW", SetSearchW),
            set_max: sym!("Everything_SetMax", SetMax),
            query: sym!("Everything_QueryW", QueryW),
            num: sym!("Everything_GetNumResults", GetNumResults),
            full_path: sym!("Everything_GetResultFullPathNameW", GetResultFullPathNameW),
        })
        // The library is intentionally never freed: it lives as long as
        // the worker thread, i.e. the process.
    }
}

impl Api {
    fn search(&self, q: &str, max: u32) -> Vec<PathBuf> {
        let w = WideString::new(q);
        // SAFETY: calls into the loaded SDK with valid arguments; buffer
        // length passed matches the buffer.
        unsafe {
            (self.set_search)(w.pcwstr());
            (self.set_max)(max);
            if !(self.query)(BOOL(1)).as_bool() {
                return Vec::new();
            }
            let n = (self.num)().min(max);
            let mut out = Vec::with_capacity(n as usize);
            let mut buf = vec![0u16; 1024];
            for i in 0..n {
                let len = (self.full_path)(i, PWSTR(buf.as_mut_ptr()), buf.len() as u32);
                if len > 0 {
                    out.push(PathBuf::from(String::from_utf16_lossy(
                        &buf[..len as usize],
                    )));
                }
            }
            out
        }
    }
}

/// Handle to the worker thread.
pub struct EverythingWorker {
    tx: Sender<(u64, String)>,
    pub results: Receiver<(u64, Vec<PathBuf>)>,
}

impl EverythingWorker {
    /// Start the worker. Results are signalled by posting `notify_msg` to
    /// `hwnd`.
    pub fn start(hwnd: HWND, notify_msg: u32, max: u32) -> Self {
        let (tx, rx) = channel::<(u64, String)>();
        let (rtx, rrx) = channel();
        let target = hwnd.0 as usize;
        std::thread::spawn(move || {
            let api = load();
            if api.is_none() {
                tracing::warn!("Everything64.dll not found or incomplete; file search disabled");
            }
            while let Ok((mut gen, mut q)) = rx.recv() {
                // Coalesce: only the latest pending query matters.
                while let Ok((g, nq)) = rx.try_recv() {
                    gen = g;
                    q = nq;
                }
                let res = api.as_ref().map(|a| a.search(&q, max)).unwrap_or_default();
                if rtx.send((gen, res)).is_err() {
                    break;
                }
                // SAFETY: posting to the launcher window.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(target as *mut _)),
                        notify_msg,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
        });
        Self { tx, results: rrx }
    }

    pub fn query(&self, generation: u64, q: &str) {
        let _ = self.tx.send((generation, q.to_string()));
    }
}
