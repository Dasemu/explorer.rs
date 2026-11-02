// SPDX-License-Identifier: GPL-3.0-or-later
//! Snapshot loader: full decode (header + checksum) and the body decoder on
//! its own (the checksum would otherwise hide almost every input from it).
//! Any successfully decoded index must satisfy its invariants and survive
//! a re-encode round trip.
#![no_main]

use explorer_index::snapshot;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = snapshot::decode(data);
    let (flags, body) = match data.split_first() {
        Some((f, b)) => (u32::from(*f & 1), b),
        None => (0, data),
    };
    if let Ok(ix) = snapshot::decode_body(body, flags) {
        ix.check_invariants().expect("decoded index violates invariants");
        let again = snapshot::decode(&snapshot::encode(&ix)).expect("re-encode must decode");
        assert_eq!(again.len(), ix.len());
    }
});
