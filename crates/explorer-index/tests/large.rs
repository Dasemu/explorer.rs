// SPDX-License-Identifier: GPL-3.0-or-later
//! Large synthetic-data tests. The heavy ones are `#[ignore]`; run them with
//! `cargo test --release -p explorer-index --test large -- --include-ignored`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicBool;

use explorer_index::index::{Index, ScannedTree, VolumeKind, NONE, SEP};
use explorer_index::query::{search, Query, QueryOptions};
use explorer_index::rules::Rules;
use explorer_index::source::{apply, Change};
use explorer_index::synth::{self, Rng};
use explorer_index::usn::{parse_records, scan_mft, SimVolume, UsnBackend};
use explorer_index::{snapshot, source::Stop};
use proptest::prelude::*;

fn tree_paths(t: &ScannedTree, root: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(t.len());
    for i in 0..t.len() {
        let p = if t.parent[i] == NONE {
            root.to_string()
        } else {
            out[t.parent[i] as usize].clone()
        };
        out.push(format!("{p}{SEP}{}", t.name(i)));
    }
    out
}

fn build(n: usize) -> (Index, ScannedTree) {
    let t = synth::tree(n, 99);
    let mut ix = Index::new();
    let v = ix.add_volume("C:\\", VolumeKind::Walk).unwrap();
    ix.replace_tree(v, &t).unwrap();
    (ix, t)
}

fn run(ix: &Index, q: &str) -> (usize, Vec<String>) {
    let q = Query::parse(q, QueryOptions::default()).unwrap();
    let r = search(ix, &q, usize::MAX, &AtomicBool::new(false)).unwrap();
    let mut v: Vec<String> = r.hits.iter().map(|h| ix.path(h.id)).collect();
    v.sort();
    (r.total, v)
}

fn check_queries_against_brute_force(n: usize) {
    let (ix, t) = build(n);
    ix.check_invariants().unwrap();
    let paths = tree_paths(&t, "C:");
    // Path reconstruction for every entry.
    let mut got: Vec<String> = ix
        .live_ids()
        .filter(|&id| !ix.is_root(id))
        .map(|id| ix.path(id))
        .collect();
    got.sort();
    let mut want = paths.clone();
    want.sort();
    assert_eq!(got, want);

    let lower: Vec<String> = (0..t.len()).map(|i| t.name(i).to_lowercase()).collect();
    for needle in [
        "report",
        "SETUP",
        "e",
        "résumé",
        "café.txt",
        "zzzz",
        "_test",
    ] {
        let f = needle.to_lowercase();
        let mut want: Vec<String> = (0..t.len())
            .filter(|&i| lower[i].contains(&f))
            .map(|i| paths[i].clone())
            .collect();
        want.sort();
        let (total, got) = run(&ix, needle);
        assert_eq!(total, want.len(), "{needle}");
        assert_eq!(got, want, "{needle}");
    }
    // ext: and prefix
    let mut want: Vec<String> = (0..t.len())
        .filter(|&i| !t.is_dir[i] && lower[i].ends_with(".pdf") && lower[i].starts_with("re"))
        .map(|i| paths[i].clone())
        .collect();
    want.sort();
    assert_eq!(run(&ix, "ext:pdf ^re").1, want);
    // path: (component substring anywhere in the path)
    let mut want: Vec<String> = (0..t.len())
        .filter(|&i| paths[i].to_lowercase().contains("project") && lower[i].contains("log"))
        .map(|i| paths[i].clone())
        .collect();
    want.sort();
    assert_eq!(run(&ix, "path:project log").1, want);
    // a term with a separator
    let pat = format!("update{SEP}driver");
    let mut want: Vec<String> = (0..t.len())
        .filter(|&i| paths[i].to_lowercase().contains(&pat))
        .map(|i| paths[i].clone())
        .collect();
    want.sort();
    assert_eq!(run(&ix, &pat).1, want);
}

#[test]
fn queries_match_brute_force_small() {
    check_queries_against_brute_force(20_000);
}

#[test]
#[ignore = "heavy: 1M entries"]
fn queries_match_brute_force_1m() {
    check_queries_against_brute_force(1_000_000);
}

