// SPDX-License-Identifier: GPL-3.0-or-later
//! Client for `explorer-rs-indexd`, designed for UI threads.
//!
//! - Every call returns immediately with a [`Pending`] handle; the request
//!   is sent by a background thread that owns the connection.
//! - Results can be polled ([`Pending::try_get`]) from a UI loop, waited for
//!   ([`Pending::wait`]), or announced through a `notify` callback (e.g. one
//!   that posts a window message).
//! - When the daemon is not running, calls resolve to
//!   [`Outcome::Unavailable`] at once; reconnection is attempted at most
//!   every `retry_after`, so a missing daemon costs nothing per keystroke.
//! - Every request has a deadline (`request_timeout`) and can be cancelled.
//!
//! No async runtime is used (see docs/decisions/0011-index-client-threading.md).

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use explorer_index::protocol::{
    read_frame, write_frame, ErrorCode, Request, Response, Status, MAX_REQUEST_FRAME,
    MAX_RESPONSE_FRAME, PROTOCOL_VERSION,
};
pub use explorer_index::protocol::{Hit, QueryFlags};
use explorer_index::transport;

/// Result of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    Ready(T),
    /// The daemon is not running or the connection dropped.
    Unavailable,
    TimedOut,
    Cancelled,
    /// The daemon answered with an error.
    Failed(ErrorCode, String),
}

impl<T> Outcome<T> {
    pub fn ok(self) -> Option<T> {
        match self {
            Self::Ready(t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Results {
    pub total: u64,
    pub offset: u32,
    pub hits: Vec<Hit>,
    pub more: bool,
    pub elapsed_us: u64,
}

#[derive(Clone)]
pub struct ClientOptions {
    pub endpoint: String,
    pub client_name: String,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    /// Minimum delay between reconnection attempts.
    pub retry_after: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            endpoint: transport::daemon_endpoint(),
            client_name: "explorer-index-client".into(),
            connect_timeout: Duration::from_millis(300),
            request_timeout: Duration::from_secs(2),
            retry_after: Duration::from_secs(3),
        }
    }
}

/// Callback invoked (on the client thread) when a result is ready.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

enum Reply {
    Query(Sender<Outcome<Results>>),
    Status(Sender<Outcome<Status>>),
    Done(Sender<Outcome<()>>),
}

impl Reply {
    fn fail(self, why: FailKind) {
        match self {
            Reply::Query(s) => drop(s.send(why.outcome())),
            Reply::Status(s) => drop(s.send(why.outcome())),
            Reply::Done(s) => drop(s.send(why.outcome())),
        }
    }
}

#[derive(Clone, Copy)]
enum FailKind {
    Unavailable,
    TimedOut,
    Cancelled,
}

impl FailKind {
    fn outcome<T>(self) -> Outcome<T> {
        match self {
            Self::Unavailable => Outcome::Unavailable,
            Self::TimedOut => Outcome::TimedOut,
            Self::Cancelled => Outcome::Cancelled,
        }
    }
}

enum Cmd {
    Send {
        id: u32,
        req: Request,
        reply: Reply,
        notify: Option<Notify>,
    },
    Cancel(u32),
    Resp(u64, Response),
    Disconnected(u64),
    Shutdown,
}

/// A request in flight.
pub struct Pending<T> {
    id: u32,
    rx: Receiver<Outcome<T>>,
    tx: Sender<Cmd>,
}

impl<T> Pending<T> {
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Non-blocking poll.
    pub fn try_get(&self) -> Option<Outcome<T>> {
        match self.rx.try_recv() {
            Ok(o) => Some(o),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Outcome::Unavailable),
        }
    }

    /// Block up to `timeout` (the request also has its own deadline).
    pub fn wait(&self, timeout: Duration) -> Outcome<T> {
        match self.rx.recv_timeout(timeout) {
            Ok(o) => o,
            Err(RecvTimeoutError::Timeout) => Outcome::TimedOut,
            Err(RecvTimeoutError::Disconnected) => Outcome::Unavailable,
        }
    }

    /// Cancel; the outcome becomes `Cancelled` unless it already arrived.
    pub fn cancel(&self) {
        let _ = self.tx.send(Cmd::Cancel(self.id));
    }
}

/// Handle to the background client thread. Cheap to share by reference;
/// dropping it stops the thread without blocking.
pub struct IndexClient {
    tx: Sender<Cmd>,
    next_id: AtomicU32,
    available: Arc<AtomicBool>,
}

impl IndexClient {
    pub fn new(opts: ClientOptions) -> Self {
        let (tx, rx) = mpsc::channel();
        let available = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            opts,
            tx: tx.clone(),
            conn: None,
            generation: 0,
            last_attempt: None,
            pending: HashMap::new(),
            available: available.clone(),
        };
        std::thread::Builder::new()
            .name("index-client".into())
            .spawn(move || worker.run(rx))
            .expect("spawn index client thread");
        Self {
            tx,
            next_id: AtomicU32::new(1),
            available,
        }
    }

