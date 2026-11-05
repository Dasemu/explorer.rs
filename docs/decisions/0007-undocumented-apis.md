# ADR 0007: Policy on undocumented APIs

- Status: accepted
- Date: 2026-11-02

## Decision
Undocumented interfaces are allowed only behind a cargo feature that is off
by default, must fail gracefully (an error, never a panic or crash loop), and
must be listed here and in STATUS.md.

| API | Used by | Feature |
|---|---|---|
| `IVirtualDesktopManagerInternal` & co. (via `winvd`) | virtual desktops | `virtual-desktops` |
| `Shell_TrayWnd` `WM_COPYDATA` payload layout | tray host (planned) | `tray` |
| `SetTaskmanWindow` (user32 export) | possibly shell mode (not used yet) | — |
| `SetShellWindow` (user32 export, `GetProcAddress`) | desktop window, shell mode only; failure ignored (ADR 0014) | — (mode-gated) |
| `msgina: ShellReadyEvent`, `ShellDesktopSwitchEvent` (named events set by the shell) | supervisor, shell mode, once per sign-in; missing events ignored | — (mode-gated) |
| `Explorer\StartupApproved\{Run,Run32,StartupFolder}` binary format | startup apps, shell mode; unknown data = enabled | — (mode-gated) |
| `HKCU\Control Panel\NotifyIconSettings\*` registry layout (`ExecutablePath`, `UID`, `IsPromoted`) — read only | taskbar tray promotion | — (missing/changed layout = every icon in the overflow; `tray.show_all = true` bypasses) |
| `"DarkMode_Explorer"` visual-style class name for `SetWindowTheme` | setup wizard: dark buttons, check boxes and scroll bars (ADR 0013) | — (cosmetic only: an unknown class leaves the light look; nothing else depends on it) |

Documented but "not intended for general use": `RegisterShellHookWindow`.
It is used unconditionally because the WinEvent hooks provide an independent
second source of the same information.
