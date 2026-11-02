# 0010. Privilege model of the file-name index

- Status: accepted (implementation unverified on Windows)
- Date: 2026-11-02

## Context

An Everything-style index reads the NTFS master file table (MFT) with
`FSCTL_ENUM_USN_DATA` and follows the change journal with
`FSCTL_READ_USN_JOURNAL`. Both need a handle to the volume (`\\.\C:`), and
opening a volume handle needs administrator rights. The rest of explorer.rs runs
as the normal, non-elevated user, and so should the daemon that parses
untrusted bytes, holds the index and talks to clients over IPC.

An MFT index also exposes **every file name on the volume**, regardless of the
folder ACLs. A normal directory walk would only see what the user may list.

Options:

1. **Run the whole daemon elevated** (scheduled task or service). Simple, but
   the pipe server, the query parser, the snapshot loader and all parsing run
   with admin rights. A bug in any of them becomes a privilege escalation.
   Clients (the launcher) talk to an elevated process.
2. **Elevated helper hands a volume handle to the daemon** (`DuplicateHandle`).
   The daemon then issues the FSCTLs itself. But a volume handle opened for
   read gives raw access to every sector: the unprivileged daemon (and anything
   that can inject into it) could read any file's *contents*, bypassing ACLs.
   That is worse than option 3.
3. **Minimal elevated helper that streams records.** A tiny elevated process
   opens the volumes and answers only three questions: journal info, one MFT
   chunk, one journal chunk, returning the raw output bytes. The daemon stays
   unprivileged and does all parsing.
4. **Windows service** (LocalSystem) instead of a scheduled task. Always on,
   but needs a service install, runs as SYSTEM (more privilege than needed),
   and its pipe must then be opened to the user explicitly.
5. **No MFT at all**: walk + `ReadDirectoryChangesW`. No privileges needed,
   ACLs respected, but the first scan of a large volume takes minutes and
   watching whole volumes is less reliable (buffer overflows force rescans).

## Decision

Option 3, with option 5 as the automatic fallback.

- `explorer-rs-index-helper.exe` runs elevated **as the logged-on user** from a
  logon scheduled task ("Run with highest privileges"). It is installed only by
  the explicit script `scripts/install-index-helper.ps1`; nothing installs or
  starts it automatically.
- It refuses to run unless elevated, enumerates fixed NTFS drives itself and
  only accepts drive letters from that list.
- Its pipe is `\\.\pipe\explorer-rs.index-helper.<user SID>` with the SDDL
  `D:P(A;;GA;;;<user SID>)S:(ML;;NW;;;ME)`: protected DACL granting only the
  user, and a *medium* mandatory label so that the non-elevated daemon may
  connect to a pipe owned by a high-integrity process. Remote clients are
  rejected (`PIPE_REJECT_REMOTE_CLIENTS`); the first instance is created with
  `FILE_FLAG_FIRST_PIPE_INSTANCE` to detect squatting.
- It accepts only clients whose image path is `explorer-rs-indexd.exe` in the
  helper's own directory (`GetNamedPipeClientProcessId` +
  `QueryFullProcessImageNameW`).
- It parses nothing: it forwards raw `FSCTL_*` output. Its input surface is
  four small, fixed messages (fuzzed: `fuzz/fuzz_targets/ipc.rs`).
- The daemon (`explorer-rs-indexd.exe`) always runs non-elevated. If it happens
  to be elevated it uses the volume directly; otherwise it tries the helper;
  otherwise it walks.
- The daemon's own pipe is `\\.\pipe\explorer-rs.index.<user SID>` with
  `D:P(A;;GA;;;<user SID>)`, `PIPE_REJECT_REMOTE_CLIENTS`. Clients verify that
  the server process runs as the same user (`GetNamedPipeServerProcessId`) and
  connect with `SECURITY_IDENTIFICATION` so a rogue server cannot impersonate
  them.

### Privacy

Because the MFT exposes names in folders the user cannot open, results are
filtered: with `[index] check_access = true` (default) every result whose
parent folder the daemon (running as the user, non-elevated) cannot list is
dropped. The check is done lazily for the page being returned, once per
distinct folder. `total` in replies counts matches *before* filtering, which can
reveal that hidden matches exist; this is documented in docs/IPC-PROTOCOL.md.
Users who want less in the index can narrow `roots` (e.g. `["%USERPROFILE%"]`)
or add `exclude_paths`.

The snapshot file (`%LOCALAPPDATA%\explorer-rs\state\index.snapshot`) contains
all indexed names, including those hidden by the filter. It inherits the
per-user ACL of `%LOCALAPPDATA%`.

## Consequences

- Least privilege: only a few hundred lines (helper main, `helper::serve`,
  `win::ntfs`, `win::volumes`, the pipe transport and frame decoding) ever run
  elevated. A compromise of the daemon yields file
  *names*, not contents and not admin rights.
- The elevated task must run a binary that non-administrators cannot replace;
  the install script checks the ACLs and refuses otherwise.
- One extra process and one more install step for the fast path. Without it,
  everything still works through walking, just with a slower first scan.
- Unverified on Windows: whether the medium-label SDDL is accepted as written,
  whether `FSCTL_ENUM_USN_DATA` with `MaxMajorVersion = 3` returns V2 or V3
  records on NTFS (both are parsed), and how fast a full MFT scan is through
  the helper pipe. See docs/TESTING-INDEX-ON-WINDOWS.md.
- Hard links: one entry per MFT record (first name seen); other names of the
  same file are not indexed.
