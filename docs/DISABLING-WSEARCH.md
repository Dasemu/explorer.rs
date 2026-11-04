# Disabling Windows Search (WSearch)

The explorer.rs file index can replace the Windows Search indexer for file-name
search. Disabling WSearch saves CPU, disk I/O and memory. **Do this only after
the explorer.rs index has been verified on your machine**
([TESTING-INDEX-ON-WINDOWS.md](TESTING-INDEX-ON-WINDOWS.md)). Nothing in
explorer.rs ever does this automatically.

What you lose: content search (text inside documents) in File Explorer and
Outlook, the search box results in Settings/Start that rely on the indexer,
and indexed searches in apps that use the Windows Search API. Searches in
File Explorer still work, but slowly (they fall back to scanning).

All commands below need an **elevated** PowerShell (Run as administrator).

## Check the current state first

```powershell
Get-Service WSearch | Select-Object Name, Status, StartType
```

Write down `StartType` (normally `Automatic` or `AutomaticDelayedStart`); you
need it to revert.

## Disable

```powershell
Stop-Service WSearch
sc.exe config WSearch start= disabled
```

(`sc.exe`, not `sc`: in PowerShell `sc` is an alias of `Set-Content`. The
space after `start=` is required.)

Verify:

```powershell
Get-Service WSearch | Select-Object Name, Status, StartType
# Status: Stopped, StartType: Disabled
```

Optional, to reclaim disk space after you are sure you will not go back soon:
the index database lives in `C:\ProgramData\Microsoft\Search\Data`. Leave it
alone if unsure; Windows rebuilds it when the service is re-enabled.

## Revert

```powershell
sc.exe config WSearch start= delayed-auto   # or: start= auto, matching what you wrote down
Start-Service WSearch
Get-Service WSearch | Select-Object Name, Status, StartType
```

Windows rebuilds its index in the background afterwards; this can take hours
on a large disk.

## Notes

- Windows feature updates sometimes re-enable WSearch. Re-check after updates.
- Group Policy (`Computer Configuration > Administrative Templates > Windows
  Components > Search`) can also control the indexer; this guide does not
  use it so that reverting stays a one-liner.
