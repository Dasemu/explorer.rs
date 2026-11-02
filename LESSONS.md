# Lessons

Non-obvious findings. Add an entry whenever something surprises you.

- **`SHAppBarMessage` needs explorer.exe.** It is delivered as `WM_COPYDATA` to
  the `Shell_TrayWnd` window. As the real shell (no explorer), nobody answers.
  We must set the work area ourselves (`SPI_SETWORKAREA`), and eventually
  implement the AppBar server side.
- **A typo'd constant in a `match` arm becomes a catch-all binding.** When
  `WM_MOUSELEAVE` was not exported where we imported it, rustc first reported
  the import error. The arm `WM_MOUSELEAVE => ...` would otherwise have
  silently matched every message. Keep `unreachable_patterns` warnings fatal
  (clippy `-D warnings` does this).
- **Window procedures re-enter.** `SetWindowPos`, `SetWindowTextW` (EN_CHANGE)
  and others send messages synchronously while our handler is running. The
  handler lives in a `RefCell`; nested messages fall back to `DefWindowProcW`
  instead of creating aliasing `&mut`. Code must not rely on receiving those
  nested messages; e.g. the launcher refreshes results itself after
  clearing the query.
- **Lone Win key suppression:** swallowing the Win key-up confuses the system's
  key state. Injecting an unassigned VK (0xE8) between Win-down and Win-up
  makes Windows see a chord, so Start does not open, and nothing is swallowed.
- **GUI-subsystem binaries and consoles:** `windows_subsystem = "windows"`
  avoids console flashes when Winlogon/the supervisor start us, but `>`
  redirection and console output become unreliable. Diagnostics take
  `--output <file>`, and binaries call `AttachConsole(ATTACH_PARENT_PROCESS)`.
- **Clicking the taskbar must not activate it** (`MA_NOACTIVATE`), otherwise
  "click the active window's button to minimize it" sees the taskbar as the
  active window.
- **windows-rs 0.62:** `BOOL` lives in `windows::core`. `CreateMutexW` and
  `CreateJobObjectW` need the `Win32_Security` feature. Many functions take
  `Option<HWND>`.
- **`winvd` 0.0.49 depends on windows-rs 0.58**, so two `windows` versions end
  up in the graph when `virtual-desktops` is on. Its HWND type differs from
  ours; keep its API surface to index-based calls.
- **cargo-xwin** works with the stable toolchain in the container. The first
  *link* downloads the MSVC CRT/SDK (cached afterwards); `check`/`clippy` do
  not need it.
- **Start Menu `.lnk` names are not what users see.** Windows localizes them
  via `desktop.ini` (`LocalizedResourceName`), so scanning the folders shows
  "Command Prompt" on a Spanish system that displays "Símbolo del sistema".
  `shell:AppsFolder` returns the localized names; use it as the source.
- **PowerShell StrictMode + registry:** `(Get-ItemProperty key).PSObject.Properties.Name`
  throws `PropertyNotFoundStrict` when the key has no values. Read values
  through `(Get-Item key).GetValueNames()` / `.GetValue()` instead.
- **Toggling a popup from a button that deactivates it:** the mouse-down on
  the Start button deactivates (and hides) the menu before the click's
  Toggle arrives, which then reopens it. Remember when we hid on
  deactivation and ignore a Toggle within ~400 ms.
- **Tray in dev mode:** `Shell_NotifyIcon` uses `FindWindow("Shell_TrayWnd")`,
  which returns the first window in Z order. A second, topmost
  `Shell_TrayWnd` that forwards everything to explorer's lets a dev-mode
  taskbar see tray icons without breaking explorer.
- **Tooltips without a manifest:** comctl32 v5 is loaded unless the exe has
  a v6 manifest. Use `TTTOOLINFOW` V2 size (without `lpReserved`), and call
  `InitCommonControlsEx` first, or the class does not exist.
