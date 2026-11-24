# explorer.rs

A native, WebView-free Windows 11 shell: small native
programs that replace the web-based parts of the Windows shell (Start, Search,
Widgets, Copilot). Reliability comes before looks: every component runs as its
own process and is restarted if it crashes.

> **Status: Release candidate — feature complete, awaiting testing on real Windows.**

This project is not affiliated with, endorsed by, or sponsored by Microsoft.
Windows is a trademark of Microsoft Corporation.

## Install

Download `explorer-rs-setup-<version>-x64.exe` and run it. By default
explorer.rs runs alongside Windows Explorer; replacing the shell is an option.
See [docs/INSTALL.md](docs/INSTALL.md). Try shell mode in a VM first.

Updates: explorer.rs checks for new releases once a day and reminds you every
few days while one is available; **Update now** downloads, verifies and
installs it (keeping your settings and start mode). Turn it off in
Settings -> Updates. Details: [docs/INSTALL.md](docs/INSTALL.md#updates).

## If something goes wrong

Press **Ctrl+Alt+Shift+E** to start Windows Explorer, or use Start menu ->
"Restore Windows Explorer". More options: [docs/RECOVERY.md](docs/RECOVERY.md).

## Features

- Taskbar with window grouping, pins, thumbnails, system tray, quick settings
  (volume, brightness, network, battery) and a calendar
- Windows 11-style Start menu with search of apps, settings, files and a
  calculator
- Automatic recovery: a guardian restarts crashed components and falls back to
  Windows Explorer
- Optional shell replacement, with wallpaper desktop and startup apps
- Fast file search (NTFS MFT/USN index)
- English and Spanish

Configuration: Start menu -> "explorer.rs settings" (`explorer-rs.exe --settings`)
opens a settings window; it edits `%APPDATA%\explorer-rs\config.toml` (see
`config/default.toml`), which `explorer-rs.exe --edit-config` opens in Notepad.
Logs: `%LOCALAPPDATA%\explorer-rs\logs\`.

## Documentation

- [Architecture](docs/ARCHITECTURE.md) and [design decisions](docs/decisions/)
- [Status](docs/STATUS.md) and [testing on Windows](docs/TESTING-ON-WINDOWS.md)
- [Installing](docs/INSTALL.md), [packaging](docs/PACKAGING.md),
  [recovery](docs/RECOVERY.md), [shell mode](docs/SHELL-MODE.md)
- [Taskbar](docs/TASKBAR.md), [Start menu](docs/START-MENU.md),
  [file index](docs/INDEX.md)
- [Lessons learned](LESSONS.md)

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
