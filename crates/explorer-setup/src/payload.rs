// SPDX-License-Identifier: GPL-3.0-or-later
//! The installer payload: a small versioned archive embedded in
//! `explorer-rs-setup.exe`.
//!
//! Layout (all integers little-endian):
//!
//! ```text
//! magic        8 bytes  "XRSPAYLD"
//! format       u32      FORMAT_VERSION
//! version      u16 len + UTF-8   product version, e.g. "0.1.0"
//! count        u32      number of entries
//! entry * count:
//!   path       u16 len + UTF-8   relative, '/'-separated, validated
//!   method     u8       0 = stored, 1 = raw DEFLATE
//!   size       u64      uncompressed size
//!   stored     u64      size of the stored bytes that follow
//!   sha256     32 bytes of the uncompressed data
//!   data       `stored` bytes
//! trailer      32 bytes SHA-256 of everything before it
//! ```
//!
//! [`unpack`] verifies the trailer, every field, every path and every
//! per-file hash before returning anything, so a corrupted or tampered
//! payload never writes a single file.

use sha2::{Digest, Sha256};

pub const MAGIC: &[u8; 8] = b"XRSPAYLD";
pub const FORMAT_VERSION: u32 = 1;
/// Upper bound for one file, to reject absurd sizes before allocating.
pub const MAX_FILE_SIZE: u64 = 512 * 1024 * 1024;

const METHOD_STORED: u8 = 0;
const METHOD_DEFLATE: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative path with '/' separators.
    pub path: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    pub product_version: String,
    pub entries: Vec<Entry>,
}

impl Payload {
    pub fn total_size(&self) -> u64 {
        self.entries.iter().map(|e| e.data.len() as u64).sum()
    }

    pub fn paths(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.path.clone()).collect()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PayloadError {
    #[error(
        "the setup program contains no payload (it was built without scripts/build-windows.sh)"
    )]
    Empty,
    #[error("payload is truncated")]
    Truncated,
    #[error("payload is not an explorer.rs payload")]
    BadMagic,
    #[error("unsupported payload format {0}")]
    UnsupportedFormat(u32),
    #[error("payload checksum mismatch (corrupted download?)")]
    ChecksumMismatch,
    #[error("payload entry {0:?}: checksum mismatch")]
    FileChecksumMismatch(String),
    #[error("payload entry {0:?}: invalid path")]
    InvalidPath(String),
    #[error("payload entry {0:?}: duplicate path")]
    DuplicatePath(String),
    #[error("payload entry {0:?}: invalid data")]
    InvalidData(String),
    #[error("payload contains no files")]
    NoFiles,
}

