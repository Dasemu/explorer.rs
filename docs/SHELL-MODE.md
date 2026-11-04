# Shell mode

When explorer.rs is registered as your shell (installer or
`scripts/install-shell.ps1`), Windows starts `explorer-rs.exe --shell` at
sign-in instead of `explorer.exe`. This page describes what explorer.rs then
does in place of Windows Explorer, and what it does not do. For getting back
to Windows Explorer see [RECOVERY.md](RECOVERY.md); the design is in
[ADR 0014](decisions/0014-guardian-and-recovery.md).

## Processes

```
explorer-rs.exe --shell                 guardian: restarts the supervisor,
│                                       emergency keys, crash-loop fallback
└─ explorer-rs.exe --supervise --shell  supervisor: components, config reload,
   ├─ explorer-rs-taskbar.exe           startup apps
   ├─ explorer-rs-launcher.exe
   ├─ explorer-rs-desktop.exe           wallpaper
   └─ explorer-rs-indexd.exe            file search (if enabled)
```

`--config <file>` and `--only <component>` given to the guardian are passed
through to the supervisor and from there to the components (`--config`).

## What shell mode does

### Tells Windows the shell is ready
Once per sign-in the supervisor sets the `msgina: ShellReadyEvent` and
`ShellDesktopSwitchEvent` events when they exist (what explorer.exe does so
the sign-in animation ends promptly). They are undocumented; if missing,
nothing happens.

### Runs your startup apps
Once per sign-in, after the taskbar is up (so tray icons appear), with
300 ms between launches, in this order:

1. `HKCU\Software\Microsoft\Windows\CurrentVersion\RunOnce` (each value is
   deleted before it runs; values whose name starts with `!` after it ran);
2. `HKLM\...\Run` and `HKLM\Software\WOW6432Node\...\Run`;
3. `HKCU\...\Run`;
4. the common Startup folder, then your Startup folder (`shell:startup`).

Apps switched off in **Settings > Apps > Startup** or Task Manager are
skipped (`...\Explorer\StartupApproved`). Environment variables are expanded;
unquoted paths with spaces are resolved like Windows does. Apps that require
administrator rights are skipped (explorer.exe does not elevate them either).
Every launch is logged in `supervisor.*.log`. Not processed: `HKLM\...\RunOnce`
(needs admin rights), `Policies\Explorer\Run`, scheduled tasks and services
(Windows runs those itself). Nothing of this runs in `--dev` mode.

A restarted supervisor (crash, `Ctrl+Alt+Shift+R`) does **not** run startup
apps again.

### Draws the desktop background
`explorer-rs-desktop.exe` paints your wallpaper on every monitor using your
settings: image (`Settings > Personalization > Background`), fit (fill, fit,
stretch, tile, center, span) and background colour. It follows changes to the
wallpaper and to the monitor layout. Right-click shows *Personalize*,
*Display settings* and *Refresh*. In shell mode the window is registered as
the shell window (`GetShellWindow`) with the classic `Progman` /
"Program Manager" name some programs look for.

Configure it with:

```toml
[desktop]
enabled = "shell"   # "shell" (default), "always" (also in --dev), "never"
```

### Detects sign-out and shutdown
Nothing is restarted while the session ends, and components closing then are
not counted as crashes.

## Limitations

- **No desktop icons.** The desktop shows the wallpaper only; files on your
  Desktop folder are reachable through File Explorer or the launcher.
- **No AppBar server.** Third-party AppBars (e.g. docks, other taskbars) that
  rely on explorer.exe's AppBar management do not get screen space reserved.
- **No notification toasts / Action Center** of explorer.exe's design;
  Windows shows toasts itself where possible (unverified).
- Slideshow and Windows Spotlight wallpapers are shown as the current image;
  explorer.rs does not advance the slideshow.
- Explorer-only features (Win+X menu, Task View hosting, etc.) are not
  provided unless other explorer.rs components implement them.

## For the lead: STATUS / TESTING / ARCHITECTURE entries

### STATUS.md (all unverified on Windows)
- Guardian process (`explorer-rs --dev|--shell`) restarting the supervisor
  (`--supervise`); crash log `%LOCALAPPDATA%\explorer-rs\state\crashlog.toml`;
  crash-loop fallback dialog; auto-restore of Windows Explorer after 3 failed
  sign-ins — unverified on Windows.
