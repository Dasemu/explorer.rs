# Index IPC protocol

Protocol between `explorer-rs-indexd` and its clients (launcher, CLI), and
between the daemon and the elevated helper. Implementation:
`crates/explorer-index/src/protocol.rs` (types and framing),
`crates/explorer-indexd/src/server.rs` (daemon side),
`crates/explorer-index-client` (client side), `crates/explorer-index/src/helper.rs`
(helper RPC).

## Transport

| | Windows | elsewhere (dev/tests) |
|---|---|---|
| daemon | `\\.\pipe\explorer-rs.index.<user SID>` | `<state dir>/index.sock` |
| helper | `\\.\pipe\explorer-rs.index-helper.<user SID>` | (not used) |
| override | env `EXPLORER_RS_INDEX_ENDPOINT` (daemon endpoint) | same |

Windows pipes: byte mode, overlapped I/O, `PIPE_REJECT_REMOTE_CLIENTS`,
DACL `D:P(A;;GA;;;<SID>)` (helper adds `S:(ML;;NW;;;ME)`), first instance
created with `FILE_FLAG_FIRST_PIPE_INSTANCE`. Clients check that the server
process belongs to the same user and connect with `SECURITY_IDENTIFICATION`.
Unix sockets are created with mode 0600. See
[decisions/0010](decisions/0010-index-privilege-model.md).

## Framing

```
+----------------+---------------------------+
| u32 LE length  | payload (length bytes)     |
+----------------+---------------------------+
```

The payload is one message encoded with [postcard](https://docs.rs/postcard)
1.x (varint integers, length-prefixed strings and vectors, enums as a varint
variant index followed by the fields in declaration order). A payload must be
consumed exactly; trailing bytes are an error.

| direction | max payload |
|---|---|
| client -> daemon (`Request`) | 16 KiB |
| daemon -> client (`Response`) | 4 MiB |
| daemon -> helper (`HelperRequest`) | 16 KiB |
| helper -> daemon (`HelperResponse`) | 2 MiB |

The length is checked **before** any allocation. An oversized length, a
truncated frame or an undecodable payload ends the connection (the daemon
first replies `Error{id: 0, ...}` when it can), because the stream position is
no longer known. The daemon never panics on malformed input (fuzzed by
`fuzz/fuzz_targets/ipc.rs`).

## Versioning

`PROTOCOL_VERSION = 1`. The first client message must be `Hello`. A different
version gets `Error{code: VersionMismatch}` and the connection is closed.
Variants are **append-only**: never reorder or remove variants or fields; add
new requests at the end and bump the version when the meaning of an existing
message changes.

## Client -> daemon: `Request`

| # | variant | fields | reply |
|---|---|---|---|
| 0 | `Hello` | `version: u16, client: String` | `Hello` |
| 1 | `Query` | `id: u32, text: String, offset: u32, limit: u32, flags: QueryFlags{match_path: bool, fuzzy: bool}` | `Results` or `Error` |
| 2 | `Cancel` | `id: u32` | the cancelled query replies `Error{Cancelled}` (if still running) |
| 3 | `Status` | `id: u32` | `Status` |
| 4 | `Reindex` | `id: u32, root: Option<String>` (None = all) | `Done` or `Error{NotFound}` |
| 5 | `AddRoot` | `id: u32, path: String` | `Done` or `Error{NotFound}` |
| 6 | `RemoveRoot` | `id: u32, path: String` | `Done` or `Error{NotFound}` |
| 7 | `Shutdown` | `id: u32` | `Done`, then the daemon saves a snapshot and exits with code 0 |

Requests may be pipelined. Replies carry the request `id` and may arrive out of
order. A new `Query` on a connection cancels the previous query of that
connection (type-ahead). `AddRoot`/`RemoveRoot` change the running daemon only;
the configuration file stays the source of truth on restart.

Query text syntax: see [INDEX.md](INDEX.md#query-language). At most 1024 bytes
and 16 terms. `limit` is capped by `[index] max_query_results` (default 1000)
and by 10 000.

## Daemon -> client: `Response`

| # | variant | fields |
|---|---|---|
| 0 | `Hello` | `version: u16, server: String` |
| 1 | `Results` | `id: u32, total: u64, offset: u32, hits: Vec<Hit>, more: bool, elapsed_us: u64` |
| 2 | `Status` | `id: u32, status: Status` |
| 3 | `Done` | `id: u32` |
| 4 | `Error` | `id: u32, code: ErrorCode, message: String` |

`Hit = { path: String, is_dir: bool, size: Option<u64>, score: u32 }`, best
first. `total` is the number of matches **before** access filtering (so it can
hint that hidden matches exist); `more` says whether more *visible* results
follow this page. `size` is only known for walked roots (the MFT enumeration
does not report sizes).

`Status = { version, entries, memory_bytes, roots: Vec<RootStatus{path, kind,
state, entries}>, uptime_s, snapshot_age_s: Option<u64>, scanning: bool }`
with `kind` = `ntfs`/`walk` and `state` = `starting`, `scanning`, `live`,
`polling`, `failed: <reason>`, `stopped`.

### Error codes (`ErrorCode`, by variant index)

| # | code | meaning |
|---|---|---|
| 0 | `BadRequest` | malformed frame/message, missing or duplicate `Hello` |
| 1 | `VersionMismatch` | unsupported `PROTOCOL_VERSION` |
| 2 | `TooLarge` | request or reply exceeds the frame limit |
| 3 | `Busy` | the daemon could not start a worker |
| 4 | `Cancelled` | query cancelled (explicitly or by a newer query) |
| 5 | `NotFound` | unknown root / path is not a folder / already indexed |
| 6 | `Unavailable` | reserved (the client library reports a missing daemon locally) |
| 7 | `Internal` | unexpected daemon error |
| 8 | `BadQuery` | query text rejected (too long, too many terms, bad `size:`) |

## Daemon <-> helper

Only after a successful `Hello` exchange; one request, one reply.

| `HelperRequest` | `HelperResponse` |
|---|---|
| 0 `Hello{version}` | 0 `Hello{version, volumes: Vec<u8>}` (drive letters served) |
| 1 `Query{volume}` | 1 `Journal(JournalInfo{journal_id, first_usn, next_usn})` |
| 2 `EnumMft{volume, start_frn, high_usn}` | 2 `Chunk{next, data}` or 3 `End` |
| 3 `ReadJournal{volume, journal_id, start_usn}` | 4 `Records{next_usn, data}` |
| any | 5 `Error(UsnError)` |

`data` is the raw `FSCTL_ENUM_USN_DATA` / `FSCTL_READ_USN_JOURNAL` output
without its leading 8-byte FRN/USN: consecutive `USN_RECORD_V2`/`V3` records,
parsed by the daemon. `UsnError` variants: `EntryDeleted` (journal wrapped),
`NotActive`, `IdMismatch` (journal recreated), `AccessDenied` (volume not
served or not elevated), `Stopped`, `Io(String)`. A request for a drive letter
not in the `Hello` list gets `Error(AccessDenied)`.

## Client library behaviour

- Calls never block: they enqueue and return a `Pending<T>`.
- Daemon down: `Outcome::Unavailable` immediately; reconnects at most every
  `retry_after` (3 s).
- Each request has a deadline (`request_timeout`, 2 s) -> `Outcome::TimedOut`,
  and the daemon is told to `Cancel`.
- Connection lost: every pending request resolves to `Unavailable`.
