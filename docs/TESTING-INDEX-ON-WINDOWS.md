# Testing the file index on Windows

Nothing in the index has run on real Windows yet. This plan checks, in order:
the daemon in walk mode (no privileges), the CLI and IPC, the elevated MFT
helper, the change journal, restarts, privacy filtering and the launcher. Each
step says what to send back. Logs are in `%LOCALAPPDATA%\explorer-rs\logs\`
(`indexd.YYYY-MM-DD.log`, `index-helper.YYYY-MM-DD.log`).

Use a VM or a machine where you are comfortable running test software. None of
these steps changes the registry; step 4 creates a scheduled task that step 9
removes.

## 0. Build and copy

On Windows: `cargo build --release -p explorer-indexd` (and
`-p explorer-launcher --features file-index` for step 8), or use the zip from
CI / `scripts/build-windows.sh`. Copy `explorer-rs-indexd.exe` and
`explorer-rs-index-helper.exe` into one folder. For step 4 that folder must not
be writable by non-administrators, e.g. `C:\Program Files\explorer-rs`
(create it from an elevated prompt and copy the files there).

Open a **normal** (non-elevated) PowerShell in that folder. Set verbose logs:

```powershell
$env:EXPLORER_RS_LOG = "debug"
```

## 1. Walk mode on a small folder (no privileges)

Create `%APPDATA%\explorer-rs\config.toml` with:

```toml
[index]
roots = ["%USERPROFILE%\\Documents"]
use_ntfs = false
```

Start the daemon. It is a GUI-subsystem program (no console window when the
supervisor starts it), so PowerShell returns immediately; it attaches to the
console and also logs to the file:

```powershell
.\explorer-rs-indexd.exe
```

In a second PowerShell:

```powershell
.\explorer-rs-indexd.exe status
.\explorer-rs-indexd.exe query docx
.\explorer-rs-indexd.exe query "ext:pdf;docx" --limit 5
.\explorer-rs-indexd.exe query "path:documents !ext:lnk" --limit 5
```

Then create, rename and delete a file in Documents (in File Explorer) and run
the matching query after each step: the change should show up within about a
second.

**Send back:** the output of `status` and the queries, how long the first
`status` took to report `live`, whether create/rename/delete were reflected,
and the `indexd` log.

## 2. IPC security checks

```powershell
# Pipe exists, with our SID in the name:
[System.IO.Directory]::GetFiles("\\.\pipe\") | Select-String explorer-rs
# Second daemon must refuse to start (exit code 0, log says already running):
(Start-Process .\explorer-rs-indexd.exe -Wait -PassThru).ExitCode
```

From an **elevated** PowerShell, `.\explorer-rs-indexd.exe status` should still
work (same user). If you have a second local account, `runas /user:other
"cmd /k explorer-rs-indexd.exe status"` must fail.

**Send back:** outputs and any error text.

## 3. Shutdown and restart from the snapshot

```powershell
.\explorer-rs-indexd.exe shutdown
dir $env:LOCALAPPDATA\explorer-rs\state\index.snapshot
.\explorer-rs-indexd.exe       # start again; log should say "snapshot loaded"
```

**Send back:** the log lines `snapshot saved` / `snapshot loaded` with their
`ms` values and the snapshot file size.

## 4. Elevated helper (MFT + USN journal)

Stop the daemon (`shutdown`). Change the config to index the whole system
drive:

```toml
[index]
roots = ["C:\\"]
use_ntfs = true
```

In an **elevated** PowerShell, from the repository checkout:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-index-helper.ps1 `
    -ExePath "C:\Program Files\explorer-rs\explorer-rs-index-helper.exe" -StartNow
```

Check `index-helper.*.log` for `index helper ready` and the list of volumes.
Then, in the **normal** PowerShell, start the daemon again and time it:

```powershell
.\explorer-rs-indexd.exe
# second window:
.\explorer-rs-indexd.exe status       # repeat until C:\ shows "ntfs ... live"
.\explorer-rs-indexd.exe query notepad.exe
.\explorer-rs-indexd.exe query "ext:dll kernel32"
Measure-Command { .\explorer-rs-indexd.exe query report } | Select TotalMilliseconds
```

**Send back:** the `MFT scan done` log line (entries, ms), `status` output
(entries, memory), the query outputs including the `matches in X ms` line, and
the helper log. If the daemon logs `no MFT access` or `NTFS source failed`,
send that line: it tells which part (pipe SDDL, client check, FSCTL) failed.

## 5. Change journal

With the daemon running on `C:\` (ntfs/live):

```powershell
New-Item -ItemType Directory C:\Temp\idxtest -Force
1..100 | ForEach-Object { New-Item "C:\Temp\idxtest\file-$_.txt" -ItemType File -Force | Out-Null }
.\explorer-rs-indexd.exe query "idxtest file-" --limit 3        # total should be 100
Rename-Item C:\Temp\idxtest C:\Temp\idxtest2
.\explorer-rs-indexd.exe query "idxtest2\file-1" --limit 3       # paths follow the renamed folder
Remove-Item C:\Temp\idxtest2 -Recurse
.\explorer-rs-indexd.exe query "file-" "path:idxtest2"           # 0 matches
```

**Send back:** outputs, and how long each change took to appear.

## 6. Restart catch-up and journal reset

```powershell
.\explorer-rs-indexd.exe shutdown
New-Item C:\Temp\made-while-stopped.txt -ItemType File -Force
.\explorer-rs-indexd.exe
.\explorer-rs-indexd.exe query made-while-stopped   # found without a full rescan
```

The log must show `resuming from saved journal position` (not `MFT scan done`).
Optional, **only in a VM**: `fsutil usn deletejournal /d C:` (elevated) while
the daemon runs, then `fsutil usn createjournal m=33554432 a=8388608 C:`; the
daemon should log `journal position lost; rescanning volume`.

**Send back:** the relevant log lines.

## 7. Privacy filtering

As a normal user, query a name that only exists in a folder you cannot open,
e.g. `.\explorer-rs-indexd.exe query "path:system volume information"` or a
file in another user's profile (`query "path:Users\otheruser"`). Results must
be empty (the `total` may be non-zero; that is documented). Then set
`check_access = false`, restart, and repeat: names appear.

**Send back:** both outputs.

## 8. Launcher integration

Build the launcher with `--features file-index`, enable the daemon in the
supervisor (`components = ["taskbar", "launcher", "indexd"]`), start
`explorer-rs.exe --dev`, open the launcher and type a file name.

**Send back:** whether a "Files" section appears, latency feel, and whether the
launcher stays responsive when the daemon is stopped (`shutdown`).

## 9. Clean up

```powershell
# elevated:
powershell -ExecutionPolicy Bypass -File .\scripts\install-index-helper.ps1 -Uninstall
```

Only after everything above works, consider [DISABLING-WSEARCH.md](DISABLING-WSEARCH.md).

## Report template

```
Windows version/build:
Machine (CPU, disk type, number of files on C:):
Step 1: status/queries/live changes:
Step 2: pipe/second instance/elevated client:
Step 3: snapshot saved/loaded ms, size:
Step 4: MFT scan entries/ms, status, query timings:
Step 5: journal changes and delays:
Step 6: resume/rescan log lines:
Step 7: privacy outputs:
Step 8: launcher:
Logs attached: indexd.*.log, index-helper.*.log
```
