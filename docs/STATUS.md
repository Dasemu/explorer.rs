# Status

Last updated: 2026-11-02. Nothing has been run on real Windows yet.

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
| Scripts | `install-shell.ps1`, `restore-explorer.ps1` (never executed) |
| Features | `quick-settings` back-ends, `virtual-desktops` (winvd, undocumented COM), `tray` broadcast |

## Known broken / missing

- **Shell mode does not run startup apps** (Run keys, Startup folder) or set
  explorer's "shell ready" events. Explorer does this; we do not yet.
- **No system tray** (feature `tray` is a scaffold). In shell mode, apps that
  rely on tray icons have nowhere to show them.
- **No AppBar server in shell mode.** Third-party AppBars are not arbitrated.
- No app icons in the taskbar or launcher (text only).
- Taskbar overflow shows only a `+N` count; there is no overflow menu.
- No quick-settings UI, no virtual-desktop UI.
- No toast notifications without explorer.exe (see NOTES.md, investigation
  only).
- Launcher does not watch Start Menu folders for changes. It re-indexes on
  show when the index is older than 2 minutes.
- A panic inside a window procedure aborts the process (Rust cannot unwind
  through `extern "system"`). The panic is logged first and the supervisor
  restarts the component.
