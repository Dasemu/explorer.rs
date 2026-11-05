# Testing explorer.rs on Windows 11

Test plan for the release candidate. explorer.rs is feature complete but has
not yet been verified on real Windows. Follow the phases in order and report
back with the template at the end. Every numbered step has one PASS/FAIL line;
each step says what to do and what should happen.

## Prerequisites

- Windows 11; run `winver` and note version and build.
- **A VM with a snapshot is recommended** (Hyper-V or VirtualBox), mandatory
  for phase C. Phases A and B reserve screen space and hook the keyboard but
  do not replace the shell.
- Ideally two monitors, with different DPI scaling on at least one.
- Read [RECOVERY.md](RECOVERY.md) first. In an emergency, Ctrl+Alt+Shift+E
  starts Windows Explorer.
- The installer `explorer-rs-setup-<version>-x64.exe`. Without it, use the zip
  `explorer-rs-<version>-x86_64-pc-windows-msvc.zip` from
  `scripts/build-windows.sh` (or `cargo build --release --workspace` on
  Windows) and run `explorer-rs.exe --dev` from its folder.
- A Spanish Windows UI is useful: several steps quote Spanish strings.
  Windows 11 in English works too.

Component exit codes, for reference: 0 stop, 77 quit all, 78 restart me,
79 fallback needed, 80 supervisor already running.

## Phase A: Install

Installer details: [PACKAGING.md](PACKAGING.md), [INSTALL.md](INSTALL.md). Start in a normal (non-elevated) session.

1. **Installer starts.** Run `explorer-rs-setup-<version>-x64.exe`. SmartScreen (More info -> Run anyway) and a UAC prompt appear. The wizard is in your Windows language (English or Spanish) and dark if apps use dark mode.
2. **Install, alongside.** Welcome -> Next. Options: "Run alongside" selected, "Fast file search" and "Start now" ticked, folder `C:\Program Files\explorer-rs`. Install. The progress list finishes without errors and explorer.rs starts next to Windows Explorer.
3. **Not elevated.** Task Manager -> Details -> add column "Elevated": every `explorer-rs*.exe` is No, except `explorer-rs-index-helper.exe` = Yes.
4. **Installed items.** Start menu folder "explorer.rs" with 4 shortcuts (incl. "Restore Windows Explorer"); Settings -> Apps -> Installed apps lists explorer.rs by "explorer.rs contributors"; `taskschd.msc` -> explorer-rs -> IndexHelper (highest privileges, at your log on); `reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v explorer-rs` shows a value.
5. **Settings shortcut.** Click "explorer.rs settings": Notepad opens `%APPDATA%\explorer-rs\config.toml` without a UAC prompt.
6. **Starts at sign-in.** Sign out and in: explorer.rs starts alongside Explorer.
7. **Upgrade in place.** Run the installer again: previous choices are pre-selected; running explorer.rs is closed and restarted; no reboot is asked.
8. **Over-the-shoulder (optional).** As a standard user, run setup and enter another admin's credentials. The Run value is written for the standard user (`reg query HKCU\...` as that user).
9. **Diagnostics dumps.** Open Notepad, Settings, Calculator and a File Explorer window. In the install folder run `explorer-rs-taskbar.exe --dump-monitors --output dump-monitors.txt`, `explorer-rs-taskbar.exe --dump-windows --output dump-windows.txt` and `explorer-rs-launcher.exe --dump-apps --output dump-apps.txt` (GUI programs: always use `--output`; wait a few seconds). The files list your monitors, the windows with "shown / why not" reasons, and your apps.

## Phase B: Alongside Explorer

Everything runs next to explorer.exe (dev mode as started by the installer, or `explorer-rs.exe --dev`). Taskbar: [TASKBAR.md](TASKBAR.md); Start menu: [START-MENU.md](START-MENU.md). Set `EXPLORER_RS_LOG=debug` for more log detail. Stop with the bar's right-click -> **Quit explorer.rs** or by ending `explorer-rs.exe` in Task Manager.

