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

Download `explorer-rs-setup-<version>-x64.exe` from the
[GitHub Releases](https://github.com/FerroApps/explorer.rs/releases) page
(`SHA256SUMS.txt` next to it has the checksums) and run it. It asks for administrator rights
(UAC), then:

1. **Welcome**: version, license summary (GPL-3.0-or-later, no warranty) and a
   note that explorer.rs is not affiliated with Microsoft.
2. **Options**
   - **When should explorer.rs start?**
     - *Run alongside Windows Explorer at sign-in*: adds
       a per-user `Run` entry (`"...\explorer-rs.exe" --dev`). Nothing in
       Windows is replaced. While explorer.rs runs, **Windows' own taskbar
       is hidden** (set to auto-hide and hidden; its previous auto-hide
       setting is saved and restored when explorer.rs stops, when our
       taskbar stops for good, and with **Ctrl+Alt+Shift+E**). Set
       `[taskbar] hide_windows_taskbar = false` to keep both.
     - *Replace Windows Explorer as my shell (recommended, default)*:
       registers explorer.rs as **your** shell (per-user Winlogon `Shell`
       value; the previous value is backed up). It takes over completely at
       your next sign-in; until then it runs next to Windows Explorer with
       Windows' taskbar hidden. To go back, press **Ctrl+Alt+Shift+E** or
       run **Restore Windows Explorer** from the Start menu.
     - *Don't start automatically*: start it from the Start menu.
   - **Fast file search** (default on): registers the elevated index helper as
     a logon scheduled task (`\explorer-rs\IndexHelper`, see
     [ADR 0010](decisions/0010-index-privilege-model.md)) and turns on the
     file index for explorer.rs. The helper runs elevated as the signed-in
     user, so it needs an **administrator** account; for a standard user
     setup skips it with a warning and the index walks folders instead
     (works, first scan slower).
   - **Check for updates automatically?** *Yes* / *No*, with no default on
     a first install: **Install** stays disabled until you pick one. *Yes*
     means once a day explorer.rs asks GitHub whether a new version exists
     and shows a reminder while one does; nothing is installed without
     asking (see [Updates](#updates)). On an upgrade your previous answer is
     pre-selected (an upgrade from a version that did not ask yet asks).
     The answer is stored machine-wide as `CheckUpdates` (DWORD 1/0) under
     `HKLM\Software\explorer-rs` and is only a default: a
     `general.check_updates` key in your `config.toml` (written when you
     change the box in Settings -> Updates) wins.
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
| explorer.rs settings | `explorer-rs.exe --settings` (runs as you, not elevated): the settings window, which edits `%APPDATA%\explorer-rs\config.toml` in place (comments kept) and creates it from the default on the first save. `explorer-rs.exe --edit-config` (also its "Edit config file…" button) creates the file if missing and opens it in Notepad |
| Uninstall explorer.rs | `uninstall.exe --uninstall` |

**Upgrading:** run the newer installer. Your previous choices are
pre-selected; your settings (`%APPDATA%\explorer-rs`) and data
(`%LOCALAPPDATA%\explorer-rs`) are kept. If explorer.rs is running, setup
closes it and starts it again at the end. If it is your shell, your taskbar
disappears for a moment.

### Silent install and command line

```
explorer-rs-setup.exe --silent [--mode=alongside|shell|none] [--file-search=on|off] [--updates=on|off] [--no-launch] [--lang=en|es]
```

Without options, a silent install uses the previous choices, or the defaults
(shell mode, fast file search on, start now). `/silent` and `/?` work too.

`--updates=on|off` answers "Check for updates automatically?". Without it, a
silent install keeps the previous install's answer; with no previous answer
(first install, or an upgrade from a version that did not ask) it uses
**on**, the app's built-in default. `--updates` also works without
`--silent`: the wizard then starts with that answer selected.

| Exit code | Meaning |
|---|---|
| 0 | success |
| 1 | error (see `%ProgramData%\explorer-rs\setup.log`) |
| 2 | cancelled by the user |
| 3 | success, but a reboot is needed to delete files that were in use |

### Updates

explorer.rs checks GitHub once a day for a newer release if you answered
*Yes* to "Check for updates automatically?" in the installer; Settings ->
**Updates** changes it at any time. Which answer applies:

1. `general.check_updates` in `%APPDATA%\explorer-rs\config.toml`, if the
   key is there (Settings writes it only when you change the box);
2. else the installer's answer (`HKLM\Software\explorer-rs\CheckUpdates`);
3. else on (portable copies, installs from before the question existed).

While
one is available, a small card above the clock says so every few days, with
**Update now**, **Later** and **Don't remind me again** (for that version).
Nothing is installed without your click, and no check runs while it is off.

**Update now** (on the card, or Settings -> Updates) downloads the new
`explorer-rs-setup-<version>-x64.exe` and the release's `SHA256SUMS.txt` to
`%LOCALAPPDATA%\explorer-rs\updates`, refuses the file unless its name,
size and SHA-256 match the release, and runs it with `--silent` after
Windows asks for administrator permission. A silent upgrade keeps the start
mode, the fast file search choice, your "check for updates" answer and all
your settings; explorer.rs is
stopped and started again. If the permission prompt is refused or does not
appear, the downloaded installer stays in that folder: run it yourself, or
get it from the [releases page](https://github.com/FerroApps/explorer.rs/releases).
A portable copy is not updated in place; it is sent to the releases page.

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
