# Status

Release candidate: feature complete, awaiting testing on real Windows.
Last updated: 2026-11-04.

Legend: **reported** = confirmed by a tester on Windows; **implemented,
unverified** = compiles for `x86_64-pc-windows-msvc` and is covered by Linux
tests where the logic is pure, but has never run on Windows.

## Reported from real Windows (Windows 11 25H2, build 26200.8655, 1 monitor at 100 %)

| Result | Item |
|---|---|
| Works | Dev mode starts; window list and filtering (`--dump-windows`: only real windows shown; suspended Settings hidden as cloaked) |
| Works | All checklist items of the round-1 plan (dev mode checklist 1-14) pass |
| Fixed, awaiting re-test | `install-shell.ps1` failed (StrictMode `PropertyNotFoundStrict` on an empty registry key) |
| Fixed, awaiting re-test | Clicking Start while the menu was open reopened it |
| Fixed, awaiting re-test | Duplicate English/localized apps in the launcher |
| Implemented, awaiting test | No icons → taskbar, Start and tray icons |
| Implemented, awaiting test | Taskbar alignment ignored → follows `TaskbarAl` |
| Implemented, awaiting test | Start button and menu unlike Windows → Windows 11-style Start menu and four-tile Start button |
| Implemented, awaiting test | No tray → tray host, with dev-mode interception |
| Implemented, awaiting test | Theme not applied → light/dark mode and accent colour, live |

This is the round-1 history. Every "awaiting re-test" row is now part of the
release-candidate plan in [TESTING-ON-WINDOWS.md](TESTING-ON-WINDOWS.md).

## Implemented, unverified on Windows

Test plan: [TESTING-ON-WINDOWS.md](TESTING-ON-WINDOWS.md). Feature docs are
linked per area.

### Installer ([PACKAGING.md](PACKAGING.md), [INSTALL.md](INSTALL.md))

| Item |
|---|
| `explorer-rs-setup.exe`: bilingual Win32 wizard (Welcome, Options, Progress, Done), light/dark, silent mode (`--silent --mode= --file-search= --no-launch`), exit codes 0/1/2/3 |
| Upgrade in place keeping user data; per-user settings written for the signed-in user (HKU\<SID>), also with over-the-shoulder elevation |
| Start modes alongside / shell / none; rollback of the shell registration on failure |
| Fast file search scheduled task `\explorer-rs\IndexHelper`; `\explorer-rs\Launch` task for unelevated launch; Start menu shortcuts (incl. "Restore Windows Explorer"); Installed apps entry; log in `%ProgramData%\explorer-rs\setup.log` |
| Uninstaller (`uninstall.exe --uninstall [--silent] [--remove-user-data]`): restores the shell for every signed-in user pointing into the install folder, removes tasks, shortcuts, ARP entry, files |
| Icon, manifest (PerMonitorV2, comctl32 v6, long paths, UTF-8) and version info embedded in every binary |

### Recovery and shell mode ([SHELL-MODE.md](SHELL-MODE.md), [RECOVERY.md](RECOVERY.md))

| Item |
|---|
| Guardian (`explorer-rs --dev\|--shell`) restarting the supervisor (`--supervise`); crash log `%LOCALAPPDATA%\explorer-rs\state\crashlog.toml`; crash-loop fallback dialog; automatic restore of Windows Explorer after 3 failed sign-ins |
| Emergency hotkeys Ctrl+Alt+Shift+E (Windows Explorer) / Ctrl+Alt+Shift+R (restart all) |
| `--restore-explorer`, `--restart`, `--edit-config`, `--status [--output]` |
| Config hot reload (2 s polling, per-component restart); `supervisor.file_search` (auto/on/off, HKLM `FileSearch`) |
| Session-end detection (no restarts and no crash counting during sign-out) |
| Shell mode: "shell ready" events; startup apps (Run, RunOnce, Startup folder, StartupApproved) |
| Supervisor: spawning, restart, job object kill-on-close, single instance, console attach |

### Desktop

| Item |
|---|
| `explorer-rs-desktop` wallpaper window (Progman, `SetShellWindow`): fit styles, solid colour, multi-monitor span, context menu, live background changes; `[desktop] enabled` = shell/always/never |

### Taskbar ([TASKBAR.md](TASKBAR.md))

| Item |
|---|
| AppBar reservation per monitor; work area in shell mode (`SPI_SETWORKAREA`); Per-Monitor v2 DPI and mixed-DPI layout; `WM_DISPLAYCHANGE` rebuild |
| Window list via shell hook + WinEvents; UWP ghost/cloak filtering; flashing; click activate/minimize; full-screen app handling |
| Grouping by app (AUMID/exe), `taskbar.combine` |
| Pins (shared file, seeding, mtime reload, launch, pin/unpin, drag reorder) |
| Window picker with DWM thumbnails (hover + sticky), minimized fallback |
| "..." app overflow flyout; show-desktop corner |
| Clock; right-click menus; quit (exit 77), restart (78), sign out (shell mode); bilingual menus and tooltips |
| Win key alone opens the launcher; mask-key suppression of explorer's Start menu in dev mode; Win+X chords unaffected |
| Theme (light/dark, accent, live), alignment (`TaskbarAl`), window and app icons (AUMID / WM_GETICON / class / exe, `IShellItemImageFactory`) |

### Tray

| Item |
|---|
| Payload layout (V2+ offsets), dev-mode interception and forwarding, Z-order re-assertion, click semantics, icons of elevated apps, re-adding after `TaskbarCreated` |
| Promotion via `NotifyIconSettings` + `^` overflow flyout, `tray.show_all` |

