// SPDX-License-Identifier: GPL-3.0-or-later
//! Versioned, checksummed on-disk snapshots of the index.
//!
//! ```text
//! header (32 bytes, little endian)
//!   0  magic        b"EXRSIDX\0"
//!   8  version      u32  (= VERSION)
//!  12  flags        u32  (bit 0: size column present)
//!  16  body_len     u64
//!  24  body_crc32   u32  (CRC-32 of the body)
//!  28  header_crc32 u32  (CRC-32 of bytes 0..28)
//! body
//!   varint volume_count
//!   volume*: u8 kind (0 walk, 1 ntfs), varint path_len, path (UTF-8),
//!            u8 has_journal, [u64 journal_id, i64 next_usn]
//!   varint entry_count
//!   entry*:  varint tag: 0 = volume root, followed by varint volume index;
//!                        d > 0 = child of the entry d positions earlier
//!            (non-roots) u8 flags (bit 0 dir), varint key+1,
//!            [varint size+1], varint name_len, name (UTF-8)
//! ```
//!
//! Entries are written in pre-order, so a parent always precedes its
//! children and a parent reference can never form a cycle. Loading rebuilds
//! the index through the normal insert path, which validates every name;
//! any inconsistency makes the whole snapshot invalid and the daemon falls
//! back to a clean rebuild. Saving writes a temporary file, fsyncs it and
//! renames it over the old snapshot, so a crash leaves either the old or the
//! new snapshot, never a mix.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use crate::index::{EntryId, Index, JournalPos, VolumeKind, MAX_NAME_BYTES, NONE};

pub const MAGIC: [u8; 8] = *b"EXRSIDX\0";
pub const VERSION: u32 = 1;
pub const HEADER_LEN: usize = 32;
const FLAG_SIZES: u32 = 1;
/// Refuse snapshots larger than this (corrupt length fields).
pub const MAX_SNAPSHOT_BYTES: u64 = 4 << 30;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("not a snapshot file")]
    BadMagic,
    #[error("unsupported snapshot version {0}")]
    Version(u32),
    #[error("checksum mismatch")]
    Checksum,
    #[error("snapshot truncated")]
    Truncated,
    #[error("snapshot corrupt: {0}")]
    Corrupt(&'static str),
}

struct CrcWriter<W: Write> {
    inner: W,
    crc: crc32fast::Hasher,
    len: u64,
}

impl<W: Write> CrcWriter<W> {
    fn bytes(&mut self, b: &[u8]) -> io::Result<()> {
        self.crc.update(b);
        self.len += b.len() as u64;
        self.inner.write_all(b)
    }
    fn varint(&mut self, mut v: u64) -> io::Result<()> {
        let mut buf = [0u8; 10];
        let mut n = 0;
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                buf[n] = b;
                n += 1;
                break;
            }
            buf[n] = b | 0x80;
            n += 1;
        }
        self.bytes(&buf[..n])
    }
}

fn header(flags: u32, body_len: u64, body_crc: u32) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[0..8].copy_from_slice(&MAGIC);
    h[8..12].copy_from_slice(&VERSION.to_le_bytes());
    h[12..16].copy_from_slice(&flags.to_le_bytes());
    h[16..24].copy_from_slice(&body_len.to_le_bytes());
    h[24..28].copy_from_slice(&body_crc.to_le_bytes());
    let c = crc32fast::hash(&h[0..28]);
    h[28..32].copy_from_slice(&c.to_le_bytes());
    h
}