/// A relative path is accepted only if it cannot escape the install folder
/// and is a valid Windows file name: '/'-separated, no empty / "." / ".."
/// components, no drive letters, no characters Windows forbids, no
/// reserved device names, no trailing dot or space.
pub fn validate_path(p: &str) -> bool {
    if p.is_empty() || p.len() > 240 || p.starts_with('/') {
        return false;
    }
    p.split('/').all(|c| {
        const RESERVED: [&str; 22] = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        let stem = c.split('.').next().unwrap_or("").to_ascii_uppercase();
        !c.is_empty()
            && c != "."
            && c != ".."
            && !c.ends_with('.')
            && !c.ends_with(' ')
            && !c
                .chars()
                .any(|ch| ch < ' ' || matches!(ch, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*'))
            && !RESERVED.contains(&stem.as_str())
    })
}

/// Build a payload. Files are DEFLATE-compressed when that saves space.
/// Panics on invalid or duplicate paths (a packaging bug, caught at build
/// time).
pub fn pack(product_version: &str, files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    put_str(&mut out, product_version);
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    let mut seen = std::collections::HashSet::new();
    for (path, data) in files {
        assert!(validate_path(path), "invalid payload path {path:?}");
        assert!(
            seen.insert(path.to_ascii_lowercase()),
            "duplicate payload path {path:?}"
        );
        let compressed = miniz_oxide::deflate::compress_to_vec(data, 9);
        let (method, stored) = if compressed.len() < data.len() {
            (METHOD_DEFLATE, compressed)
        } else {
            (METHOD_STORED, data.clone())
        };
        put_str(&mut out, path);
        out.push(method);
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(&(stored.len() as u64).to_le_bytes());
        out.extend_from_slice(&Sha256::digest(data));
        out.extend_from_slice(&stored);
    }
    let digest = Sha256::digest(&out);
    out.extend_from_slice(&digest);
    out
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let len = u16::try_from(s.len()).expect("string too long for payload");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], PayloadError> {
        let end = self.pos.checked_add(n).ok_or(PayloadError::Truncated)?;
        let s = self.buf.get(self.pos..end).ok_or(PayloadError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, PayloadError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, PayloadError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, PayloadError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, PayloadError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String, PayloadError> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| PayloadError::BadMagic)
    }
}

/// Parse and fully verify a payload.
pub fn unpack(bytes: &[u8]) -> Result<Payload, PayloadError> {
    if bytes.is_empty() {
        return Err(PayloadError::Empty);
    }
    if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Err(PayloadError::BadMagic);
    }
    if bytes.len() < MAGIC.len() + 4 + 32 {
        return Err(PayloadError::Truncated);
    }
    let (body, trailer) = bytes.split_at(bytes.len() - 32);
    if Sha256::digest(body).as_slice() != trailer {
        return Err(PayloadError::ChecksumMismatch);
    }
    let mut r = Reader { buf: body, pos: 8 };
    let format = r.u32()?;
    if format != FORMAT_VERSION {
        return Err(PayloadError::UnsupportedFormat(format));
    }
    let product_version = r.string()?;
    let count = r.u32()?;
    if count == 0 {
        return Err(PayloadError::NoFiles);
    }
    let mut entries = Vec::with_capacity(count.min(4096) as usize);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..count {
        let path = r.string()?;
        if !validate_path(&path) {
            return Err(PayloadError::InvalidPath(path));
        }
        if !seen.insert(path.to_ascii_lowercase()) {
            return Err(PayloadError::DuplicatePath(path));
        }
        let method = r.u8()?;
        let size = r.u64()?;
        let stored_len = r.u64()?;
        let hash: [u8; 32] = r.take(32)?.try_into().unwrap();
        if size > MAX_FILE_SIZE || stored_len > MAX_FILE_SIZE {
            return Err(PayloadError::InvalidData(path));
        }
        let stored = r.take(stored_len as usize)?;
        let data = match method {
            METHOD_STORED => stored.to_vec(),
            METHOD_DEFLATE => {
                miniz_oxide::inflate::decompress_to_vec_with_limit(stored, size as usize)
                    .map_err(|_| PayloadError::InvalidData(path.clone()))?
            }
            _ => return Err(PayloadError::InvalidData(path)),
        };
        if data.len() as u64 != size {
            return Err(PayloadError::InvalidData(path));
        }
        if Sha256::digest(&data).as_slice() != hash {
            return Err(PayloadError::FileChecksumMismatch(path));
        }
        entries.push(Entry { path, data });
    }
    if r.pos != body.len() {
        return Err(PayloadError::InvalidData("<trailing data>".into()));
    }
    Ok(Payload {
        product_version,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<(String, Vec<u8>)> {
        vec![
            ("explorer-rs.exe".into(), b"MZ fake exe ".repeat(500)),
            ("LICENSE".into(), b"GPL".to_vec()),
            ("scripts/install.ps1".into(), vec![]),
            (
                "random.bin".into(),
                (0..2000u32).map(|i| (i * 7919 % 251) as u8).collect(),
            ),
        ]
    }

    #[test]
    fn roundtrip() {
        let files = sample();
        let bytes = pack("1.2.3", &files);
        let p = unpack(&bytes).unwrap();
        assert_eq!(p.product_version, "1.2.3");
        assert_eq!(p.entries.len(), files.len());
        for (e, (path, data)) in p.entries.iter().zip(&files) {
            assert_eq!(&e.path, path);
            assert_eq!(&e.data, data);
        }
        assert_eq!(
            p.total_size(),
            files.iter().map(|f| f.1.len() as u64).sum::<u64>()
        );
        // Repetitive data really was compressed.
        assert!(bytes.len() < 3000);
    }

    #[test]
    fn empty_and_garbage() {
        assert_eq!(unpack(b""), Err(PayloadError::Empty));
        assert_eq!(unpack(b"hello world"), Err(PayloadError::BadMagic));
        assert_eq!(unpack(b"XRSPAYLD"), Err(PayloadError::Truncated));
        assert_eq!(unpack(&pack("1", &[])), Err(PayloadError::NoFiles));
    }

    #[test]
    fn every_single_byte_corruption_is_detected() {
        let bytes = pack("1.0.0", &sample());
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0x5A;
            assert!(unpack(&b).is_err(), "flip at {i} not detected");
        }
    }

    #[test]
    fn truncation_is_detected() {
        let bytes = pack("1.0.0", &sample());
        for n in [0, 7, 8, 20, bytes.len() / 2, bytes.len() - 1] {
            assert!(unpack(&bytes[..n]).is_err(), "truncated to {n}");
        }
    }

    /// Re-seal a body with a valid trailer, to test the inner checks the
    /// trailer would otherwise mask.
    fn reseal(mut body: Vec<u8>) -> Vec<u8> {
        body.truncate(body.len() - 32);
        let d = Sha256::digest(&body);
        body.extend_from_slice(&d);
        body
    }

    #[test]
    fn per_file_hash_is_checked_even_with_a_valid_trailer() {
        let files = vec![("a.txt".to_string(), b"abc".to_vec())];
        let mut bytes = pack("1", &files);
        // Stored (too small to compress): the data is the last 3 bytes
        // before the trailer.
        let at = bytes.len() - 32 - 1;
        bytes[at] = b'X';
        assert_eq!(
            unpack(&reseal(bytes)),
            Err(PayloadError::FileChecksumMismatch("a.txt".into()))
        );
    }

    #[test]
    fn hostile_paths_are_rejected() {
        for bad in [
            "../evil.exe",
            "a/../../b",
            "/abs",
            "C:/x",
            "c:x",
            "a\\b",
            "",
            "a//b",
            "./a",
            "CON",
            "nul.txt",
            "trail.",
            "trail ",
            "x*y",
        ] {
            assert!(!validate_path(bad), "{bad:?} accepted");
        }
        for good in [
            "explorer-rs.exe",
            "scripts/install.ps1",
            "README.md",
            "config.example.toml",
        ] {
            assert!(validate_path(good), "{good:?} rejected");
        }
        // A hostile entry in an otherwise valid (re-sealed) archive.
        let mut tampered = pack("1", &[("zz_x".to_string(), b"1".to_vec())]);
        let pos = tampered.windows(4).position(|w| w == b"zz_x").unwrap();
        tampered[pos..pos + 4].copy_from_slice(b"../x");
        assert!(matches!(
            unpack(&reseal(tampered)),
            Err(PayloadError::InvalidPath(_))
        ));
    }

    #[test]
    #[should_panic(expected = "duplicate")]
    fn pack_rejects_case_insensitive_duplicates() {
        pack("1", &[("A.txt".into(), vec![1]), ("a.TXT".into(), vec![2])]);
    }
}