    /// True after the last connection attempt succeeded and the connection
    /// is still open.
    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    fn id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn send<T>(
        &self,
        id: u32,
        req: Request,
        reply: Reply,
        rx: Receiver<Outcome<T>>,
        notify: Option<Notify>,
    ) -> Pending<T> {
        if self
            .tx
            .send(Cmd::Send {
                id,
                req,
                reply,
                notify,
            })
            .is_err()
        {
            tracing::warn!("index client thread is gone");
        }
        Pending {
            id,
            rx,
            tx: self.tx.clone(),
        }
    }

    /// Search. Starting a new query cancels the previous one on the daemon
    /// side (type-ahead).
    pub fn query(
        &self,
        text: &str,
        offset: u32,
        limit: u32,
        flags: QueryFlags,
        notify: Option<Notify>,
    ) -> Pending<Results> {
        let id = self.id();
        let (s, r) = mpsc::channel();
        let req = Request::Query {
            id,
            text: text.to_string(),
            offset,
            limit,
            flags,
        };
        self.send(id, req, Reply::Query(s), r, notify)
    }

    pub fn status(&self) -> Pending<Status> {
        let id = self.id();
        let (s, r) = mpsc::channel();
        self.send(id, Request::Status { id }, Reply::Status(s), r, None)
    }

    pub fn reindex(&self, root: Option<String>) -> Pending<()> {
        let id = self.id();
        let (s, r) = mpsc::channel();
        self.send(id, Request::Reindex { id, root }, Reply::Done(s), r, None)
    }

    pub fn add_root(&self, path: &str) -> Pending<()> {
        let id = self.id();
        let (s, r) = mpsc::channel();
        let req = Request::AddRoot {
            id,
            path: path.to_string(),
        };
        self.send(id, req, Reply::Done(s), r, None)
    }

    pub fn remove_root(&self, path: &str) -> Pending<()> {
        let id = self.id();
        let (s, r) = mpsc::channel();
        let req = Request::RemoveRoot {
            id,
            path: path.to_string(),
        };
        self.send(id, req, Reply::Done(s), r, None)
    }

    pub fn shutdown_daemon(&self) -> Pending<()> {
        let id = self.id();
        let (s, r) = mpsc::channel();
        self.send(id, Request::Shutdown { id }, Reply::Done(s), r, None)
    }
}