10. **Taskbar on each monitor.** The explorer.rs taskbar appears on every monitor.
11. **Work area.** Maximize a window on each monitor: it does not overlap the taskbar.
12. **Mixed DPI.** Taskbar size, text and icons are sharp and sensible on each monitor; dragging a window between monitors behaves well.
13. **Buttons appear and vanish.** Open Notepad, Settings (UWP), Calculator, File Explorer and Task Manager: each gets a button; closing removes it.
14. **No ghost buttons.** Open and close Calculator and Settings: no stale buttons for suspended UWP apps.
15. **Icons and titles.** Each button shows the app's icon (classic, UWP, File Explorer); hovering shows the window title.
16. **Click behaviour.** Click a button: the window activates; click the active window's button: it minimizes. With one Edge window: click activates, click again minimizes; Shift+click and middle-click open a new Edge window.
17. **Clock.** Shows the correct time and updates.
18. **Alignment.** Change Settings -> Personalization -> Taskbar -> Taskbar behaviors -> Taskbar alignment: the bar follows within a second.
19. **Theme.** Switch Dark/Light, change the accent colour, toggle "Show accent color on Start and taskbar": taskbar and Start follow live.
20. **Win key.** Press the Win key alone: the explorer.rs Start menu opens and Windows' Start does NOT.
21. **Win chords.** `Win+E` opens File Explorer and `Win+R` opens Run.
22. **Start button toggle.** Click the Start button: the menu opens; click again: it closes and does NOT reopen.
23. **Default pins.** On first run the taskbar shows File Explorer, Edge and Microsoft Store pinned before running apps; `%LOCALAPPDATA%\explorer-rs\taskbar-pins.toml` exists.
24. **Grouping and thumbnails.** Open three Notepad windows: one Notepad button with a running indicator. Hover 0.5 s: thumbnails of all three with titles; move away: they hide.
25. **Window picker.** Click the Notepad button: the picker stays open; Esc closes it. Click again, then a thumbnail: that window comes to front. Click x on a card: that window closes and the card disappears.
26. **Minimized card.** Minimize one Notepad and hover: its card shows a thumbnail or icon + title (note which).
27. **Pin and unpin.** Right-click Edge: menu offers open, "Unpin from taskbar", "Close window" (Spanish: Desanclar de la barra de tareas, Cerrar ventana). Unpin, check the file, re-pin via a running window's menu. Pin an app from the Start menu: it appears within ~2 s.
28. **Drag reorder.** Drag a pinned button past another and release: order changes and persists after restarting the taskbar.
29. **combine = never.** Set `combine = "never"` under `[taskbar]` in config.toml and restart the taskbar: one button per window.
30. **App overflow.** Open ~40 windows (or use a small bar): a "..." button appears; its list activates/launches correctly.
31. **Tray icons.** Icons of running apps (volume, network, OneDrive, Steam...) appear at the right; left- and right-click an app icon (e.g. OneDrive): its window or menu opens; explorer's own tray keeps working. If anything misbehaves set `[tray] intercept_in_dev_mode = false` and report it.
32. **Tray promotion and overflow.** Icons promoted in Settings -> Personalization -> Taskbar -> Other system tray icons are on the bar; others are behind `^`. Toggle one in Settings: it moves within ~5 s. Left/right/double-click icons inside the flyout. `tray.show_all = true` puts all icons on the bar.
33. **Quick settings.** Click the network/volume/battery glyphs: Quick settings opens at the right. Drag the volume slider (system volume follows); click the speaker (mute toggles, glyph updates); wheel over the volume glyph (+-2 %). Laptop: brightness slider works (absent on a desktop). Network name/status are correct and change within ~5 s when you disconnect; unplugging the charger updates the battery.
34. **Calendar.** Click the clock: calendar with full date, today highlighted, Monday first in Spanish (Sunday in en-US); prev/next, mouse wheel and clicking the title work.
35. **Flyout dismissal.** Click the opener of an open flyout: it closes and does not reopen. Click another app: the flyout closes.
36. **Show desktop.** Click the far-right strip: all windows minimize; click again: they return. Repeat on a 150 % monitor: flyouts are scaled and placed on that monitor.
37. **Tooltips and log.** Hover every glyph/button: bilingual tooltips appear. `taskbar.*.log` has no unexplained warnings (WMI, catalog, NotifyIconSettings).
38. **Start menu layout.** Start opens instantly above the taskbar, centred or left like the taskbar, with the placeholder "Buscar aplicaciones, configuracion y documentos" and headers "Anclado", "Todas las aplicaciones", "Recomendado" (Spanish UI).
39. **Pinned apps and pages.** Pinned apps show icons. Pin more than 18 apps (All apps -> right-click -> Pin to Start): page dots appear; wheel, dots and PgDn change pages. Right-click -> Pin / Unpin / Move to front work and survive a restart.
40. **All apps, no duplicates.** "All apps" lists everything alphabetically with letter headers; typing switches to search. No duplicates such as "Command Prompt" next to "Simbolo del sistema".
41. **Recommended.** Open a few documents in Notepad/Word and reopen Start: they appear under Recommended with "Hoy HH:MM". Turn off Settings -> Personalization -> Start -> "Show recently opened items": they disappear. Right-click a recent file -> "Quitar de la lista" works.
42. **Search and actions.** Type `blo`: "Bloc de notas" is Best match; the right panel offers Abrir, Abrir ubicacion del archivo (classic apps), Anclar a Inicio, Anclar a la barra de tareas, Desinstalar. Typing filters, Enter launches, Esc hides, arrows move the selection. "Anclar a la barra de tareas" shows the pin on the taskbar within seconds.
43. **Launch UWP app.** Launch Calculator from Start: it starts.
44. **Launch classic app.** Launch Notepad from Start: it starts.
45. **Settings and tools.** Type `pantalla`, `wifi`, `bluetooth`, `fondo de pantalla`, `administrador de dispositivos`, `regedit`: Enter opens the right Settings page / tool.
46. **Calculator.** `6*7` shows "= 42"; `1/4` shows "= 0,25"; Enter shows "Copiado" and the clipboard holds `0,25`. `14:30` and `2024` show no calculator row.
47. **Run commands.** `>ipconfig` + Enter opens a Command Prompt running ipconfig; `%APPDATA%`, `C:\Windows` and `notepad C:\Windows\win.ini` each open. `https://example.com`, `www.example.com`, `msedge https://x` show no Run row. Ctrl+Shift+Enter on `cmd` shows a UAC prompt.
48. **Keyboard only.** Tab goes from the search box through results, detail actions (one by one), footer buttons and back; arrows work inside grids/lists; Esc clears the query, then returns Home, then closes. The focus ring is visible after keyboard moves.
49. **Footer and power menu.** Account picture (or person glyph) shown; File Explorer and Settings buttons open; power menu shows Lock / Sign out / Sleep / Shut down / Restart (Sleep/Hibernate hidden if unsupported) - test only Lock. "..." -> "Reiniciar el menu Inicio" restarts the launcher.
50. **English UI.** Switch Windows to English (or `[general] language = "en"`): all Start and taskbar strings are English.
51. **Kill launcher.** End `explorer-rs-launcher.exe` in Task Manager: the taskbar survives and the launcher is back within seconds (Win key works again).
52. **Kill taskbar.** End `explorer-rs-taskbar.exe`: reserved space is released briefly and the taskbar comes back.
53. **Guardian logs and desktop.** `%LOCALAPPDATA%\explorer-rs\logs` contains `guardian.*.log` and `supervisor.*.log`. `explorer-rs-desktop.exe` is not running (it exits at once in dev mode with the default config); Explorer's desktop icons are untouched.
54. **Kill supervisor.** End `explorer-rs.exe --supervise` (child of the guardian): it restarts within a second and `--status` shows a crash event.
55. **Status report.** `explorer-rs.exe --status --output status.txt`: shows version, mode "dev", components, shell registration, config validity and recent events.
56. **Config hot reload.** Edit config.toml (`[taskbar] height = 40`) and save: within ~4 s only the taskbar restarts with the new height. Save `height = 1`: the log shows an error and nothing restarts. Remove `"launcher"` from `components`: the launcher stops; add it back: it starts.
57. **Edit config.** With no config file, `explorer-rs.exe --edit-config` creates it from the default and opens it in Notepad.