/// Write the body; returns (flags, body length, body crc).
fn write_body<W: Write>(ix: &Index, w: W) -> io::Result<(u32, u64, u32)> {
    let flags = if ix.has_sizes() { FLAG_SIZES } else { 0 };
    let mut w = CrcWriter {
        inner: w,
        crc: crc32fast::Hasher::new(),
        len: 0,
    };
    let vols: Vec<_> = ix.volumes().collect();
    w.varint(vols.len() as u64)?;
    for (_, v) in &vols {
        w.bytes(&[match v.kind {
            VolumeKind::Walk => 0,
            VolumeKind::Ntfs => 1,
        }])?;
        w.varint(v.path.len() as u64)?;
        w.bytes(v.path.as_bytes())?;
        match v.journal {
            Some(j) => {
                w.bytes(&[1])?;
                w.bytes(&j.journal_id.to_le_bytes())?;
                w.bytes(&j.next_usn.to_le_bytes())?;
            }
            None => w.bytes(&[0])?,
        }
    }
    // Every live entry belongs to exactly one volume (index invariant).
    w.varint(ix.len() as u64)?;
    let mut pos: u64 = 0;
    let mut written = 0usize;
    // Position (in output order) of each id written so far.
    let mut out_pos: Vec<u64> = vec![u64::MAX; ix.id_bound()];
    for (vi, (_, v)) in vols.iter().enumerate() {
        for id in ix.subtree(v.root) {
            out_pos[id as usize] = pos;
            if id == v.root {
                written += 1;
                w.varint(0)?;
                w.varint(vi as u64)?;
            } else {
                let p = out_pos[ix.parent(id) as usize];
                w.varint(pos - p)?;
                w.bytes(&[u8::from(ix.is_dir(id))])?;
                let k = ix.key(id);
                w.varint(if k == NONE { 0 } else { k as u64 + 1 })?;
                if flags & FLAG_SIZES != 0 {
                    w.varint(ix.size(id).map_or(0, |s| s.saturating_add(1)))?;
                }
                let name = ix.name(id);
                w.varint(name.len() as u64)?;
                w.bytes(name.as_bytes())?;
                written += 1;
            }
            pos += 1;
        }
    }
    if written != ix.len() {
        return Err(io::Error::other(
            "index invariant violated: entry outside any volume",
        ));
    }
    w.inner.flush()?;
    Ok((flags, w.len, w.crc.finalize()))
}

/// Serialize to memory (tests, fuzz seeds).
pub fn encode(ix: &Index) -> Vec<u8> {
    let mut body = Vec::new();
    let (flags, len, crc) = write_body(ix, &mut body).expect("writing to a Vec cannot fail");
    let mut out = header(flags, len, crc).to_vec();
    out.extend(body);
    out
}

/// Atomically replace `path` with `bytes` (temp file, fsync, rename). Lets
/// the daemon encode under the index read lock and do the I/O without it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Atomically write the snapshot to `path`, streaming (low memory).
/// Returns the file size.
pub fn save(ix: &Index, path: &Path) -> io::Result<u64> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(&[0u8; HEADER_LEN])?;
    let (flags, len, crc) = {
        let mut bw = BufWriter::with_capacity(1 << 20, &mut f);
        let r = write_body(ix, &mut bw)?;
        bw.flush()?;
        r
    };
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&header(flags, len, crc))?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(HEADER_LEN as u64 + len)
}

/// Load and fully validate a snapshot file.
pub fn load(path: &Path) -> Result<Index, SnapshotError> {
    let len = std::fs::metadata(path)?.len();
    if len > MAX_SNAPSHOT_BYTES {
        return Err(SnapshotError::Corrupt("file too large"));
    }
    decode(&std::fs::read(path)?)
}

/// Decode a complete snapshot (header + body).
pub fn decode(bytes: &[u8]) -> Result<Index, SnapshotError> {
    if bytes.len() < HEADER_LEN {
        return Err(SnapshotError::Truncated);
    }
    let h = &bytes[..HEADER_LEN];
    if h[0..8] != MAGIC {
        return Err(SnapshotError::BadMagic);
    }
    let rd32 = |o: usize| u32::from_le_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]);
    if crc32fast::hash(&h[0..28]) != rd32(28) {
        return Err(SnapshotError::Checksum);
    }
    let version = rd32(8);
    if version != VERSION {
        return Err(SnapshotError::Version(version));
    }
    let flags = rd32(12);
    let body_len = u64::from_le_bytes(h[16..24].try_into().unwrap_or_default());
    let body = &bytes[HEADER_LEN..];
    if body.len() as u64 != body_len {
        return Err(SnapshotError::Truncated);
    }
    if crc32fast::hash(body) != rd32(24) {
        return Err(SnapshotError::Checksum);
    }
    decode_body(body, flags)
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self.p.checked_add(n).ok_or(SnapshotError::Truncated)?;
        let s = self.b.get(self.p..end).ok_or(SnapshotError::Truncated)?;
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.take(1)?[0])
    }
    fn u64(&mut self) -> Result<u64, SnapshotError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().unwrap_or_default(),
        ))
    }
    fn varint(&mut self) -> Result<u64, SnapshotError> {
        let mut v: u64 = 0;
        for i in 0..10 {
            let b = self.u8()?;
            if i == 9 && b > 1 {
                return Err(SnapshotError::Corrupt("varint overflow"));
            }
            v |= ((b & 0x7F) as u64) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(SnapshotError::Corrupt("varint too long"))
    }
    fn str(&mut self, max: usize) -> Result<&'a str, SnapshotError> {
        let n = self.varint()?;
        if n > max as u64 {
            return Err(SnapshotError::Corrupt("string too long"));
        }
        std::str::from_utf8(self.take(n as usize)?).map_err(|_| SnapshotError::Corrupt("bad utf-8"))
    }
}

