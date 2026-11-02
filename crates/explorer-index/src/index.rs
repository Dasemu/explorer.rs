// SPDX-License-Identifier: GPL-3.0-or-later
//! The in-memory file-name index.
//!
//! Layout (struct of arrays, one slot per entry id):
//!
//! | column        | bytes | meaning                                        |
//! |---------------|-------|------------------------------------------------|
//! | `flags`       | 1     | live / dir / root / how the original name is kept |
//! | `parent`      | 4     | parent id, [`NONE`] for volume roots           |
//! | `first_child`, `next_sib`, `prev_sib` | 12 | intrusive child list   |
//! | `seg`         | 4     | name segment number in the folded arena        |
//! | `aux`         | 4     | offset in `aux_arena` (case bits or original)  |
//! | `key`         | 4     | NTFS MFT record number, or volume id for roots |
//! | `size`        | 8     | optional column, only allocated when sizes are known |
//!
//! Names are stored once, case-folded, in one contiguous arena (`folded`),
//! each followed by a NUL byte. A substring query is a single `memmem` pass
//! over that arena. The original spelling is recovered from a per-name
//! "case mask" (ASCII names: one bit per byte) or, for non-ASCII names whose
//! lowercase form differs, from a copy of the original in `aux_arena`. Names
//! that are already lowercase cost nothing extra.
//!
//! Ids are stable for the lifetime of an entry and recycled through a free
//! list. Renames and deletions leave dead segments in the arena; the arena is
//! compacted when dead segments outnumber half the live ones.

use std::borrow::Cow;

/// Entry id. Stable while the entry exists.
pub type EntryId = u32;
/// Volume id: index into [`Index::volumes`].
pub type VolId = u16;
/// "No entry" marker.
pub const NONE: EntryId = u32::MAX;

/// Longest accepted name in bytes (NTFS allows 255 UTF-16 units, i.e. at
/// most 765 UTF-8 bytes; Linux allows 255 bytes).
pub const MAX_NAME_BYTES: usize = 1024;

/// Path separator used when building paths.
pub const SEP: char = std::path::MAIN_SEPARATOR;

const F_LIVE: u8 = 1;
const F_DIR: u8 = 2;
const F_ROOT: u8 = 4;
const F_AUX_CASE: u8 = 8;
const F_AUX_ORIG: u8 = 16;
const UNKNOWN_SIZE: u64 = u64::MAX;

/// Compact when at least this many segments are dead and they are more than
/// half the number of live segments.
const COMPACT_MIN_DEAD: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IndexError {
    #[error("invalid file name")]
    InvalidName,
    #[error("no such entry")]
    NoSuchEntry,
    #[error("move would create a cycle")]
    Cycle,
    #[error("index is full")]
    Full,
    #[error("no such volume")]
    NoSuchVolume,
}

/// How a volume is kept up to date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeKind {
    /// Directory walk (+ change notifications where available).
    Walk,
    /// NTFS master file table + USN change journal.
    Ntfs,
}

/// Position in a volume's USN change journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JournalPos {
    pub journal_id: u64,
    pub next_usn: i64,
}

#[derive(Debug, Clone)]
pub struct Volume {
    /// Root path as configured, e.g. `C:\` or `/home/user`.
    pub path: String,
    pub kind: VolumeKind,
    pub root: EntryId,
    /// Where to resume reading the change journal (NTFS only).
    pub journal: Option<JournalPos>,
    /// MFT record number -> entry id (NTFS only). Stale slots are detected
    /// on lookup, so freeing an entry never has to touch this map.
    frn: Vec<u32>,
}

/// A freshly scanned tree, in topological order (parents before children).
/// Produced off-lock by sources, then grafted with [`Index::replace_tree`].
#[derive(Debug, Default, Clone)]
pub struct ScannedTree {
    /// Index into this tree, or [`NONE`] for children of the volume root.
    pub parent: Vec<u32>,
    pub is_dir: Vec<bool>,
    pub key: Vec<u32>,
    pub size: Vec<u64>,
    names: String,
    name_end: Vec<u32>,
}

impl ScannedTree {
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    /// Append an entry; `parent` must be [`NONE`] or a smaller index.
    pub fn push(
        &mut self,
        parent: u32,
        name: &str,
        is_dir: bool,
        size: Option<u64>,
        key: u32,
    ) -> u32 {
        let i = self.parent.len() as u32;
        self.parent.push(parent);
        self.is_dir.push(is_dir);
        self.key.push(key);
        self.size.push(size.unwrap_or(UNKNOWN_SIZE));
        self.names.push_str(name);
        self.name_end.push(self.names.len() as u32);
        i
    }

    pub fn name(&self, i: usize) -> &str {
        let end = self.name_end[i] as usize;
        let start = if i == 0 {
            0
        } else {
            self.name_end[i - 1] as usize
        };
        &self.names[start..end]
    }
}