#[test]
#[ignore = "heavy: 1M entries"]
fn snapshot_roundtrip_and_corruption_1m() {
    let (ix, _) = build(1_000_000);
    let bytes = snapshot::encode(&ix);
    let back = snapshot::decode(&bytes).unwrap();
    back.check_invariants().unwrap();
    assert_eq!(back.len(), ix.len());
    let (_, a) = run(&ix, "report");
    let (_, b) = run(&back, "report");
    assert_eq!(a, b);
    let mut rng = Rng::new(5);
    for _ in 0..200 {
        let mut c = bytes.clone();
        let i = rng.below(c.len() as u64) as usize;
        c[i] ^= 1 << rng.below(8);
        assert!(snapshot::decode(&c).is_err());
        let cut = rng.below(bytes.len() as u64) as usize;
        assert!(snapshot::decode(&bytes[..cut]).is_err());
    }
}

/// Random renames/moves/deletes/creates on a large index stay consistent
/// with a simple path model.
fn random_mutations(n: usize, ops: usize) {
    let (mut ix, _) = build(n);
    let v = 0;
    let mut rng = Rng::new(11);
    let ids: Vec<u32> = ix.live_ids().filter(|&i| !ix.is_root(i)).collect();
    let root = ix.volume(v).unwrap().root;
    for k in 0..ops {
        let id = ids[rng.below(ids.len() as u64) as usize];
        if !ix.is_live(id) {
            continue;
        }
        match rng.below(4) {
            0 => {
                let _ = ix.remove(id);
            }
            1 => {
                let _ = ix.rename(id, ix.parent(id), &format!("renamed-{k}"));
            }
            2 => {
                let target = ids[rng.below(ids.len() as u64) as usize];
                if ix.is_live(target) && ix.is_dir(target) {
                    let _ = ix.rename(id, target, &format!("moved-{k}"));
                }
            }
            _ => {
                let p = if ix.is_dir(id) { id } else { root };
                ix.insert(p, &format!("new-{k}.txt"), false, None, NONE)
                    .unwrap();
            }
        }
    }
    ix.check_invariants().unwrap();
    // Every live entry's path resolves back to itself.
    for id in ix.live_ids().filter(|&i| !ix.is_root(i)).step_by(97) {
        let p = ix.path(id);
        let rel: Vec<&str> = p.split(SEP).skip(1).collect();
        assert_eq!(ix.lookup(v, rel), Some(id), "{p}");
    }
    // Queries agree with a brute-force scan after mutations.
    let names: Vec<(u32, String)> = ix
        .live_ids()
        .filter(|&i| !ix.is_root(i))
        .map(|i| (i, ix.name(i).to_lowercase()))
        .collect();
    for needle in ["renamed-", "moved-1", "new-2", "report"] {
        let want = names.iter().filter(|(_, n)| n.contains(needle)).count();
        assert_eq!(run(&ix, needle).0, want, "{needle}");
    }
    // Snapshot of the mutated index round-trips.
    let back = snapshot::decode(&snapshot::encode(&ix)).unwrap();
    assert_eq!(back.len(), ix.len());
}

#[test]
fn random_mutations_small() {
    random_mutations(20_000, 20_000);
}

#[test]
#[ignore = "heavy: 1M entries"]
fn random_mutations_1m() {
    random_mutations(1_000_000, 300_000);
}

