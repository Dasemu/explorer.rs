# 0012. Index data layout, snapshot format and IPC encoding

- Status: accepted
- Date: 2026-11-02

## Context

Targets: < 100 bytes per entry, < 50 ms for typical queries at 1M entries,
fast restart, crash safety, and parsers that are safe on hostile input.

## Decision

**In-memory layout.** Struct of arrays indexed by a `u32` entry id (flags,
parent, intrusive doubly-linked child list, name segment, aux offset, MFT
record number, optional size column). Names live once, case-folded, in a single
NUL-separated arena; a substring query is one `memchr::memmem` pass over the
arena with a galloping cursor mapping hits back to entries. The original
spelling is kept as a per-name case bitmask (ASCII) or a copy of the original
(non-ASCII names whose lowercase differs). No per-entry heap allocation, no
hash maps: NTFS record numbers map to ids through a dense `Vec<u32>`.

**Snapshot.** A custom versioned binary format (`EXRSIDX\0`, version, CRC-32
of header and body) listing entries in pre-order with parent references as
backward deltas. Loading rebuilds the index through the normal insert path, so
a snapshot can never create a cycle or an out-of-range reference and every name
is revalidated. Written to a temp file, fsynced, renamed. Per-volume journal id
and next USN are stored so restarts resume from the journal.

Rejected: dumping the in-memory arrays directly (faster load, but every array
would need cross-validation against the others; one missed check means a
panic or an infinite loop on a corrupted file); serde/bincode of the whole
index (allocation-heavy, larger).

**IPC encoding.** `postcard` (MIT OR Apache-2.0, `default-features = false`,
`alloc`) inside `u32`-length-prefixed frames, with per-direction maximum
frame sizes checked before allocating. Compact, serde-based, no unsafe parsing
of our own; enums are encoded by variant index, so variants are append-only.
`bincode` was not chosen because of its recent licensing/maintenance churn.

## Consequences

- Measured on the container (see docs/INDEX.md): ~64 bytes/entry, typical
  queries 3-10 ms at 1M entries.
- Snapshot load costs a full re-insert (~0.25 s per million entries); accepted
  for robustness.
- Any change to message enums must append variants and bump
  `PROTOCOL_VERSION` when semantics change.
