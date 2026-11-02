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
| `SetTaskmanWindow` / `SetShellWindow` (user32 exports) | possibly shell mode (not used yet) | — |

Documented but "not intended for general use": `RegisterShellHookWindow`.
It is used unconditionally because the WinEvent hooks provide an independent
second source of the same information.
