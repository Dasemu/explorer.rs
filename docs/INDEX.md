# File-name index

An Everything-style instant file-name index for explorer.rs. It backs the
launcher's "Files" results and the future file manager, and is meant to make
the Windows Search indexer unnecessary for file-name search
([DISABLING-WSEARCH.md](DISABLING-WSEARCH.md)). No web, no telemetry, nothing
leaves the machine.

**Status:** engine, protocol, client and daemon tested on Linux; everything
Windows-specific (MFT/USN access, helper, named pipes, `ReadDirectoryChangesW`)
is **unverified on Windows**. Test plan:
[TESTING-INDEX-ON-WINDOWS.md](TESTING-INDEX-ON-WINDOWS.md).

## Components

```
                          (optional, elevated, logon task)
                         explorer-rs-index-helper.exe
                           | FSCTL_ENUM_USN_DATA / FSCTL_READ_USN_JOURNAL
                           | raw record bytes over \\.\pipe\explorer-rs.index-helper.<SID>
                           v
 launcher ---pipe--->  explorer-rs-indexd.exe (user, not elevated)
 CLI      \\.\pipe\explorer-rs.index.<SID>
                           |  sources: NTFS (MFT+USN) | walk + ReadDirectoryChangesW | walk + rescans
                           |  index (RwLock) <- change batches
                           |  snapshot: %LOCALAPPDATA%\explorer-rs\state\index.snapshot
```

| crate | role |
|---|---|
| `explorer-index` | portable engine: index, queries, snapshots, rules, USN parsing/replay, walker, notification parsing, protocol, transport, helper RPC; `win::` FSCTL backend, watcher, drive discovery |
| `explorer-indexd` | `explorer-rs-indexd` (daemon + CLI) and `explorer-rs-index-helper` (elevated helper) |
| `explorer-index-client` | non-blocking client for UIs (background thread, channels, timeouts, cancel) |

Privilege model and privacy: [decisions/0010](decisions/0010-index-privilege-model.md).
Threading: [decisions/0011](decisions/0011-index-client-threading.md).
Data layout and formats: [decisions/0012](decisions/0012-index-data-layout-and-formats.md).
Wire protocol: [IPC-PROTOCOL.md](IPC-PROTOCOL.md).

## Enabling it

The daemon is a known supervisor component but **not enabled by default**
until verified on Windows. To enable, in `%APPDATA%\explorer-rs\config.toml`:

```toml
[supervisor]
components = ["taskbar", "launcher", "indexd"]
```

For the fast NTFS path also install the helper once (elevated, explicit):
`scripts/install-index-helper.ps1`. Without it the daemon walks and watches the
configured roots. The launcher shows a "Files" section when built with
`--features file-index`.

Settings (`[index]`, documented in `config/default.toml`): `roots` (empty =
all fixed drives on Windows, `$HOME` elsewhere; `%VAR%` and `~` are expanded),
`use_ntfs`, `exclude_names` (`*` wildcards), `exclude_paths` (prefixes),
`check_access`, `snapshot_interval_s`, `rescan_interval_s`,
`max_query_results`, `launcher_results`.

## Sources

Every source runs on its own thread and emits batches of `Change`s
(`source.rs`); the daemon applies them under the index write lock. Per root:

