// SPDX-License-Identifier: GPL-3.0-or-later
//! "Files" results from the explorer.rs index daemon (feature `file-index`).
//!
//! Same shape as `everything::EverythingWorker` so the launcher treats both
//! back-ends alike: queries go to a worker thread, which asks
//! `explorer-rs-indexd` through `explorer-index-client`, sends
//! `(generation, paths)` on `results` and posts `notify_msg` to the window.
//! Only the newest pending query is run; a new query also cancels the
//! previous one in the daemon. If the daemon is not running the worker
//! answers with an empty list immediately, so the UI never waits on it.
//! Unverified on Windows.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use explorer_index_client::{ClientOptions, IndexClient, Outcome, QueryFlags};
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Handle to the worker thread.
pub struct IndexWorker {
    tx: Sender<(u64, String)>,
    pub results: Receiver<(u64, Vec<PathBuf>)>,
}

impl IndexWorker {
    /// Start the worker. Results are signalled by posting `notify_msg` to
    /// `hwnd`.
    pub fn start(hwnd: HWND, notify_msg: u32, max: u32) -> Self {
        let (tx, rx) = channel::<(u64, String)>();
        let (rtx, rrx) = channel();
        let target = hwnd.0 as usize;
        std::thread::spawn(move || {
            let client = IndexClient::new(ClientOptions {
                client_name: "explorer-rs-launcher".into(),
                request_timeout: Duration::from_millis(1500),
                ..Default::default()
            });
            while let Ok((mut generation, mut q)) = rx.recv() {
                // Coalesce: only the latest pending query matters.
                while let Ok((g, nq)) = rx.try_recv() {
                    generation = g;
                    q = nq;
                }
                let pending = client.query(&q, 0, max, QueryFlags::default(), None);
                let paths = match pending.wait(Duration::from_secs(2)) {
                    Outcome::Ready(r) => {
                        r.hits.into_iter().map(|h| PathBuf::from(h.path)).collect()
                    }
                    Outcome::Unavailable => Vec::new(),
                    other => {
                        tracing::debug!(?other, "file index query did not complete");
                        Vec::new()
                    }
                };
                if rtx.send((generation, paths)).is_err() {
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
