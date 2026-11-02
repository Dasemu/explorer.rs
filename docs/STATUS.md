# Status

Last updated: 2026-11-02.

## Reported from real Windows (Windows 11 25H2, build 26200.8655, 1 monitor at 100 %)

| Result | Item |
|---|---|
| Works | Dev mode starts; window list and filtering (`--dump-windows`: only real windows shown; suspended Settings hidden as cloaked) |
| Works | All checklist items in TESTING-ON-WINDOWS phase 2 pass |
| Fixed, awaiting re-test | `install-shell.ps1` failed (StrictMode `PropertyNotFoundStrict` on an empty registry key) |
| Fixed, awaiting re-test | Clicking Start while the menu was open reopened it |
| Fixed, awaiting re-test | Duplicate English/localized apps in the launcher |
| Implemented, awaiting test | No icons → taskbar, Start and tray icons |
| Implemented, awaiting test | Taskbar alignment ignored → follows `TaskbarAl` |
| Implemented, awaiting test | Start button and menu unlike Windows → Windows 11-style Start menu and four-tile Start button |
| Implemented, awaiting test | No tray → tray host, with dev-mode interception |
| Implemented, awaiting test | Theme not applied → light/dark mode and accent colour, live |

Legend: **verified** = tested on Linux / in CI; **unverified on Windows** =
compiles for `x86_64-pc-windows-msvc` but has never run on Windows;
**known broken / missing** = known gaps.

## Verified (Linux)

- Workspace builds; `cargo fmt`, `clippy -D warnings` (Linux and the MSVC
  cross target, all features), and `cargo test` (56 tests) pass.
- MSVC release binaries link with `cargo xwin` (`scripts/build-windows.sh`).
- Pure logic under unit test:
  - config parsing, validation and fallback;
  - path resolution;
  - Start Menu folder scanning and noise filtering;
  - AppsFolder entry filtering and AUMID detection;
  - catalog deduplication;
  - launch history, frecency and atomic save;
  - fuzzy search ranking;
  - window-filter rules, including UWP ghost/cloaked cases;
  - taskbar model ordering, activation and flashing;
  - Win-key tap detection;
  - supervisor backoff and crash-loop policy;
  - bar layout and hit testing;
  - clock formatting;
  - log file creation;
  - supervisor CLI safety (no mode flag means no start).
- File index (`explorer-index`, `explorer-indexd`, `explorer-index-client`):
  index, queries and ranking (checked against brute force at 1M entries),
  snapshots (round trip, every truncation and bit flip rejected), USN record
  parsing and journal replay on a simulated NTFS volume (wrap, reset, rename,
  move, delete, record reuse), walker, notification parsing, IPC framing,
  helper RPC, daemon end-to-end over Unix sockets, client timeouts/cancel/
  daemon-down. Fuzzed with cargo-fuzz (snapshot, ipc, records, query: 3 min
  each, no crashes). Benchmarks: [INDEX.md](INDEX.md#performance).

## Unverified on Windows (everything below)

| Area | Item |
|---|---|
| Supervisor | spawning, restart, job object kill-on-close, single instance, console attach, explorer.exe fallback in shell mode |
| Taskbar | AppBar reservation per monitor; work area in shell mode (`SPI_SETWORKAREA`); Per-Monitor v2 DPI and mixed-DPI layout; `WM_DISPLAYCHANGE` rebuild |
| Taskbar | window list via shell hook + WinEvents; UWP ghost/cloak filtering; flashing; click activate/minimize (foreground rules with `MA_NOACTIVATE`) |
| Taskbar | clock; right-click menus; quit (exit 77), restart (78), sign out (shell mode); full-screen app handling |
| Taskbar | Win key alone opens launcher; mask-key suppression of Start menu in dev mode; Win+X chords unaffected |
| Launcher | Start Menu + `shell:AppsFolder` enumeration; packaged-app activation; `ShellExecuteW`/`runas`; foreground acquisition (`AttachThreadInput` fallback) |
| Launcher | EDIT control colours/font/DPI; keyboard navigation; hide on deactivate; rounded corners |
| Launcher | `everything` feature (needs Everything running + Everything64.dll) |
| Scripts | `install-shell.ps1`, `restore-explorer.ps1` (fixed after the first report), `install.ps1`, `uninstall.ps1` (never executed) |
| Theme | registry values, live update on `WM_SETTINGCHANGE`, accent on taskbar |
| Icons | window icons (AUMID / WM_GETICON / class / exe), app icons via `IShellItemImageFactory`, alpha handling |
| Tray | payload layout (V2+ offsets), dev-mode interception and forwarding, Z-order re-assertion, click semantics, icons of elevated apps, re-adding after `TaskbarCreated` |
| Start menu | anchoring above the taskbar, pins and defaults, power menu (sleep via `SetSuspendState`), context menu, tooltips (comctl32 without a v6 manifest) |
| Index | NTFS MFT enumeration and USN journal via `FSCTL_*` (direct and through `explorer-rs-index-helper`); helper pipe SDDL with medium label and client image check; daemon pipe DACL, `PIPE_REJECT_REMOTE_CLIENTS`, server-SID check; overlapped pipe I/O; `ReadDirectoryChangesW` watcher; fixed-drive discovery; access filtering of results; `scripts/install-index-helper.ps1` (never executed); launcher `file-index` feature. Test plan: [TESTING-INDEX-ON-WINDOWS.md](TESTING-INDEX-ON-WINDOWS.md) |
| Features | `quick-settings` back-ends, `virtual-desktops` (winvd, undocumented COM), `tray` broadcast |

## Known broken / missing

- **Shell mode does not run startup apps** (Run keys, Startup folder) or set
  explorer's "shell ready" events. Explorer does this; we do not yet.
- Tray: no overflow ("hidden icons") flyout, no balloon/toast notifications,
  no `NIN_POPUPOPEN` hover events. Tooltips show the icon's tip text.
- **No AppBar server in shell mode.** Third-party AppBars are not arbitrated.
- No taskbar pinning; the taskbar shows running windows only.
- No jump lists; no grouping of windows by app.
- Taskbar overflow shows only a `+N` count; there is no overflow menu.
- No quick-settings UI, no virtual-desktop UI.
- No toast notifications without explorer.exe (see NOTES.md, investigation
  only).
- Launcher does not watch Start Menu folders for changes. It re-indexes on
  show when the index is older than 2 minutes.
- A panic inside a window procedure aborts the process (Rust cannot unwind
  through `extern "system"`). The panic is logged first and the supervisor
  restarts the component.
- File index: `indexd` is not in the default `supervisor.components` until
  verified on Windows. No snapshot on logoff (`WM_ENDSESSION`); recovery relies
  on periodic snapshots + journal catch-up. Hard links indexed once; no file
  sizes for NTFS roots; overlapping roots indexed twice. One-letter, path and
  fuzzy queries take 160-250 ms at 5M entries.