### Quick settings and flyouts

| Item |
|---|
| System glyphs and Quick settings: volume/mute/device (WASAPI), WMI brightness, network (NLM), battery |
| Calendar flyout (locale first weekday, bilingual); flyout infrastructure (dismiss, DPI, per-monitor placement) |
| `virtual-desktops` feature (winvd, undocumented COM; no UI) |

### Start menu ([START-MENU.md](START-MENU.md))

| Item |
|---|
| Windows 11-style menu above the taskbar, bilingual UI and search placeholder; start button toggle without reopening |
| Sectioned search (Best match, Apps, Settings, Documents, Commands) with detail panel actions |
| Settings pages and system tools (`ms-settings:`, `.msc`, Terminal only if installed) |
| Calculator (Enter copies); run commands (`>cmd`, paths, programs via App Paths/SearchPathW), never URLs |
| Recommended: recent files (IShellLinkW, honours Start_TrackDocs), frequently used apps |
| Pins with pages (>18), Pin to taskbar, Uninstall, Remove from list, Move to front |
| Footer: account picture, File Explorer, Settings, "..." (restart Start menu), power menu (`GetPwrCapabilities`) |
| Keyboard focus zones, focus ring, Esc semantics, context-menu key |
| `shell:AppsFolder` + Start Menu enumeration, packaged-app activation, `ShellExecuteW`/`runas`, foreground acquisition, EDIT control colours/DPI, rounded corners, tooltips |
| `everything` feature (needs Everything running + Everything64.dll) |

### File index ([INDEX.md](INDEX.md), [TESTING-INDEX-ON-WINDOWS.md](TESTING-INDEX-ON-WINDOWS.md))

| Item |
|---|
| NTFS MFT enumeration and USN journal via `FSCTL_*` (direct and through `explorer-rs-index-helper`) |
| Helper pipe SDDL with medium label and client image check; daemon pipe DACL, `PIPE_REJECT_REMOTE_CLIENTS`, server-SID check; overlapped pipe I/O |
| `ReadDirectoryChangesW` watcher; fixed-drive discovery; access filtering of results |
| Launcher `file-index` feature ("Files" results) |

### Scripts

| Item |
|---|
| `install-shell.ps1`, `restore-explorer.ps1` (fixed after the first report), `install.ps1`, `uninstall.ps1`, `install-index-helper.ps1` (none executed on Windows since the fixes) |

## Known limitations

Shell mode and desktop:
- **No desktop icons.** The desktop shows the wallpaper only; Desktop-folder
  files are reachable through File Explorer or the Start menu.
- **No AppBar server in shell mode.** Third-party AppBars are not arbitrated.
- No notification toasts / Action Center of explorer.exe's design (see
  NOTES.md, investigation only).
- Slideshow and Spotlight wallpapers show the current image; the slideshow is
  not advanced.
- Explorer-only features (Win+X menu, Task View hosting, ...) are not provided.

Taskbar and tray:
- No jump lists, no "recent" items in the context menu.
- No Wi-Fi signal strength, Wi-Fi/Bluetooth/airplane toggles or external
  monitor (DDC/CI) brightness; no virtual-desktop UI.
- Tray: no balloon/toast notifications, no `NIN_POPUPOPEN` hover events. In
  shell mode nobody creates `NotifyIconSettings` entries for new apps, so
  their icons stay in the overflow until promoted or `tray.show_all = true`.
- Path pins not in `shell:AppsFolder` show the file stem as name. Dragging
  unpinned buttons, and dragging with `combine = "never"`, is unsupported.
- No drop shadow on flyouts.

Installer:
- The uninstaller does not check signed-out users for a shell registration.
- Installer is not code signed (SmartScreen warning); see PACKAGING.md.

Other:
- The launcher does not watch Start Menu folders; it re-indexes on show when
  the index is older than 2 minutes.
- A panic inside a window procedure aborts the process (Rust cannot unwind
  through `extern "system"`). It is logged first and the component restarted.
- File index: no snapshot on logoff (`WM_ENDSESSION`); recovery relies on
  periodic snapshots + journal catch-up. Hard links indexed once; no file
  sizes for NTFS roots; overlapping roots indexed twice. One-letter, path and
  fuzzy queries take 160-250 ms at 5M entries.

## Verified on Linux

- 304 unit and integration tests pass (`cargo test --workspace`).
- `cargo fmt --all --check` and `cargo clippy -D warnings` pass for Linux and
  for the `x86_64-pc-windows-msvc` target (all features); `cargo-deny` passes.
- MSVC release binaries and the installer link with `cargo xwin`
  (`scripts/build-windows.sh`).
- Pure logic under test: config parsing and fallback, paths, app catalog and
  frecency, fuzzy search, window-filter rules (UWP ghost/cloaked), taskbar
  model, groups and layout, tray protocol and promotion, calendar, Win-key
  state machine, supervisor/guardian policy and crash log, startup-app
  selection, wallpaper layout, Start menu logic (settings pages, calculator,
  run commands, recent files, results), installer planning and task XML,
  supervisor CLI safety.
- File index: queries and ranking (vs brute force at 1M entries), snapshots
  (every truncation and bit flip rejected), USN parsing and replay on a
  simulated NTFS volume, walker, IPC framing, helper RPC, daemon end-to-end
  over Unix sockets, client timeouts. Fuzzed with cargo-fuzz (snapshot, ipc,
  records, query: 3 min each, no crashes). Benchmarks:
  [INDEX.md](INDEX.md#performance).