/// Journal replay at scale: a simulated volume with many files and a long
/// stream of random operations; replay must equal a fresh MFT scan.
fn journal_replay(files: u32, ops: usize) {
    let mut vol = SimVolume::new(1);
    vol.chunk = 4096;
    let mut rng = Rng::new(3);
    let mut dirs: Vec<u32> = vec![5];
    let mut next = 16u32;
    for _ in 0..files {
        let dir = rng.below(10) == 0;
        let parent = dirs[rng.below(dirs.len() as u64) as usize];
        vol.create(next, parent, &format!("f{next}"), dir);
        if dir {
            dirs.push(next);
        }
        next += 1;
    }
    let tree = scan_mft(&mut vol, i64::MAX, &Rules::default(), "C:\\", &Stop::new()).unwrap();
    let mut ix = Index::new();
    let v = ix.add_volume("C:\\", VolumeKind::Ntfs).unwrap();
    ix.replace_tree(v, &tree).unwrap();
    let start = vol.next_usn;
    for k in 0..ops {
        let recs: Vec<u32> = vec![16 + rng.below((next - 16) as u64) as u32];
        let rec = recs[0];
        if !vol.files.contains_key(&rec) {
            continue;
        }
        let is_dir = vol.files[&rec].2;
        match rng.below(5) {
            0 if !is_dir || !vol.files.values().any(|f| f.0 == rec) => vol.delete(rec),
            1 => vol.rename(rec, vol.files[&rec].0, &format!("r{k}")),
            2 => {
                // Move, avoiding cycles like a real file system would.
                let target = dirs[rng.below(dirs.len() as u64) as usize];
                let mut a = target;
                let mut cyc = false;
                while a != 5 {
                    if a == rec {
                        cyc = true;
                        break;
                    }
                    match vol.files.get(&a) {
                        Some(f) => a = f.0,
                        None => {
                            cyc = true;
                            break;
                        }
                    }
                }
                if !cyc {
                    vol.rename(rec, target, &format!("m{k}"));
                }
            }
            3 => vol.write(rec),
            _ => {
                let parent = if is_dir { rec } else { 5 };
                vol.create(next, parent, &format!("n{k}"), false);
                next += 1;
            }
        }
    }
    let mut pos = start;
    loop {
        let (n, bytes) = vol.read_journal(vol.journal_id, pos).unwrap();
        let (records, err) = parse_records(&bytes);
        assert!(err.is_none());
        apply(
            &mut ix,
            &Rules::default(),
            vec![Change::Usn { vol: v, records }],
        );
        if n == pos {
            break;
        }
        pos = n;
    }
    ix.check_invariants().unwrap();
    let fresh_tree = scan_mft(&mut vol, i64::MAX, &Rules::default(), "C:\\", &Stop::new()).unwrap();
    let mut a: Vec<String> = ix.live_ids().map(|i| ix.path(i)).collect();
    let mut b = tree_paths(&fresh_tree, "C:");
    b.push("C:\\".into());
    a.sort();
    b.sort();
    assert_eq!(a, b);
}

#[test]
fn journal_replay_small() {
    journal_replay(5_000, 5_000);
}

#[test]
#[ignore = "heavy: 1M-file simulated volume"]
fn journal_replay_1m() {
    journal_replay(1_000_000, 200_000);
}

// ----- randomized model test (proptest) --------------------------------------

#[derive(Debug, Clone)]
enum Op {
    Up(Vec<u8>, bool),
    Rm(Vec<u8>),
    Mv(Vec<u8>, Vec<u8>),
}

fn path_of(p: &[u8]) -> Vec<String> {
    p.iter().map(|c| format!("n{}", c % 6)).collect()
}