- Emergency hotkeys Ctrl+Alt+Shift+E / Ctrl+Alt+Shift+R — unverified on Windows.
- `--restore-explorer`, `--edit-config`, `--status [--output]` — unverified on Windows.
- Config hot reload (2 s polling, per-component restart) — unverified on Windows.
- `supervisor.file_search` (auto/on/off, HKLM FileSearch) — unverified on Windows.
- Session-end detection (no restarts / no crash counting during sign-out) — unverified on Windows.
- Shell mode: ready events, startup apps with StartupApproved, desktop
  wallpaper component (`explorer-rs-desktop`, Progman, SetShellWindow) —
  unverified on Windows.

### TESTING-ON-WINDOWS.md (numbered steps)
1. Dev mode: run `explorer-rs.exe --dev`; check `guardian.*.log` and
   `supervisor.*.log` exist and the taskbar appears. `explorer-rs-desktop`
   must exit immediately (no window) with the default config.
2. Press Ctrl+Alt+Shift+E: a File Explorer window opens. Press
   Ctrl+Alt+Shift+R: taskbar and launcher disappear briefly and come back.
3. In Task Manager kill `explorer-rs.exe --supervise` (the child of the
   guardian): it restarts within a second; `--status` shows a crash event.
4. `explorer-rs.exe --status --output %TEMP%\s.txt`: check version, mode
   "dev", components, shell registration, config validity, recent events.
5. Edit `config.toml` (`[taskbar] height = 40`), save: within ~4 s only the
   taskbar restarts with the new height. Save an invalid value
   (`height = 1`): the log shows an error and nothing restarts. Remove
   `"launcher"` from `components`: the launcher stops; add it back: it starts.
6. `explorer-rs.exe --edit-config` with no config file: the file is created
   from the default and opens in Notepad.
7. Set `[desktop] enabled = "always"` in dev mode: the wallpaper window
   appears behind all windows (it covers explorer's desktop icons), follows
   Settings > Personalization > Background changes (each fit style, a solid
   colour, two monitors with "span"), and its right-click menu works in the
   UI language. Set it back to "shell".
8. In a VM with a snapshot, register shell mode. Sign in: startup apps from
   Run keys and `shell:startup` start (tray icons appear); an app disabled in
   Settings > Apps > Startup does not; a `RunOnce` value runs once and is gone.
   `GetShellWindow`-dependent apps work; the sign-in animation ends promptly.
9. Shell mode crash loop: temporarily rename `explorer-rs-taskbar.exe`, sign
   in: after ~3 failures a bilingual Yes/No dialog appears. Choose No:
   explorer.exe starts; sign out/in twice more choosing No: on the third
   failure Windows Explorer is restored automatically with an explanation.
   Rename the file back and re-register.
10. Shell mode: choose Yes in the dialog: Windows Explorer becomes the shell
    (full taskbar) and the next sign-in uses explorer.exe.
11. Shell mode: sign out and shut down normally several times: no crash
    events are recorded for components (check `--status`), and the failed
    sign-in counter stays 0.
12. Start-menu shortcut / `explorer-rs.exe --restore-explorer` in shell mode:
    explorer.rs stops, explorer.exe starts, a confirmation box appears.

### ARCHITECTURE.md
- Process tree: `explorer-rs.exe` (guardian) → `explorer-rs.exe --supervise`
  (supervisor, kill-on-close job) → taskbar, launcher, desktop, indexd. The
  guardian is not in a job, uses no COM, owns the hotkeys and the crash log,
  and is the only process that talks to the user about recovery.
- Exit codes: 0 stop, 77 quit all, 78 restart, 79 fallback needed
  (supervisor → guardian), 80 supervisor already running.
- New pure modules in explorer-core: `crashlog`, `guardian`, `components`,
  `startup`, `wallpaper`. New explorer-win modules: `session`, `process`,
  `registry`, `startup`, `wallpaper`, `desktop`, `dialog`.
- New crate `explorer-desktop` (binary `explorer-rs-desktop`). The build
  script picks it up automatically (`explorer-rs*.exe`); the installer must
  install and uninstall it with the other binaries, and create the
  "Restore Windows Explorer" Start-menu shortcut running
  `explorer-rs.exe --restore-explorer`.
- `explorer-rs.exe --restart` posts the Ctrl+Alt+Shift+R command to the
  guardian's message-only window (class `ExplorerRs.Hotkeys`): it kills and
  restarts the supervisor and therefore every component. The Start menu's
  "Restart" item can run it (or post `WM_HOTKEY` with id `0xE002` to that
  window directly) to restart everything instead of only itself (exit 78).
