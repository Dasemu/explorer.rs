// SPDX-License-Identifier: GPL-3.0-or-later
//! NTFS master file table enumeration and USN change-journal replay.
//!
//! Everything here is pure and portable: records are parsed from byte
//! buffers exactly as `FSCTL_ENUM_USN_DATA` / `FSCTL_READ_USN_JOURNAL` return
//! them, and the source state machine ([`run_ntfs`]) talks to the volume
//! through the [`UsnBackend`] trait. On Windows the backend is either a
//! direct volume handle or the elevated helper (see
//! docs/decisions/0010-index-privilege-model.md); in tests it is a simulated
//! volume.
//!
//! Replay rule (idempotent, so replaying a record twice is harmless):
//! - `FILE_DELETE` -> remove the entry (and anything still below it);
//! - `RENAME_OLD_NAME` without `RENAME_NEW_NAME` -> ignore (stale name);
//! - anything else -> upsert: every record carries the file's *current*
//!   name and parent, so create, rename-new and data changes are handled
//!   alike. Records whose parent is not indexed (excluded, or outside the
//!   indexed tree) remove the entry if it exists.

use std::time::Duration;

use crate::index::{EntryId, Index, JournalPos, ScannedTree, VolId, NONE, ROOT_RECORD, SEP};
use crate::rules::Rules;
use crate::source::{ApplyStats, Change, ChangeSink, SourceState, Stop};

pub const USN_REASON_DATA_OVERWRITE: u32 = 0x0000_0001;
pub const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const USN_REASON_HARD_LINK_CHANGE: u32 = 0x0001_0000;
pub const USN_REASON_CLOSE: u32 = 0x8000_0000;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// Smallest possible V2 record (header + one UTF-16 unit, rounded).
const V2_HEADER: usize = 60;
const V3_HEADER: usize = 76;
/// Largest record we accept (header + 255 UTF-16 units, generously rounded).
const MAX_RECORD: usize = 4096;

/// A parsed USN record (V2 or V3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsnRecord {
    pub frn: u64,
    pub parent_frn: u64,
    pub usn: i64,
    pub reason: u32,
    pub attributes: u32,
    pub name: String,
}

impl UsnRecord {
    pub fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

/// MFT record number (low 48 bits) of a file reference, if it fits in u32.
pub fn record_number(frn: u64) -> Option<u32> {
    let r = frn & 0x0000_FFFF_FFFF_FFFF;
    (r < NONE as u64).then_some(r as u32)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("record truncated")]
    Truncated,
    #[error("bad record length {0}")]
    BadLength(u32),
    #[error("unsupported record version {0}")]
    Version(u16),
    #[error("bad file name field")]
    BadName,
    #[error("128-bit file id not supported")]
    WideFileId,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap_or([0; 4]))
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap_or([0; 8]))
}

/// Parse one record at the start of `buf`. Returns the record (None for
/// records we skip, e.g. V4 range records) and its length.
pub fn parse_record(buf: &[u8]) -> Result<(Option<UsnRecord>, usize), ParseError> {
    if buf.len() < 8 {
        return Err(ParseError::Truncated);
    }
    let len = u32_at(buf, 0);
    let major = u16_at(buf, 4);
    let l = len as usize;
    if !(8..=MAX_RECORD).contains(&l) {
        return Err(ParseError::BadLength(len));
    }
    if l > buf.len() {
        return Err(ParseError::Truncated);
    }
    let rec = &buf[..l];
    let (frn, parent, base) = match major {
        2 => {
            if l < V2_HEADER {
                return Err(ParseError::BadLength(len));
            }
            (u64_at(rec, 8), u64_at(rec, 16), 24)
        }
        3 => {
            if l < V3_HEADER {
                return Err(ParseError::BadLength(len));
            }
            if u64_at(rec, 16) != 0 || u64_at(rec, 32) != 0 {
                return Err(ParseError::WideFileId);
            }
            (u64_at(rec, 8), u64_at(rec, 24), 40)
        }
        4 => return Ok((None, l)),
        v => return Err(ParseError::Version(v)),
    };
    let usn = u64_at(rec, base) as i64;
    let reason = u32_at(rec, base + 16);
    let attributes = u32_at(rec, base + 28);
    let name_len = u16_at(rec, base + 32) as usize;
    let name_off = u16_at(rec, base + 34) as usize;
    if name_len % 2 != 0 || name_off < base + 36 || name_off + name_len > l || name_len == 0 {
        return Err(ParseError::BadName);
    }
    let units: Vec<u16> = rec[name_off..name_off + name_len]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let name = String::from_utf16_lossy(&units);
    Ok((
        Some(UsnRecord {
            frn,
            parent_frn: parent,
            usn,
            reason,
            attributes,
            name,
        }),
        l,
    ))
}

