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
apps again, nor does explorer.rs restarted by setup after an upgrade
(`--shell --resume`). Startup apps are not in explorer.rs's job object, so
they keep running when explorer.rs restarts.

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

### Provides Explorer's keyboard shortcuts
Many Win+ shortcuts are not handled by Windows but by explorer.exe, so
without it they did nothing. In shell mode the taskbar registers these
(`RegisterHotKey`; alongside Explorer, Explorer keeps handling them):

| Keys | Does |
|------|------|
| Win+Shift+S | Screen clip overlay (`ms-screenclip:`, Snipping Tool) |
| Print Screen | The same, when Settings -> Accessibility -> Keyboard -> "Use the Print screen key to open screen capture" is on (`HKCU\Control Panel\Keyboard\PrintScreenKeyForSnippingEnabled`; missing means on in Windows 11). When it is off the key is left to Windows, which copies the screen to the clipboard |
| Win+Print Screen | Whole screen (all monitors) to `Pictures\Screenshots\Screenshot (N).png` ("Captura de pantalla (N).png" in Spanish; N continues Windows' `ScreenshotIndex` counter) and to the clipboard. The screen is not dimmed |
| Win+E | A File Explorer window (as the Start menu's File Explorer button) |
| Win+R | The Start menu, whose search box runs commands (there is no Run dialog) |
| Win+I | Settings |
| Win+D | Show desktop / bring the windows back |
| Win+A | Quick settings |
| Win+N | The calendar (there is no notification centre) |

A shortcut another program already holds is skipped and logged
(`cannot register shortcut` in `taskbar.*.log`).

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
- Explorer shortcuts not provided: the Win+X menu, Win+Tab (Task View),
  Win+1..9 (taskbar apps), Win+T and Win+B (focus the taskbar or tray),
  Win+M / Win+Shift+M, Win+Home and Win+, (peek at the desktop). Win+V,
  Win+., Win+K, Win+P and Win+H belong to other Windows components and may
  or may not work without Explorer (unverified). Win+L and Ctrl+Alt+Del are
  handled by Windows itself.

## For the lead

Merged into ARCHITECTURE.md, STATUS.md and TESTING-ON-WINDOWS.md.
