# Lessons

Non-obvious findings. Add an entry whenever something surprises you.

- **`SHAppBarMessage` needs explorer.exe.** It is delivered as `WM_COPYDATA` to
  the `Shell_TrayWnd` window. As the real shell (no explorer), nobody answers.
  We must set the work area ourselves (`SPI_SETWORKAREA`), and eventually
  implement the AppBar server side.
- **A typo'd constant in a `match` arm becomes a catch-all binding.** When
  `WM_MOUSELEAVE` was not exported where we imported it, rustc first reported
  the import error. The arm `WM_MOUSELEAVE => ...` would otherwise have
  silently matched every message. Keep `unreachable_patterns` warnings fatal
  (clippy `-D warnings` does this).
- **Window procedures re-enter.** `SetWindowPos`, `SetWindowTextW` (EN_CHANGE)
  and others send messages synchronously while our handler is running. The
  handler lives in a `RefCell`; nested messages fall back to `DefWindowProcW`
  instead of creating aliasing `&mut`. Code must not rely on receiving those
  nested messages; e.g. the launcher refreshes results itself after
  clearing the query.
- **Lone Win key suppression:** swallowing the Win key-up confuses the system's
  key state. Injecting an unassigned VK (0xE8) between Win-down and Win-up
  makes Windows see a chord, so Start does not open, and nothing is swallowed.
- **GUI-subsystem binaries and consoles:** `windows_subsystem = "windows"`
  avoids console flashes when Winlogon/the supervisor start us, but `>`
  redirection and console output become unreliable. Diagnostics take
  `--output <file>`, and binaries call `AttachConsole(ATTACH_PARENT_PROCESS)`.
- **Clicking the taskbar must not activate it** (`MA_NOACTIVATE`), otherwise
  "click the active window's button to minimize it" sees the taskbar as the
  active window.
- **windows-rs 0.62:** `BOOL` lives in `windows::core`. `CreateMutexW` and
  `CreateJobObjectW` need the `Win32_Security` feature. Many functions take
  `Option<HWND>`.
- **`winvd` 0.0.49 depends on windows-rs 0.58**, so two `windows` versions end
  up in the graph when `virtual-desktops` is on. Its HWND type differs from
  ours; keep its API surface to index-based calls.
- **cargo-xwin** works with the stable toolchain in the container. The first
  *link* downloads the MSVC CRT/SDK (cached afterwards); `check`/`clippy` do
  not need it.
- **Start Menu `.lnk` names are not what users see.** Windows localizes them
  via `desktop.ini` (`LocalizedResourceName`), so scanning the folders shows
  "Command Prompt" on a Spanish system that displays "Símbolo del sistema".
  `shell:AppsFolder` returns the localized names; use it as the source.
- **PowerShell StrictMode + registry:** `(Get-ItemProperty key).PSObject.Properties.Name`
  throws `PropertyNotFoundStrict` when the key has no values. Read values
  through `(Get-Item key).GetValueNames()` / `.GetValue()` instead.
- **Toggling a popup from a button that deactivates it:** the mouse-down on
  the Start button deactivates (and hides) the menu before the click's
  Toggle arrives, which then reopens it. Remember when we hid on
  deactivation and ignore a Toggle within ~400 ms.
- **Tray in dev mode:** `Shell_NotifyIcon` uses `FindWindow("Shell_TrayWnd")`,
  which returns the first window in Z order. A second, topmost
  `Shell_TrayWnd` that forwards everything to explorer's lets a dev-mode
  taskbar see tray icons without breaking explorer.
- **Tooltips without a manifest:** comctl32 v5 is loaded unless the exe has
  a v6 manifest. Use `TTTOOLINFOW` V2 size (without `lpReserved`), and call
  `InitCommonControlsEx` first, or the class does not exist.

## File index (crates/explorer-index, explorer-indexd)

- **Synchronous pipe handles serialize I/O.** On a handle opened without
  `FILE_FLAG_OVERLAPPED`, a `ReadFile` blocked in one thread makes a
  `WriteFile` from another thread on the same handle wait. A duplex protocol
  (reader thread + writer) therefore needs overlapped pipes; we wait on a
  per-operation event to keep a blocking API, and `CancelIoEx` unblocks reads.
- **Elevated pipe servers need an explicit medium integrity label.** Objects
  created by a high-integrity process get a high mandatory label with
  no-write-up, so the user's normal processes cannot open the pipe even though
  the DACL allows them. The helper's SDDL adds `S:(ML;;NW;;;ME)`. (Expected
  from documentation; unverified on Windows.)
- **USN replay does not need rename pairing.** Every USN record carries the
  file's current name and parent, so "upsert from any record except delete and
  rename-old" is idempotent and handles create, rename, move and data changes
  alike. Replaying a range twice is harmless, which makes crash recovery easy.
- **MFT enumeration is in record-number order, not tree order.** Children come
  before their parents. Collect everything, build child lists (CSR) by parent
  record, then BFS from the root record (5); unreachable records (metafiles,
  `$Extend`, orphans) fall out naturally.
- **Load snapshots through the normal insert path.** Rebuilding costs ~0.25 s
  per million entries, but a corrupted or malicious snapshot can then never
  create cycles or dangling references, and the fuzzer has nothing to find in
  cross-array consistency checks.
- **Snapshot encoding runs under the read lock, so its cost matters.**
  Restoring the original case bytewise instead of `chars().collect()` and
  dropping a separate counting pass took 1M-entry encoding from 750 to 605 ms.
- **Probing a Unix socket leaves a connection in the backlog.** "Is another
  daemon alive?" via `connect()` makes the live daemon accept an empty
  connection; harmless, but tests must expect it.
- **Synthetic trees need unique sibling names.** Real file systems guarantee
  them (case-insensitively on Windows); a generator that does not makes path
  lookups descend into the wrong twin and looks like an index bug.
- **`Modify`/`FullControl` include read bits.** An ACL check "is this file
  writable by non-admins" must mask with write-only rights (`WriteData`,
  `AppendData`, `Delete`, `ChangePermissions`, ...), or every reader is flagged.
- **cargo-fuzz inside a nested checkout** (git worktree below the main repo)
  picks up the parent directory's workspace; `fuzz/Cargo.toml` needs its own
  `[workspace]` table.

## Taskbar flyouts

- **Never `DestroyWindow` a window from inside its own handler** with the
  `RefCell` handler model: the nested `WM_NCDESTROY` frees the slot while the
  outer borrow is alive. The taskbar reuses one hidden flyout window and keeps
  its state in shared data instead.
- **`MA_NOACTIVATE` on the bar makes flyout toggling simple**: clicking the
  opener does not deactivate the open flyout, so the click can close it;
  the deactivation debounce is only a safety net.