/// Parse a buffer of consecutive records (the part after the leading
/// 8-byte USN/FRN that both FSCTLs prepend). Malformed records stop the
/// parse; the records parsed so far are returned together with the error.
pub fn parse_records(mut buf: &[u8]) -> (Vec<UsnRecord>, Option<ParseError>) {
    let mut out = Vec::new();
    while !buf.is_empty() {
        match parse_record(buf) {
            Ok((r, l)) => {
                if let Some(r) = r {
                    out.push(r);
                }
                buf = &buf[l..];
            }
            Err(ParseError::WideFileId) => {
                // Skip records we cannot represent, keep going.
                let l = u32_at(buf, 0) as usize;
                buf = &buf[l.min(buf.len())..];
            }
            Err(e) => return (out, Some(e)),
        }
    }
    (out, None)
}

fn encode(rec: &UsnRecord, v3: bool) -> Vec<u8> {
    let units: Vec<u16> = rec.name.encode_utf16().collect();
    let header = if v3 { V3_HEADER } else { V2_HEADER };
    let len = (header + units.len() * 2).div_ceil(8) * 8;
    let mut b = vec![0u8; len];
    b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
    b[4..6].copy_from_slice(&(if v3 { 3u16 } else { 2u16 }).to_le_bytes());
    let base = if v3 {
        b[8..16].copy_from_slice(&rec.frn.to_le_bytes());
        b[24..32].copy_from_slice(&rec.parent_frn.to_le_bytes());
        40
    } else {
        b[8..16].copy_from_slice(&rec.frn.to_le_bytes());
        b[16..24].copy_from_slice(&rec.parent_frn.to_le_bytes());
        24
    };
    b[base..base + 8].copy_from_slice(&rec.usn.to_le_bytes());
    b[base + 16..base + 20].copy_from_slice(&rec.reason.to_le_bytes());
    b[base + 28..base + 32].copy_from_slice(&rec.attributes.to_le_bytes());
    b[base + 32..base + 34].copy_from_slice(&((units.len() * 2) as u16).to_le_bytes());
    b[base + 34..base + 36].copy_from_slice(&(header as u16).to_le_bytes());
    for (i, u) in units.iter().enumerate() {
        b[header + 2 * i..header + 2 * i + 2].copy_from_slice(&u.to_le_bytes());
    }
    b
}

/// Encode a record in the `USN_RECORD_V2` layout (tests, fuzz seeds,
/// simulated volumes).
pub fn encode_v2(rec: &UsnRecord) -> Vec<u8> {
    encode(rec, false)
}

/// Encode a record in the `USN_RECORD_V3` layout.
pub fn encode_v3(rec: &UsnRecord) -> Vec<u8> {
    encode(rec, true)
}

// ----- initial scan ----------------------------------------------------

