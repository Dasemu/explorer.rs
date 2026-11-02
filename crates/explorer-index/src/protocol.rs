// SPDX-License-Identifier: GPL-3.0-or-later
//! IPC protocol between `explorer-rs-indexd`, its clients and the elevated
//! helper. Documented in docs/IPC-PROTOCOL.md.
//!
//! Frame: `u32` little-endian payload length, then the payload, which is a
//! `postcard`-encoded message. Each direction has its own maximum frame
//! size; oversized or malformed frames are errors that close the connection
//! (the stream position is unknown afterwards). Enum variants are encoded by
//! index: **never reorder or remove variants, only append**.

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::usn::{JournalInfo, UsnError};

/// Bumped on incompatible changes. Clients send it in `Hello`.
pub const PROTOCOL_VERSION: u16 = 1;
/// Largest client -> daemon frame.
pub const MAX_REQUEST_FRAME: usize = 16 * 1024;
/// Largest daemon -> client frame.
pub const MAX_RESPONSE_FRAME: usize = 4 * 1024 * 1024;
/// Largest helper -> daemon frame (one MFT/journal chunk plus overhead).
pub const MAX_HELPER_FRAME: usize = 2 * 1024 * 1024;
/// Hard cap on hits per page regardless of configuration.
pub const MAX_PAGE: u32 = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("connection closed")]
    Closed,
    #[error("frame of {0} bytes exceeds limit")]
    TooLarge(usize),
    #[error("malformed message: {0}")]
    Malformed(String),
}

/// Write one frame.
pub fn write_frame<T: Serialize>(
    w: &mut impl Write,
    msg: &T,
    max: usize,
) -> Result<(), FrameError> {
    let body = postcard::to_allocvec(msg).map_err(|e| FrameError::Malformed(e.to_string()))?;
    if body.len() > max {
        return Err(FrameError::TooLarge(body.len()));
    }
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(&body);
    w.write_all(&buf)?;
    w.flush()?;
    Ok(())
}

/// Read one frame. A clean EOF before the length prefix is `Closed`.
pub fn read_frame<T: DeserializeOwned>(r: &mut impl Read, max: usize) -> Result<T, FrameError> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Err(FrameError::Closed),
            Ok(0) => return Err(FrameError::Io(io::ErrorKind::UnexpectedEof.into())),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > max {
        return Err(FrameError::TooLarge(n));
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    decode(&body)
}

/// Decode a payload (exposed for fuzzing).
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    let (msg, rest) =
        postcard::take_from_bytes(body).map_err(|e| FrameError::Malformed(e.to_string()))?;
    if !rest.is_empty() {
        return Err(FrameError::Malformed("trailing bytes".into()));
    }
    Ok(msg)
}

