// SPDX-License-Identifier: GPL-3.0-or-later
//! IPC server: one thread per connection, one query thread per connection
//! at a time (a new query cancels the previous one). See
//! docs/IPC-PROTOCOL.md.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use explorer_index::access::{AccessCheck, AllowAll, FsAccess};
use explorer_index::protocol::{
    read_frame, write_frame, ErrorCode, FrameError, Hit, QueryFlags, Request, Response, MAX_PAGE,
    MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME, PROTOCOL_VERSION,
};
use explorer_index::query::{search, QueryError};
use explorer_index::transport::{Connection, Listener};
use explorer_index::{Query, QueryOptions};

use crate::daemon::Daemon;

/// Most simultaneous client connections.
pub const MAX_CONNECTIONS: usize = 32;

fn err(id: u32, code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Error {
        id,
        code,
        message: message.into(),
    }
}

/// Run a query: rank under the read lock, check access without it, page.
pub fn execute_query(
    d: &Daemon,
    id: u32,
    text: &str,
    offset: u32,
    limit: u32,
    flags: QueryFlags,
    cancel: &AtomicBool,
) -> Response {
    let t0 = Instant::now();
    let limit = limit.min(d.cfg.max_query_results as u32).min(MAX_PAGE);
    let opts = QueryOptions {
        match_path: flags.match_path,
        fuzzy: flags.fuzzy,
    };
    let q = match Query::parse(text, opts) {
        Ok(q) => q,
        Err(e) => return err(id, ErrorCode::BadQuery, e.to_string()),
    };
    let need = offset as usize + limit as usize + 1;
    let mut want = need * 2 + 16;
    let mut check: Box<dyn AccessCheck> = if d.cfg.check_access {
        Box::new(FsAccess::default())
    } else {
        Box::new(AllowAll)
    };
    let (total, accepted) = loop {
        let (total, cands) = {
            let ix = d.read();
            match search(&ix, &q, want, cancel) {
                Ok(r) => (
                    r.total,
                    r.hits
                        .iter()
                        .map(|h| Hit {
                            path: ix.path(h.id),
                            is_dir: ix.is_dir(h.id),
                            size: ix.size(h.id),
                            score: h.score,
                        })
                        .collect::<Vec<_>>(),
                ),
                Err(QueryError::Cancelled) => return err(id, ErrorCode::Cancelled, "cancelled"),
                Err(e) => return err(id, ErrorCode::BadQuery, e.to_string()),
            }
        };
        let seen = cands.len();
        let mut accepted = Vec::with_capacity(need.min(seen));
        for h in cands {
            if cancel.load(Ordering::Relaxed) {
                return err(id, ErrorCode::Cancelled, "cancelled");
            }
            if Path::new(&h.path)
                .parent()
                .is_none_or(|p| check.can_list(p))
            {
                accepted.push(h);
                if accepted.len() >= need {
                    break;
                }
            }
        }
        if accepted.len() >= need || seen >= total {
            break (total, accepted);
        }
        want = want.saturating_mul(4);
    };
    let more = accepted.len() > offset as usize + limit as usize;
    let hits: Vec<Hit> = accepted
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    Response::Results {
        id,
        total: total as u64,
        offset,
        hits,
        more,
        elapsed_us: t0.elapsed().as_micros() as u64,
    }
}

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

fn send(w: &SharedWriter, r: &Response) -> bool {
    let mut g = w.lock().unwrap_or_else(|e| e.into_inner());
    match write_frame(&mut *g, r, MAX_RESPONSE_FRAME) {
        Ok(()) => true,
        Err(FrameError::TooLarge(n)) => {
            tracing::warn!(bytes = n, "response too large; sending an error instead");
            let id = match r {
                Response::Results { id, .. } | Response::Status { id, .. } => *id,
                _ => 0,
            };
            write_frame(
                &mut *g,
                &err(id, ErrorCode::TooLarge, "result page too large"),
                MAX_RESPONSE_FRAME,
            )
            .is_ok()
        }
        Err(_) => false,
    }
}

