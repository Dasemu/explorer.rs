# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Registers explorer-rs.exe as the current user's Windows shell (HKCU only).

.DESCRIPTION
    Backs up the current per-user Winlogon "Shell" value to HKCU:\Software\explorer-rs,
    then sets HKCU\Software\Microsoft\Windows NT\CurrentVersion\Winlogon\Shell to
    "<full path>\explorer-rs.exe" --shell.

    - Does NOT require administrator rights.
    - Never touches HKLM.
    - Idempotent: running it again keeps the ORIGINAL backup and only refreshes the Shell value.
    - Never run automatically. Test in a VM with a snapshot first (see docs/RECOVERY.md).

.PARAMETER ExePath
    Path to explorer-rs.exe. Defaults to ..\explorer-rs.exe or .\explorer-rs.exe relative to this script.

.PARAMETER Force
    Skip the interactive "type YES" confirmation.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\scripts\install-shell.ps1
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$ExePath,
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$WinlogonKey = 'HKCU:\Software\Microsoft\Windows NT\CurrentVersion\Winlogon'
$BackupKey   = 'HKCU:\Software\explorer-rs'

# Safety: this script only ever uses HKCU paths.
foreach ($k in @($WinlogonKey, $BackupKey)) {
    if (-not $k.StartsWith('HKCU:\')) { throw "Refusing to touch non-HKCU key: $k" }
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

# Resolve the executable.
if ([string]::IsNullOrWhiteSpace($ExePath)) {
    $candidates = @(
        (Join-Path $scriptDir '..\explorer-rs.exe'),
        (Join-Path $scriptDir 'explorer-rs.exe')
    )
    $ExePath = $null
    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c -PathType Leaf) { $ExePath = $c; break }
    }
    if (-not $ExePath) {
        throw "explorer-rs.exe not found next to the scripts folder. Pass -ExePath <path>."
    }
}
if (-not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
    throw "Executable not found: $ExePath"
}
$ExePath = (Resolve-Path -LiteralPath $ExePath).ProviderPath
if ((Split-Path -Leaf $ExePath) -ne 'explorer-rs.exe') {
    throw "Expected a file named explorer-rs.exe, got: $ExePath"
}

# The restore script must exist before we change anything.
$restoreScript = Join-Path $scriptDir 'restore-explorer.ps1'
if (-not (Test-Path -LiteralPath $restoreScript -PathType Leaf)) {
    throw "restore-explorer.ps1 not found next to this script ($restoreScript). Refusing to continue."
}
$restoreScript = (Resolve-Path -LiteralPath $restoreScript).ProviderPath

$newShell = '"{0}" --shell' -f $ExePath

# Read the current per-user Shell value, if any.
$existed = $false
$current = ''
if (Test-Path -LiteralPath $WinlogonKey) {
    $item = Get-ItemProperty -LiteralPath $WinlogonKey
    if ($item.PSObject.Properties.Name -contains 'Shell') {
        $existed = $true
        $current = [string]$item.Shell
    }
}

if ($existed -and $current -eq $newShell) {
    Write-Host 'The Shell value is already set to explorer-rs. Nothing to change.'
    Write-Host "  Shell = $current"
    return
}

Write-Host ''
Write-Host '################################################################' -ForegroundColor Red
Write-Host '#                          WARNING                             #' -ForegroundColor Red
Write-Host '################################################################' -ForegroundColor Red
Write-Host ' This replaces your Windows shell (explorer.exe) for YOUR USER.'
Write-Host ' explorer.rs is in EARLY DEVELOPMENT and UNVERIFIED on real Windows.'
Write-Host ' After you sign out and back in, you may see no usable UI.'
Write-Host ''
Write-Host ' Strongly recommended before continuing:'
Write-Host '   - Test in a VM with a snapshot first.'
Write-Host '   - Create a second local administrator account.'
Write-Host '   - Read docs\RECOVERY.md.'
Write-Host ''
Write-Host " Current per-user Shell : $(if ($existed) { $current } else { '(not set; the system default explorer.exe applies)' })"
Write-Host " New per-user Shell     : $newShell"
Write-Host ''
Write-Host ' Recovery if the screen is empty after sign-in:'
Write-Host '   1. Press Ctrl+Shift+Esc to open Task Manager.'
Write-Host '   2. File -> Run new task.'
Write-Host '   3. Run:  powershell -ExecutionPolicy Bypass -File "' -NoNewline
Write-Host "$restoreScript`""
Write-Host '      or simply:  explorer.exe'
Write-Host '################################################################' -ForegroundColor Red

if (-not $Force) {
    $answer = Read-Host 'Type YES (uppercase) to continue'
    if ($answer -cne 'YES') {
        Write-Host 'Aborted. Nothing was changed.'
        return
    }
}

# Back up only once so re-running never overwrites the original value.
if (-not (Test-Path -LiteralPath $BackupKey)) {
    New-Item -Path $BackupKey -Force | Out-Null
}
$backup = Get-ItemProperty -LiteralPath $BackupKey
$hasBackup = $backup.PSObject.Properties.Name -contains 'PreviousShellExisted'
if (-not $hasBackup) {
    Set-ItemProperty -LiteralPath $BackupKey -Name 'PreviousShell' -Value $current -Type String
    Set-ItemProperty -LiteralPath $BackupKey -Name 'PreviousShellExisted' -Value ([int]$existed) -Type DWord
    Set-ItemProperty -LiteralPath $BackupKey -Name 'InstalledAt' -Value ((Get-Date).ToString('o')) -Type String
    Write-Host 'Backed up the previous Shell value to HKCU:\Software\explorer-rs.'
} else {
    Write-Host 'A backup already exists; keeping the original backup.'
}

if (-not (Test-Path -LiteralPath $WinlogonKey)) {
    New-Item -Path $WinlogonKey -Force | Out-Null
}
Set-ItemProperty -LiteralPath $WinlogonKey -Name 'Shell' -Value $newShell -Type String

Write-Host ''
Write-Host 'Done. HKCU Winlogon Shell is now:' -ForegroundColor Green
Write-Host "  $newShell"
Write-Host ''
Write-Host 'Sign out and sign back in to use explorer.rs as your shell.'
Write-Host ''
Write-Host 'To get back:'
Write-Host '  Ctrl+Shift+Esc -> File -> Run new task ->'
Write-Host "    powershell -ExecutionPolicy Bypass -File `"$restoreScript`""
Write-Host '  or run explorer.exe, or see docs\RECOVERY.md.'
