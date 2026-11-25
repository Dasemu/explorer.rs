# Resource use (RAM / CPU)

explorer.rs exists to be lighter than the Windows 11 shell it replaces, so
this page says how to measure that and what has been measured so far. The
numbers below come from **one sample in one VM**; treat them as a first
indication, not as a benchmark.

## How to measure

[`scripts/measure-idle.ps1`](../scripts/measure-idle.ps1) is read-only. It
takes the processes of the Windows shell and of explorer.rs, waits
`-Seconds` (default 30), and prints per process:

- **CPU**: CPU time used during the window, as a percentage of one core;
- **WS**: working set (`WorkingSet64`): physical pages currently resident,
  *including* pages shared with other processes (system DLLs);
- **Private**: `PrivateMemorySize64`: memory committed by that process alone;
- threads and handles,

followed by the totals for the explorer.rs processes and for the Windows
shell processes (`explorer`, `StartMenuExperienceHost`, `SearchHost`,
`ShellExperienceHost`, `Widgets`, `WidgetService`, `TextInputHost`).

```powershell
powershell -ExecutionPolicy Bypass -File scripts\measure-idle.ps1 -Seconds 30
```

To compare configurations, run it in the same machine, with the same
amount of time since sign-in (about 10 minutes, so start-up work has
finished), without using the PC during the sample:

| Scenario | How to get there |
|---|---|
| A. Stock Windows | Before installing explorer.rs, or after "Restore Windows Explorer". |
| B. Alongside | Install in alongside mode: explorer.rs and the Windows shell both run. |
| C. Shell mode | Install in shell mode and sign in again: no `explorer.exe` shell, Start or Search host. |

Repeat each scenario several times (reboot between runs) and report the
spread, not one number. For system-wide effects also compare *Committed
Bytes* (`Get-Counter '\Memory\Committed Bytes'`) rather than "memory in use",
which includes the file cache.

## Results so far

Windows 11 Pro 26H2 (build 26300.9457) in VirtualBox: 4 vCPU, **3 GB RAM**,
Spanish UI, explorer.rs v0.1.7, 30 s samples, no user load, one sample per
scenario.

| Scenario | Processes counted | Working set | Private |
|---|---|---|---|
| A. Stock Windows | `explorer` 275 MB, `StartMenuExperienceHost` 107, `SearchHost` 108, `ShellExperienceHost` 50, `WidgetService` 24 | ≈ 564 MB | ≈ 140 MB |
| C. explorer.rs, shell mode | `explorer-rs` 16 + 14, `-taskbar` 55, `-launcher` 48, `-desktop` 50, `-indexd` 45, `-index-helper` 11 | ≈ 239 MB | ≈ 144 MB |

- **Working set** fell from about 564 MB to about 239 MB (−58 %).
- **Private memory** did not fall: about 140 MB vs. about 144 MB. Most of the
  working-set difference is shared system pages, so it overstates the saving
  in "RAM really needed". Of the explorer.rs total, `indexd` is the largest
  private consumer (about 52–67 MB with the file index built); without file
  search it is smaller.
- **CPU** at idle was about 0 % in both (at most 0.05 % of one core for any
  explorer.rs process).
- System-wide "memory in use" did not show a clear difference in this VM
  (1983 MB stock, 2161 MB shell mode): file cache and VM state vary between
  samples, so it proves nothing either way.

## Limitations

- One sample per scenario: no error bars, no repeatability data.
- A 3 GB VM with 4 vCPU, not real hardware: working sets under memory
  pressure may differ from a machine with plenty of RAM.
- Idle only: no windows opened, no Start menu use, no file search load, no
  battery or power measurement.
- Process lists differ between scenarios (which Windows processes are
  counted is a judgement call, listed above).
- The index daemon was measured after its first scan; its memory grows with
  the number of files on the volume.

Contributions with more machines, repeated runs and workloads are welcome.
