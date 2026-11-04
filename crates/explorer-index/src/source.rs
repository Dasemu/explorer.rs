// SPDX-License-Identifier: GPL-3.0-or-later
//! Change events emitted by sources and applied to the index.
//!
//! Every source (NTFS MFT/USN, directory walk, `ReadDirectoryChangesW`)
//! runs on its own thread and emits batches of [`Change`]s through a
//! [`ChangeSink`]. The daemon's sink applies them under the index write
//! lock with [`apply`]. Sources never touch the index directly, which keeps
//! lock hold times short and makes every source testable against a plain
//! `Vec<Change>`.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::index::{fold, EntryId, Index, IndexError, JournalPos, ScannedTree, VolId, SEP};
use crate::rules::Rules;
use crate::usn::{self, UsnRecord};

#[derive(Debug, Clone)]
pub enum Change {
    /// Replace everything below the volume root.
    ReplaceTree { vol: VolId, tree: ScannedTree },
    /// Replay NTFS change-journal records.
    Usn { vol: VolId, records: Vec<UsnRecord> },
    /// Create or update the entry at `path` (components below the root).
    Upsert {
        vol: VolId,
        path: Vec<String>,
        is_dir: bool,
        size: Option<u64>,
    },
    /// Remove the entry at `path` and everything below it.
    Remove { vol: VolId, path: Vec<String> },
    /// Rename/move; `is_dir`/`size` describe the destination as stat'ed.
    Rename {
        vol: VolId,
        from: Vec<String>,
        to: Vec<String>,
        is_dir: bool,
        size: Option<u64>,
    },
    /// Remember the journal position reached (persisted in snapshots).
    Journal { vol: VolId, pos: Option<JournalPos> },
}

/// What a source is doing, for `Status` replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceState {
    Starting,
    Scanning,
    /// Up to date and following changes.
    Live,
    /// Up to date as of the last scan; no live change notifications.
    Polling,
    Failed(String),
    Stopped,
}

impl SourceState {
    pub fn label(&self) -> String {
        match self {
            Self::Starting => "starting".into(),
            Self::Scanning => "scanning".into(),
            Self::Live => "live".into(),
            Self::Polling => "polling".into(),
            Self::Failed(e) => format!("failed: {e}"),
            Self::Stopped => "stopped".into(),
        }
    }
}

/// Receiver of change batches.
pub trait ChangeSink: Send + Sync {
    fn apply(&self, changes: Vec<Change>);
    fn set_state(&self, vol: VolId, state: SourceState);
}

/// Test sink that records everything.
#[derive(Default)]
pub struct VecSink {
    pub changes: Mutex<Vec<Change>>,
    pub states: Mutex<Vec<(VolId, SourceState)>>,
}

impl ChangeSink for VecSink {
    fn apply(&self, changes: Vec<Change>) {
        self.changes.lock().unwrap().extend(changes);
    }
    fn set_state(&self, vol: VolId, state: SourceState) {
        self.states.lock().unwrap().push((vol, state));
    }
}