## Phase C: Shell mode (VM only)

Only in a VM with a snapshot, with a second admin account and [RECOVERY.md](RECOVERY.md) read. Details: [SHELL-MODE.md](SHELL-MODE.md). Take the snapshot first. If anything is stuck, roll back the snapshot.

58. **Register shell mode.** Run the installer and choose Replace (or `powershell -ExecutionPolicy Bypass -File .\scripts\install-shell.ps1`, type `YES`). Sign out and in: explorer.rs is the shell; the taskbar appears, the Win key opens Start, apps launch, killing a component recovers it, and the sign-in animation ends promptly.
59. **Startup apps.** Apps from the Run keys and `shell:startup` start (tray icons appear); an app disabled in Settings -> Apps -> Startup does not; a `RunOnce` value runs once and is gone. Apps that depend on `GetShellWindow` work.
60. **Wallpaper desktop.** The wallpaper window sits behind all windows and follows Settings -> Personalization -> Background (each fit style, a solid colour, two monitors with "span"). Its right-click menu works in the UI language. (Also testable in dev mode with `[desktop] enabled = "always"`; set it back to `"shell"`.)
61. **Emergency hotkeys.** Ctrl+Alt+Shift+E opens a File Explorer window. Ctrl+Alt+Shift+R: taskbar and Start disappear briefly and come back.
62. **Upgrade while shell.** Run the installer again: Options shows the red "explorer.rs is your shell right now" note; after Install the taskbar comes back (via the `\explorer-rs\Launch` task).
63. **Crash loop, No.** Temporarily rename `explorer-rs-taskbar.exe` and sign in: after ~3 failures a bilingual Yes/No dialog appears. Choose No: explorer.exe starts. Sign out/in twice more choosing No: on the third failure Windows Explorer is restored automatically with an explanation. Rename the file back and re-register shell mode.
64. **Crash loop, Yes.** Repeat the failure and choose Yes: Windows Explorer becomes the shell (full taskbar) and the next sign-in uses explorer.exe. Re-register shell mode.
65. **Clean sign-out.** Sign out and shut down normally several times: no component crash events in `--status` and the failed sign-in counter stays 0.
66. **Restore Explorer.** Start menu -> "Restore Windows Explorer" (or `explorer-rs.exe --restore-explorer`): explorer.rs stops, explorer.exe starts, a confirmation box appears. Sign out/in: the normal desktop returns. Fallback: Task Manager -> Run new task -> `powershell -ExecutionPolicy Bypass -File <path>\scripts\restore-explorer.ps1 -StartExplorer`.

