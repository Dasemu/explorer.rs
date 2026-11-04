# Architecture

explorer.rs replaces the web-based parts of the Windows 11 shell (Start,
Search, Widgets, Copilot) with small native programs. Reliability and speed
come first; looks are deliberately plain. No WebView, Electron, Chromium or
web content anywhere.

## Processes

```
explorer-rs-setup.exe / uninstall.exe   installer and uninstaller (elevated)
explorer-rs.exe  (guardian, --dev | --shell)
 └── explorer-rs.exe --supervise   (supervisor, kill-on-close job object)
      ├── explorer-rs-taskbar.exe   taskbar: bars, window list, grouping,
      │                             pins, tray, quick settings, calendar,
      │                             Win-key hook, shell hooks
      ├── explorer-rs-launcher.exe  Windows 11-style Start menu + search
      ├── explorer-rs-desktop.exe   wallpaper desktop (shell mode)
      └── explorer-rs-indexd.exe    file-name index daemon (optional)
           └── explorer-rs-index-helper.exe   elevated MFT/USN helper,
                                              started by a scheduled task
```

- **Guardian** (`explorer-rs.exe --dev|--shell`): not in a job object, no COM.
  It starts and restarts the supervisor, owns the emergency hotkeys
  (Ctrl+Alt+Shift+E starts Windows Explorer, Ctrl+Alt+Shift+R restarts
  everything), keeps the crash log
  (`%LOCALAPPDATA%\explorer-rs\state\crashlog.toml`), and is the only
  process that talks to the user about recovery (crash-loop dialog,
  automatic restore of Windows Explorer after 3 failed sign-ins).
  `explorer-rs.exe --restart` posts the restart command to its message-only
  window (class `ExplorerRs.Hotkeys`). Details: [SHELL-MODE.md](SHELL-MODE.md),
  [RECOVERY.md](RECOVERY.md), ADR 0014.
- **Supervisor** (`--supervise`): runs the components from
  `supervisor.components`, hot-reloads `config.toml` (2 s polling,
  per-component restart) and detects session end so sign-out does not count
  as a crash. `supervisor.file_search` (auto/on/off, or the installer's
  `HKLM\Software\explorer-rs\FileSearch`) decides whether `indexd` runs.
- Each component is a separate process (ADR 0002). The supervisor restarts a
  crashed component with exponential backoff (500 ms doubling to 30 s, reset
  after 60 s of healthy uptime). It gives up on a component after more than
  5 crashes in 60 s. Other components are unaffected.
- Exit codes: `0` stop (no restart), `77` quit all of explorer.rs, `78`
  restart that component, `79` fallback to Windows Explorer needed
  (supervisor to guardian), `80` supervisor already running. Anything else
  counts as a crash.
- Components are in a kill-on-close job object, so they die with the
  supervisor (no orphaned AppBars).
- If the taskbar is given up on in shell mode, the guardian offers to start
  Windows Explorer (crash-loop dialog).
- Every binary is single-instance per session (named mutex). The desktop
  component exits immediately unless `[desktop] enabled` says otherwise
  (default `shell`: only in shell mode).
