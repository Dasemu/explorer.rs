// SPDX-License-Identifier: GPL-3.0-or-later
//! USN record and FILE_NOTIFY_INFORMATION parsers, and journal replay of
//! whatever parses: arbitrary records applied to an index must keep its
//! invariants.
#![no_main]

use explorer_index::index::{Index, VolumeKind};
use explorer_index::notify::{parse_notify, translate};
use explorer_index::rules::Rules;
use explorer_index::source::{apply, Change};
use explorer_index::usn::parse_records;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (records, _) = parse_records(data);
    let mut ix = Index::new();
    let v = ix.add_volume("C:\\", VolumeKind::Ntfs).unwrap();
    let rules = Rules::new(&["$Recycle.Bin".into()], &["C:\\skip".into()]);
    // Replay twice: replay must be idempotent and never break invariants.
    apply(&mut ix, &rules, vec![Change::Usn { vol: v, records: records.clone() }]);
    apply(&mut ix, &rules, vec![Change::Usn { vol: v, records }]);
    ix.check_invariants().expect("invariants after USN replay");

    if let Ok(events) = parse_notify(data) {
        let t = translate(v, &events, |p| Some((p.len() % 2 == 0, None)));
        let mut ix = Index::new();
        let v = ix.add_volume("C:\\", VolumeKind::Walk).unwrap();
        let _ = v;
        apply(&mut ix, &rules, t.changes);
        ix.check_invariants().expect("invariants after notifications");
    }
});