fn op() -> impl Strategy<Value = Op> {
    let p = proptest::collection::vec(any::<u8>(), 1..4);
    prop_oneof![
        (p.clone(), any::<bool>()).prop_map(|(p, d)| Op::Up(p, d)),
        p.clone().prop_map(Op::Rm),
        (p.clone(), p).prop_map(|(a, b)| Op::Mv(a, b)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    /// Path-based changes keep the index consistent with a map model.
    #[test]
    fn path_changes_match_model(ops in proptest::collection::vec(op(), 1..60)) {
        let mut ix = Index::new();
        let v = ix.add_volume("/r", VolumeKind::Walk).unwrap();
        let mut model: BTreeMap<Vec<String>, bool> = BTreeMap::new();
        let rules = Rules::default();
        for o in ops {
            match o {
                Op::Up(p, d) => {
                    let p = path_of(&p);
                    // Only file-system-valid operations: no kind changes of
                    // existing entries, no children below files.
                    if model.get(&p).is_some_and(|&k| k != d)
                        || (p.len() > 1 && model.get(&p[..p.len() - 1]) == Some(&false))
                    {
                        continue;
                    }
                    let parent_ok = p.len() == 1 || model.get(&p[..p.len() - 1]) == Some(&true);
                    apply(&mut ix, &rules, vec![Change::Upsert { vol: v, path: p.clone(), is_dir: d, size: None }]);
                    if parent_ok {
                        model.insert(p, d);
                    }
                }
                Op::Rm(p) => {
                    let p = path_of(&p);
                    apply(&mut ix, &rules, vec![Change::Remove { vol: v, path: p.clone() }]);
                    model.retain(|k, _| !k.starts_with(&p));
                }
                Op::Mv(a, b) => {
                    let (a, b) = (path_of(&a), path_of(&b));
                    let exists = model.contains_key(&a);
                    let dst_parent_ok = b.len() == 1 || model.get(&b[..b.len() - 1]) == Some(&true);
                    let into_self = b.starts_with(&a);
                    let dst_taken = model.contains_key(&b) && a != b;
                    let dst_parent_file = b.len() > 1 && model.get(&b[..b.len() - 1]) == Some(&false);
                    if dst_taken || into_self || dst_parent_file {
                        continue; // file systems refuse these; sources never emit them
                    }
                    let is_dir = model.get(&a).copied().unwrap_or(false);
                    apply(&mut ix, &rules, vec![Change::Rename { vol: v, from: a.clone(), to: b.clone(), is_dir, size: None }]);
                    if exists && dst_parent_ok {
                        let moved: Vec<(Vec<String>, bool)> = model
                            .iter()
                            .filter(|(k, _)| k.starts_with(&a))
                            .map(|(k, d)| (k.clone(), *d))
                            .collect();
                        model.retain(|k, _| !k.starts_with(&a));
                        for (k, d) in moved {
                            let mut nk = b.clone();
                            nk.extend_from_slice(&k[a.len()..]);
                            model.insert(nk, d);
                        }
                    } else if exists {
                        model.retain(|k, _| !k.starts_with(&a));
                    } else if dst_parent_ok {
                        model.insert(b, is_dir);
                    }
                }
            }
        }
        ix.check_invariants().map_err(TestCaseError::fail)?;
        let mut got: HashMap<Vec<String>, bool> = HashMap::new();
        for id in ix.live_ids().filter(|&i| !ix.is_root(i)) {
            let p = ix.path(id);
            let rel: Vec<String> = p["/r".len() + 1..].split(SEP).map(String::from).collect();
            got.insert(rel, ix.is_dir(id));
        }
        let want: HashMap<Vec<String>, bool> = model.into_iter().collect();
        prop_assert_eq!(got.keys().collect::<std::collections::BTreeSet<_>>(), want.keys().collect::<std::collections::BTreeSet<_>>());
    }
}

/// Writes seed inputs for the cargo-fuzz targets into `fuzz/corpus/`.
/// Run: `cargo test -p explorer-index --test large write_fuzz_seeds -- --ignored`.
#[test]
#[ignore = "generates fuzz seed corpus"]
fn write_fuzz_seeds() {
    use explorer_index::protocol::{write_frame, QueryFlags, Request, MAX_REQUEST_FRAME};
    use explorer_index::usn::{encode_v2, encode_v3, UsnRecord};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus");
    let put = |target: &str, name: &str, data: &[u8]| {
        let d = root.join(target);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(name), data).unwrap();
    };
    let (ix, _) = build(50);
    let snap = snapshot::encode(&ix);
    put("snapshot", "full", &snap);
    let mut body = vec![1u8];
    body.extend_from_slice(&snap[snapshot::HEADER_LEN..]);
    put("snapshot", "body", &body);
    let mut frames = Vec::new();
    for r in [
        Request::Hello {
            version: 1,
            client: "seed".into(),
        },
        Request::Query {
            id: 1,
            text: "ext:rs main".into(),
            offset: 0,
            limit: 10,
            flags: QueryFlags::default(),
        },
        Request::Status { id: 2 },
    ] {
        write_frame(&mut frames, &r, MAX_REQUEST_FRAME).unwrap();
    }
    put("ipc", "requests", &frames);
    let rec = |frn: u64, parent: u64, reason: u32, name: &str, attributes: u32| UsnRecord {
        frn,
        parent_frn: parent,
        usn: 0,
        reason,
        attributes,
        name: name.into(),
    };
    let mut recs = Vec::new();
    recs.extend(encode_v2(&rec(20, 5, 0x100, "dir", 0x10)));
    recs.extend(encode_v3(&rec(21, 20, 0x100, "file.txt", 0x20)));
    recs.extend(encode_v2(&rec(21, 20, 0x2000, "renamed.txt", 0x20)));
    recs.extend(encode_v2(&rec(20, 5, 0x200, "dir", 0x10)));
    put("records", "usn", &recs);
    put(
        "records",
        "notify",
        &explorer_index::notify::encode_notify(&[(1, "a\\b"), (4, "a\\b"), (5, "a\\c")]),
    );
    put(
        "query",
        "ops",
        b"ext:rs;toml !path:target ^main size:>1kb \"a b\" ~fzy dir: src\\lib",
    );
}
