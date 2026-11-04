// SPDX-License-Identifier: GPL-3.0-or-later
//! RPC between the unprivileged daemon and the elevated MFT/USN helper.
//!
//! The helper is deliberately dumb: it answers three questions about fixed
//! NTFS volumes it enumerated itself (journal info, one MFT chunk, one
//! journal chunk) and returns raw record bytes. All parsing happens in the
//! unprivileged daemon. See docs/decisions/0010-index-privilege-model.md.
//!
//! [`HelperBackend`] is the daemon side and implements [`UsnBackend`];
//! [`serve`] is the helper side, generic over the backend so it is tested
//! on Linux with [`crate::usn::SimVolume`].

use std::collections::HashMap;
use std::io::{Read, Write};
use std::time::Duration;

use crate::protocol::{
    read_frame, write_frame, FrameError, HelperRequest, HelperResponse, MAX_HELPER_FRAME,
    MAX_REQUEST_FRAME, PROTOCOL_VERSION,
};
use crate::transport::{self, Connection};
use crate::usn::{JournalInfo, UsnBackend, UsnError};

/// Daemon-side backend that forwards every call to the helper.
pub struct HelperBackend {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    volume: u8,
}

fn io(e: impl std::fmt::Display) -> UsnError {
    UsnError::Io(format!("helper: {e}"))
}

impl HelperBackend {
    /// Connect to the helper and check that it serves `volume`.
    pub fn connect(endpoint: &str, volume: u8, timeout: Duration) -> Result<Self, UsnError> {
        let c = transport::connect(endpoint, timeout).map_err(io)?;
        Self::over(c, volume)
    }

    pub fn over(c: Connection, volume: u8) -> Result<Self, UsnError> {
        let (reader, writer, _) = c.split();
        let mut b = Self {
            reader,
            writer,
            volume: volume.to_ascii_uppercase(),
        };
        match b.call(&HelperRequest::Hello {
            version: PROTOCOL_VERSION,
        })? {
            HelperResponse::Hello { version, volumes } if version == PROTOCOL_VERSION => {
                if !volumes.contains(&b.volume) {
                    return Err(UsnError::AccessDenied);
                }
                Ok(b)
            }
            other => Err(io(format!("unexpected hello reply {other:?}"))),
        }
    }

    fn call(&mut self, req: &HelperRequest) -> Result<HelperResponse, UsnError> {
        write_frame(&mut self.writer, req, MAX_REQUEST_FRAME).map_err(io)?;
        match read_frame(&mut self.reader, MAX_HELPER_FRAME).map_err(io)? {
            HelperResponse::Error(e) => Err(e),
            r => Ok(r),
        }
    }
}

impl UsnBackend for HelperBackend {
    fn query(&mut self) -> Result<JournalInfo, UsnError> {
        match self.call(&HelperRequest::Query {
            volume: self.volume,
        })? {
            HelperResponse::Journal(j) => Ok(j),
            r => Err(io(format!("unexpected reply {r:?}"))),
        }
    }

    fn enum_chunk(
        &mut self,
        start_frn: u64,
        high_usn: i64,
    ) -> Result<Option<(u64, Vec<u8>)>, UsnError> {
        match self.call(&HelperRequest::EnumMft {
            volume: self.volume,
            start_frn,
            high_usn,
        })? {
            HelperResponse::Chunk { next, data } => Ok(Some((next, data))),
            HelperResponse::End => Ok(None),
            r => Err(io(format!("unexpected reply {r:?}"))),
        }
    }

    fn read_journal(
        &mut self,
        journal_id: u64,
        start_usn: i64,
    ) -> Result<(i64, Vec<u8>), UsnError> {
        match self.call(&HelperRequest::ReadJournal {
            volume: self.volume,
            journal_id,
            start_usn,
        })? {
            HelperResponse::Records { next_usn, data } => Ok((next_usn, data)),
            r => Err(io(format!("unexpected reply {r:?}"))),
        }
    }
}

/// Opens the backend for a drive letter (helper side).
pub type OpenFn<'a> = dyn FnMut(u8) -> Result<Box<dyn UsnBackend>, UsnError> + 'a;