1. **NTFS** (Windows, `use_ntfs`, root is an NTFS volume root such as `C:\`).
   Backend: the volume handle directly if the daemon is elevated, otherwise the
   elevated helper. `usn::run_ntfs` is a pure state machine over the
   `UsnBackend` trait:
   - query the journal; if the snapshot's `(journal id, next USN)` is still
     inside `[first_usn, next_usn]` of the same journal, resume from it,
     otherwise do a full MFT enumeration;
   - full scan: `FSCTL_ENUM_USN_DATA` chunks up to the journal's `next_usn`,
     records collected, then `build_tree` (CSR child lists by parent record,
     BFS from the root record 5; orphans, metafiles < 16 and excluded subtrees
     dropped) and grafted with one `ReplaceTree`;
   - follow: `FSCTL_READ_USN_JOURNAL` from the saved USN, polled every 500 ms
     when idle; records are replayed with the idempotent rule below and the new
     position is stored with the same batch;
   - `ERROR_JOURNAL_ENTRY_DELETED` (wrap), journal id change or
     `ERROR_JOURNAL_NOT_ACTIVE`: rescan; malformed records: rescan.
2. **Walk + `ReadDirectoryChangesW`** (Windows fallback): the watch is armed
   before the initial walk, notifications are parsed by `notify.rs`, renames
   are paired, new folders are walked, overflow triggers a full rewalk.
3. **Walk + periodic rescans** (other platforms, or if watching fails).

Symbolic links and junctions are indexed but never followed.

### USN replay rule

Each record carries the file's current name and parent, so replay is
idempotent: `FILE_DELETE` removes the entry (and anything still below it);
`RENAME_OLD_NAME` alone is ignored; anything else is an upsert (create,
rename-new, data change). A record whose parent is not indexed (excluded or
unknown) removes the entry if it exists. A move into the entry's own subtree is
refused. Record numbers are mapped to ids through a dense vector; stale slots
are detected on lookup (key and volume must match), so freeing entries never
touches the map.

## Index

Struct of arrays per entry: flags, parent, intrusive doubly-linked child list,
name segment, aux offset, MFT record number, optional size. Names are stored
once, lowercase, in one NUL-separated arena; original spelling is a case
bitmask (ASCII) or a copy (non-ASCII whose lowercase differs). Renames append a
new segment; the arena is compacted when dead segments exceed half of the live
ones. Ids are recycled through a free list. Paths are rebuilt by walking
parents. See [decisions/0012](decisions/0012-index-data-layout-and-formats.md).

## Query language

Whitespace-separated terms, all must match; `"..."` groups spaces.

| term | matches |
|---|---|
| `foo` | name contains `foo` (case-insensitive) |
| `^foo` / `foo$` / `^foo$` | name starts with / ends with / equals |
| `~foo` | fuzzy name match (nucleo) |
| `src\main` (any `\` or `/`) | consecutive path components: `...src` + `\` + `main...` |
| `path:foo` | full path contains `foo` |
| `dir:foo` | a parent folder's path contains `foo` |
| `dir:` / `folder:` / `file:` | folders only / files only |
| `ext:rs;toml` | file extension in list |
| `size:>10mb` | `>`, `>=`, `<`, `<=`, `=`; units b, kb, mb, gb, tb (sizes known for walked roots only) |
| `!term` | negation |

Options per request: `match_path` (plain terms match the full path), `fuzzy`
(plain terms are fuzzy).

Evaluation: the longest positive substring/prefix/suffix term drives one
`memmem` pass over the folded arena; each candidate is checked against the
other terms; path terms use a per-query memo over ancestors (O(entries) worst
case). Ranking: exact name > prefix > word-boundary > substring (+ fuzzy
score), then shorter names, then folders. Top-k selection
(`select_nth_unstable`) instead of a full sort.

Access filtering (`check_access`): for the page being returned, results whose
parent folder the daemon cannot list are dropped (cached per folder).

## Persistence and crash safety

Snapshot: versioned header with CRC-32 of header and body, entries in
pre-order with backward parent deltas, per-volume journal positions. Written to
`index.tmp`, fsynced, renamed over `index.snapshot`; encoded under the read
lock, written without it. Saved every `snapshot_interval_s` when changed and on
clean shutdown. On load, any error (truncation, bit flip, wrong version,
inconsistent structure) moves the file to `index.bad` and the index is rebuilt.
After a crash or kill the daemon restarts from the last snapshot and catches up
from the journal (the snapshot never contains a journal position newer than the
index state it was taken with, because both change in the same batch under the
same lock).

## Performance

Measured in the Linux build container with `cargo bench -p explorer-index`
(criterion, release build) on synthetic trees from `synth::tree` (Windows-like
names, ~8% folders, average folded name length 17 bytes, depth up to ~10).
**Caveat:** a shared cloud VM (4 vCPU Intel Xeon @ 2.10 GHz, 15 GiB RAM; single-threaded queries), not a desktop; numbers on real
hardware and real file names will differ. Query times are for computing the
total and the best 50 hits.

| | 1M entries | 5M entries |
|---|---|---|
| heap memory (`memory_usage`) | 61 MiB, **64.3 bytes/entry** | 306 MiB, **64.3 bytes/entry** |
| build (`replace_tree` from a scanned tree) | 257 ms | 1.81 s |
| snapshot size | 23 MiB | 119 MiB |
| snapshot encode (under read lock) | 605 ms | 4.05 s |
| snapshot load (decode + rebuild) | 204 ms | 1.08 s |
| query `setup.exe` | 2.6 ms | 23 ms |
| query `report` | 6.7 ms | 55 ms |
| query `^read` | 7.6 ms | 56 ms |
| query `ext:pdf budget` | 6.7 ms | 57 ms |
| query `config !ext:json` | 6.5 ms | 55 ms |
| query `path:windows driver` | 20.8 ms | 160 ms |
| query `size:>10mb ext:mp4` (no name term: full scan) | 22.6 ms | 112 ms |
| query `e` (matches ~70% of entries) | 24.3 ms | 169 ms |
| query `program\update` (path pattern) | 36.8 ms | 254 ms |
| query `~prjfnl` (fuzzy) | 39.4 ms | 192 ms |

Targets: typical queries < 50 ms at 1M (met: 3-7 ms; the worst case measured
is 39 ms), < 100 bytes/entry (met: 64). At 5M, name queries stay around
50 ms, but one-letter, path-pattern and fuzzy queries take 160-250 ms;
parallelising the scan or caching the previous query's candidates for
type-ahead are the obvious next steps if that matters in practice. Snapshot
encoding at 5M holds the read lock for ~4 s (writers wait, readers do not).

Memory is the exact heap size of the index structures (`Index::memory_usage`),
not process RSS; the daemon adds thread stacks, the allocator's overhead and,
transiently, a snapshot buffer while saving.

## Testing

- Unit tests in every module; `tests/large.rs`: brute-force comparison of
  queries and path reconstruction, random rename/move/delete/create sequences,
  journal replay on a simulated volume compared with a fresh scan, snapshot
  round trips and corruption; heavy variants with 1M entries are `#[ignore]`
  (`cargo test --release -p explorer-index --test large -- --include-ignored`).
- Proptest model test of path-based changes.
- `usn::SimVolume` simulates an NTFS volume (MFT, bounded journal, wrap,
  reset); `run_ntfs` is tested against it, including over the helper RPC.
- `crates/explorer-indexd/tests/e2e.rs`: daemon + server + client over a Unix
  socket.
- Fuzzing (`fuzz/`, cargo-fuzz): `snapshot`, `ipc`, `records`, `query`.
  Seed corpus: `cargo test -p explorer-index --test large write_fuzz_seeds -- --ignored`,
  then `cargo +nightly fuzz run <target>`.
  Run on 2026-11-02 in the container, 3 minutes per target, no crashes:
  `ipc` 4.4M executions, `snapshot` 592k, `records` 306k, `query` 97k.
- The 1M-entry ignored tests were run once in release mode and pass
  (brute-force queries, snapshot corruption, 300k random mutations, 200k
  simulated journal operations on a 1M-file volume).

## Known limitations

- Hard links: one entry per MFT record (first name seen).
- File sizes are unknown for NTFS roots (the MFT enumeration does not return
  them); `size:` only works on walked roots.
- Names that are not valid UTF-16 (unpaired surrogates) are indexed with
  U+FFFD replacements and will not match their real spelling.
- Overlapping roots (`C:\` and `C:\Users`) are indexed twice.
- ReFS (128-bit file ids) is not supported by the NTFS source; such volumes are
  walked.
- The daemon does not yet save a snapshot on logoff (`WM_ENDSESSION`); it
  relies on periodic snapshots plus journal catch-up.
