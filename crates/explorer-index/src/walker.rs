// SPDX-License-Identifier: GPL-3.0-or-later
//! Directory-walk source (portable, `std::fs`).
//!
//! Used for non-NTFS volumes, for folders below a volume root, and whenever
//! the MFT cannot be read (no elevated helper). Symbolic links and NTFS
//! junctions are indexed as entries but never followed, so cycles such as
//! `C:\Users\me\Application Data` cannot loop the walk.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::index::{ScannedTree, VolId, NONE, SEP};
use crate::rules::Rules;
use crate::source::{Change, ChangeSink, SourceState, Stop};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WalkStats {
    pub dirs: u64,
    pub files: u64,
    pub errors: u64,
    /// Names that are not valid Unicode (skipped).
    pub skipped: u64,
}

/// Walk `root` and return the tree below it. Returns `None` if stopped.
pub fn walk(root: &Path, rules: &Rules, stop: &Stop) -> Option<(ScannedTree, WalkStats)> {
    let mut tree = ScannedTree::default();
    let mut st = WalkStats::default();
    let mut stack: Vec<(PathBuf, u32)> = vec![(root.to_path_buf(), NONE)];
    let mut n = 0u32;
    while let Some((dir, tidx)) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => {
                st.errors += 1;
                continue;
            }
        };
        for entry in rd {
            n = n.wrapping_add(1);
            if n % 4096 == 0 && stop.is_stopped() {
                return None;
            }
            let Ok(entry) = entry else {
                st.errors += 1;
                continue;
            };
            let fname = entry.file_name();
            let Some(name) = fname.to_str() else {
                st.skipped += 1;
                continue;
            };
            if rules.excludes_name(name) {
                continue;
            }
            let path = entry.path();
            if rules.has_path_rules() && rules.excludes_path(&path.to_string_lossy()) {
                continue;
            }
            let Ok(ft) = entry.file_type() else {
                st.errors += 1;
                continue;
            };
            let is_dir = ft.is_dir();
            let size = if ft.is_file() {
                entry.metadata().ok().map(|m| m.len())
            } else {
                None
            };
            let t = tree.push(tidx, name, is_dir, size, NONE);
            if is_dir {
                st.dirs += 1;
                stack.push((path, t));
            } else {
                st.files += 1;
            }
        }
    }
    Some((tree, st))
}

/// Walk `root/rel` and emit an `Upsert` for every descendant (used when a
/// directory appears through a change notification).
pub fn walk_into_changes(vol: VolId, root: &Path, rel: &[String], rules: &Rules) -> Vec<Change> {
    let mut dir = root.to_path_buf();
    dir.extend(rel);
    let Some((tree, _)) = walk(&dir, rules, &Stop::new()) else {
        return Vec::new();
    };
    let mut paths: Vec<Vec<String>> = Vec::with_capacity(tree.len());
    let mut out = Vec::with_capacity(tree.len());
    for i in 0..tree.len() {
        let mut p = if tree.parent[i] == NONE {
            rel.to_vec()
        } else {
            paths[tree.parent[i] as usize].clone()
        };
        p.push(tree.name(i).to_string());
        out.push(Change::Upsert {
            vol,
            path: p.clone(),
            is_dir: tree.is_dir[i],
            size: (tree.size[i] != u64::MAX).then_some(tree.size[i]),
        });
        paths.push(p);
    }
    out
}

/// Walk `root` once, then rescan every `rescan` (if non-zero) until stopped.
/// Used where no change notifications are available.
pub fn run_walk(
    vol: VolId,
    root: &Path,
    rules: &Rules,
    sink: &dyn ChangeSink,
    stop: &Stop,
    rescan: Duration,
) {
    loop {
        sink.set_state(vol, SourceState::Scanning);
        let t0 = Instant::now();
        let Some((tree, st)) = walk(root, rules, stop) else {
            sink.set_state(vol, SourceState::Stopped);
            return;
        };
        tracing::info!(
            root = %root.display(),
            entries = tree.len(),
            errors = st.errors,
            ms = t0.elapsed().as_millis() as u64,
            "walk done"
        );
        sink.apply(vec![Change::ReplaceTree { vol, tree }]);
        sink.set_state(vol, SourceState::Polling);
        if rescan.is_zero() {
            while !stop.sleep(Duration::from_secs(3600)) {}
        } else if stop.sleep(rescan) {
            break;
        }
        if stop.is_stopped() {
            break;
        }
    }
    sink.set_state(vol, SourceState::Stopped);
}

/// Display form of a root (`C:` -> `C:\`).
pub fn root_display(root: &Path) -> String {
    let s = root.to_string_lossy().to_string();
    if s.len() == 2 && s.ends_with(':') {
        format!("{s}{SEP}")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Index, VolumeKind};
    use crate::source::{apply, VecSink};

    fn mk(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn walk_tree_with_rules() {
        let d = tempfile::tempdir().unwrap();
        mk(d.path(), "a/b/c.txt", "hello");
        mk(d.path(), "a/skip.tmp", "");
        mk(d.path(), "node_modules/x/y.js", "");
        mk(d.path(), "top.rs", "fn main(){}");
        let rules = Rules::new(&["*.tmp".into(), "node_modules".into()], &[]);
        let (tree, st) = walk(d.path(), &rules, &Stop::new()).unwrap();
        assert_eq!(st.files, 2);
        assert_eq!(st.dirs, 2);
        let mut ix = Index::new();
        let v = ix
            .add_volume(&d.path().to_string_lossy(), VolumeKind::Walk)
            .unwrap();
        apply(&mut ix, &rules, vec![Change::ReplaceTree { vol: v, tree }]);
        let c = ix.lookup(v, ["a", "b", "c.txt"]).unwrap();
        assert_eq!(ix.size(c), Some(5));
        assert_eq!(ix.path(c), d.path().join("a/b/c.txt").to_string_lossy());
        ix.check_invariants().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_are_not_followed() {
        let d = tempfile::tempdir().unwrap();
        mk(d.path(), "a/f", "");
        std::os::unix::fs::symlink(d.path(), d.path().join("a/loop")).unwrap();
        let (tree, _) = walk(d.path(), &Rules::default(), &Stop::new()).unwrap();
        assert_eq!(tree.len(), 3);
    }

    #[test]
    fn walk_into_changes_and_run_walk() {
        let d = tempfile::tempdir().unwrap();
        mk(d.path(), "new/sub/f.txt", "");
        let mut ix = Index::new();
        let v = ix
            .add_volume(&d.path().to_string_lossy(), VolumeKind::Walk)
            .unwrap();
        let mut ch = vec![Change::Upsert {
            vol: v,
            path: vec!["new".into()],
            is_dir: true,
            size: None,
        }];
        ch.extend(walk_into_changes(
            v,
            d.path(),
            &["new".into()],
            &Rules::default(),
        ));
        apply(&mut ix, &Rules::default(), ch);
        assert!(ix.lookup(v, ["new", "sub", "f.txt"]).is_some());

        let sink = VecSink::default();
        let stop = Stop::new();
        std::thread::scope(|s| {
            s.spawn(|| {
                run_walk(
                    v,
                    d.path(),
                    &Rules::default(),
                    &sink,
                    &stop,
                    Duration::from_millis(5),
                )
            });
            std::thread::sleep(Duration::from_millis(40));
            stop.stop();
        });
        assert!(sink.changes.lock().unwrap().len() >= 2, "rescans happened");
        assert_eq!(
            sink.states.lock().unwrap().last().unwrap().1,
            SourceState::Stopped
        );
        assert_eq!(root_display(Path::new("C:")), format!("C:{SEP}"));
    }
}