- **Installer** (`explorer-rs-setup.exe`, copied as `uninstall.exe`): a Win32
  wizard or silent CLI. It copies the binaries, registers the start mode
  (alongside / shell / none), creates the `\explorer-rs\IndexHelper` and
  `\explorer-rs\Launch` scheduled tasks and shortcuts (including "Restore
  Windows Explorer", which runs `explorer-rs.exe --restore-explorer`), and
  writes `%ProgramData%\explorer-rs\setup.log`. See
  [PACKAGING.md](PACKAGING.md), [INSTALL.md](INSTALL.md), ADR 0013.

## Inter-process communication

Deliberately minimal (ADR 0002): components find each other by window class
(`ExplorerRs.Launcher`, `ExplorerRs.Taskbar`, `ExplorerRs.Hotkeys`) and talk
with registered window messages such as `ExplorerRs.LauncherCommand` (wParam:
0 toggle, 1 show, 2 hide). A message to a dead peer is simply dropped. Before
posting, the taskbar calls `AllowSetForegroundWindow` for the launcher's PID,
so the launcher may take the foreground. The file index uses a per-user named
pipe ([IPC-PROTOCOL.md](IPC-PROTOCOL.md)).

## Crates

| Crate | Kind | Contents | Tested on Linux |
|---|---|---|---|
| `explorer-core` | lib | config, paths, logging, app catalog + launch history, fuzzy search (`nucleo-matcher`), window-filter rules, taskbar model/groups/layout, tray promotion, calendar, system glyphs, Win-key state machine, supervisor policy, `crashlog`, `guardian`, `components`, `startup`, `wallpaper`, `setup`, Start menu logic (`settings_pages`, `calc`, `run_command`, `recent`, `results`) | yes, fully |
| `explorer-win` | lib | thin Win32/COM wrappers (theme, icons, tray, quick settings back-ends, `session`, `process`, `registry`, `startup`, `wallpaper`, `desktop`, `dialog`); empty on non-Windows | compiles only |
| `explorer-supervisor` | bin `explorer-rs` | guardian and supervisor (`--supervise`), `--status`, `--edit-config`, `--restore-explorer`, `--restart` | yes (CLI + policy) |
| `explorer-taskbar` | bin `explorer-rs-taskbar` | taskbar UI | cross-compiled |
| `explorer-launcher` | bin `explorer-rs-launcher` | Start menu UI | cross-compiled |
| `explorer-desktop` | bin `explorer-rs-desktop` | wallpaper desktop window (Progman, `SetShellWindow`) | cross-compiled |
| `explorer-setup` | bins `explorer-rs-setup` (+ `uninstall.exe` copy), `explorer-setup-pack` (host tool) | installer/uninstaller: pure planning in `payload`, `cli`, `plan`, `task_xml`; Win32 execution and wizard in `win` | planning yes; wizard cross-compiled |
| `explorer-index` | lib | file-name index engine, queries, snapshots, USN/MFT parsing and replay, walker, IPC protocol; `win::` FSCTL backend and watcher | yes (Windows parts compile only) |
| `explorer-index-client` | lib | non-blocking index client | yes |
| `explorer-indexd` | bins `explorer-rs-indexd`, `explorer-rs-index-helper` | index daemon + CLI, elevated MFT/USN helper | yes (daemon end-to-end over Unix sockets) |

`assets/` holds the icon (and its generator), the manifest template and the
shared resource build script used by every binary's `build.rs` (icon,
PerMonitorV2/comctl32 v6/long-path/UTF-8 manifest, version info).

Rule: decisions live in `explorer-core` as pure functions over plain data.
`explorer-win` only collects data from the OS and executes actions. For
example, `winlist::snapshot` fills a `WindowInfo` and
`window_filter::FilterRules::classify` decides whether that window gets a
button, and why.

## Taskbar

- A hidden **controller** window owns the `TaskbarModel`, the shell hooks,
  the Win-key hook, the clock timer and a 30 s full-resync safety net. It
  rebuilds the bars on `WM_DISPLAYCHANGE` (debounced) and on `TaskbarCreated`
  (explorer restarted in dev mode).
- One **bar** window per monitor (`WS_EX_TOOLWINDOW | WS_EX_TOPMOST`), drawn
  with Direct2D/DirectWrite in physical pixels. The scale is the monitor DPI / 96
  (Per-Monitor v2 awareness).
- Window events come from `RegisterShellHookWindow` (created, destroyed,
  activated, flash, redraw) plus out-of-context `SetWinEventHook` (name
  change, show/hide, cloak/uncloak, foreground, minimize). Each event triggers
  a fresh snapshot of that window, which is re-filtered.
- Filtering rules (`explorer-core/src/window_filter.rs`):
  - Hidden: not visible, child, shell classes, user exclusions, DWM-cloaked
    (suspended UWP, other virtual desktops), and UWP ghost frames
    (`ApplicationFrameWindow` without a `Windows.UI.Core.CoreWindow` child).
  - `WS_EX_APPWINDOW` forces a button. Otherwise tool windows,
    `WS_EX_NOACTIVATE`, owned windows and untitled windows are hidden.
- Clicking a button activates the window, or minimizes it if it is already
  active. Both use `WM_SYSCOMMAND` like Windows does. The bar returns
  `MA_NOACTIVATE` so clicking it does not steal activation.
- Screen space (ADR 0003): `SHAppBarMessage` in dev mode. In shell mode,
  `SPI_SETWORKAREA` per monitor, restored on exit, because no explorer.exe
  is there to answer AppBar messages.
- Win key (ADR 0005): a `WH_KEYBOARD_LL` hook feeds a pure state machine. On
  a lone tap it injects an unassigned "mask" key (VK 0xE8) so explorer's
  Start menu does not also open, then toggles the launcher. No key is ever
  swallowed, so Win+E, Win+L etc. keep working.