/// Serve one client connection until it closes.
pub fn handle_connection(d: Arc<Daemon>, conn: Connection) {
    let (mut r, w, closer) = conn.split();
    let w: SharedWriter = Arc::new(Mutex::new(w));
    match read_frame::<Request>(&mut r, MAX_REQUEST_FRAME) {
        Ok(Request::Hello { version, client }) if version == PROTOCOL_VERSION => {
            tracing::debug!(client = %client, "client connected");
            send(
                &w,
                &Response::Hello {
                    version: PROTOCOL_VERSION,
                    server: format!("explorer-rs-indexd {}", env!("CARGO_PKG_VERSION")),
                },
            );
        }
        Ok(Request::Hello { version, .. }) => {
            send(
                &w,
                &err(
                    0,
                    ErrorCode::VersionMismatch,
                    format!("server speaks version {PROTOCOL_VERSION}, client {version}"),
                ),
            );
            closer();
            return;
        }
        _ => {
            send(&w, &err(0, ErrorCode::BadRequest, "expected Hello"));
            closer();
            return;
        }
    }
    let mut current: Option<(u32, Arc<AtomicBool>)> = None;
    loop {
        let req = match read_frame::<Request>(&mut r, MAX_REQUEST_FRAME) {
            Ok(req) => req,
            Err(FrameError::Closed) => break,
            Err(FrameError::TooLarge(n)) => {
                send(
                    &w,
                    &err(0, ErrorCode::TooLarge, format!("request of {n} bytes")),
                );
                break;
            }
            Err(FrameError::Malformed(m)) => {
                send(&w, &err(0, ErrorCode::BadRequest, m));
                break;
            }
            Err(_) => break,
        };
        match req {
            Request::Query {
                id,
                text,
                offset,
                limit,
                flags,
            } => {
                if let Some((_, c)) = current.take() {
                    c.store(true, Ordering::Relaxed);
                }
                let cancel = Arc::new(AtomicBool::new(false));
                current = Some((id, cancel.clone()));
                let (d2, w2) = (d.clone(), w.clone());
                let spawned = std::thread::Builder::new()
                    .name("index-query".into())
                    .spawn(move || {
                        let resp = execute_query(&d2, id, &text, offset, limit, flags, &cancel);
                        send(&w2, &resp);
                    });
                if spawned.is_err() {
                    send(&w, &err(id, ErrorCode::Busy, "cannot start query thread"));
                }
            }
            Request::Cancel { id } => {
                if let Some((cid, c)) = &current {
                    if *cid == id {
                        c.store(true, Ordering::Relaxed);
                    }
                }
            }
            Request::Status { id } => {
                send(
                    &w,
                    &Response::Status {
                        id,
                        status: d.status(),
                    },
                );
            }
            Request::Reindex { id, root } => {
                let r = d.reindex(root.as_deref());
                send(&w, &done_or(id, r));
            }
            Request::AddRoot { id, path } => {
                let r = d.add_root(&path);
                send(&w, &done_or(id, r));
            }
            Request::RemoveRoot { id, path } => {
                let r = d.remove_root(&path);
                send(&w, &done_or(id, r));
            }
            Request::Shutdown { id } => {
                tracing::info!("shutdown requested over IPC");
                send(&w, &Response::Done { id });
                d.stop.stop();
                break;
            }
            Request::Hello { .. } => {
                send(&w, &err(0, ErrorCode::BadRequest, "duplicate Hello"));
                break;
            }
        }
    }
    if let Some((_, c)) = current {
        c.store(true, Ordering::Relaxed);
    }
    closer();
}

fn done_or(id: u32, r: Result<(), String>) -> Response {
    match r {
        Ok(()) => Response::Done { id },
        Err(e) => err(id, ErrorCode::NotFound, e),
    }
}

/// Accept connections until the daemon stops.
pub fn serve(d: Arc<Daemon>, mut listener: Listener) {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let conn = match listener.accept(&d.stop) {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                if d.stop.sleep(std::time::Duration::from_millis(500)) {
                    break;
                }
                continue;
            }
        };
        if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            tracing::warn!("too many index clients; refusing connection");
            conn.close();
            continue;
        }
        active.fetch_add(1, Ordering::Relaxed);
        let (d2, a2) = (d.clone(), active.clone());
        let spawned = std::thread::Builder::new()
            .name("index-conn".into())
            .spawn(move || {
                handle_connection(d2, conn);
                a2.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::Relaxed);
        }
    }
}