impl Drop for IndexClient {
    fn drop(&mut self) {
        // Never blocks: the worker closes the connection and exits.
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

struct Conn {
    writer: Box<dyn Write + Send>,
    closer: Box<dyn Fn() + Send + Sync>,
}

struct Waiting {
    reply: Reply,
    deadline: Instant,
    notify: Option<Notify>,
}

struct Worker {
    opts: ClientOptions,
    tx: Sender<Cmd>,
    conn: Option<Conn>,
    generation: u64,
    last_attempt: Option<Instant>,
    pending: HashMap<u32, Waiting>,
    available: Arc<AtomicBool>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Cmd>) {
        loop {
            let timeout = self
                .pending
                .values()
                .map(|w| w.deadline)
                .min()
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(3600));
            match rx.recv_timeout(timeout) {
                Ok(Cmd::Send {
                    id,
                    req,
                    reply,
                    notify,
                }) => self.send(id, req, reply, notify),
                Ok(Cmd::Cancel(id)) => {
                    if let Some(w) = self.pending.remove(&id) {
                        w.reply.fail(FailKind::Cancelled);
                        if let Some(n) = w.notify {
                            n();
                        }
                        self.write(&Request::Cancel { id });
                    }
                }
                Ok(Cmd::Resp(generation, r)) if generation == self.generation => {
                    self.on_response(r)
                }
                Ok(Cmd::Resp(..)) => {}
                Ok(Cmd::Disconnected(generation)) => {
                    if generation == self.generation {
                        self.drop_conn();
                    }
                }
                Ok(Cmd::Shutdown) => break,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let now = Instant::now();
            let expired: Vec<u32> = self
                .pending
                .iter()
                .filter(|(_, w)| w.deadline <= now)
                .map(|(&id, _)| id)
                .collect();
            for id in expired {
                if let Some(w) = self.pending.remove(&id) {
                    w.reply.fail(FailKind::TimedOut);
                    if let Some(n) = w.notify {
                        n();
                    }
                    self.write(&Request::Cancel { id });
                }
            }
        }
        self.drop_conn();
    }

    fn write(&mut self, req: &Request) -> bool {
        let Some(c) = self.conn.as_mut() else {
            return false;
        };
        match write_frame(&mut c.writer, req, MAX_REQUEST_FRAME) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!(error = %e, "index daemon write failed");
                self.drop_conn();
                false
            }
        }
    }

    fn drop_conn(&mut self) {
        if let Some(c) = self.conn.take() {
            (c.closer)();
        }
        self.generation += 1;
        self.available.store(false, Ordering::Relaxed);
        for (_, w) in self.pending.drain() {
            w.reply.fail(FailKind::Unavailable);
            if let Some(n) = w.notify {
                n();
            }
        }
    }

    fn ensure_conn(&mut self) -> bool {
        if self.conn.is_some() {
            return true;
        }
        if self
            .last_attempt
            .is_some_and(|t| t.elapsed() < self.opts.retry_after)
        {
            return false;
        }
        self.last_attempt = Some(Instant::now());
        let c = match transport::connect(&self.opts.endpoint, self.opts.connect_timeout) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, endpoint = %self.opts.endpoint, "index daemon unavailable");
                return false;
            }
        };
        let (mut reader, mut writer, closer) = c.split();
        let hello = Request::Hello {
            version: PROTOCOL_VERSION,
            client: self.opts.client_name.clone(),
        };
        if write_frame(&mut writer, &hello, MAX_REQUEST_FRAME).is_err() {
            closer();
            return false;
        }
        self.generation += 1;
        let generation = self.generation;
        let tx = self.tx.clone();
        let spawned = std::thread::Builder::new()
            .name("index-client-reader".into())
            .spawn(move || {
                loop {
                    match read_frame::<Response>(&mut reader, MAX_RESPONSE_FRAME) {
                        Ok(Response::Hello { version, .. }) if version == PROTOCOL_VERSION => {}
                        Ok(Response::Hello { version, .. }) => {
                            tracing::warn!(version, "index daemon speaks another protocol version");
                            break;
                        }
                        Ok(r) => {
                            if tx.send(Cmd::Resp(generation, r)).is_err() {
                                return;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = tx.send(Cmd::Disconnected(generation));
            });
        if spawned.is_err() {
            closer();
            return false;
        }
        self.conn = Some(Conn { writer, closer });
        self.available.store(true, Ordering::Relaxed);
        true
    }

    fn send(&mut self, id: u32, req: Request, reply: Reply, notify: Option<Notify>) {
        if !self.ensure_conn() || !self.write(&req) {
            reply.fail(FailKind::Unavailable);
            if let Some(n) = notify {
                n();
            }
            return;
        }
        self.pending.insert(
            id,
            Waiting {
                reply,
                deadline: Instant::now() + self.opts.request_timeout,
                notify,
            },
        );
    }

    fn on_response(&mut self, r: Response) {
        let id = match &r {
            Response::Results { id, .. }
            | Response::Status { id, .. }
            | Response::Done { id }
            | Response::Error { id, .. } => *id,
            Response::Hello { .. } => return,
        };
        let Some(w) = self.pending.remove(&id) else {
            return;
        };
        match (w.reply, r) {
            (
                Reply::Query(s),
                Response::Results {
                    total,
                    offset,
                    hits,
                    more,
                    elapsed_us,
                    ..
                },
            ) => drop(s.send(Outcome::Ready(Results {
                total,
                offset,
                hits,
                more,
                elapsed_us,
            }))),
            (Reply::Status(s), Response::Status { status, .. }) => {
                drop(s.send(Outcome::Ready(status)))
            }
            (Reply::Done(s), Response::Done { .. }) => drop(s.send(Outcome::Ready(()))),
            (reply, Response::Error { code, message, .. }) => match reply {
                Reply::Query(s) => drop(s.send(Outcome::Failed(code, message))),
                Reply::Status(s) => drop(s.send(Outcome::Failed(code, message))),
                Reply::Done(s) => drop(s.send(Outcome::Failed(code, message))),
            },
            (reply, other) => {
                tracing::warn!(?other, "unexpected reply type from index daemon");
                reply.fail(FailKind::Unavailable);
            }
        }
        if let Some(n) = w.notify {
            n();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(ep: &str) -> ClientOptions {
        ClientOptions {
            endpoint: ep.into(),
            client_name: "test".into(),
            connect_timeout: Duration::from_millis(100),
            request_timeout: Duration::from_millis(300),
            retry_after: Duration::from_millis(50),
        }
    }

    #[test]
    fn daemon_down_is_immediately_unavailable() {
        let d = tempfile::tempdir().unwrap();
        let c = IndexClient::new(opts(&d.path().join("none.sock").to_string_lossy()));
        let t0 = Instant::now();
        let p = c.query("x", 0, 10, QueryFlags::default(), None);
        assert_eq!(p.wait(Duration::from_secs(2)), Outcome::Unavailable);
        // Retries are throttled: the next call does not even try to connect.
        let p = c.status();
        assert_eq!(p.wait(Duration::from_secs(2)), Outcome::Unavailable);
        assert!(t0.elapsed() < Duration::from_secs(1));
        assert!(!c.is_available());
    }

    #[cfg(unix)]
    #[test]
    fn fake_daemon_results_timeout_cancel_and_disconnect() {
        use explorer_index::source::Stop;
        use explorer_index::transport::{Access, Listener};

        let d = tempfile::tempdir().unwrap();
        let ep = d.path().join("d.sock").to_string_lossy().to_string();
        let mut l = Listener::bind(&ep, Access::CurrentUser).unwrap();
        let stop = Stop::new();
        let server = std::thread::spawn(move || {
            let c = l.accept(&stop).unwrap().unwrap();
            let (mut r, mut w, closer) = c.split();
            loop {
                match read_frame::<Request>(&mut r, MAX_REQUEST_FRAME) {
                    Ok(Request::Hello { .. }) => write_frame(
                        &mut w,
                        &Response::Hello {
                            version: PROTOCOL_VERSION,
                            server: "fake".into(),
                        },
                        MAX_RESPONSE_FRAME,
                    )
                    .unwrap(),
                    Ok(Request::Query { id, text, .. }) if text == "slow" => {
                        let _ = id; // never answered: client times out
                    }
                    Ok(Request::Query { id, text, .. }) if text == "bye" => {
                        let _ = id;
                        closer();
                        return;
                    }
                    Ok(Request::Query { id, offset, .. }) => write_frame(
                        &mut w,
                        &Response::Results {
                            id,
                            total: 1,
                            offset,
                            hits: vec![Hit {
                                path: "/x".into(),
                                is_dir: false,
                                size: None,
                                score: 1,
                            }],
                            more: false,
                            elapsed_us: 5,
                        },
                        MAX_RESPONSE_FRAME,
                    )
                    .unwrap(),
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
        let c = IndexClient::new(opts(&ep));
        let notified = Arc::new(AtomicU32::new(0));
        let n2 = notified.clone();
        let p = c.query(
            "x",
            3,
            10,
            QueryFlags::default(),
            Some(Arc::new(move || {
                n2.fetch_add(1, Ordering::Relaxed);
            })),
        );
        let r = p.wait(Duration::from_secs(2)).ok().unwrap();
        assert_eq!(r.hits[0].path, "/x");
        assert_eq!(r.offset, 3);
        assert_eq!(notified.load(Ordering::Relaxed), 1);
        assert!(c.is_available());

        let slow = c.query("slow", 0, 1, QueryFlags::default(), None);
        assert_eq!(slow.wait(Duration::from_secs(2)), Outcome::TimedOut);
        let slow2 = c.query("slow", 0, 1, QueryFlags::default(), None);
        slow2.cancel();
        assert_eq!(slow2.wait(Duration::from_secs(2)), Outcome::Cancelled);

        let bye = c.query("bye", 0, 1, QueryFlags::default(), None);
        assert_eq!(bye.wait(Duration::from_secs(2)), Outcome::Unavailable);
        server.join().unwrap();
        std::thread::sleep(Duration::from_millis(60));
        let after = c.query("x", 0, 1, QueryFlags::default(), None);
        assert_eq!(after.wait(Duration::from_secs(2)), Outcome::Unavailable);
    }
}