- Process layout (details: [TASKBAR.md](TASKBAR.md)): the controller window
  owns the model, hooks, timers and workers; one `Bar` per monitor; one
  reusable flyout window (window picker, "..." overflow, tray overflow, quick
  settings, calendar). Worker threads: `taskbar-shell` (STA: icons,
  AppsFolder catalog) and `taskbar-sys` (MTA: WASAPI, NLM, WMI).
- Features: grouping of windows by app (AUMID/exe, `taskbar.combine`), pins
  shared with the Start menu (`taskbar-pins.toml`, reloaded on mtime change,
  drag to reorder), window picker with DWM thumbnails, show-desktop corner,
  bilingual menus and tooltips.
- Pure logic in `explorer-core`: `taskbar_groups`, `taskbar_layout`,
  `flyout_layout`, `tray_promotion`, `shellpath`, `calendar`, `system_glyphs`.

## Theme and alignment

`explorer-win::theme` reads the Windows personalisation settings from HKCU:
- `SystemUsesLightTheme` (light/dark);
- `ColorPrevalence` (accent on Start and taskbar);
- `DWM\AccentColor`;
- `TaskbarAl` (centred or left).

`explorer_core::theme::Palette` turns them into colours. Both processes
re-read them on `WM_SETTINGCHANGE`, so changes apply live. Config
`general.theme` and `taskbar.alignment` can override them.

## Icons

`explorer-win::icons` produces CPU images (`explorer_core::image::Image`,
premultiplied BGRA). Each render target caches its own GPU copy.

| What | Source |
|---|---|
| Apps | `IShellItemImageFactory` on `shell:AppsFolder\<id>` or the file path; extracted on the launcher's worker thread |
| Windows | AppUserModelID from the window property store (packaged apps / UWP frames), then `WM_GETICON` (with `SendMessageTimeout`), then the class icon, then the process image |
| Tray | `HICON` drawn into a 32-bit DIB; alpha is derived from the mask for legacy icons |

## System tray and quick settings

- Pure protocol parsing, the icon model and promotion rules live in
  `explorer_core::tray` and `tray_promotion` (unit-tested, including
  truncated input).
- `explorer-win::tray` hosts a `Shell_TrayWnd` window. Icons promoted in
  Windows' own settings (`NotifyIconSettings`) sit on the bar; the others are
  behind a `^` overflow flyout (`tray.show_all` shows all). Clicks are
  forwarded to the owning application using the documented
  `NOTIFYICON_VERSION_4` or legacy callback semantics.
- **Dev mode:** our invisible `Shell_TrayWnd` is kept first in Z order, so
  `Shell_NotifyIcon`'s `FindWindow` reaches us. Every message is forwarded
  to explorer's real tray window, so explorer keeps working. At start-up we
  broadcast `TaskbarCreated` so running apps re-add their icons. Disable
  with `tray.intercept_in_dev_mode = false`.
- **Shell mode:** we are the only tray. AppBar traffic (`dwData == 0`) is
  not served yet.
- **Quick settings** (volume/mute/device via WASAPI, brightness via WMI,
  network via NLM, battery) and the **calendar** flyout open from the system
  glyphs and the clock. The back-ends live in `explorer-win` and are always
  built.

## Start menu (launcher)

`explorer-rs-launcher` is a Windows 11-style Start menu anchored above the
taskbar (centred or left, following the alignment); it is not just a search
box. Views: Home (pinned grid with pages, Recommended = recent files and
frequently used apps, footer with account, File Explorer, Settings and power
menu), All apps, and sectioned Search (Best match, Apps, Settings, Documents,
Commands, calculator, run commands) with a detail panel. The UI is bilingual
(English/Spanish). User-facing details: [START-MENU.md](START-MENU.md).

Pins are stored in `%LOCALAPPDATA%\explorer-rs\pins.toml`, seeded once with
common apps that exist on the machine. Taskbar pins are separate
(`taskbar-pins.toml`).

Decisions live in `explorer-core` (`settings_pages`, `calc`, `run_command`,
`recent`, `results`) and the portable `model.rs`; the Windows layer
(`app.rs`, `sys.rs`, `worker.rs`, `cue.rs`) only draws rows and runs actions.
Background work: indexer thread (apps + icons), `start-aux` (program
resolution, recent files, avatar, optional tools), `start-icons` (on-demand
icons), file-results worker.

### Launcher internals

- Sources (ADR 0004, amended):
  - `shell:AppsFolder` via `IShellItem`/`IEnumShellItems` is primary. It
    contains every Start Menu shortcut with its **localized** name, plus
    packaged apps.
  - Scanning the Start Menu folders with `std::fs` is only a fallback.
    Those `.lnk` file names are often English, which caused duplicates
    such as "Command Prompt" next to "Símbolo del sistema".
  - Config `extra_folders` is added on top.