## Phase D: File search

Full plan: [TESTING-INDEX-ON-WINDOWS.md](TESTING-INDEX-ON-WINDOWS.md) (walk mode, CLI/IPC, elevated MFT helper, change journal, restarts, privacy filtering). Summary: install with "Fast file search" ticked; check `explorer-rs-indexd.exe` runs unelevated and the helper elevated; type part of a file name in Start and look for Files results.

67. **Files in Start search.** Type part of a file name: "Documentos" shows files with icons and folders; Abrir / Abrir ubicacion del archivo / Copiar ruta de acceso work. Send back the logs `indexd.*.log` and `index-helper.*.log`.
68. **Index plan.** Work through TESTING-INDEX-ON-WINDOWS.md and send back what it asks for (mark PASS/FAIL here for the whole plan).

## Phase E: Uninstall

Back in a normal alongside setup (not shell mode).

69. **Uninstall, keep data.** Uninstall from Installed apps with "Also remove my settings" unticked: files, shortcuts, tasks and the Installed apps entry are gone; `%APPDATA%\explorer-rs` is kept; Windows Explorer is the shell.
70. **Uninstall, remove data.** Install again, then uninstall with the box ticked: both AppData folders are gone.
71. **Silent install/uninstall.** `explorer-rs-setup.exe --silent --mode=none --file-search=off`, then `echo %ERRORLEVEL%` is 0; `"C:\Program Files\explorer-rs\uninstall.exe" --uninstall --silent` returns 0.
72. **Setup log.** `%ProgramData%\explorer-rs\setup.log` has an entry for every run and no unexplained errors.

## What to send back

- The logs folder `%LOCALAPPDATA%\explorer-rs\logs`, zipped
- `%ProgramData%\explorer-rs\setup.log`
- `status.txt` from `explorer-rs.exe --status --output status.txt`
- `dump-monitors.txt`, `dump-windows.txt`, `dump-apps.txt`
- Screenshots: the taskbar on each monitor, the Start menu, Quick settings,
  the calendar, and anything that looks wrong
- `winver` output (version and build) and display scaling per monitor
- The filled-in report below

## Report template

