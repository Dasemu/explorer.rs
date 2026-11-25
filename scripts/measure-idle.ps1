<#
.SYNOPSIS
  Idle RAM / CPU of the shell processes: explorer.rs vs. stock Windows.
.DESCRIPTION
  Samples per-process CPU over -Seconds, then reports CPU (% of one core),
  working set and private memory for every process that belongs to the
  Windows shell or to explorer.rs. Read-only: it changes nothing.
  See docs/PERFORMANCE.md for the methodology and its limits.
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\measure-idle.ps1 -Seconds 30
#>
param(
    [int]$Seconds = 30,
    [string[]]$Names = @(
        'explorer', 'StartMenuExperienceHost', 'SearchHost', 'ShellExperienceHost',
        'Widgets', 'WidgetService', 'TextInputHost', 'explorer-rs*'
    )
)

function Get-ShellProcess {
    Get-Process | Where-Object {
        $n = $_.ProcessName
        $Names | Where-Object { $n -like $_ }
    } | ForEach-Object {
        [pscustomobject]@{
            Id      = $_.Id
            Name    = $_.ProcessName
            CPU     = $_.CPU
            WSMB    = [math]::Round($_.WorkingSet64 / 1MB, 1)
            PrivMB  = [math]::Round($_.PrivateMemorySize64 / 1MB, 1)
            Threads = $_.Threads.Count
            Handles = $_.HandleCount
        }
    }
}

$before = Get-ShellProcess
$start = Get-Date
Start-Sleep -Seconds $Seconds
$after = Get-ShellProcess
$elapsed = ((Get-Date) - $start).TotalSeconds

$rows = foreach ($p in $after) {
    $old = $before | Where-Object Id -eq $p.Id
    if (-not $old) { continue }   # started during the sample: no CPU delta
    [pscustomobject]@{
        Name            = $p.Name
        Id              = $p.Id
        'CPU% (1 core)' = [math]::Round((($p.CPU - $old.CPU) / $elapsed) * 100, 2)
        WSMB            = $p.WSMB
        PrivMB          = $p.PrivMB
        Threads         = $p.Threads
        Handles         = $p.Handles
    }
}

$rows | Sort-Object Name | Format-Table -AutoSize | Out-String -Width 200

$os = Get-CimInstance Win32_OperatingSystem
$totalMB = [int]($os.TotalVisibleMemorySize / 1024)
$freeMB = [int]($os.FreePhysicalMemory / 1024)
"System: total RAM $totalMB MB, free $freeMB MB, used $($totalMB - $freeMB) MB; " +
    "processes $((Get-Process).Count); logical cores " +
    "$((Get-CimInstance Win32_Processor | Measure-Object NumberOfLogicalProcessors -Sum).Sum)"

foreach ($group in @(
        @{ Label = 'explorer.rs (explorer-rs*)'; Filter = { $_.Name -like 'explorer-rs*' } },
        @{ Label = 'Windows shell (everything else)'; Filter = { $_.Name -notlike 'explorer-rs*' } })) {
    $set = @($rows | Where-Object $group.Filter)
    if ($set.Count -eq 0) { continue }
    $ws = ($set | Measure-Object WSMB -Sum).Sum
    $priv = ($set | Measure-Object PrivMB -Sum).Sum
    $cpu = ($set | Measure-Object 'CPU% (1 core)' -Sum).Sum
    "{0}: {1} processes, working set {2:N1} MB, private {3:N1} MB, CPU {4:N2}% of one core" -f `
        $group.Label, $set.Count, $ws, $priv, $cpu
}
