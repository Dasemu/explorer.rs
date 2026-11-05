// SPDX-License-Identifier: GPL-3.0-or-later
//! Background threads of the Start menu, so the UI thread never waits on
//! the file system or the Shell:
//! - **icons**: shell icons for recent files, file results and pins;
//! - **aux**: resolving typed program names (`SearchPathW`, App Paths),
//!   listing recent files, loading the account picture, checking optional
//!   tools.
//!
//! Each thread drains its queue in batches (stale "resolve" requests are
//! dropped), sends replies on one channel and posts `notify_msg` to the
//! window. Both threads initialise COM (STA).

use std::sync::mpsc::{channel, Receiver, Sender};

use explorer_core::catalog::LaunchTarget;
use explorer_core::image::SharedImage;
use explorer_core::recent::RecentFile;
use explorer_core::run_command::RunCommand;
use explorer_core::settings_pages::ENTRIES;
use explorer_win::util::ComApartment;
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Size icons are extracted at (scaled when drawn).
pub const ICON_PX: u32 = 48;
/// Size of the account picture.
const AVATAR_PX: u32 = 64;

pub enum Job {
    Icons(Vec<LaunchTarget>),
    /// Check that a typed program name exists before offering "Run".
    Resolve {
        generation: u64,
        command: RunCommand,
    },
    Recent {
        max: usize,
    },
    Avatar,
    /// Which optional settings-table tools are installed.
    Tools,
}

pub enum Reply {
    Icons(Vec<(String, SharedImage)>),
    Resolved {
        generation: u64,
        command: Option<RunCommand>,
    },
    Recent(Vec<RecentFile>),
    Avatar(Option<SharedImage>),
    Tools(Vec<&'static str>),
}

pub struct Workers {
    aux: Sender<Job>,
    icons: Sender<Job>,
    pub replies: Receiver<Reply>,
}

fn spawn(
    name: &'static str,
    hwnd: HWND,
    notify_msg: u32,
    replies: Sender<Reply>,
    mut handle: impl FnMut(Vec<Job>) -> Vec<Reply> + Send + 'static,
) -> Sender<Job> {
    let (tx, rx) = channel::<Job>();
    let target = hwnd.0 as usize;
    let r = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _com = ComApartment::sta();
            while let Ok(first) = rx.recv() {
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                for reply in handle(batch) {
                    if replies.send(reply).is_err() {
                        return;
                    }
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
    if let Err(e) = r {
        tracing::error!(error = %e, worker = name, "cannot start worker thread");
    }
    tx
}

fn icons(batch: Vec<Job>) -> Vec<Reply> {
    let mut out = Vec::new();
    let started = std::time::Instant::now();
    for job in batch {
        if let Job::Icons(targets) = job {
            for t in targets {
                if let Some(img) = explorer_win::icons::for_target(&t, ICON_PX) {
                    out.push((t.key(), img));
                }
            }
        }
    }
    tracing::debug!(
        icons = out.len(),
        ms = started.elapsed().as_millis() as u64,
        "extracted icons"
    );
    vec![Reply::Icons(out)]
}

fn aux(batch: Vec<Job>) -> Vec<Reply> {
    let mut out = Vec::new();
    // Only the newest resolve request matters.
    let last_resolve = batch.iter().rposition(|j| matches!(j, Job::Resolve { .. }));
    for (i, job) in batch.into_iter().enumerate() {
        match job {
            Job::Resolve {
                generation,
                command,
            } => {
                if Some(i) != last_resolve {
                    continue;
                }
                let found = crate::sys::resolve_program(&command.program);
                tracing::debug!(program = %command.program, ?found, "resolved command");
                out.push(Reply::Resolved {
                    generation,
                    command: found.map(|_| command),
                });
            }
            Job::Recent { max } => {
                let started = std::time::Instant::now();
                let files = if crate::sys::recent_files_enabled() {
                    crate::sys::recent_files(max)
                } else {
                    Vec::new()
                };
                tracing::debug!(
                    files = files.len(),
                    ms = started.elapsed().as_millis() as u64,
                    "listed recent files"
                );
                out.push(Reply::Recent(files));
            }
            Job::Avatar => {
                let img = crate::sys::account_picture_path()
                    .and_then(|p| crate::sys::thumbnail(&p, AVATAR_PX))
                    .and_then(|i| crate::avatar::circle(&i));
                out.push(Reply::Avatar(img));
            }
            Job::Tools => {
                let ids = ENTRIES
                    .iter()
                    .filter(|e| {
                        e.requires
                            .is_none_or(|r| crate::sys::resolve_program(r).is_some())
                    })
                    .map(|e| e.id)
                    .collect();
                out.push(Reply::Tools(ids));
            }
            Job::Icons(_) => {}
        }
    }
    out
}

impl Workers {
    pub fn start(hwnd: HWND, notify_msg: u32) -> Self {
        let (rtx, rrx) = channel();
        Self {
            aux: spawn("start-aux", hwnd, notify_msg, rtx.clone(), aux),
            icons: spawn("start-icons", hwnd, notify_msg, rtx, icons),
            replies: rrx,
        }
    }

    pub fn send(&self, job: Job) {
        let tx = match job {
            Job::Icons(_) => &self.icons,
            _ => &self.aux,
        };
        let _ = tx.send(job);
    }
}