#[derive(Debug, Default)]
pub struct Index {
    flags: Vec<u8>,
    parent: Vec<u32>,
    first_child: Vec<u32>,
    next_sib: Vec<u32>,
    prev_sib: Vec<u32>,
    seg: Vec<u32>,
    aux: Vec<u32>,
    key: Vec<u32>,
    size: Vec<u64>,

    folded: Vec<u8>,
    seg_start: Vec<u32>,
    seg_id: Vec<u32>,
    aux_arena: Vec<u8>,

    free: Vec<u32>,
    live: usize,
    dead_segs: usize,
    volumes: Vec<Option<Volume>>,
    generation: u64,
}

/// Validate a single path component.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name != "."
        && name != ".."
        && !name.bytes().any(|b| b == 0 || b == b'/' || b == b'\\')
}

/// Case-fold `s` the same way names are folded in the arena.
pub fn fold(s: &str) -> String {
    if s.is_ascii() {
        s.to_ascii_lowercase()
    } else {
        s.to_lowercase()
    }
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live entries, volume roots included.
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// One past the largest id ever handed out.
    pub fn id_bound(&self) -> usize {
        self.flags.len()
    }

    /// Bumped on every mutation; used for "dirty since last snapshot".
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn has_sizes(&self) -> bool {
        !self.size.is_empty()
    }

    // ----- volumes ---------------------------------------------------------

    /// Create a volume and its root entry. The root's name is the path with
    /// trailing separators removed (`C:\` -> `C:`), so that joining children
    /// with [`SEP`] yields normal paths.
    pub fn add_volume(&mut self, path: &str, kind: VolumeKind) -> Result<VolId, IndexError> {
        let slot = match self.volumes.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                if self.volumes.len() >= u16::MAX as usize {
                    return Err(IndexError::Full);
                }
                self.volumes.push(None);
                self.volumes.len() - 1
            }
        };
        let root_name = path.trim_end_matches(['/', '\\']);
        if root_name.len() > MAX_NAME_BYTES || root_name.contains('\0') {
            return Err(IndexError::InvalidName);
        }
        let id = self.alloc()?;
        self.flags[id as usize] = F_LIVE | F_DIR | F_ROOT;
        self.parent[id as usize] = NONE;
        self.key[id as usize] = slot as u32;
        self.push_name(id, root_name)?;
        self.live += 1;
        self.volumes[slot] = Some(Volume {
            path: path.to_string(),
            kind,
            root: id,
            journal: None,
            frn: Vec::new(),
        });
        self.touch();
        Ok(slot as VolId)
    }

    pub fn volume(&self, vol: VolId) -> Option<&Volume> {
        self.volumes.get(vol as usize).and_then(Option::as_ref)
    }

    pub fn volumes(&self) -> impl Iterator<Item = (VolId, &Volume)> {
        self.volumes
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_ref().map(|v| (i as VolId, v)))
    }

    pub fn find_volume(&self, path: &str) -> Option<VolId> {
        let want = path.trim_end_matches(['/', '\\']);
        self.volumes()
            .find(|(_, v)| {
                v.path
                    .trim_end_matches(['/', '\\'])
                    .eq_ignore_ascii_case(want)
            })
            .map(|(i, _)| i)
    }

    pub fn set_journal(&mut self, vol: VolId, pos: Option<JournalPos>) {
        if let Some(Some(v)) = self.volumes.get_mut(vol as usize) {
            if v.journal != pos {
                v.journal = pos;
                self.touch();
            }
        }
    }

    pub fn set_volume_kind(&mut self, vol: VolId, kind: VolumeKind) {
        if let Some(Some(v)) = self.volumes.get_mut(vol as usize) {
            v.kind = kind;
        }
    }

    pub fn remove_volume(&mut self, vol: VolId) -> Result<(), IndexError> {
        let root = self.volume(vol).ok_or(IndexError::NoSuchVolume)?.root;
        self.free_subtree(root);
        self.volumes[vol as usize] = None;
        self.maybe_compact();
        self.touch();
        Ok(())
    }

    /// Number of entries below (and including) the volume root.
    pub fn volume_len(&self, vol: VolId) -> usize {
        self.volume(vol)
            .map(|v| self.subtree(v.root).count())
            .unwrap_or(0)
    }

    /// Replace everything below the volume root with `tree`. Keys of NTFS
    /// entries are registered in the record-number map.
    pub fn replace_tree(&mut self, vol: VolId, tree: &ScannedTree) -> Result<(), IndexError> {
        let root = self.volume(vol).ok_or(IndexError::NoSuchVolume)?.root;
        while let Some(c) = self
            .first_child
            .get(root as usize)
            .copied()
            .filter(|&c| c != NONE)
        {
            self.unlink(c);
            self.free_subtree(c);
        }
        let ntfs = self.volume(vol).is_some_and(|v| v.kind == VolumeKind::Ntfs);
        let mut frn = Vec::new();
        let mut map: Vec<u32> = Vec::with_capacity(tree.len());
        for i in 0..tree.len() {
            let p = tree.parent[i];
            let parent = if p == NONE {
                root
            } else {
                match map.get(p as usize) {
                    Some(&id) if id != NONE => id,
                    _ => {
                        map.push(NONE);
                        continue;
                    }
                }
            };
            let size = (tree.size[i] != UNKNOWN_SIZE).then_some(tree.size[i]);
            match self.insert(parent, tree.name(i), tree.is_dir[i], size, tree.key[i]) {
                Ok(id) => {
                    let k = tree.key[i];
                    if ntfs && k != NONE {
                        set_frn(&mut frn, k, id);
                    }
                    map.push(id);
                }
                Err(_) => map.push(NONE),
            }
        }
        if let Some(Some(v)) = self.volumes.get_mut(vol as usize) {
            v.frn = frn;
            v.frn.shrink_to_fit();
        }
        self.maybe_compact();
        self.touch();
        Ok(())
    }

    // ----- entries ---------------------------------------------------------

    pub fn is_live(&self, id: EntryId) -> bool {
        self.flags.get(id as usize).is_some_and(|f| f & F_LIVE != 0)
    }

    pub fn is_dir(&self, id: EntryId) -> bool {
        self.flags.get(id as usize).is_some_and(|f| f & F_DIR != 0)
    }

    pub fn is_root(&self, id: EntryId) -> bool {
        self.flags.get(id as usize).is_some_and(|f| f & F_ROOT != 0)
    }

    pub fn parent(&self, id: EntryId) -> EntryId {
        self.parent.get(id as usize).copied().unwrap_or(NONE)
    }

    pub fn key(&self, id: EntryId) -> u32 {
        self.key.get(id as usize).copied().unwrap_or(NONE)
    }

    pub fn size(&self, id: EntryId) -> Option<u64> {
        self.size
            .get(id as usize)
            .copied()
            .filter(|&s| s != UNKNOWN_SIZE)
    }

    pub fn set_size(&mut self, id: EntryId, size: Option<u64>) {
        if !self.is_live(id) {
            return;
        }
        let v = size.unwrap_or(UNKNOWN_SIZE);
        if self.size.is_empty() {
            if v == UNKNOWN_SIZE {
                return;
            }
            self.size = vec![UNKNOWN_SIZE; self.flags.len()];
        }
        self.size[id as usize] = v;
    }

    /// Insert a new entry below `parent`.
    pub fn insert(
        &mut self,
        parent: EntryId,
        name: &str,
        is_dir: bool,
        size: Option<u64>,
        key: u32,
    ) -> Result<EntryId, IndexError> {
        if !self.is_live(parent) {
            return Err(IndexError::NoSuchEntry);
        }
        if !valid_name(name) {
            return Err(IndexError::InvalidName);
        }
        let id = self.alloc()?;
        let i = id as usize;
        self.flags[i] = F_LIVE | if is_dir { F_DIR } else { 0 };
        self.key[i] = key;
        if let Err(e) = self.push_name(id, name) {
            self.flags[i] = 0;
            self.free.push(id);
            return Err(e);
        }
        self.link(id, parent);
        self.live += 1;
        self.set_size(id, size);
        self.touch();
        Ok(id)
    }

    /// Remove `id` and everything below it. Roots are removed with
    /// [`Index::remove_volume`].
    pub fn remove(&mut self, id: EntryId) -> Result<usize, IndexError> {
        if !self.is_live(id) || self.is_root(id) {
            return Err(IndexError::NoSuchEntry);
        }
        self.unlink(id);
        let n = self.free_subtree(id);
        self.maybe_compact();
        self.touch();
        Ok(n)
    }

    /// Rename and/or move `id` below `new_parent`.
    pub fn rename(
        &mut self,
        id: EntryId,
        new_parent: EntryId,
        new_name: &str,
    ) -> Result<(), IndexError> {
        if !self.is_live(id) || self.is_root(id) || !self.is_live(new_parent) {
            return Err(IndexError::NoSuchEntry);
        }
        if !valid_name(new_name) {
            return Err(IndexError::InvalidName);
        }
        // Refuse to move a directory into its own subtree.
        let mut a = new_parent;
        while a != NONE {
            if a == id {
                return Err(IndexError::Cycle);
            }
            a = self.parent[a as usize];
        }
        if self.parent[id as usize] != new_parent {
            self.unlink(id);
            self.link(id, new_parent);
        }
        if self.name(id) != new_name {
            self.retire_name(id);
            self.push_name(id, new_name)?;
            self.maybe_compact();
        }
        self.touch();
        Ok(())
    }

    pub fn set_dir(&mut self, id: EntryId, is_dir: bool) {
        if let Some(f) = self.flags.get_mut(id as usize) {
            if *f & F_LIVE != 0 && *f & F_ROOT == 0 {
                if is_dir {
                    *f |= F_DIR;
                } else {
                    *f &= !F_DIR;
                }
            }
        }
    }

    // ----- names and paths -------------------------------------------------

    /// Folded (lowercase) name bytes, without the trailing NUL.
    pub fn folded_name(&self, id: EntryId) -> &[u8] {
        let Some(&s) = self.seg.get(id as usize) else {
            return &[];
        };
        let (a, b) = self.seg_range(s as usize);
        &self.folded[a..b]
    }

    fn seg_range(&self, s: usize) -> (usize, usize) {
        let a = self.seg_start[s] as usize;
        let b = match self.seg_start.get(s + 1) {
            Some(&n) => n as usize - 1,
            None => self.folded.len() - 1,
        };
        (a, b)
    }

    /// Original spelling of the name.
    pub fn name(&self, id: EntryId) -> Cow<'_, str> {
        let folded = self.folded_name(id);
        let flags = self.flags.get(id as usize).copied().unwrap_or(0);
        let aux = self.aux.get(id as usize).copied().unwrap_or(NONE) as usize;
        if flags & F_AUX_CASE != 0 {
            let mask = &self.aux_arena[aux..aux + folded.len().div_ceil(8)];
            let mut v = folded.to_vec();
            for (i, b) in v.iter_mut().enumerate() {
                if mask[i / 8] & (1 << (i % 8)) != 0 {
                    b.make_ascii_uppercase();
                }
            }
            // ASCII in, ASCII out: always valid UTF-8.
            Cow::Owned(String::from_utf8(v).unwrap_or_default())
        } else if flags & F_AUX_ORIG != 0 {
            let len = u16::from_le_bytes([self.aux_arena[aux], self.aux_arena[aux + 1]]) as usize;
            String::from_utf8_lossy(&self.aux_arena[aux + 2..aux + 2 + len])
        } else {
            String::from_utf8_lossy(folded)
        }
    }

    /// Full path of `id`.
    pub fn path(&self, id: EntryId) -> String {
        let mut out = String::new();
        self.write_path(id, &mut out);
        out
    }

    /// Append the full path of `id` to `out`.
    pub fn write_path(&self, id: EntryId, out: &mut String) {
        if !self.is_live(id) {
            return;
        }
        if self.is_root(id) {
            if let Some(v) = self.volume(self.key[id as usize] as VolId) {
                out.push_str(&v.path);
                return;
            }
        }
        let mut chain = Vec::with_capacity(16);
        let mut a = id;
        while a != NONE {
            chain.push(a);
            a = self.parent[a as usize];
        }
        for (n, &c) in chain.iter().rev().enumerate() {
            if n > 0 {
                out.push(SEP);
            }
            out.push_str(&self.name(c));
        }
    }

    /// Volume that contains `id`.
    pub fn volume_of(&self, id: EntryId) -> Option<VolId> {
        if !self.is_live(id) {
            return None;
        }
        let mut a = id;
        loop {
            let p = self.parent[a as usize];
            if p == NONE {
                return self.is_root(a).then(|| self.key[a as usize] as VolId);
            }
            a = p;
        }
    }

    /// Depth below the volume root (root = 0).
    pub fn depth(&self, id: EntryId) -> usize {
        let mut d = 0;
        let mut a = self.parent(id);
        while a != NONE {
            d += 1;
            a = self.parent[a as usize];
        }
        d
    }

    pub fn children(&self, id: EntryId) -> Children<'_> {
        Children {
            index: self,
            next: self.first_child.get(id as usize).copied().unwrap_or(NONE),
        }
    }

    /// `id` and all its descendants (pre-order).
    pub fn subtree(&self, id: EntryId) -> Subtree<'_> {
        Subtree {
            index: self,
            stack: if self.is_live(id) { vec![id] } else { vec![] },
        }
    }

    /// Child of `parent` whose name equals `name` case-insensitively.
    pub fn child_by_name(&self, parent: EntryId, name: &str) -> Option<EntryId> {
        let f = fold(name);
        self.children(parent)
            .find(|&c| self.folded_name(c) == f.as_bytes())
    }

    /// Resolve components relative to a volume root.
    pub fn lookup<'a>(
        &self,
        vol: VolId,
        components: impl IntoIterator<Item = &'a str>,
    ) -> Option<EntryId> {
        let mut cur = self.volume(vol)?.root;
        for c in components {
            if c.is_empty() || c == "." {
                continue;
            }
            cur = self.child_by_name(cur, c)?;
        }
        Some(cur)
    }

    /// Live ids, in id order.
    pub fn live_ids(&self) -> impl Iterator<Item = EntryId> + '_ {
        self.flags
            .iter()
            .enumerate()
            .filter(|(_, &f)| f & F_LIVE != 0)
            .map(|(i, _)| i as EntryId)
    }

    // ----- NTFS record-number map -----------------------------------------

    /// Entry of MFT record `rec` on volume `vol`, if indexed.
    pub fn ntfs_lookup(&self, vol: VolId, rec: u32) -> Option<EntryId> {
        let v = self.volume(vol)?;
        if rec == NONE {
            return None;
        }
        if self.is_root(v.root) && rec == ROOT_RECORD {
            return Some(v.root);
        }
        let id = *v.frn.get(rec as usize)?;
        if id == NONE || !self.is_live(id) || self.is_root(id) || self.key[id as usize] != rec {
            return None;
        }
        (self.volume_of(id) == Some(vol)).then_some(id)
    }

    pub fn ntfs_map(&mut self, vol: VolId, rec: u32, id: EntryId) {
        if let Some(Some(v)) = self.volumes.get_mut(vol as usize) {
            set_frn(&mut v.frn, rec, id);
        }
    }

    pub fn set_key(&mut self, id: EntryId, key: u32) {
        if self.is_live(id) && !self.is_root(id) {
            self.key[id as usize] = key;
        }
    }

    // ----- raw access for the query engine --------------------------------

    pub(crate) fn arena(&self) -> &[u8] {
        &self.folded
    }

    /// Segment containing arena offset `pos`, searching from `hint`.
    pub(crate) fn seg_at(&self, pos: usize, hint: usize) -> usize {
        let starts = &self.seg_start;
        let mut s = hint.min(starts.len().saturating_sub(1));
        if starts[s] as usize > pos {
            return starts.partition_point(|&st| st as usize <= pos) - 1;
        }
        for _ in 0..8 {
            match starts.get(s + 1) {
                Some(&n) if n as usize <= pos => s += 1,
                _ => return s,
            }
        }
        s + starts[s..].partition_point(|&st| st as usize <= pos) - 1
    }

    pub(crate) fn seg_bounds(&self, s: usize) -> (usize, usize) {
        self.seg_range(s)
    }

    pub(crate) fn seg_owner(&self, s: usize) -> EntryId {
        self.seg_id[s]
    }

    /// Exact heap usage of the index structures in bytes.
    pub fn memory_usage(&self) -> usize {
        use std::mem::size_of;
        self.flags.capacity()
            + 4 * (self.parent.capacity()
                + self.first_child.capacity()
                + self.next_sib.capacity()
                + self.prev_sib.capacity()
                + self.seg.capacity()
                + self.aux.capacity()
                + self.key.capacity()
                + self.seg_start.capacity()
                + self.seg_id.capacity()
                + self.free.capacity())
            + 8 * self.size.capacity()
            + self.folded.capacity()
            + self.aux_arena.capacity()
            + self.volumes.capacity() * size_of::<Option<Volume>>()
            + self
                .volumes()
                .map(|(_, v)| v.frn.capacity() * 4 + v.path.capacity())
                .sum::<usize>()
    }

    /// Release spare capacity (call after bulk loads).
    pub fn shrink_to_fit(&mut self) {
        self.flags.shrink_to_fit();
        self.parent.shrink_to_fit();
        self.first_child.shrink_to_fit();
        self.next_sib.shrink_to_fit();
        self.prev_sib.shrink_to_fit();
        self.seg.shrink_to_fit();
        self.aux.shrink_to_fit();
        self.key.shrink_to_fit();
        self.size.shrink_to_fit();
        self.folded.shrink_to_fit();
        self.seg_start.shrink_to_fit();
        self.seg_id.shrink_to_fit();
        self.aux_arena.shrink_to_fit();
        self.free.shrink_to_fit();
    }

    /// Rebuild the name arenas without dead segments. Ids do not change.
    pub fn compact(&mut self) {
        let mut folded = Vec::with_capacity(self.folded.len() - self.dead_bytes_estimate());
        let mut seg_start = Vec::with_capacity(self.live);
        let mut seg_id = Vec::with_capacity(self.live);
        let mut aux_arena = Vec::with_capacity(self.aux_arena.len() / 2);
        for id in 0..self.flags.len() {
            let f = self.flags[id];
            if f & F_LIVE == 0 {
                continue;
            }
            let (a, b) = self.seg_range(self.seg[id] as usize);
            seg_start.push(folded.len() as u32);
            seg_id.push(id as u32);
            folded.extend_from_slice(&self.folded[a..=b]);
            self.seg[id] = (seg_start.len() - 1) as u32;
            let old = self.aux[id] as usize;
            if f & F_AUX_CASE != 0 {
                let n = (b - a).div_ceil(8);
                self.aux[id] = aux_arena.len() as u32;
                aux_arena.extend_from_slice(&self.aux_arena[old..old + n]);
            } else if f & F_AUX_ORIG != 0 {
                let n = u16::from_le_bytes([self.aux_arena[old], self.aux_arena[old + 1]]) as usize;
                self.aux[id] = aux_arena.len() as u32;
                aux_arena.extend_from_slice(&self.aux_arena[old..old + 2 + n]);
            }
        }
        self.folded = folded;
        self.seg_start = seg_start;
        self.seg_id = seg_id;
        self.aux_arena = aux_arena;
        self.dead_segs = 0;
    }

    fn dead_bytes_estimate(&self) -> usize {
        if self.seg_id.is_empty() {
            return 0;
        }
        self.folded.len() * self.dead_segs / self.seg_id.len()
    }

    // ----- internals -------------------------------------------------------

    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn alloc(&mut self) -> Result<EntryId, IndexError> {
        if let Some(id) = self.free.pop() {
            let i = id as usize;
            self.parent[i] = NONE;
            self.first_child[i] = NONE;
            self.next_sib[i] = NONE;
            self.prev_sib[i] = NONE;
            self.aux[i] = NONE;
            if !self.size.is_empty() {
                self.size[i] = UNKNOWN_SIZE;
            }
            return Ok(id);
        }
        let id = self.flags.len();
        if id >= (NONE - 1) as usize {
            return Err(IndexError::Full);
        }
        self.flags.push(0);
        self.parent.push(NONE);
        self.first_child.push(NONE);
        self.next_sib.push(NONE);
        self.prev_sib.push(NONE);
        self.seg.push(NONE);
        self.aux.push(NONE);
        self.key.push(NONE);
        if !self.size.is_empty() {
            self.size.push(UNKNOWN_SIZE);
        }
        Ok(id as EntryId)
    }

    fn push_name(&mut self, id: EntryId, name: &str) -> Result<(), IndexError> {
        let start = self.folded.len();
        if start + name.len() * 3 + 1 >= u32::MAX as usize {
            return Err(IndexError::Full);
        }
        let i = id as usize;
        self.flags[i] &= !(F_AUX_CASE | F_AUX_ORIG);
        self.aux[i] = NONE;
        if name.is_ascii() {
            let bytes = name.as_bytes();
            let mut has_upper = false;
            for &b in bytes {
                has_upper |= b.is_ascii_uppercase();
                self.folded.push(b.to_ascii_lowercase());
            }
            if has_upper {
                self.aux[i] = self.aux_arena.len() as u32;
                self.flags[i] |= F_AUX_CASE;
                let base = self.aux_arena.len();
                self.aux_arena.resize(base + bytes.len().div_ceil(8), 0);
                for (k, &b) in bytes.iter().enumerate() {
                    if b.is_ascii_uppercase() {
                        self.aux_arena[base + k / 8] |= 1 << (k % 8);
                    }
                }
            }
        } else {
            let lower = name.to_lowercase();
            self.folded.extend_from_slice(lower.as_bytes());
            if lower != name {
                self.aux[i] = self.aux_arena.len() as u32;
                self.flags[i] |= F_AUX_ORIG;
                self.aux_arena
                    .extend_from_slice(&(name.len() as u16).to_le_bytes());
                self.aux_arena.extend_from_slice(name.as_bytes());
            }
        }
        self.folded.push(0);
        self.seg_start.push(start as u32);
        self.seg_id.push(id);
        self.seg[i] = (self.seg_start.len() - 1) as u32;
        Ok(())
    }

    fn retire_name(&mut self, id: EntryId) {
        let s = self.seg[id as usize];
        if s != NONE {
            self.seg_id[s as usize] = NONE;
            self.dead_segs += 1;
        }
    }

    fn link(&mut self, id: EntryId, parent: EntryId) {
        let (i, p) = (id as usize, parent as usize);
        let head = self.first_child[p];
        self.parent[i] = parent;
        self.prev_sib[i] = NONE;
        self.next_sib[i] = head;
        if head != NONE {
            self.prev_sib[head as usize] = id;
        }
        self.first_child[p] = id;
    }

    fn unlink(&mut self, id: EntryId) {
        let i = id as usize;
        let (p, prev, next) = (self.parent[i], self.prev_sib[i], self.next_sib[i]);
        if prev != NONE {
            self.next_sib[prev as usize] = next;
        } else if p != NONE {
            self.first_child[p as usize] = next;
        }
        if next != NONE {
            self.prev_sib[next as usize] = prev;
        }
        self.parent[i] = NONE;
        self.prev_sib[i] = NONE;
        self.next_sib[i] = NONE;
    }

    /// Free `id` and all descendants. `id` must already be unlinked (or be a
    /// root). Returns the number of entries freed.
    fn free_subtree(&mut self, id: EntryId) -> usize {
        let mut stack = vec![id];
        let mut n = 0;
        while let Some(x) = stack.pop() {
            let mut c = self.first_child[x as usize];
            while c != NONE {
                stack.push(c);
                c = self.next_sib[c as usize];
            }
            self.retire_name(x);
            let i = x as usize;
            self.flags[i] = 0;
            self.first_child[i] = NONE;
            self.seg[i] = NONE;
            self.key[i] = NONE;
            self.free.push(x);
            n += 1;
        }
        self.live -= n.min(self.live);
        n
    }

    fn maybe_compact(&mut self) {
        // Compact once dead segments exceed half the live ones.
        if self.dead_segs >= COMPACT_MIN_DEAD
            && self.dead_segs * 2 > self.seg_id.len() - self.dead_segs
        {
            self.compact();
        }
    }

    /// Consistency check used by tests and after loading untrusted data.
    pub fn check_invariants(&self) -> Result<(), String> {
        let mut live = 0;
        for id in 0..self.flags.len() as u32 {
            if !self.is_live(id) {
                continue;
            }
            live += 1;
            let p = self.parent(id);
            if self.is_root(id) != (p == NONE) {
                return Err(format!("{id}: root flag / parent mismatch"));
            }
            if p != NONE && !self.children(p).any(|c| c == id) {
                return Err(format!("{id}: not in parent's child list"));
            }
            if self.seg_id[self.seg[id as usize] as usize] != id {
                return Err(format!("{id}: segment owner mismatch"));
            }
            if self.volume_of(id).is_none() {
                return Err(format!("{id}: not under a volume root"));
            }
        }
        if live != self.live {
            return Err(format!("live count {} != {}", self.live, live));
        }
        Ok(())
    }
}

