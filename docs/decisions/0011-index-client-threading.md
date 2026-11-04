# 0011. Index client and daemon: threads and channels, no async runtime

- Status: accepted
- Date: 2026-11-02

## Context

The launcher must query the index on every keystroke without ever blocking its
UI thread, cope with the daemon being absent or restarting, and cancel stale
queries. The daemon serves a handful of local clients at most.

## Decision

No async runtime (tokio, async-std, smol). The client is a background thread
that owns the connection plus one reader thread per connection; the API hands
out `Pending<T>` handles backed by `std::sync::mpsc` channels, with optional
`notify` callbacks (e.g. post a window message) and per-request deadlines.
The daemon uses one thread per connection and one query thread per active
query.

Named pipes are opened with `FILE_FLAG_OVERLAPPED` and every operation waits
on its own event, so a blocked read does not serialize writes (as it would on a
synchronous handle) and pending I/O can be cancelled with `CancelIoEx`.

## Consequences

- No large dependency, fast builds, simple stack traces, nothing to configure.
- Thread count is bounded by `MAX_CONNECTIONS` (32) and one query per
  connection.
- If the index is ever exposed to many concurrent clients, revisit.