// ----- client <-> daemon ----------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryFlags {
    /// Plain terms match the full path.
    pub match_path: bool,
    /// Plain terms are fuzzy.
    pub fuzzy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Must be the first message on a connection.
    Hello {
        version: u16,
        client: String,
    },
    /// Search. A new query on the same connection cancels the previous one.
    Query {
        id: u32,
        text: String,
        offset: u32,
        limit: u32,
        flags: QueryFlags,
    },
    /// Cancel a running query (best effort; it replies `Error{Cancelled}`).
    Cancel {
        id: u32,
    },
    Status {
        id: u32,
    },
    /// Rescan one root (or all roots when `root` is `None`).
    Reindex {
        id: u32,
        root: Option<String>,
    },
    AddRoot {
        id: u32,
        path: String,
    },
    RemoveRoot {
        id: u32,
        path: String,
    },
    /// Save a snapshot and exit (exit code 0: the supervisor will not
    /// restart it).
    Shutdown {
        id: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub score: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootStatus {
    pub path: String,
    /// "ntfs" or "walk".
    pub kind: String,
    /// "starting", "scanning", "live", "polling", "failed: ...", "stopped".
    pub state: String,
    pub entries: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub entries: u64,
    pub memory_bytes: u64,
    pub roots: Vec<RootStatus>,
    pub uptime_s: u64,
    /// Seconds since the last snapshot was written (None: never).
    pub snapshot_age_s: Option<u64>,
    /// True while an initial scan is still running anywhere.
    pub scanning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    BadRequest,
    VersionMismatch,
    TooLarge,
    Busy,
    Cancelled,
    NotFound,
    Unavailable,
    Internal,
    BadQuery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Hello {
        version: u16,
        server: String,
    },
    Results {
        id: u32,
        /// Matches before access filtering.
        total: u64,
        offset: u32,
        hits: Vec<Hit>,
        /// More (filtered) results exist after this page.
        more: bool,
        elapsed_us: u64,
    },
    Status {
        id: u32,
        status: Status,
    },
    Done {
        id: u32,
    },
    Error {
        id: u32,
        code: ErrorCode,
        message: String,
    },
}

// ----- daemon <-> elevated helper ---------------------------------------------

/// Requests the unprivileged daemon may send to the elevated helper. The
/// helper only answers questions about fixed NTFS volumes it enumerated
/// itself; `volume` is a drive letter (`b'C'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HelperRequest {
    Hello {
        version: u16,
    },
    Query {
        volume: u8,
    },
    EnumMft {
        volume: u8,
        start_frn: u64,
        high_usn: i64,
    },
    ReadJournal {
        volume: u8,
        journal_id: u64,
        start_usn: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HelperResponse {
    Hello {
        version: u16,
        volumes: Vec<u8>,
    },
    Journal(JournalInfo),
    /// MFT chunk: next start reference and raw record bytes.
    Chunk {
        next: u64,
        data: Vec<u8>,
    },
    /// End of MFT enumeration.
    End,
    /// Journal records: next USN and raw record bytes.
    Records {
        next_usn: i64,
        data: Vec<u8>,
    },
    Error(UsnError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_requests() {
        let reqs = vec![
            Request::Hello {
                version: PROTOCOL_VERSION,
                client: "test".into(),
            },
            Request::Query {
                id: 7,
                text: "ext:rs main".into(),
                offset: 10,
                limit: 20,
                flags: QueryFlags {
                    match_path: true,
                    fuzzy: false,
                },
            },
            Request::Cancel { id: 7 },
            Request::Status { id: 1 },
            Request::Reindex { id: 2, root: None },
            Request::AddRoot {
                id: 3,
                path: "D:\\".into(),
            },
            Request::RemoveRoot {
                id: 4,
                path: "D:\\".into(),
            },
            Request::Shutdown { id: 5 },
        ];
        let mut buf = Vec::new();
        for r in &reqs {
            write_frame(&mut buf, r, MAX_REQUEST_FRAME).unwrap();
        }
        let mut cur = io::Cursor::new(buf);
        for r in &reqs {
            let got: Request = read_frame(&mut cur, MAX_REQUEST_FRAME).unwrap();
            assert_eq!(&got, r);
        }
        assert!(matches!(
            read_frame::<Request>(&mut cur, MAX_REQUEST_FRAME),
            Err(FrameError::Closed)
        ));
    }

    #[test]
    fn limits_and_garbage() {
        let big = Request::Hello {
            version: 1,
            client: "x".repeat(MAX_REQUEST_FRAME),
        };
        assert!(matches!(
            write_frame(&mut Vec::new(), &big, MAX_REQUEST_FRAME),
            Err(FrameError::TooLarge(_))
        ));
        // Length prefix over the limit is rejected before allocating.
        let mut cur = io::Cursor::new(u32::MAX.to_le_bytes().to_vec());
        assert!(matches!(
            read_frame::<Request>(&mut cur, MAX_REQUEST_FRAME),
            Err(FrameError::TooLarge(_))
        ));
        // Truncated body.
        let mut cur = io::Cursor::new(vec![10, 0, 0, 0, 1, 2]);
        assert!(read_frame::<Request>(&mut cur, MAX_REQUEST_FRAME).is_err());
        // Garbage bodies never panic.
        let mut seed = 7u32;
        for len in 0..2000usize {
            let body: Vec<u8> = (0..len % 64)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (seed >> 16) as u8
                })
                .collect();
            let _ = decode::<Request>(&body);
            let _ = decode::<Response>(&body);
            let _ = decode::<HelperRequest>(&body);
            let _ = decode::<HelperResponse>(&body);
        }
        // Trailing bytes are rejected.
        let mut b = postcard::to_allocvec(&Request::Status { id: 1 }).unwrap();
        b.push(0);
        assert!(decode::<Request>(&b).is_err());
        // A huge declared string length inside a small frame fails cleanly.
        assert!(decode::<Request>(&[0, 1, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F]).is_err());
    }

    #[test]
    fn helper_roundtrip() {
        let msgs = vec![
            HelperResponse::Hello {
                version: 1,
                volumes: vec![b'C', b'D'],
            },
            HelperResponse::Journal(JournalInfo {
                journal_id: 1,
                first_usn: 2,
                next_usn: 3,
            }),
            HelperResponse::Chunk {
                next: 99,
                data: vec![1, 2, 3],
            },
            HelperResponse::End,
            HelperResponse::Error(UsnError::EntryDeleted),
        ];
        let mut buf = Vec::new();
        for m in &msgs {
            write_frame(&mut buf, m, MAX_HELPER_FRAME).unwrap();
        }
        let mut cur = io::Cursor::new(buf);
        for m in &msgs {
            assert_eq!(
                &read_frame::<HelperResponse>(&mut cur, MAX_HELPER_FRAME).unwrap(),
                m
            );
        }
    }
}