/// Build a topologically ordered tree from MFT enumeration records.
///
/// `root_path` is the volume root as displayed (e.g. `C:\`), used for path
/// exclusion rules. Records not reachable from the root directory (system
/// metafiles, `$Extend`, orphans) are dropped.
pub fn build_tree(records: &[UsnRecord], rules: &Rules, root_path: &str) -> ScannedTree {
    // Dense record-number -> item map (capped against corrupt input).
    const CAP: usize = 64 << 20;
    let mut items: Vec<(u32, u32)> = Vec::with_capacity(records.len()); // (rec, parent rec)
    let mut max_rec = 0usize;
    for r in records {
        let (Some(rec), Some(par)) = (record_number(r.frn), record_number(r.parent_frn)) else {
            items.push((NONE, NONE));
            continue;
        };
        if (rec as usize) >= CAP || rec < 16 || rec == par {
            items.push((NONE, NONE));
            continue;
        }
        max_rec = max_rec.max(rec as usize);
        items.push((rec, par));
    }
    let mut by_rec = vec![NONE; max_rec + 1];
    for (i, &(rec, _)) in items.iter().enumerate() {
        if rec != NONE {
            by_rec[rec as usize] = i as u32;
        }
    }
    // Children in CSR form, keyed by parent item index; slot n = volume root.
    let n = items.len();
    let parent_slot = |i: usize| -> Option<usize> {
        let (rec, par) = items[i];
        if rec == NONE {
            return None;
        }
        if par == ROOT_RECORD {
            return Some(n);
        }
        match by_rec.get(par as usize) {
            Some(&p) if p != NONE => Some(p as usize),
            _ => None,
        }
    };
    let mut start = vec![0u32; n + 2];
    for i in 0..n {
        if let Some(p) = parent_slot(i) {
            start[p + 1] += 1;
        }
    }
    for i in 0..=n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut kids = vec![0u32; start[n + 1] as usize];
    for i in 0..n {
        if let Some(p) = parent_slot(i) {
            kids[fill[p] as usize] = i as u32;
            fill[p] += 1;
        }
    }
    drop(fill);

    let mut tree = ScannedTree::default();
    let root = root_path.trim_end_matches(['/', '\\']).to_string();
    // BFS: (item slot, tree index, path if path rules are in use)
    let mut queue = std::collections::VecDeque::new();
    queue.push_back((n, NONE, root));
    let mut seen = vec![false; n];
    while let Some((slot, tidx, path)) = queue.pop_front() {
        for &k in &kids[start[slot] as usize..start[slot + 1] as usize] {
            let k = k as usize;
            if seen[k] {
                continue;
            }
            seen[k] = true;
            let r = &records[k];
            if !crate::index::valid_name(&r.name) || rules.excludes_name(&r.name) {
                continue;
            }
            let child_path = if rules.has_path_rules() {
                let p = format!("{path}{SEP}{}", r.name);
                if rules.excludes_path(&p) {
                    continue;
                }
                p
            } else {
                String::new()
            };
            let t = tree.push(tidx, &r.name, r.is_dir(), None, items[k].0);
            if r.is_dir() {
                queue.push_back((k, t, child_path));
            }
        }
    }
    tree
}

// ----- replay ------------------------------------------------------------

/// Apply one journal record to the index (see module docs for the rule).
pub fn apply_record(ix: &mut Index, vol: VolId, rules: &Rules, r: &UsnRecord, st: &mut ApplyStats) {
    let Some(rec) = record_number(r.frn) else {
        st.ignored += 1;
        return;
    };
    let existing = ix.ntfs_lookup(vol, rec);
    if r.reason & USN_REASON_FILE_DELETE != 0 {
        match existing {
            Some(id) if !ix.is_root(id) => st.removed += ix.remove(id).unwrap_or(0),
            _ => st.ignored += 1,
        }
        return;
    }
    if r.reason & USN_REASON_RENAME_OLD_NAME != 0 && r.reason & USN_REASON_RENAME_NEW_NAME == 0 {
        st.ignored += 1;
        return;
    }
    let parent = record_number(r.parent_frn).and_then(|p| ix.ntfs_lookup(vol, p));
    let remove = |ix: &mut Index, st: &mut ApplyStats, id: Option<EntryId>| match id {
        Some(id) if !ix.is_root(id) => st.removed += ix.remove(id).unwrap_or(0),
        _ => st.ignored += 1,
    };
    let Some(parent) = parent.filter(|&p| ix.is_dir(p)) else {
        remove(ix, st, existing);
        return;
    };
    if rec < 16 || !crate::index::valid_name(&r.name) || rules.excludes_name(&r.name) {
        remove(ix, st, existing);
        return;
    }
    if rules.has_path_rules() {
        let mut p = ix.path(parent);
        if !p.ends_with(['/', '\\']) {
            p.push(SEP);
        }
        p.push_str(&r.name);
        if rules.excludes_path(&p) {
            remove(ix, st, existing);
            return;
        }
    }
    match existing {
        Some(id) if ix.is_root(id) => st.ignored += 1,
        Some(id) => {
            if ix.parent(id) != parent || ix.name(id) != r.name.as_str() {
                match ix.rename(id, parent, &r.name) {
                    Ok(()) => st.renamed += 1,
                    Err(_) => {
                        st.ignored += 1;
                        return;
                    }
                }
            }
            ix.set_dir(id, r.is_dir());
        }
        None => match ix.insert(parent, &r.name, r.is_dir(), None, rec) {
            Ok(id) => {
                ix.ntfs_map(vol, rec, id);
                st.inserted += 1;
            }
            Err(_) => st.ignored += 1,
        },
    }
}

// ----- source state machine ------------------------------------------------

