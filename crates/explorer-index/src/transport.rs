// SPDX-License-Identifier: GPL-3.0-or-later
//! Byte-stream transport for the index IPC.
//!
//! Windows: named pipes (`explorer_win::pipe`) whose DACL only grants the
//! current user, with remote clients rejected. Clients additionally check
//! that the server process runs as the same user (defence against pipe
//! squatting). Elsewhere: Unix domain sockets (mode 0600) in the state
//! directory, so the daemon and client can be exercised end-to-end on Linux.

use std::io::{self, Read, Write};
use std::time::Duration;

use crate::source::Stop;

/// Overrides the daemon endpoint (pipe name or socket path).
pub const ENDPOINT_ENV: &str = "EXPLORER_RS_INDEX_ENDPOINT";

/// Who may connect to a listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Processes of the current user (same integrity level or higher).
    CurrentUser,
    /// The current user's medium-integrity processes may connect to this
    /// (elevated) server. Used by the elevated helper only.
    CurrentUserFromElevated,
}

/// A bidirectional connection split into independently usable halves.
pub struct Connection {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    closer: Box<dyn Fn() + Send + Sync>,
    /// Peer process id when the platform reports it.
    pub peer_pid: Option<u32>,
}

impl Connection {
    /// Unblock pending reads and close the connection (any thread).
    pub fn close(&self) {
        (self.closer)()
    }

    /// Split into (reader, writer, closer).
    #[allow(clippy::type_complexity)]
    pub fn split(
        self,
    ) -> (
        Box<dyn Read + Send>,
        Box<dyn Write + Send>,
        Box<dyn Fn() + Send + Sync>,
    ) {
        (self.reader, self.writer, self.closer)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use explorer_win::pipe::{PipeServer, PipeStream};
    use explorer_win::security;

    fn sid() -> String {
        security::current_user_sid().unwrap_or_else(|_| "unknown".into())
    }

    pub fn daemon_endpoint() -> String {
        std::env::var(ENDPOINT_ENV)
            .unwrap_or_else(|_| format!(r"\\.\pipe\explorer-rs.index.{}", sid()))
    }

    pub fn helper_endpoint() -> String {
        format!(r"\\.\pipe\explorer-rs.index-helper.{}", sid())
    }

    fn wrap(p: PipeStream, peer_pid: Option<u32>) -> Connection {
        let (r, w, c) = (p.clone(), p.clone(), p);
        Connection {
            reader: Box::new(r),
            writer: Box::new(w),
            closer: Box::new(move || c.cancel_io()),
            peer_pid,
        }
    }

    pub fn connect(endpoint: &str, timeout: Duration) -> io::Result<Connection> {
        let p = PipeStream::connect(endpoint, timeout)?;
        let pid = p.server_pid()?;
        let me = security::current_user_sid().map_err(io::Error::other)?;
        let them = security::process_user_sid(pid).map_err(io::Error::other)?;
        if me != them {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("pipe server (pid {pid}) runs as another user"),
            ));
        }
        Ok(wrap(p, Some(pid)))
    }

    pub struct Listener(PipeServer);

    impl Listener {
        pub fn bind(endpoint: &str, access: Access) -> io::Result<Self> {
            let me = security::current_user_sid().map_err(io::Error::other)?;
            // Protected DACL: only the current user, no inherited ACEs.
            let sddl = match access {
                Access::CurrentUser => format!("D:P(A;;GA;;;{me})"),
                // Medium mandatory label: the user's non-elevated processes
                // may write to this pipe owned by an elevated process.
                Access::CurrentUserFromElevated => format!("D:P(A;;GA;;;{me})S:(ML;;NW;;;ME)"),
            };
            Ok(Self(PipeServer::new(endpoint, &sddl)?))
        }

        pub fn accept(&mut self, stop: &Stop) -> io::Result<Option<Connection>> {
            Ok(self.0.accept(&|| stop.is_stopped())?.map(|p| {
                let pid = p.client_pid().ok();
                wrap(p, pid)
            }))
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    fn state_dir() -> PathBuf {
        explorer_core::paths::AppPaths::from_env().state_dir
    }

    pub fn daemon_endpoint() -> String {
        std::env::var(ENDPOINT_ENV)
            .unwrap_or_else(|_| state_dir().join("index.sock").to_string_lossy().into())
    }

    pub fn helper_endpoint() -> String {
        state_dir()
            .join("index-helper.sock")
            .to_string_lossy()
            .into()
    }

    fn wrap(s: UnixStream) -> io::Result<Connection> {
        let r = s.try_clone()?;
        let c = s.try_clone()?;
        Ok(Connection {
            reader: Box::new(r),
            writer: Box::new(s),
            closer: Box::new(move || {
                let _ = c.shutdown(std::net::Shutdown::Both);
            }),
            peer_pid: None,
        })
    }

    pub fn connect(endpoint: &str, timeout: Duration) -> io::Result<Connection> {
        let s = UnixStream::connect(endpoint)?;
        s.set_write_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        wrap(s)
    }

    pub struct Listener(UnixListener, PathBuf);

    impl Listener {
        pub fn bind(endpoint: &str, _access: Access) -> io::Result<Self> {
            let path = PathBuf::from(endpoint);
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d)?;
            }
            if path.exists() {
                // Refuse to steal a live socket; remove a stale one.
                if UnixStream::connect(&path).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "another index daemon is listening",
                    ));
                }
                std::fs::remove_file(&path)?;
            }
            let l = UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            l.set_nonblocking(true)?;
            Ok(Self(l, path))
        }

        pub fn accept(&mut self, stop: &Stop) -> io::Result<Option<Connection>> {
            loop {
                match self.0.accept() {
                    Ok((s, _)) => {
                        s.set_nonblocking(false)?;
                        return wrap(s).map(Some);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if stop.sleep(Duration::from_millis(50)) {
                            return Ok(None);
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.1);
        }
    }
}

pub use imp::{connect, daemon_endpoint, helper_endpoint, Listener};

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::protocol::{read_frame, write_frame, Request, MAX_REQUEST_FRAME};

    #[test]
    fn unix_roundtrip_and_stale_socket() {
        let d = tempfile::tempdir().unwrap();
        let ep = d.path().join("t.sock").to_string_lossy().to_string();
        let stop = Stop::new();
        let mut l = Listener::bind(&ep, Access::CurrentUser).unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                let mut c = connect(&ep, Duration::from_secs(1)).unwrap();
                write_frame(&mut c.writer, &Request::Status { id: 9 }, MAX_REQUEST_FRAME).unwrap();
            });
            let mut c = l.accept(&stop).unwrap().unwrap();
            let r: Request = read_frame(&mut c.reader, MAX_REQUEST_FRAME).unwrap();
            assert_eq!(r, Request::Status { id: 9 });
        });
        assert!(
            Listener::bind(&ep, Access::CurrentUser).is_err(),
            "live socket kept"
        );
        stop.stop();
        // The probe connection made by the second bind may be pending.
        let _ = l.accept(&stop);
        assert!(l.accept(&stop).unwrap().is_none());
        drop(l);
        assert!(connect(&ep, Duration::from_millis(10)).is_err());
    }
}
