# Architecture

explorer.rs replaces the web-based parts of the Windows 11 shell (Start,
Search, Widgets, Copilot) with small native programs. Reliability and speed
come first; looks are deliberately plain. No WebView, Electron, Chromium or
web content anywhere.

## Processes

```
explorer-rs.exe  (supervisor, --dev | --shell)
 ├── explorer-rs-taskbar.exe   one AppBar per monitor, window list, clock,
 │                             Win-key hook, shell hooks
 └── explorer-rs-launcher.exe  hidden popup; app index + fuzzy search
```

- Each component is a separate process (ADR 0002). The supervisor restarts a
  crashed component with exponential backoff (500 ms doubling to 30 s, reset
  after 60 s of healthy uptime). It gives up on a component after more than
  5 crashes in 60 s. Other components are unaffected.
- Exit codes: `0` stop (no restart), `77` quit all of explorer.rs, `78`
  restart now. Anything else counts as a crash.
- Children are placed in a kill-on-close job object, so they die with the
  supervisor (no orphaned AppBars).
- In shell mode, if the taskbar is given up on, the supervisor starts
  `explorer.exe` as a last-resort UI.
- Every binary is single-instance per session (named mutex).

## Inter-process communication

Deliberately minimal (ADR 0002): components find each other by window class
(`ExplorerRs.Launcher`, `ExplorerRs.Taskbar`) and talk with the registered
window message `ExplorerRs.LauncherCommand` (wParam: 0 toggle, 1 show,
2 hide). A message to a dead peer is simply dropped. Before posting, the
taskbar calls `AllowSetForegroundWindow` for the launcher's PID, so the
launcher may take the foreground.

## Crates

| Crate | Kind | Contents | Tested on Linux |
|---|---|---|---|
| `explorer-core` | lib | config, paths, logging, app catalog + launch history, fuzzy search (`nucleo-matcher`), window-filter rules, taskbar model, Win-key state machine, supervisor policy, layout/hit-testing, clock formatting | yes, fully |
| `explorer-win` | lib | thin Win32/COM wrappers; empty on non-Windows | compiles only |
| `explorer-supervisor` | bin `explorer-rs` | process supervision | yes (CLI + policy) |
| `explorer-taskbar` | bin `explorer-rs-taskbar` | taskbar UI | cross-compiled |
| `explorer-launcher` | bin `explorer-rs-launcher` | launcher UI | cross-compiled |

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

## System tray

- Pure protocol parsing and the icon model live in `explorer_core::tray`
  (unit-tested, including truncated input).
- `explorer-win::tray` hosts a `Shell_TrayWnd` window. Its icons are shown
  on the primary monitor's bar. Clicks are forwarded to the owning
  application using the documented `NOTIFYICON_VERSION_4` or legacy callback
  semantics.
- **Dev mode:** our invisible `Shell_TrayWnd` is kept first in Z order, so
  `Shell_NotifyIcon`'s `FindWindow` reaches us. Every message is forwarded
  to explorer's real tray window, so explorer keeps working. At start-up we
  broadcast `TaskbarCreated` so running apps re-add their icons. Disable
  with `tray.intercept_in_dev_mode = false`.
- **Shell mode:** we are the only tray. AppBar traffic (`dwData == 0`) is
  not served yet.

## Start menu (launcher)

The launcher is a Windows 11-style Start menu, anchored above the taskbar
(centred or left, following the alignment). It has three views:

| View | Contents |
|---|---|
| Home | Search box; pinned apps grid (6×3); "Most used" (frecency); footer with user name and power menu (lock, sign out, sleep, shut down, restart) |
| All apps | Alphabetical list with letter headers |
| Search | Any typing switches here |

Pins are stored in `%LOCALAPPDATA%\explorer-rs\pins.toml`. They are
seeded once with common apps that exist on the machine (Edge, File Explorer,
Settings, Store, Terminal, Notepad, Calculator, Photos…), after which the
user's own choices are kept. Right-clicking an app offers Pin/Unpin, Move
to front, Run as administrator and Open file location.

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
| Logs | `%LOCALAPPDATA%\explorer-rs\logs\<component>.YYYY-MM-DD.log`, daily rotation, 7 files kept |
| Launch history | `%LOCALAPPDATA%\explorer-rs\launch-history.toml` |
| Override | `EXPLORER_RS_HOME` puts everything under one folder; `EXPLORER_RS_LOG` sets the tracing filter |

A broken config never prevents startup: defaults are used and the error is
logged. Unknown keys are rejected, so typos are reported instead of being
ignored. Panics are logged with a backtrace by a panic hook before the
process dies.

## Feature flags

The tray is no longer a feature flag; it is controlled by the `[tray]`
config section.

| Feature | Crate(s) | State |
|---|---|---|
| `quick-settings` | explorer-win, explorer-taskbar | battery, volume and connectivity back-ends implemented; no UI yet |
| `virtual-desktops` | explorer-win, explorer-taskbar | thin `winvd` wrapper (undocumented COM); no UI yet |
| `everything` | explorer-launcher | implemented, dynamic DLL load |

## Building

- Linux checks: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`.
- Windows cross-check: `cargo xwin clippy --target x86_64-pc-windows-msvc
  --workspace --all-targets --all-features -- -D warnings`.
- Release zip: `scripts/build-windows.sh` produces
  `dist/explorer-rs-<version>-x86_64-pc-windows-msvc.zip`. Set
  `FEATURES=all` or `FEATURES="everything"` to include optional features, or
  `TARGET=x86_64-pc-windows-gnu` for the MinGW fallback. See ADR 0006.