/// Serve one daemon connection until it closes. `volumes` are the drive
/// letters this helper is willing to read; anything else is refused.
pub fn serve(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    volumes: &[u8],
    open: &mut OpenFn<'_>,
) -> Result<(), FrameError> {
    let mut r = reader;
    let mut w = writer;
    match read_frame::<HelperRequest>(&mut r, MAX_REQUEST_FRAME)? {
        HelperRequest::Hello { version } if version == PROTOCOL_VERSION => {
            write_frame(
                &mut w,
                &HelperResponse::Hello {
                    version: PROTOCOL_VERSION,
                    volumes: volumes.to_vec(),
                },
                MAX_HELPER_FRAME,
            )?;
        }
        _ => return Err(FrameError::Malformed("expected hello".into())),
    }
    let mut open_backends: HashMap<u8, Box<dyn UsnBackend>> = HashMap::new();
    loop {
        let req = match read_frame::<HelperRequest>(&mut r, MAX_REQUEST_FRAME) {
            Err(FrameError::Closed) => return Ok(()),
            r => r?,
        };
        let vol = match &req {
            HelperRequest::Hello { .. } => {
                return Err(FrameError::Malformed("duplicate hello".into()));
            }
            HelperRequest::Query { volume }
            | HelperRequest::EnumMft { volume, .. }
            | HelperRequest::ReadJournal { volume, .. } => volume.to_ascii_uppercase(),
        };
        let reply = if !volumes.contains(&vol) {
            HelperResponse::Error(UsnError::AccessDenied)
        } else {
            let backend = match open_backends.entry(vol) {
                std::collections::hash_map::Entry::Occupied(e) => Ok(e.into_mut()),
                std::collections::hash_map::Entry::Vacant(e) => open(vol).map(|b| e.insert(b)),
            };
            match backend {
                Err(e) => HelperResponse::Error(e),
                Ok(b) => match req {
                    HelperRequest::Query { .. } => match b.query() {
                        Ok(j) => HelperResponse::Journal(j),
                        Err(e) => HelperResponse::Error(e),
                    },
                    HelperRequest::EnumMft {
                        start_frn,
                        high_usn,
                        ..
                    } => match b.enum_chunk(start_frn, high_usn) {
                        Ok(Some((next, data))) => HelperResponse::Chunk { next, data },
                        Ok(None) => HelperResponse::End,
                        Err(e) => HelperResponse::Error(e),
                    },
                    HelperRequest::ReadJournal {
                        journal_id,
                        start_usn,
                        ..
                    } => match b.read_journal(journal_id, start_usn) {
                        Ok((next_usn, data)) => HelperResponse::Records { next_usn, data },
                        Err(e) => HelperResponse::Error(e),
                    },
                    HelperRequest::Hello { .. } => unreachable!("handled above"),
                },
            }
        };
        write_frame(&mut w, &reply, MAX_HELPER_FRAME)?;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::index::{Index, VolumeKind};
    use crate::rules::Rules;
    use crate::source::{apply, Stop, VecSink};
    use crate::transport::{Access, Listener};
    use crate::usn::{run_ntfs, SimVolume};

    #[test]
    fn helper_rpc_end_to_end() {
        let d = tempfile::tempdir().unwrap();
        let ep = d.path().join("helper.sock").to_string_lossy().to_string();
        let mut sim = SimVolume::new(3);
        sim.chunk = 7;
        for i in 0..50 {
            sim.create(20 + i, 5, &format!("f{i}"), false);
        }
        let stop = Stop::new();
        let stop2 = Stop::new();
        let mut listener = Listener::bind(&ep, Access::CurrentUserFromElevated).unwrap();
        std::thread::scope(|s| {
            let srv = s.spawn(|| {
                // Two connections: one refused volume probe, one real client.
                for _ in 0..2 {
                    let c = listener.accept(&stop).unwrap().unwrap();
                    let (mut r, mut w, _) = c.split();
                    let sim = sim.clone();
                    let mut open = move |_v: u8| -> Result<Box<dyn UsnBackend>, UsnError> {
                        Ok(Box::new(sim.clone()))
                    };
                    let _ = serve(&mut r, &mut w, b"C", &mut open);
                }
            });
            assert_eq!(
                HelperBackend::connect(&ep, b'd', Duration::from_secs(1)).err(),
                Some(UsnError::AccessDenied)
            );
            let mut b = HelperBackend::connect(&ep, b'c', Duration::from_secs(1)).unwrap();
            let sink = VecSink::default();
            let stop2_ref = &stop2;
            s.spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                stop2_ref.stop();
            });
            run_ntfs(
                &mut b,
                0,
                "C:\\",
                None,
                &Rules::default(),
                &sink,
                &stop2,
                Duration::from_millis(5),
            )
            .unwrap();
            drop(b);
            srv.join().unwrap();
            let mut ix = Index::new();
            let v = ix.add_volume("C:\\", VolumeKind::Ntfs).unwrap();
            apply(
                &mut ix,
                &Rules::default(),
                std::mem::take(&mut *sink.changes.lock().unwrap()),
            );
            assert_eq!(ix.volume_len(v), 51);
        });
    }

    #[test]
    fn serve_rejects_garbage() {
        let mut out = Vec::new();
        let mut open =
            |_v: u8| -> Result<Box<dyn UsnBackend>, UsnError> { Err(UsnError::NotActive) };
        let mut input: &[u8] = &[3, 0, 0, 0, 9, 9, 9];
        assert!(serve(&mut input, &mut out, b"C", &mut open).is_err());
        // Valid hello, then a request for a volume that is not served.
        let mut buf = Vec::new();
        write_frame(
            &mut buf,
            &HelperRequest::Hello {
                version: PROTOCOL_VERSION,
            },
            99,
        )
        .unwrap();
        write_frame(&mut buf, &HelperRequest::Query { volume: b'Z' }, 99).unwrap();
        write_frame(&mut buf, &HelperRequest::Query { volume: b'C' }, 99).unwrap();
        let mut input: &[u8] = &buf;
        let mut out = Vec::new();
        serve(&mut input, &mut out, b"C", &mut open).unwrap();
        let mut o: &[u8] = &out;
        assert!(matches!(
            read_frame::<HelperResponse>(&mut o, MAX_HELPER_FRAME).unwrap(),
            HelperResponse::Hello { .. }
        ));
        assert_eq!(
            read_frame::<HelperResponse>(&mut o, MAX_HELPER_FRAME).unwrap(),
            HelperResponse::Error(UsnError::AccessDenied)
        );
        assert_eq!(
            read_frame::<HelperResponse>(&mut o, MAX_HELPER_FRAME).unwrap(),
            HelperResponse::Error(UsnError::NotActive)
        );
    }
}