/// MFT record number of the root directory of an NTFS volume.
pub const ROOT_RECORD: u32 = 5;

fn set_frn(map: &mut Vec<u32>, rec: u32, id: EntryId) {
    let r = rec as usize;
    // Guard against absurd record numbers from corrupt input: the map is
    // dense, so cap it (64M records = 256 MiB).
    if r >= 64 << 20 {
        return;
    }
    if map.len() <= r {
        map.resize(r + 1, NONE);
    }
    map[r] = id;
}

pub struct Children<'a> {
    index: &'a Index,
    next: EntryId,
}

impl Iterator for Children<'_> {
    type Item = EntryId;
    fn next(&mut self) -> Option<EntryId> {
        if self.next == NONE {
            return None;
        }
        let c = self.next;
        self.next = self.index.next_sib[c as usize];
        Some(c)
    }
}

pub struct Subtree<'a> {
    index: &'a Index,
    stack: Vec<EntryId>,
}

impl Iterator for Subtree<'_> {
    type Item = EntryId;
    fn next(&mut self) -> Option<EntryId> {
        let x = self.stack.pop()?;
        self.stack.extend(self.index.children(x));
        Some(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(parts: &[&str]) -> String {
        parts.join(&SEP.to_string())
    }

    #[test]
    fn insert_names_and_paths() {
        let mut ix = Index::new();
        let v = ix.add_volume("C:\\", VolumeKind::Walk).unwrap();
        let root = ix.volume(v).unwrap().root;
        let users = ix.insert(root, "Users", true, None, NONE).unwrap();
        let f = ix
            .insert(users, "ReadMe.TXT", false, Some(42), NONE)
            .unwrap();
        let g = ix
            .insert(users, "Ünïcødé Straße.md", false, None, NONE)
            .unwrap();
        let h = ix.insert(users, "lower", false, None, NONE).unwrap();
        assert_eq!(ix.name(f), "ReadMe.TXT");
        assert_eq!(ix.folded_name(f), b"readme.txt");
        assert_eq!(ix.name(g), "Ünïcødé Straße.md");
        assert_eq!(ix.name(h), "lower");
        assert_eq!(ix.path(f), p(&["C:", "Users", "ReadMe.TXT"]));
        assert_eq!(ix.path(root), "C:\\");
        assert_eq!(ix.size(f), Some(42));
        assert_eq!(ix.size(g), None);
        assert_eq!(ix.child_by_name(users, "README.txt"), Some(f));
        assert_eq!(ix.lookup(v, ["users", "LOWER"]), Some(h));
        assert_eq!(ix.len(), 5);
        ix.check_invariants().unwrap();
    }

    #[test]
    fn invalid_names_rejected() {
        let mut ix = Index::new();
        let v = ix.add_volume("/r", VolumeKind::Walk).unwrap();
        let root = ix.volume(v).unwrap().root;
        for bad in ["", ".", "..", "a/b", "a\\b", "nul\0"] {
            assert_eq!(
                ix.insert(root, bad, false, None, NONE),
                Err(IndexError::InvalidName)
            );
        }
        let long = "x".repeat(MAX_NAME_BYTES + 1);
        assert!(ix.insert(root, &long, false, None, NONE).is_err());
        ix.check_invariants().unwrap();
    }

    #[test]
    fn rename_move_and_cycle() {
        let mut ix = Index::new();
        let v = ix.add_volume("/r", VolumeKind::Walk).unwrap();
        let root = ix.volume(v).unwrap().root;
        let a = ix.insert(root, "a", true, None, NONE).unwrap();
        let b = ix.insert(a, "b", true, None, NONE).unwrap();
        let f = ix.insert(b, "f.txt", false, None, NONE).unwrap();
        assert_eq!(ix.rename(a, b, "a"), Err(IndexError::Cycle));
        assert_eq!(ix.rename(a, a, "a"), Err(IndexError::Cycle));
        ix.rename(b, root, "B2").unwrap();
        assert_eq!(ix.path(f), p(&["/r", "B2", "f.txt"]));
        ix.rename(f, a, "g.TXT").unwrap();
        assert_eq!(ix.path(f), p(&["/r", "a", "g.TXT"]));
        assert_eq!(ix.children(b).count(), 0);
        ix.check_invariants().unwrap();
    }

    #[test]
    fn remove_subtree_and_reuse() {
        let mut ix = Index::new();
        let v = ix.add_volume("/r", VolumeKind::Walk).unwrap();
        let root = ix.volume(v).unwrap().root;
        let a = ix.insert(root, "a", true, None, NONE).unwrap();
        for i in 0..10 {
            let d = ix.insert(a, &format!("d{i}"), true, None, NONE).unwrap();
            ix.insert(d, "x", false, None, NONE).unwrap();
        }
        let keep = ix.insert(root, "keep", false, None, NONE).unwrap();
        assert_eq!(ix.remove(a).unwrap(), 21);
        assert_eq!(ix.len(), 2);
        assert!(!ix.is_live(a));
        let n = ix.insert(root, "new", false, None, NONE).unwrap();
        assert!(n < 22, "freed ids are reused");
        assert_eq!(ix.name(keep), "keep");
        ix.check_invariants().unwrap();
        assert!(ix.remove(root).is_err());
    }

    #[test]
    fn compaction_keeps_names() {
        let mut ix = Index::new();
        let v = ix.add_volume("/r", VolumeKind::Walk).unwrap();
        let root = ix.volume(v).unwrap().root;
        let ids: Vec<_> = (0..10_000)
            .map(|i| {
                ix.insert(root, &format!("File{i}"), false, None, NONE)
                    .unwrap()
            })
            .collect();
        for (i, &id) in ids.iter().enumerate() {
            ix.rename(id, root, &format!("Renamed-{i}.Ünï")).unwrap();
        }
        assert!(ix.dead_segs < 10_000, "compaction ran");
        for (i, &id) in ids.iter().enumerate().step_by(997) {
            assert_eq!(ix.name(id), format!("Renamed-{i}.Ünï"));
        }
        ix.compact();
        assert_eq!(ix.name(ids[5]), "Renamed-5.Ünï");
        ix.check_invariants().unwrap();
    }

    #[test]
    fn replace_tree_and_ntfs_map() {
        let mut ix = Index::new();
        let v = ix.add_volume("C:\\", VolumeKind::Ntfs).unwrap();
        let mut t = ScannedTree::default();
        let d = t.push(NONE, "Dir", true, None, 40);
        t.push(d, "file.rs", false, None, 41);
        t.push(77, "orphan-bad-parent", false, None, 42);
        ix.replace_tree(v, &t).unwrap();
        let f = ix.ntfs_lookup(v, 41).unwrap();
        assert_eq!(ix.path(f), p(&["C:", "Dir", "file.rs"]));
        assert_eq!(
            ix.ntfs_lookup(v, ROOT_RECORD),
            Some(ix.volume(v).unwrap().root)
        );
        assert_eq!(ix.ntfs_lookup(v, 42), None);
        // Replace again: old entries disappear, map is rebuilt.
        let mut t2 = ScannedTree::default();
        t2.push(NONE, "Other", false, None, 50);
        ix.replace_tree(v, &t2).unwrap();
        assert_eq!(ix.ntfs_lookup(v, 41), None);
        assert_eq!(ix.len(), 2);
        ix.check_invariants().unwrap();
        ix.remove_volume(v).unwrap();
        assert_eq!(ix.len(), 0);
    }
}
