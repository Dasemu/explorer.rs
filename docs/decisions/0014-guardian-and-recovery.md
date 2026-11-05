# ADR 0014: Guardian process, automatic recovery and shell-mode duties

- Status: accepted
- Date: 2026-11-04

## Context
In shell mode explorer.rs is the only user interface. A crash loop of the
supervisor or the taskbar, or an endless crash at every sign-in, must never
leave the user without a way back to Windows Explorer. As a shell we also
inherit explorer.exe's duties: telling Winlogon the shell is ready, running
startup apps, and drawing the desktop background.

## Decision
- **Guardian.** `explorer-rs.exe --dev|--shell` is a tiny process that runs
  `explorer-rs.exe --supervise --dev|--shell [--config] [--only]` as a child
  and restarts it with the same backoff policy as components
  (`explorer_core::guardian::GuardianPolicy`). It uses no COM and no UI except
  a hidden session window, a message-only hotkey window and message boxes. It
  is the only writer of the crash log. The restart loop is portable and
  driven through a `Host` trait so it is integration-tested on Linux with a
  fake child.
- **Exit-code protocol** (extends ADR 0002): supervisor `79` = component
  crash loop needs the explorer.exe fallback, `80` = another supervisor is
  running (retry later, not a crash). `0`/`77` end the guardian in dev mode;
  in shell mode it idles so the emergency keys keep working.
- **Crash loops.** Shell mode: 3 supervisor failures (or 3 taskbar failures)
  within 2 minutes after sign-in, or the existing GiveUp decision → fallback.
  The user is asked *before* explorer.exe starts, so that "restore" can
  rewrite the registration first and explorer.exe comes up as the full shell.
  `MessageBoxW` Yes/No is used (no common-controls manifest needed).
- **Failed sign-ins.** `%LOCALAPPDATA%\explorer-rs\state\crashlog.toml` keeps
  a 100-event ring buffer and one record per shell sign-in. A sign-in failed
  if it fell back, or if it neither became healthy (3 minutes of supervisor
  uptime) nor ended cleanly (WM_ENDSESSION). After 3 consecutive failures
  Windows Explorer is restored without asking. Counting "neither healthy nor
  clean" as a failure catches hangs that force a power-off; the cost is that
  3 power losses within 3 minutes of sign-in in a row also restore Explorer,
  which is the safe direction.
- **Emergency hotkeys** `Ctrl+Alt+Shift+E` (start explorer.exe) and
  `Ctrl+Alt+Shift+R` (restart everything) live in the guardian.
- **Session end.** Guardian and supervisor have a hidden top-level window
  (message-only windows do not receive `WM_QUERYENDSESSION`). After
  `WM_QUERYENDSESSION` component exits are not crashes for 30 s; after
  `WM_ENDSESSION` nothing restarts.
- **Config hot reload.** The supervisor polls (mtime, size) every 2 s and
  applies a change once the file has been stable for one poll; invalid files
  are logged and ignored. Only components whose config sections changed are
  restarted (`ExplorerRs.Shutdown`, then kill after 3 s); added/removed
  components are started/stopped. Cross-section dependencies count: the
  taskbar also restarts on `[launcher] win_key_opens_launcher`, the launcher
  on any `[taskbar]` change.
- **Graceful stop** (revised after the integration review). Components no
  longer close on `WM_CLOSE` (any program can send it); the registered
  message `ExplorerRs.Shutdown` asks a component to exit with 0, the
  supervisor to stop its components, the guardian to stop its supervisor
  and record a clean end (`CrashState::mark_clean_stop`). A component's exit
  code is the one set by a deliberate path; otherwise (main window
  destroyed, stray `WM_QUIT`) it is `EXIT_RESTART`.
- **Job object.** `KILL_ON_JOB_CLOSE` plus
  `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`: only the components (assigned
  explicitly) are members; apps they launch are not. Startup apps are
  launched by the supervisor, which is not in its own job.
- **Never two shells.** A shell-mode guardian starts the supervisor in dev
  mode while the Windows Explorer shell runs (owner of the shell window or
  of a `Shell_TrayWnd` is explorer.exe).
- **Orphans.** The guardian passes its pid (`EXPLORER_RS_GUARDIAN_PID`);
  the supervisor waits on that process and, once it is gone, handles
  fallbacks itself instead of exiting with 79. `EXIT_ALREADY_RUNNING` (80)
  retries back off 5 s → 60 s and are logged once.
- **Resume.** `--resume` (with `--dev`/`--shell`) skips the sign-in
  accounting and marks the shell duties as done; setup uses it when it
  restarts an explorer.rs it stopped.
- **Crash log writers.** The guardian is the writer while it runs;
  `--restore-explorer` posts it `ExplorerRs.Restored` instead of writing.
  Writes use unique temporary names and re-read the file first when it
  changed on disk (`CrashLogFile`).
- **File search default.** `supervisor.file_search = "auto"|"on"|"off"`;
  `auto` adds `indexd` when `HKLM\Software\explorer-rs\FileSearch = 1` and
  `components` is not set explicitly in the user's file.
- **Shell duties once per sign-in.** Guarded by the session-local event
  `Local\ExplorerRs.ShellDutiesClaimed`, which the guardian holds open so a
  restarted supervisor sees it set. The supervisor claims it *before* doing
  the work, so a crash in the middle never repeats startup apps.
- **Startup apps** follow explorer.exe: HKCU RunOnce (deleted before running,
  `!` prefix after), HKLM Run (64-bit and WOW6432Node), HKCU Run, common and
  per-user Startup folders, honouring `Explorer\StartupApproved\{Run,Run32,
  StartupFolder}` in HKCU and HKLM (first byte bit 0 set = disabled; values
  02/06 enabled, 03/07 disabled as observed; the format is undocumented).
  Run entries use CreateProcess (apps that require elevation are skipped, as
  explorer does), falling back to ShellExecuteEx; Startup-folder items use
  ShellExecuteEx. 300 ms between launches. HKLM RunOnce (needs admin) and
  the `Policies\Explorer\Run` key are not processed.
- **Desktop component** `explorer-rs-desktop` (component `desktop`,
  `[desktop] enabled = "shell"` by default): wallpaper only, no icons.
  Geometry is pure (`explorer_core::wallpaper`); WIC decodes at the smallest
  size any monitor needs. It uses its own Direct2D target because the shared
  painter only draws square icons. In shell mode it uses the class
  `Progman`/"Program Manager" and calls `SetShellWindow`.

## Consequences
- One more process (the guardian, a few MB). Killing the supervisor kills all
  components through its job object; the guardian is not in a job, so the
  supervisor survives a guardian crash (without restart protection).
- Undocumented: `SetShellWindow`, the `msgina: ShellReadyEvent` /
  `ShellDesktopSwitchEvent` events, and the `StartupApproved` binary format;
  all fail gracefully and are listed in ADR 0007.