/// Decode a body without checking the header (exposed for fuzzing).
pub fn decode_body(body: &[u8], flags: u32) -> Result<Index, SnapshotError> {
    let corrupt = SnapshotError::Corrupt;
    let mut r = Reader { b: body, p: 0 };
    let mut ix = Index::new();
    let nvol = r.varint()?;
    if nvol > 4096 {
        return Err(corrupt("too many volumes"));
    }
    struct V {
        path: String,
        kind: VolumeKind,
        journal: Option<JournalPos>,
        id: Option<u16>,
    }
    let mut vols = Vec::with_capacity(nvol as usize);
    for _ in 0..nvol {
        let kind = match r.u8()? {
            0 => VolumeKind::Walk,
            1 => VolumeKind::Ntfs,
            _ => return Err(corrupt("bad volume kind")),
        };
        let path = r.str(4096)?.to_string();
        let journal = match r.u8()? {
            0 => None,
            1 => Some(JournalPos {
                journal_id: r.u64()?,
                next_usn: r.u64()? as i64,
            }),
            _ => return Err(corrupt("bad journal flag")),
        };
        vols.push(V {
            path,
            kind,
            journal,
            id: None,
        });
    }
    let count = r.varint()?;
    // Every entry takes at least 2 bytes; reject impossible counts before
    // allocating anything proportional to them.
    if count > (body.len() as u64) / 2 + 1 {
        return Err(corrupt("entry count exceeds body size"));
    }
    let mut ids: Vec<EntryId> = Vec::with_capacity(count as usize);
    for i in 0..count {
        let tag = r.varint()?;
        if tag == 0 {
            let vi = r.varint()? as usize;
            let v = vols.get_mut(vi).ok_or(corrupt("bad volume index"))?;
            if v.id.is_some() {
                return Err(corrupt("duplicate volume root"));
            }
            let vid = ix
                .add_volume(&v.path, v.kind)
                .map_err(|_| corrupt("bad volume"))?;
            ix.set_journal(vid, v.journal);
            v.id = Some(vid);
            ids.push(ix.volume(vid).map(|v| v.root).unwrap_or(NONE));
            continue;
        }
        if tag > i {
            return Err(corrupt("parent reference out of range"));
        }
        let parent = ids[(i - tag) as usize];
        let fl = r.u8()?;
        if fl > 1 {
            return Err(corrupt("bad entry flags"));
        }
        let key = match r.varint()? {
            0 => NONE,
            k if k < NONE as u64 => (k - 1) as u32,
            _ => return Err(corrupt("bad key")),
        };
        let size = if flags & FLAG_SIZES != 0 {
            r.varint()?.checked_sub(1)
        } else {
            None
        };
        let name = r.str(MAX_NAME_BYTES)?;
        if !ix.is_dir(parent) {
            return Err(corrupt("parent is not a directory"));
        }
        let id = ix
            .insert(parent, name, fl & 1 != 0, size, key)
            .map_err(|_| corrupt("invalid entry"))?;
        if key != NONE {
            if let Some(vid) = ix.volume_of(id) {
                if ix.volume(vid).is_some_and(|v| v.kind == VolumeKind::Ntfs) {
                    ix.ntfs_map(vid, key, id);
                }
            }
        }
        ids.push(id);
    }
    if r.p != body.len() {
        return Err(corrupt("trailing bytes"));
    }
    if vols.iter().any(|v| v.id.is_none()) {
        return Err(corrupt("volume without root"));
    }
    ix.shrink_to_fit();
    Ok(ix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Index {
        let mut ix = Index::new();
        let c = ix.add_volume("C:\\", VolumeKind::Ntfs).unwrap();
        ix.set_journal(
            c,
            Some(JournalPos {
                journal_id: 0xDEAD_BEEF,
                next_usn: 123_456,
            }),
        );
        let root = ix.volume(c).unwrap().root;
        let u = ix.insert(root, "Users", true, None, 20).unwrap();
        ix.ntfs_map(c, 20, u);
        let f = ix
            .insert(u, "Ünïcødé File.TXT", false, Some(7), 21)
            .unwrap();
        ix.ntfs_map(c, 21, f);
        let d = ix.add_volume("/data", VolumeKind::Walk).unwrap();
        let droot = ix.volume(d).unwrap().root;
        let a = ix.insert(droot, "a", true, None, NONE).unwrap();
        ix.insert(a, "b.rs", false, Some(0), NONE).unwrap();
        ix
    }

    fn dump(ix: &Index) -> Vec<String> {
        let mut v: Vec<String> = ix
            .live_ids()
            .map(|id| {
                format!(
                    "{}|{}|{:?}|{}",
                    ix.path(id),
                    ix.is_dir(id),
                    ix.size(id),
                    ix.key(id)
                )
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn roundtrip_memory_and_file() {
        let ix = sample();
        let bytes = encode(&ix);
        let back = decode(&bytes).unwrap();
        assert_eq!(dump(&ix), dump(&back));
        back.check_invariants().unwrap();
        let c = back.find_volume("C:\\").unwrap();
        assert_eq!(back.volume(c).unwrap().journal.unwrap().next_usn, 123_456);
        assert_eq!(
            back.name(back.ntfs_lookup(c, 21).unwrap()),
            "Ünïcødé File.TXT"
        );

        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("index.snapshot");
        let n = save(&ix, &p).unwrap();
        assert_eq!(n, bytes.len() as u64);
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        assert_eq!(dump(&load(&p).unwrap()), dump(&ix));
        assert!(!p.with_extension("tmp").exists());
        write_atomic(&p, &bytes).unwrap();
        assert_eq!(dump(&load(&p).unwrap()), dump(&ix));
    }

    #[test]
    fn empty_index_roundtrip() {
        let ix = Index::new();
        let back = decode(&encode(&ix)).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn every_truncation_and_bit_flip_is_rejected_without_panic() {
        let bytes = encode(&sample());
        for cut in 0..bytes.len() {
            assert!(decode(&bytes[..cut]).is_err(), "truncated at {cut}");
        }
        for i in 0..bytes.len() {
            for bit in 0..8 {
                let mut b = bytes.clone();
                b[i] ^= 1 << bit;
                assert!(decode(&b).is_err(), "flip {i}:{bit} accepted");
            }
        }
    }

    #[test]
    fn wrong_version_and_magic() {
        let mut b = encode(&sample());
        b[8] = 2;
        let c = crc32fast::hash(&b[0..28]);
        b[28..32].copy_from_slice(&c.to_le_bytes());
        assert!(matches!(decode(&b), Err(SnapshotError::Version(2))));
        b[0] = b'X';
        assert!(matches!(decode(&b), Err(SnapshotError::BadMagic)));
    }

    #[test]
    fn body_decoder_survives_garbage() {
        // The checksum normally shields the body decoder; make sure it is
        // robust on its own (this is what the fuzz target exercises).
        let body = encode(&sample())[HEADER_LEN..].to_vec();
        let mut seed = 0x1234_5678u32;
        for _ in 0..20_000 {
            let mut b = body.clone();
            for _ in 0..3 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let i = (seed as usize >> 8) % b.len();
                b[i] = (seed >> 24) as u8;
            }
            if let Ok(ix) = decode_body(&b, FLAG_SIZES) {
                ix.check_invariants().unwrap();
            }
            let _ = decode_body(&b[..(seed as usize % b.len())], 0);
        }
        // Self-referencing and forward parent references.
        assert!(decode_body(&[0, 1, 1, 0, 0, 1, b'a'], 0).is_err());
    }

    #[test]
    fn missing_file_is_io_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(
            load(&d.path().join("nope")),
            Err(SnapshotError::Io(_))
        ));
    }
}