/// `FSCTL_QUERY_USN_JOURNAL` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct JournalInfo {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
pub enum UsnError {
    /// `ERROR_JOURNAL_ENTRY_DELETED`: the journal wrapped past our position.
    #[error("journal entries were deleted")]
    EntryDeleted,
    /// `ERROR_JOURNAL_NOT_ACTIVE` / `ERROR_JOURNAL_DELETE_IN_PROGRESS`.
    #[error("change journal is not active")]
    NotActive,
    /// The journal was deleted and recreated (journal id changed).
    #[error("change journal id changed")]
    IdMismatch,
    #[error("access denied")]
    AccessDenied,
    #[error("stopped")]
    Stopped,
    #[error("{0}")]
    Io(String),
}

/// Access to one NTFS volume's MFT and change journal.
pub trait UsnBackend: Send {
    fn query(&mut self) -> Result<JournalInfo, UsnError>;
    /// One chunk of MFT records starting at file reference `start_frn`,
    /// limited to records with USN < `high_usn`. Returns the next start
    /// reference and the raw record bytes, or `None` at the end.
    fn enum_chunk(
        &mut self,
        start_frn: u64,
        high_usn: i64,
    ) -> Result<Option<(u64, Vec<u8>)>, UsnError>;
    /// Raw journal records from `start_usn` and the next USN to read.
    fn read_journal(&mut self, journal_id: u64, start_usn: i64)
        -> Result<(i64, Vec<u8>), UsnError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    FullScan,
    From(JournalPos),
}

/// Can we continue from a saved journal position?
pub fn resume_decision(saved: Option<JournalPos>, info: &JournalInfo) -> Resume {
    match saved {
        Some(p)
            if p.journal_id == info.journal_id
                && p.next_usn >= info.first_usn
                && p.next_usn <= info.next_usn =>
        {
            Resume::From(p)
        }
        _ => Resume::FullScan,
    }
}

/// Enumerate the whole MFT through `backend` and build the tree.
pub fn scan_mft<B: UsnBackend + ?Sized>(
    backend: &mut B,
    high_usn: i64,
    rules: &Rules,
    root_path: &str,
    stop: &Stop,
) -> Result<ScannedTree, UsnError> {
    let mut records = Vec::new();
    let mut start = 0u64;
    while let Some((next, bytes)) = backend.enum_chunk(start, high_usn)? {
        if stop.is_stopped() {
            return Err(UsnError::Stopped);
        }
        let (mut recs, err) = parse_records(&bytes);
        if let Some(e) = err {
            tracing::warn!(error = %e, "malformed MFT enumeration record; rest of chunk skipped");
        }
        records.append(&mut recs);
        if next <= start {
            break;
        }
        start = next;
    }
    Ok(build_tree(&records, rules, root_path))
}

