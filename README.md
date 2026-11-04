# explorer.rs

A native, WebView-free Windows 11 shell replacement written in Rust: a taskbar,
a launcher, and a tray. It replaces the web-based parts of the Windows shell
(Start, Search, Widgets, Copilot) with small native components. Reliability
comes before looks: each component runs as its own process and a supervisor
restarts it if it crashes.

> **Status: Early development. Unverified on real Windows. Test in a VM.**

This project is not affiliated with, endorsed by, or sponsored by Microsoft.
Windows is a trademark of Microsoft Corporation.

## Quick start

Run alongside your normal desktop (does not replace explorer.exe):

```
explorer-rs.exe --dev
```

Running without `--dev` or `--shell` prints help and exits. Do not register it
as your shell before reading [docs/RECOVERY.md](docs/RECOVERY.md).

Configuration lives in `%APPDATA%\explorer-rs\config.toml` (see
`config/default.toml`). Logs are in `%LOCALAPPDATA%\explorer-rs\logs\`.

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Status](docs/STATUS.md)
- [Installing on Windows](docs/INSTALL.md)
- [Testing on Windows](docs/TESTING-ON-WINDOWS.md)
- [Recovery](docs/RECOVERY.md)
- [Design decisions](docs/decisions/)
- [Lessons learned](LESSONS.md)

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