/// Cooperative stop signal with interruptible sleeps.
#[derive(Default)]
pub struct Stop {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Stop {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stop(&self) {
        *self.flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        *self.flag.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sleep up to `d`; returns true if stop was requested.
    pub fn sleep(&self, d: Duration) -> bool {
        let g = self.flag.lock().unwrap_or_else(|e| e.into_inner());
        if *g {
            return true;
        }
        let (g, _) = self
            .cv
            .wait_timeout(g, d)
            .unwrap_or_else(|e| e.into_inner());
        *g
    }
}

/// Counters returned by [`apply`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ApplyStats {
    pub inserted: usize,
    pub removed: usize,
    pub renamed: usize,
    pub ignored: usize,
}

fn resolve(ix: &Index, vol: VolId, path: &[String]) -> Option<EntryId> {
    ix.lookup(vol, path.iter().map(String::as_str))
}

fn excluded(ix: &Index, rules: &Rules, parent: EntryId, name: &str) -> bool {
    if rules.excludes_name(name) {
        return true;
    }
    if rules.has_path_rules() {
        let mut p = ix.path(parent);
        if !p.ends_with(['/', '\\']) {
            p.push(SEP);
        }
        p.push_str(name);
        return rules.excludes_path(&p);
    }
    false
}

/// Apply a batch of changes to the index.
pub fn apply(ix: &mut Index, rules: &Rules, changes: Vec<Change>) -> ApplyStats {
    let mut st = ApplyStats::default();
    for c in changes {
        match c {
            Change::ReplaceTree { vol, tree } => {
                if ix.replace_tree(vol, &tree).is_ok() {
                    st.inserted += tree.len();
                }
            }
            Change::Usn { vol, records } => {
                for r in &records {
                    usn::apply_record(ix, vol, rules, r, &mut st);
                }
            }
            Change::Upsert {
                vol,
                path,
                is_dir,
                size,
            } => upsert(ix, rules, vol, &path, is_dir, size, &mut st),
            Change::Remove { vol, path } => match resolve(ix, vol, &path) {
                Some(id) if !path.is_empty() => {
                    st.removed += ix.remove(id).unwrap_or(0);
                }
                _ => st.ignored += 1,
            },
            Change::Rename {
                vol,
                from,
                to,
                is_dir,
                size,
            } => {
                let src = if from.is_empty() {
                    None
                } else {
                    resolve(ix, vol, &from)
                };
                let Some((name, dir)) = to.split_last() else {
                    st.ignored += 1;
                    continue;
                };
                let dst_parent = resolve(ix, vol, dir);
                match (src, dst_parent) {
                    (Some(id), Some(p)) if !excluded(ix, rules, p, name) => {
                        match ix.rename(id, p, name) {
                            Ok(()) => {
                                ix.set_dir(id, is_dir);
                                ix.set_size(id, size);
                                st.renamed += 1;
                            }
                            Err(IndexError::Cycle) | Err(_) => st.ignored += 1,
                        }
                    }
                    (Some(id), _) => st.removed += ix.remove(id).unwrap_or(0),
                    (None, _) => upsert(ix, rules, vol, &to, is_dir, size, &mut st),
                }
            }
            Change::Journal { vol, pos } => ix.set_journal(vol, pos),
        }
    }
    st
}

fn upsert(
    ix: &mut Index,
    rules: &Rules,
    vol: VolId,
    path: &[String],
    is_dir: bool,
    size: Option<u64>,
    st: &mut ApplyStats,
) {
    let Some((name, dir)) = path.split_last() else {
        st.ignored += 1;
        return;
    };
    let Some(parent) = resolve(ix, vol, dir) else {
        st.ignored += 1;
        return;
    };
    if excluded(ix, rules, parent, name) {
        st.ignored += 1;
        return;
    }
    match ix.child_by_name(parent, name) {
        Some(id) => {
            if ix.name(id) != name.as_str() && fold(&ix.name(id)) == fold(name) {
                let _ = ix.rename(id, parent, name);
            }
            ix.set_dir(id, is_dir);
            ix.set_size(id, size);
        }
        None => match ix.insert(parent, name, is_dir, size, crate::index::NONE) {
            Ok(_) => st.inserted += 1,
            Err(_) => st.ignored += 1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::VolumeKind;

    fn p(s: &str) -> Vec<String> {
        s.split('/').map(String::from).collect()
    }

    #[test]
    fn path_changes() {
        let mut ix = Index::new();
        let v = ix.add_volume("/r", VolumeKind::Walk).unwrap();
        let rules = Rules::new(&["*.tmp".into()], &["/r/skip".into()]);
        let up = |path: &str, d: bool| Change::Upsert {
            vol: v,
            path: p(path),
            is_dir: d,
            size: None,
        };
        let st = apply(
            &mut ix,
            &rules,
            vec![
                up("a", true),
                up("a/b", true),
                up("a/b/f.txt", false),
                up("a/x.tmp", false),
                up("skip", true),
                up("missing/parent.txt", false),
            ],
        );
        assert_eq!(st.inserted, 3);
        assert_eq!(st.ignored, 3);
        apply(
            &mut ix,
            &rules,
            vec![Change::Rename {
                vol: v,
                from: p("a/b"),
                to: p("c"),
                is_dir: true,
                size: None,
            }],
        );
        let f = ix.lookup(v, ["c", "f.txt"]).unwrap();
        assert!(ix.path(f).ends_with("f.txt"));
        // Upsert of an existing entry with different case renames it.
        apply(&mut ix, &rules, vec![up("C", true)]);
        assert_eq!(ix.name(ix.lookup(v, ["c"]).unwrap()), "C");
        apply(
            &mut ix,
            &rules,
            vec![Change::Remove {
                vol: v,
                path: p("c"),
            }],
        );
        assert_eq!(ix.len(), 2); // root + a
                                 // Rename onto an excluded name removes the entry.
        apply(
            &mut ix,
            &rules,
            vec![Change::Rename {
                vol: v,
                from: p("a"),
                to: p("a.tmp"),
                is_dir: true,
                size: None,
            }],
        );
        assert_eq!(ix.len(), 1);
        ix.check_invariants().unwrap();
    }

    #[test]
    fn stop_sleep() {
        let s = Stop::new();
        assert!(!s.sleep(Duration::from_millis(1)));
        s.stop();
        assert!(s.sleep(Duration::from_secs(10)));
    }
}