/// Run an NTFS source until `stop`: full scan unless `saved` can be
/// resumed, then follow the journal, rescanning on wrap or reset.
#[allow(clippy::too_many_arguments)]
pub fn run_ntfs<B: UsnBackend + ?Sized>(
    backend: &mut B,
    vol: VolId,
    root_path: &str,
    mut saved: Option<JournalPos>,
    rules: &Rules,
    sink: &dyn ChangeSink,
    stop: &Stop,
    poll: Duration,
) -> Result<(), UsnError> {
    'outer: loop {
        if stop.is_stopped() {
            return Ok(());
        }
        let info = backend.query()?;
        let mut pos = match resume_decision(saved, &info) {
            Resume::From(p) => {
                tracing::info!(
                    vol,
                    usn = p.next_usn,
                    "resuming from saved journal position"
                );
                p
            }
            Resume::FullScan => {
                sink.set_state(vol, SourceState::Scanning);
                let t0 = std::time::Instant::now();
                let tree = match scan_mft(backend, info.next_usn, rules, root_path, stop) {
                    Err(UsnError::Stopped) => return Ok(()),
                    r => r?,
                };
                tracing::info!(
                    vol,
                    entries = tree.len(),
                    ms = t0.elapsed().as_millis() as u64,
                    "MFT scan done"
                );
                let pos = JournalPos {
                    journal_id: info.journal_id,
                    next_usn: info.next_usn,
                };
                sink.apply(vec![
                    Change::ReplaceTree { vol, tree },
                    Change::Journal {
                        vol,
                        pos: Some(pos),
                    },
                ]);
                pos
            }
        };
        sink.set_state(vol, SourceState::Live);
        loop {
            if stop.is_stopped() {
                return Ok(());
            }
            match backend.read_journal(pos.journal_id, pos.next_usn) {
                Ok((next, bytes)) => {
                    let (records, err) = parse_records(&bytes);
                    if let Some(e) = err {
                        tracing::warn!(vol, error = %e, "malformed journal record; rescanning");
                        saved = None;
                        continue 'outer;
                    }
                    let moved = next > pos.next_usn;
                    if moved {
                        pos.next_usn = next;
                    }
                    if !records.is_empty() || moved {
                        sink.apply(vec![
                            Change::Usn { vol, records },
                            Change::Journal {
                                vol,
                                pos: Some(pos),
                            },
                        ]);
                    }
                    if !moved && stop.sleep(poll) {
                        return Ok(());
                    }
                }
                Err(e @ (UsnError::EntryDeleted | UsnError::IdMismatch | UsnError::NotActive)) => {
                    tracing::warn!(vol, error = %e, "journal position lost; rescanning volume");
                    saved = None;
                    if e == UsnError::NotActive && stop.sleep(poll) {
                        return Ok(());
                    }
                    continue 'outer;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

// ----- simulated volume (tests and benchmarks) ---------------------------------

/// An in-memory NTFS volume: an MFT plus a bounded change journal.
#[derive(Debug, Clone)]
pub struct SimVolume {
    pub journal_id: u64,
    /// Live files: rec -> (parent rec, name, is_dir).
    pub files: std::collections::BTreeMap<u32, (u32, String, bool)>,
    /// Journal records in order; `first_usn` is the USN of `journal[0]`.
    pub journal: Vec<UsnRecord>,
    pub first_usn: i64,
    pub next_usn: i64,
    /// Max records kept; older ones are dropped (wrap).
    pub capacity: usize,
    /// MFT records per enumeration chunk.
    pub chunk: usize,
}

impl SimVolume {
    pub fn new(journal_id: u64) -> Self {
        Self {
            journal_id,
            files: Default::default(),
            journal: Vec::new(),
            first_usn: 0,
            next_usn: 0,
            capacity: usize::MAX,
            chunk: 1000,
        }
    }

    fn frn(rec: u32) -> u64 {
        // Use a non-zero sequence number like real volumes do.
        (1u64 << 48) | rec as u64
    }

    fn log(&mut self, rec: u32, reason: u32) {
        let (parent, name, dir) = self.files[&rec].clone();
        let r = UsnRecord {
            frn: Self::frn(rec),
            parent_frn: Self::frn(parent),
            usn: self.next_usn,
            reason,
            attributes: if dir { FILE_ATTRIBUTE_DIRECTORY } else { 0x20 },
            name,
        };
        self.next_usn += encode_v2(&r).len() as i64;
        self.journal.push(r);
        while self.journal.len() > self.capacity {
            self.journal.remove(0);
            self.first_usn = self.journal[0].usn;
        }
    }

    pub fn create(&mut self, rec: u32, parent: u32, name: &str, dir: bool) {
        self.files.insert(rec, (parent, name.into(), dir));
        self.log(rec, USN_REASON_FILE_CREATE);
        self.log(rec, USN_REASON_FILE_CREATE | USN_REASON_CLOSE);
    }

    pub fn rename(&mut self, rec: u32, parent: u32, name: &str) {
        self.log(rec, USN_REASON_RENAME_OLD_NAME);
        let e = self.files.get_mut(&rec).expect("rename of missing file");
        e.0 = parent;
        e.1 = name.into();
        self.log(rec, USN_REASON_RENAME_NEW_NAME);
        self.log(rec, USN_REASON_RENAME_NEW_NAME | USN_REASON_CLOSE);
    }

    pub fn write(&mut self, rec: u32) {
        self.log(rec, USN_REASON_DATA_OVERWRITE | USN_REASON_CLOSE);
    }

    pub fn delete(&mut self, rec: u32) {
        self.log(rec, USN_REASON_FILE_DELETE | USN_REASON_CLOSE);
        self.files.remove(&rec);
    }

    /// Recreate the journal with a new id (like `fsutil usn deletejournal`).
    pub fn reset_journal(&mut self, new_id: u64) {
        self.journal_id = new_id;
        self.journal.clear();
        self.first_usn = self.next_usn;
    }
}

impl UsnBackend for SimVolume {
    fn query(&mut self) -> Result<JournalInfo, UsnError> {
        Ok(JournalInfo {
            journal_id: self.journal_id,
            first_usn: self.first_usn,
            next_usn: self.next_usn,
        })
    }

    fn enum_chunk(
        &mut self,
        start_frn: u64,
        _high: i64,
    ) -> Result<Option<(u64, Vec<u8>)>, UsnError> {
        let start = (start_frn & 0x0000_FFFF_FFFF_FFFF) as u32;
        let mut out = Vec::new();
        let mut last = None;
        for (&rec, (parent, name, dir)) in self.files.range(start..).take(self.chunk) {
            out.extend(encode_v2(&UsnRecord {
                frn: Self::frn(rec),
                parent_frn: Self::frn(*parent),
                usn: 0,
                reason: 0,
                attributes: if *dir { FILE_ATTRIBUTE_DIRECTORY } else { 0x20 },
                name: name.clone(),
            }));
            last = Some(rec);
        }
        Ok(last.map(|l| (l as u64 + 1, out)))
    }

    fn read_journal(
        &mut self,
        journal_id: u64,
        start_usn: i64,
    ) -> Result<(i64, Vec<u8>), UsnError> {
        if journal_id != self.journal_id {
            return Err(UsnError::IdMismatch);
        }
        if start_usn < self.first_usn {
            return Err(UsnError::EntryDeleted);
        }
        let mut out = Vec::new();
        let mut next = start_usn.max(self.first_usn);
        for r in self.journal.iter().filter(|r| r.usn >= start_usn).take(256) {
            let b = encode_v2(r);
            next = r.usn + b.len() as i64;
            out.extend(b);
        }
        if out.is_empty() {
            next = self.next_usn.max(start_usn);
        }
        Ok((next, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::VolumeKind;
    use crate::source::{apply, VecSink};

    fn rec(frn: u64, parent: u64, reason: u32, name: &str, dir: bool) -> UsnRecord {
        UsnRecord {
            frn,
            parent_frn: parent,
            usn: 0,
            reason,
            attributes: if dir { FILE_ATTRIBUTE_DIRECTORY } else { 0 },
            name: name.into(),
        }
    }

    #[test]
    fn v2_v3_roundtrip_and_errors() {
        let r = rec(
            0x0002_0000_0000_0040,
            5,
            USN_REASON_FILE_CREATE,
            "Héllo wörld.txt",
            false,
        );
        for b in [encode_v2(&r), encode_v3(&r)] {
            let (p, l) = parse_record(&b).unwrap();
            assert_eq!(p.unwrap(), r);
            assert_eq!(l, b.len());
            assert_eq!(l % 8, 0);
            for cut in 0..b.len() {
                assert!(parse_record(&b[..cut]).is_err());
            }
        }
        let mut b = encode_v2(&r);
        b[4] = 9;
        assert_eq!(parse_record(&b), Err(ParseError::Version(9)));
        let mut b = encode_v2(&r);
        b[56] = 0xFF; // name length beyond record
        assert!(parse_record(&b).is_err());
        let mut b = encode_v3(&r);
        b[20] = 1; // high half of 128-bit id
        assert_eq!(parse_record(&b), Err(ParseError::WideFileId));
        let (ok, err) = parse_records(&[encode_v2(&r), b, encode_v2(&r)].concat());
        assert_eq!(ok.len(), 2);
        assert!(err.is_none());
        let (ok, err) = parse_records(&[encode_v2(&r), vec![1, 2, 3]].concat());
        assert_eq!(ok.len(), 1);
        assert!(err.is_some());
        assert_eq!(record_number(0xFFFF_0000_0000_0007), Some(7));
    }

    #[test]
    fn build_tree_from_mft() {
        let rules = Rules::new(&["$Recycle.Bin".into()], &["C:\\skip".into()]);
        let recs = vec![
            rec(30, 20, 0, "deep.txt", false), // child listed before parent
            rec(20, 5, 0, "Users", true),
            rec(21, 5, 0, "$Recycle.Bin", true),
            rec(22, 21, 0, "hidden.txt", false),
            rec(23, 5, 0, "skip", true),
            rec(24, 99, 0, "orphan", false),
            rec(6, 5, 0, "$Extend-ish", true), // metafile range
            rec(25, 25, 0, "self-parent", true),
            rec(26, 5, 0, "bad/name", false),
        ];
        let t = build_tree(&recs, &rules, "C:\\");
        let names: Vec<_> = (0..t.len()).map(|i| t.name(i).to_string()).collect();
        assert_eq!(names, vec!["Users", "deep.txt"]);
        assert_eq!(t.parent, vec![NONE, 0]);
        assert_eq!(t.key, vec![20, 30]);
    }

    fn index_with(vol: &mut SimVolume) -> (Index, VolId) {
        let mut ix = Index::new();
        let v = ix.add_volume("C:\\", VolumeKind::Ntfs).unwrap();
        let tree = scan_mft(vol, vol.next_usn, &Rules::default(), "C:\\", &Stop::new()).unwrap();
        ix.replace_tree(v, &tree).unwrap();
        (ix, v)
    }

    fn replay(ix: &mut Index, v: VolId, vol: &mut SimVolume, from: i64) -> i64 {
        let mut pos = from;
        loop {
            let (next, bytes) = vol.read_journal(vol.journal_id, pos).unwrap();
            let (records, err) = parse_records(&bytes);
            assert!(err.is_none());
            apply(ix, &Rules::default(), vec![Change::Usn { vol: v, records }]);
            if next == pos {
                return pos;
            }
            pos = next;
        }
    }

    /// The index after replaying the journal equals a fresh scan.
    fn assert_matches_scan(ix: &Index, v: VolId, vol: &mut SimVolume) {
        let (fresh, fv) = index_with(vol);
        let mut a: Vec<String> = ix
            .subtree(ix.volume(v).unwrap().root)
            .map(|id| ix.path(id))
            .collect();
        let mut b: Vec<String> = fresh
            .subtree(fresh.volume(fv).unwrap().root)
            .map(|id| fresh.path(id))
            .collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        ix.check_invariants().unwrap();
    }

    #[test]
    fn journal_replay_create_rename_move_delete() {
        let mut vol = SimVolume::new(1);
        vol.create(20, 5, "Users", true);
        vol.create(21, 20, "me", true);
        vol.create(22, 21, "a.txt", false);
        let (mut ix, v) = index_with(&mut vol);
        let start = vol.next_usn;
        vol.create(23, 21, "docs", true);
        vol.create(24, 23, "report.docx", false);
        vol.rename(22, 23, "b.txt"); // move + rename
        vol.write(24);
        vol.rename(23, 20, "Docs2"); // move a directory: children follow
        vol.create(25, 20, "tmp", true);
        vol.create(26, 25, "x", false);
        vol.delete(26);
        vol.delete(25);
        vol.create(26, 20, "reused-record", false); // record number reuse
        replay(&mut ix, v, &mut vol, start);
        let b = ix.ntfs_lookup(v, 22).unwrap();
        assert!(ix.path(b).ends_with(&format!("Users{SEP}Docs2{SEP}b.txt")));
        assert_eq!(ix.name(ix.ntfs_lookup(v, 26).unwrap()), "reused-record");
        assert_eq!(ix.ntfs_lookup(v, 25), None);
        assert_matches_scan(&ix, v, &mut vol);
        // Replaying the same records again is a no-op (idempotence).
        let n = ix.len();
        replay(&mut ix, v, &mut vol, start);
        assert_eq!(ix.len(), n);
        assert_matches_scan(&ix, v, &mut vol);
    }

    #[test]
    fn replay_handles_missed_child_deletes_and_unknown_parents() {
        let mut vol = SimVolume::new(1);
        vol.create(20, 5, "d", true);
        vol.create(21, 20, "f", false);
        let (mut ix, v) = index_with(&mut vol);
        let mut st = ApplyStats::default();
        // Directory deleted while we never saw the child's delete record.
        apply_record(
            &mut ix,
            v,
            &Rules::default(),
            &rec(
                SimVolume::frn(20),
                5 | (1 << 48),
                USN_REASON_FILE_DELETE,
                "d",
                true,
            ),
            &mut st,
        );
        assert_eq!(ix.len(), 1);
        // Record with unknown parent: ignored (never panics).
        apply_record(
            &mut ix,
            v,
            &Rules::default(),
            &rec(40, 999, USN_REASON_FILE_CREATE, "x", false),
            &mut st,
        );
        assert_eq!(ix.len(), 1);
        // Moving a known file below an excluded folder removes it.
        let rules = Rules::new(&["secret".into()], &[]);
        apply_record(
            &mut ix,
            v,
            &rules,
            &rec(41, 5, USN_REASON_FILE_CREATE, "secret", true),
            &mut st,
        );
        apply_record(
            &mut ix,
            v,
            &rules,
            &rec(42, 5, USN_REASON_FILE_CREATE, "y", false),
            &mut st,
        );
        assert_eq!(ix.len(), 2);
        apply_record(
            &mut ix,
            v,
            &rules,
            &rec(42, 41, USN_REASON_RENAME_NEW_NAME, "y", false),
            &mut st,
        );
        assert_eq!(ix.len(), 1);
        // A record trying to move a directory into its own child is ignored.
        apply_record(
            &mut ix,
            v,
            &rules,
            &rec(50, 5, USN_REASON_FILE_CREATE, "p", true),
            &mut st,
        );
        apply_record(
            &mut ix,
            v,
            &rules,
            &rec(51, 50, USN_REASON_FILE_CREATE, "c", true),
            &mut st,
        );
        apply_record(
            &mut ix,
            v,
            &rules,
            &rec(50, 51, USN_REASON_RENAME_NEW_NAME, "p", true),
            &mut st,
        );
        ix.check_invariants().unwrap();
    }

    #[test]
    fn resume_rules() {
        let info = JournalInfo {
            journal_id: 7,
            first_usn: 100,
            next_usn: 500,
        };
        let p = |id, usn| {
            Some(JournalPos {
                journal_id: id,
                next_usn: usn,
            })
        };
        assert_eq!(resume_decision(None, &info), Resume::FullScan);
        assert_eq!(
            resume_decision(p(7, 300), &info),
            Resume::From(p(7, 300).unwrap())
        );
        assert_eq!(
            resume_decision(p(7, 100), &info),
            Resume::From(p(7, 100).unwrap())
        );
        assert_eq!(resume_decision(p(7, 99), &info), Resume::FullScan); // wrapped
        assert_eq!(resume_decision(p(8, 300), &info), Resume::FullScan); // reset
        assert_eq!(resume_decision(p(7, 501), &info), Resume::FullScan); // from the future
    }

    /// Drive `run_ntfs` against a simulated volume on a thread, mutating
    /// the volume concurrently, including a journal wrap and a reset.
    #[test]
    fn source_state_machine_with_wrap_and_reset() {
        use std::sync::{Arc, Mutex};

        struct Shared(Arc<Mutex<SimVolume>>);
        impl UsnBackend for Shared {
            fn query(&mut self) -> Result<JournalInfo, UsnError> {
                self.0.lock().unwrap().query()
            }
            fn enum_chunk(&mut self, s: u64, h: i64) -> Result<Option<(u64, Vec<u8>)>, UsnError> {
                self.0.lock().unwrap().enum_chunk(s, h)
            }
            fn read_journal(&mut self, id: u64, s: i64) -> Result<(i64, Vec<u8>), UsnError> {
                self.0.lock().unwrap().read_journal(id, s)
            }
        }

        let mut sim = SimVolume::new(1);
        sim.capacity = 50;
        sim.create(20, 5, "root-dir", true);
        let vol = Arc::new(Mutex::new(sim));
        let sink = Arc::new(VecSink::default());
        let stop = Arc::new(Stop::new());
        let t = {
            let (vol, sink, stop) = (vol.clone(), sink.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut b = Shared(vol);
                run_ntfs(
                    &mut b,
                    0,
                    "C:\\",
                    None,
                    &Rules::default(),
                    &*sink,
                    &stop,
                    Duration::from_millis(2),
                )
            })
        };
        let wait_live = |n: usize| {
            for _ in 0..2000 {
                let live = sink
                    .states
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|s| s.1 == SourceState::Live)
                    .count();
                if live >= n {
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            panic!("source never became live");
        };
        wait_live(1);
        vol.lock().unwrap().create(21, 20, "file-a", false);
        std::thread::sleep(Duration::from_millis(30));
        // Wrap: write far more records than the journal keeps, faster than
        // the source can read them (the volume lock is held throughout).
        {
            let mut v = vol.lock().unwrap();
            for i in 0..200 {
                v.create(100 + i, 20, &format!("bulk-{i}"), false);
            }
        }
        wait_live(2);
        vol.lock().unwrap().reset_journal(2);
        vol.lock().unwrap().create(400, 20, "after-reset", false);
        wait_live(3);
        std::thread::sleep(Duration::from_millis(30));
        stop.stop();
        t.join().unwrap().unwrap();

        let mut ix = Index::new();
        let v = ix.add_volume("C:\\", VolumeKind::Ntfs).unwrap();
        let changes = std::mem::take(&mut *sink.changes.lock().unwrap());
        let rescans = changes
            .iter()
            .filter(|c| matches!(c, Change::ReplaceTree { .. }))
            .count();
        assert!(rescans >= 3, "initial scan + wrap + reset, got {rescans}");
        apply(&mut ix, &Rules::default(), changes);
        let mut sim = vol.lock().unwrap().clone();
        assert_matches_scan(&ix, v, &mut sim);
        assert_eq!(ix.volume(v).unwrap().journal.unwrap().journal_id, 2);
    }
}
