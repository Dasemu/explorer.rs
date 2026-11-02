# Testing explorer.rs on Windows 11

explorer.rs has not yet been verified on real Windows. This is the test plan.
Please follow it in order and report back using the template at the end.

## Prerequisites

- Windows 11. Run `winver` and note the version and build number.
- **A VM with a snapshot is recommended** (Hyper-V or VirtualBox), especially
  for phase 3. Phases 1 and 2 do not change the registry, but they do hook the
  keyboard and reserve screen space.
- Ideally two monitors, with different DPI scaling on at least one.
- Know the recovery steps first: [RECOVERY.md](RECOVERY.md).

## Getting the binaries

Option A: use the zip `explorer-rs-<version>-x86_64-pc-windows-msvc.zip`
built in the Linux container with `scripts/build-windows.sh`. Extract it to a
folder you own, e.g. `C:\explorer-rs`. All binaries live in the same folder:
`explorer-rs.exe`, `explorer-rs-taskbar.exe`, `explorer-rs-launcher.exe`.

Option B: build natively on Windows with the MSVC toolchain:

```
cargo build --release --workspace
```

The binaries are in `target\release\`.

Open PowerShell in the folder with the binaries for the commands below.

## Phase 1: diagnostics only

These write information to a text file and exit. The binaries are
GUI-subsystem programs, so always use `--output` rather than `>` redirection,
and wait a couple of seconds for each file to appear:

```
.\explorer-rs-taskbar.exe --dump-monitors --output dump-monitors.txt
.\explorer-rs-taskbar.exe --dump-windows  --output dump-windows.txt
.\explorer-rs-launcher.exe --dump-apps    --output dump-apps.txt
```

Before `--dump-windows`, open Notepad, Settings, Calculator, and a File
Explorer window so they appear in the list. Skim the files: are your monitors
and windows listed? Are the "shown / why not" reasons plausible?

## Phase 2: dev mode

```
.\explorer-rs.exe --dev
```

Dev mode runs alongside explorer.exe. The taskbar is a normal AppBar and the
launcher is a normal window. Logs go to
`%LOCALAPPDATA%\explorer-rs\logs\`. For more detail set `EXPLORER_RS_LOG=debug`
before starting. Stop it by right-clicking an empty area of the explorer.rs taskbar and
choosing **Quit explorer.rs**, or by ending `explorer-rs.exe` in Task Manager
(its children are in a kill-on-close job object and exit with it). Ctrl+C in
the console that started it should also work but is unverified.

Checklist (record pass / fail / notes for each):

1. Taskbar appears on each monitor.
2. Maximized windows do not overlap the taskbar, on each monitor.
3. Mixed-DPI monitors: taskbar size, text and icons are sharp and sensibly
   sized on each monitor. Dragging a window between monitors behaves well.
4. Window buttons appear when you open Notepad, Settings (UWP), Calculator,
   and File Explorer windows, and Task Manager. They disappear on close.
5. No ghost buttons for suspended UWP apps (open and close Calculator and
   Settings; check nothing stale remains).
6. Click a button: that window activates. Click the active window's button
   again: it minimizes.
7. The clock shows the correct time and updates.
8. Press the Win key alone: the launcher opens and the Start menu does NOT.
9. `Win+E` still opens File Explorer and `Win+R` still opens Run.
10. Launcher: typing filters results; Enter launches the selected item; Esc
    hides the launcher; the arrow keys move the selection.
11. Launch a UWP app (Calculator) from the launcher. It starts.
12. Launch a classic app (e.g. Notepad) from the launcher. It starts.
13. Kill `explorer-rs-launcher.exe` in Task Manager: the taskbar survives and
    the launcher is back within a few seconds (Win key works again).
14. Kill `explorer-rs-taskbar.exe` in Task Manager: the reserved AppBar space
    is released (maximized windows use the full screen briefly) and the taskbar
    comes back.

Component exit codes, for reference: 0 = intentional stop (not restarted),
77 = quit all of explorer.rs, 78 = restart me.

## Phase 3: shell mode (optional, VM only)

Only do this in a VM with a snapshot, with a second admin account created and
RECOVERY.md read. Do not do this on your daily machine yet.

1. Take a VM snapshot.
2. From the folder containing `scripts\` and the binaries:
   ```
   powershell -ExecutionPolicy Bypass -File .\scripts\install-shell.ps1
   ```
   Read the warning and type `YES`.
3. Sign out and sign back in.
4. Check: taskbar appears, Win key opens the launcher, apps can be launched,
   and killing a component recovers it as in phase 2.
5. Optional: kill the taskbar repeatedly (more than 5 times within 60 seconds)
   and confirm the supervisor falls back to starting explorer.exe.
6. Restore: Ctrl+Shift+Esc -> File -> Run new task ->
   ```
   powershell -ExecutionPolicy Bypass -File <path>\scripts\restore-explorer.ps1 -StartExplorer
   ```
   then sign out and in and confirm the normal desktop returns.
7. If anything is stuck, roll back the snapshot.

## What to send back

- `dump-monitors.txt`, `dump-windows.txt`, `dump-apps.txt`
- The log folder `%LOCALAPPDATA%\explorer-rs\logs\`, zipped
- Screenshots of the taskbar on each monitor, and of the launcher
- `winver` output (version and build)
- Display scaling per monitor (Settings -> System -> Display)
- The checklist below with pass / fail / notes per item

## Report template

```
## Environment
winver: <version / build>
Machine: <physical / Hyper-V / VirtualBox / other>
Monitors: <count>; per monitor: <resolution> @ <scaling %>
Binaries: <zip name or git commit>

## Phase 1
dump-monitors: <ok / problem + notes>
dump-windows:  <ok / problem + notes>
dump-apps:     <ok / problem + notes>

## Phase 2 (--dev)
1  Taskbar on each monitor:                  PASS / FAIL / notes:
2  Maximized windows don't overlap:          PASS / FAIL / notes:
3  Mixed DPI:                                PASS / FAIL / notes:
4  Buttons appear/disappear (5 app types):   PASS / FAIL / notes:
5  No ghost buttons for suspended UWP apps:  PASS / FAIL / notes:
6  Click activates, click again minimizes:   PASS / FAIL / notes:
7  Clock updates:                            PASS / FAIL / notes:
8  Win key opens launcher, not Start:        PASS / FAIL / notes:
9  Win+E and Win+R still work:               PASS / FAIL / notes:
10 Launcher typing/Enter/Esc/arrows:         PASS / FAIL / notes:
11 Launch UWP app (Calculator):              PASS / FAIL / notes:
12 Launch classic app:                       PASS / FAIL / notes:
13 Kill launcher -> comes back:              PASS / FAIL / notes:
14 Kill taskbar -> space freed, comes back:  PASS / FAIL / notes:

## Phase 3 (VM only, optional)
Install: PASS / FAIL / notes:
Shell session usable: PASS / FAIL / notes:
Fallback to explorer.exe: PASS / FAIL / not tested
Restore: PASS / FAIL / notes:

## Anything else
<free text>
```
