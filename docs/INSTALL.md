# Installing explorer.rs on Windows 11

explorer.rs is early software and parts of it are not yet verified on real
Windows. The **installer** is the recommended way to install it. The portable
zip and its PowerShell scripts remain available as an alternative that needs
no administrator rights.

Before you start:

- **SmartScreen.** The installer is not code-signed yet, so Windows may show
  "Windows protected your PC". Click **More info** -> **Run anyway**. Some
  antivirus products need the same confirmation.
- **Managed PCs** (work, school) may forbid unsigned programs or a replaced
  shell. Do not try this there without permission.
- **Shell replacement:** try it in a VM with a snapshot first, and read
  [RECOVERY.md](RECOVERY.md).

## 1. Installer (recommended)

Run `explorer-rs-setup-<version>-x64.exe`. It asks for administrator rights
(UAC), then:

1. **Welcome**: version, license summary (GPL-3.0-or-later, no warranty) and a
   note that explorer.rs is not affiliated with Microsoft.
2. **Options**
   - **When should explorer.rs start?**
     - *Run alongside Windows Explorer at sign-in (recommended, default)*: adds
       a per-user `Run` entry (`"...\explorer-rs.exe" --dev`). Nothing in
       Windows is replaced.
     - *Replace Windows Explorer as my shell (advanced)*: registers
       explorer.rs as **your** shell (per-user Winlogon `Shell` value; the
       previous value is backed up). To go back, press **Ctrl+Alt+Shift+E** or
       run **Restore Windows Explorer** from the Start menu.
     - *Don't start automatically*: start it from the Start menu.
   - **Fast file search** (default on): registers the elevated index helper as
     a logon scheduled task (`\explorer-rs\IndexHelper`, see
     [ADR 0010](decisions/0010-index-privilege-model.md)) and turns on the
     file index for explorer.rs.
   - **Start explorer.rs now** (default on).
   - The install folder is fixed: `%ProgramFiles%\explorer-rs`. Only
     administrators can change files there, which the elevated index helper
     requires.
3. **Progress** and **Done**. Every step is logged to
   `%ProgramData%\explorer-rs\setup.log`.

Per-user choices apply to the user who is signed in, even if the UAC prompt
was answered with a different administrator account.

The installer creates these Start menu entries (all users,
`Start menu\Programs\explorer.rs`):

| Shortcut | Runs |
|---|---|
| explorer.rs | `explorer-rs.exe --dev` |
| Restore Windows Explorer | `explorer-rs.exe --restore-explorer` |
| explorer.rs settings | `explorer-rs-settings.exe`: creates `%APPDATA%\explorer-rs\config.toml` if missing and opens it in Notepad |
| Uninstall explorer.rs | `uninstall.exe --uninstall` |

**Upgrading:** run the newer installer. Your previous choices are
pre-selected; your settings (`%APPDATA%\explorer-rs`) and data
(`%LOCALAPPDATA%\explorer-rs`) are kept. If explorer.rs is running, setup
closes it and starts it again at the end. If it is your shell, your taskbar
disappears for a moment.

### Silent install and command line

```
explorer-rs-setup.exe --silent [--mode=alongside|shell|none] [--file-search=on|off] [--no-launch] [--lang=en|es]
```

Without options, a silent install uses the previous choices, or the defaults
(alongside, fast file search on, start now). `/silent` and `/?` work too.

| Exit code | Meaning |
|---|---|
| 0 | success |
| 1 | error (see `%ProgramData%\explorer-rs\setup.log`) |
| 2 | cancelled by the user |
| 3 | success, but a reboot is needed to delete files that were in use |

### Uninstall

Settings -> Apps -> **Installed apps** -> explorer.rs -> Uninstall, or the
**Uninstall explorer.rs** Start menu shortcut. The uninstaller:

- stops explorer.rs;
- makes Windows Explorer your shell again if explorer.rs was your shell, for
  you **and for every other signed-in user** whose shell points into the
  install folder, and removes the sign-in entries;
- removes the scheduled tasks, Start menu shortcuts, the Installed apps entry
  and the installed files;
- with **Also remove my settings and logs** (off by default), deletes
  `%APPDATA%\explorer-rs` and `%LOCALAPPDATA%\explorer-rs`;
- starts Windows Explorer if no desktop is running.

Users who are **signed out** while you uninstall are not checked (their
registry is not loaded). If one of them had explorer.rs as their shell,
follow [RECOVERY.md](RECOVERY.md) for that account.

Silent: `"%ProgramFiles%\explorer-rs\uninstall.exe" --uninstall --silent
[--remove-user-data]`. It returns the exit codes above. After a silent
uninstall, `uninstall.exe` itself is deleted at the next reboot.

### Signing the installer (maintainers)

The build is unsigned. To sign it with a code-signing certificate on Windows:

```
signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /a explorer-rs-setup-<version>-x64.exe
```

Sign the binaries in the payload *before* packing (see
[PACKAGING.md](PACKAGING.md)), then sign the setup. Signing removes the
"unknown publisher" warning; SmartScreen reputation still builds over time.

## 2. Portable zip (no administrator rights)

`explorer-rs-<version>-x86_64-pc-windows-msvc.zip` contains the same programs
plus PowerShell scripts. Right-click the zip -> Properties -> tick **Unblock**
before extracting. Run scripts as
`powershell -ExecutionPolicy Bypass -File <script>`.

- **Try it** without installing: `.\explorer-rs.exe --dev` (quit: right-click
  an empty area of the taskbar -> Quit explorer.rs).
- **Per-user install**: `.\scripts\install.ps1 -Autostart -StartNow` copies the
  program to `%LOCALAPPDATA%\Programs\explorer-rs`; remove it with
  `scripts\uninstall.ps1` there. Do not combine this with the installer.
- **Replace the shell** from the per-user install: `scripts\install-shell.ps1`;
  go back with `scripts\restore-explorer.ps1 -StartExplorer`.
- **Fast file search** with the zip: copy the binaries to a folder only
  administrators can modify, then run `scripts\install-index-helper.ps1` from
  an elevated PowerShell.

If you have no usable UI: press `Ctrl+Shift+Esc` -> File -> **Run new task**
-> `explorer.exe`. More options in [RECOVERY.md](RECOVERY.md).
