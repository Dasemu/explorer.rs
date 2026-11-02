# ADR 0003: Screen-space reservation by run mode

- Status: accepted
- Date: 2026-11-02

## Context
Maximized windows must not overlap the taskbar. The documented mechanism,
`SHAppBarMessage`, is implemented by explorer.exe (`Shell_TrayWnd`).

## Decision
- **Dev mode** (explorer.exe running): register as an AppBar
  (`ABM_NEW`/`ABM_QUERYPOS`/`ABM_SETPOS`), react to `ABN_POSCHANGED` and
  `ABN_FULLSCREENAPP`, and release with `ABM_REMOVE`.
- **Shell mode**: set each monitor's work area with `SPI_SETWORKAREA`, and
  restore it on exit.

## Consequences
- Dev mode coexists with explorer's own taskbar (the bars stack).
- In shell mode, other applications' AppBars are not arbitrated until we
  implement the AppBar server side (tied to the `tray` feature, which also
  needs `Shell_TrayWnd`).
- If the taskbar is killed in shell mode, the work area stays reduced until
  it restarts (the supervisor restarts it within ~0.5 s).
