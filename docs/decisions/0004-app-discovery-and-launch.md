# ADR 0004: App discovery via Start Menu + shell:AppsFolder

- Status: accepted
- Date: 2026-11-02

## Context
The launcher must list classic and packaged (UWP/MSIX) apps, without
depending on Windows Search or explorer.exe.

## Decision
- Scan the Start Menu `Programs` known folders (user + common) with
  `std::fs`. This part is pure and testable.
- Enumerate `shell:AppsFolder` with `IShellItem` + `BHID_EnumItems`. The
  parent-relative parsing name is the AUMID for packaged apps.
- Launch packaged apps with `IApplicationActivationManager::ActivateApplication`
  and everything else with `ShellExecuteW`. All of these APIs are documented.
- Merge by display name; a Start Menu shortcut wins over an AppsFolder item.

## Consequences
- No undocumented APIs on the launcher path.
- No dependency on WSearch, no web results.
- AppsFolder enumeration can be slow (hundreds of ms), so it runs on a worker
  thread.
- Display-name merge can occasionally keep two entries for the same app with
  different names; acceptable for now.