```
## Environment
winver: <version / build>
Machine: <physical / Hyper-V / VirtualBox / other>
Windows UI language: <en / es / other>
Monitors: <count>; per monitor: <resolution> @ <scaling %>
Installer / binaries: <file name or git commit>

## Phase A: Install
1  Installer starts: PASS / FAIL / notes:
2  Install, alongside: PASS / FAIL / notes:
3  Not elevated: PASS / FAIL / notes:
4  Installed items: PASS / FAIL / notes:
5  Settings shortcut: PASS / FAIL / notes:
6  Starts at sign-in: PASS / FAIL / notes:
7  Upgrade in place: PASS / FAIL / notes:
8  Over-the-shoulder (optional): PASS / FAIL / notes:
9  Diagnostics dumps: PASS / FAIL / notes:

## Phase B: Alongside Explorer
10 Taskbar on each monitor: PASS / FAIL / notes:
11 Work area: PASS / FAIL / notes:
12 Mixed DPI: PASS / FAIL / notes:
13 Buttons appear and vanish: PASS / FAIL / notes:
14 No ghost buttons: PASS / FAIL / notes:
15 Icons and titles: PASS / FAIL / notes:
16 Click behaviour: PASS / FAIL / notes:
17 Clock: PASS / FAIL / notes:
18 Alignment: PASS / FAIL / notes:
19 Theme: PASS / FAIL / notes:
20 Win key: PASS / FAIL / notes:
21 Win chords: PASS / FAIL / notes:
22 Start button toggle: PASS / FAIL / notes:
23 Default pins: PASS / FAIL / notes:
24 Grouping and thumbnails: PASS / FAIL / notes:
25 Window picker: PASS / FAIL / notes:
26 Minimized card: PASS / FAIL / notes:
27 Pin and unpin: PASS / FAIL / notes:
28 Drag reorder: PASS / FAIL / notes:
29 combine = never: PASS / FAIL / notes:
30 App overflow: PASS / FAIL / notes:
31 Tray icons: PASS / FAIL / notes:
32 Tray promotion and overflow: PASS / FAIL / notes:
33 Quick settings: PASS / FAIL / notes:
34 Calendar: PASS / FAIL / notes:
35 Flyout dismissal: PASS / FAIL / notes:
36 Show desktop: PASS / FAIL / notes:
37 Tooltips and log: PASS / FAIL / notes:
38 Start menu layout: PASS / FAIL / notes:
39 Pinned apps and pages: PASS / FAIL / notes:
40 All apps, no duplicates: PASS / FAIL / notes:
41 Recommended: PASS / FAIL / notes:
42 Search and actions: PASS / FAIL / notes:
43 Launch UWP app: PASS / FAIL / notes:
44 Launch classic app: PASS / FAIL / notes:
45 Settings and tools: PASS / FAIL / notes:
46 Calculator: PASS / FAIL / notes:
47 Run commands: PASS / FAIL / notes:
48 Keyboard only: PASS / FAIL / notes:
49 Footer and power menu: PASS / FAIL / notes:
50 English UI: PASS / FAIL / notes:
51 Kill launcher: PASS / FAIL / notes:
52 Kill taskbar: PASS / FAIL / notes:
53 Guardian logs and desktop: PASS / FAIL / notes:
54 Kill supervisor: PASS / FAIL / notes:
55 Status report: PASS / FAIL / notes:
56 Config hot reload: PASS / FAIL / notes:
57 Edit config: PASS / FAIL / notes:

## Phase C: Shell mode (VM only)
58 Register shell mode: PASS / FAIL / notes:
59 Startup apps: PASS / FAIL / notes:
60 Wallpaper desktop: PASS / FAIL / notes:
61 Emergency hotkeys: PASS / FAIL / notes:
62 Upgrade while shell: PASS / FAIL / notes:
63 Crash loop, No: PASS / FAIL / notes:
64 Crash loop, Yes: PASS / FAIL / notes:
65 Clean sign-out: PASS / FAIL / notes:
66 Restore Explorer: PASS / FAIL / notes:

## Phase D: File search
67 Files in Start search: PASS / FAIL / notes:
68 Index plan: PASS / FAIL / notes:

## Phase E: Uninstall
69 Uninstall, keep data: PASS / FAIL / notes:
70 Uninstall, remove data: PASS / FAIL / notes:
71 Silent install/uninstall: PASS / FAIL / notes:
72 Setup log: PASS / FAIL / notes:

## Anything else
<free text>
```
