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
