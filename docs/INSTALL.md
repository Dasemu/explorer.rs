# Installing explorer.rs on Windows 11

explorer.rs is early development software, unverified on real Windows. Pick the
path that matches how far you want to go. Paths 1 and 2 never replace your shell.

Before you start:

- **Unblock the zip first.** Right-click the downloaded `.zip` -> Properties ->
  tick **Unblock** -> OK, then extract it.
- **SmartScreen / antivirus** may block the unsigned `.exe`. Click
  "More info" -> "Run anyway", or allow it in your antivirus.
- **Managed PCs** (work, school) may forbid running unsigned programs or
  changing the shell. Do not try this there without permission.
- **Scripts and ExecutionPolicy.** Run every script like this:
  `powershell -ExecutionPolicy Bypass -File <script>`

## 1. Try it (nothing is installed)

1. Unzip, open the folder in PowerShell.
2. Run `.\explorer-rs.exe --dev`

It runs next to explorer.exe and changes no registry values. To quit:
right-click an empty area of the explorer.rs taskbar -> **Quit explorer.rs**.

## 2. Install for your user (optional autostart)

No admin rights needed; only HKCU and your user folders are touched.

```
powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -Autostart -StartNow
```

- Copies the program to `%LOCALAPPDATA%\Programs\explorer-rs`
  (change with `-Destination`).
- Creates `%APPDATA%\explorer-rs\config.toml` if it does not exist.
- `-Autostart` starts it in dev mode (`--dev`, alongside explorer.exe) at
  sign-in. `-StartNow` starts it immediately. `-Force` skips the confirmation.

Uninstall:

```
powershell -ExecutionPolicy Bypass -File "%LOCALAPPDATA%\Programs\explorer-rs\scripts\uninstall.ps1"
```

It removes the autostart entry and the install folder, and keeps your config and
logs unless you add `-RemoveData`. If the shell is still replaced (path 3), it
refuses and asks you to run `restore-explorer.ps1` first.

## 3. Replace the shell (try in a VM with a snapshot first)

1. Do path 2 first, so the shell points at a stable location (not Downloads).
2. Create a second local administrator account and read
   [RECOVERY.md](RECOVERY.md).
3. Run, from the installed folder:

```
powershell -ExecutionPolicy Bypass -File "%LOCALAPPDATA%\Programs\explorer-rs\scripts\install-shell.ps1"
```

4. Type `YES` when asked, then sign out and back in.

### Going back

```
powershell -ExecutionPolicy Bypass -File "%LOCALAPPDATA%\Programs\explorer-rs\scripts\restore-explorer.ps1" -StartExplorer
```

If you have no usable UI: press `Ctrl+Shift+Esc` -> File -> **Run new task** ->
`explorer.exe`, then run the restore script. More options in
[RECOVERY.md](RECOVERY.md).
