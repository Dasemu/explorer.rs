# Recovery: getting back to explorer.exe

explorer.rs can replace the Windows shell for your user account. It is early
development software and is unverified on real Windows. If you end up with an
empty or unusable desktop, use one of the paths below. They are ordered from
easiest to most involved.

## Before you install: prevention

1. **Test in a VM first, with a snapshot** (Hyper-V or VirtualBox). Roll back
   the snapshot if anything goes wrong.
2. **Create a second local administrator account** before registering the
   shell. It lets you repair the first account even if it cannot reach a UI.
3. Run `explorer-rs.exe --dev` for a while first. Dev mode runs alongside
   explorer.exe and changes no registry values.

Shell registration is per user (HKCU) and never automatic. Only
`scripts/install-shell.ps1` does it, and it saves a backup of your previous
value.

An empty `HKCU\Software\explorer-rs` key (no values) can be left behind by an
interrupted install. It is harmless: the scripts treat it as "no backup".

## Built-in safety net

In `--shell` mode the supervisor restarts crashed components with exponential
backoff. A component that crashes more than 5 times in 60 seconds is given up
on, and the others keep running. If the **taskbar** is given up on, the
supervisor launches `explorer.exe` as a fallback so you are not left without a
UI. This only helps if the supervisor itself starts and keeps running. If it
fails to start at all, use the paths below.

## Path 1: Task Manager, run explorer.exe

1. Press `Ctrl+Shift+Esc` (or `Ctrl+Alt+Del` then Task Manager).
2. `File` -> `Run new task`.
3. Type `explorer.exe` and press Enter. You get the normal desktop for this
   session only.

This does not undo the registration. Do Path 2 as well, or you will get
explorer.rs again at next sign-in.

## Path 2: run the restore script

From the same "Run new task" box (or any PowerShell window):

```
powershell -ExecutionPolicy Bypass -File <path>\restore-explorer.ps1
```

Add `-StartExplorer` to also stop explorer.rs and start explorer.exe now, and
`-Force` to skip the confirmation. The script restores the backed-up `Shell`
value, or deletes the per-user value if none existed. It needs no admin rights
and works without the backup key (it then just deletes the per-user value).

## Path 3: reg command

```
reg delete "HKCU\Software\Microsoft\Windows NT\CurrentVersion\Winlogon" /v Shell /f
```

With no per-user `Shell` value, the machine-wide default (`explorer.exe`)
applies at the next sign-in. Run it from Task Manager's "Run new task" (use
`cmd /k reg delete ...`) or from any command prompt. Optionally also run
`reg delete "HKCU\Software\explorer-rs" /f` to remove the backup key.

## Path 4: repair from another admin account

Use this when you cannot get any UI in the broken account.

1. Sign out of the broken account completely (its hive must not be loaded).
2. Sign in as the second administrator.
3. Open an elevated Command Prompt and run:

```
reg load HKU\Broken C:\Users\<name>\NTUSER.DAT
reg delete "HKU\Broken\Software\Microsoft\Windows NT\CurrentVersion\Winlogon" /v Shell /f
reg unload HKU\Broken
```

If `reg unload` fails with "access denied", close regedit and any other tool
using the hive, wait a few seconds, and retry. Then sign in as the original
user.

## Safe Mode

Whether the per-user HKCU `Shell` value is honored in Safe Mode is
**UNVERIFIED**. Do not rely on Safe Mode as your recovery plan. Prefer Paths 1
to 4 and the VM snapshot.

## Last resorts

- Roll back the VM snapshot, or use System Restore if you have a restore point.
- Create a new user profile and move your files over.