- Indexing runs on a worker thread at startup, and again on show if the index
  is older than 2 minutes.
- Search: `nucleo-matcher` fuzzy score, plus a prefix/word-prefix bonus,
  plus a frecency boost from `launch-history.toml` (written atomically).
- Launch, on a worker thread: packaged apps go through
  `IApplicationActivationManager::ActivateApplication`, which works without
  explorer.exe. Everything else uses `ShellExecuteW` (`runas` with
  Ctrl+Shift+Enter).
- UI: a native EDIT control for the query (IME, clipboard). The message loop
  intercepts navigation keys before the EDIT control sees them. Results are
  drawn with Direct2D. The window hides on Esc or on deactivation.
- Optional `everything` feature: Everything64.dll is loaded at runtime and
  queried on a dedicated worker thread. Results appear as "File" rows.

## Configuration, logs, state

| What | Where |
|---|---|
| Config | `%APPDATA%\explorer-rs\config.toml` (documented default: `config/default.toml`, embedded and tested to equal `Config::default()`) |
| Logs | `%LOCALAPPDATA%\explorer-rs\logs\<component>.YYYY-MM-DD.log` (`guardian`, `supervisor`, `taskbar`, ...), daily rotation, 7 files kept |
| Launch history | `%LOCALAPPDATA%\explorer-rs\launch-history.toml` |
| Pins | `%LOCALAPPDATA%\explorer-rs\pins.toml` (Start), `taskbar-pins.toml` (taskbar) |
| Guardian state | `%LOCALAPPDATA%\explorer-rs\state\crashlog.toml` |
| Installer log | `%ProgramData%\explorer-rs\setup.log` |
| Override | `EXPLORER_RS_HOME` puts everything under one folder; `EXPLORER_RS_LOG` sets the tracing filter |

A broken config never prevents startup: defaults are used and the error is
logged. Unknown keys are rejected, so typos are reported instead of being
ignored. Panics are logged with a backtrace by a panic hook before the
process dies.

## Desktop and shell mode

`explorer-rs-desktop` draws the wallpaper behind all windows (Progman,
`SetShellWindow`), following the Windows background settings. In shell mode
the supervisor side also signals the "shell ready" events and runs the
startup apps (Run/RunOnce keys, Startup folder, honouring StartupApproved).
There are no desktop icons. Details and limits: [SHELL-MODE.md](SHELL-MODE.md).

## File index

`explorer-rs-indexd` keeps an Everything-style in-memory file-name index
(NTFS MFT + USN journal through a minimal elevated helper, or folder walking +
`ReadDirectoryChangesW`), persists it in checksummed snapshots and serves
queries over a per-user named pipe. The helper runs elevated through the
`\explorer-rs\IndexHelper` scheduled task created by the installer. `indexd`
is a supervisor component enabled by the installer's "Fast file search"
option (`supervisor.file_search`). Details: [INDEX.md](INDEX.md),
[IPC-PROTOCOL.md](IPC-PROTOCOL.md), ADRs 0010-0012.

## Feature flags

The tray and the quick-settings back-ends are always built; the `[tray]`
config section controls the tray.

| Feature | Crate(s) | State |
|---|---|---|
| `quick-settings` | explorer-win, explorer-taskbar | no-op, kept for old build scripts (back-ends are always built) |
| `virtual-desktops` | explorer-win, explorer-taskbar | thin `winvd` wrapper (undocumented COM); no UI yet |
| `everything` | explorer-launcher | implemented, dynamic DLL load |
| `file-index` | explorer-launcher | "Files" results from `explorer-rs-indexd`; wins over `everything` if both are on |

## Building

- Linux checks: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`.
- Windows cross-check: `cargo xwin clippy --target x86_64-pc-windows-msvc
  --workspace --all-targets --all-features -- -D warnings`.
- Installer: `explorer-rs-setup-<version>-x64.exe` is packed by the pipeline
  in [PACKAGING.md](PACKAGING.md) (`explorer-setup-pack`).
- Release zip: `scripts/build-windows.sh` produces
  `dist/explorer-rs-<version>-x86_64-pc-windows-msvc.zip`. Set
  `FEATURES=all` or `FEATURES="everything"` to include optional features, or
  `TARGET=x86_64-pc-windows-gnu` for the MinGW fallback. See ADR 0006.
