// SPDX-License-Identifier: GPL-3.0-or-later
//! Query parser and evaluator on a small fixed index.
#![no_main]

use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

use explorer_index::index::{Index, VolumeKind};
use explorer_index::query::{search, Query, QueryOptions};
use explorer_index::synth;
use libfuzzer_sys::fuzz_target;

fn index() -> &'static Index {
    static IX: OnceLock<Index> = OnceLock::new();
    IX.get_or_init(|| {
        let mut ix = Index::new();
        let v = ix.add_volume("C:\\", VolumeKind::Walk).unwrap();
        ix.replace_tree(v, &synth::tree(2000, 1)).unwrap();
        ix
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    for (match_path, fuzzy) in [(false, false), (true, false), (false, true)] {
        if let Ok(q) = Query::parse(text, QueryOptions { match_path, fuzzy }) {
            let r = search(index(), &q, 20, &AtomicBool::new(false)).unwrap();
            assert!(r.hits.len() <= r.total.min(20));
        }
    }
});
