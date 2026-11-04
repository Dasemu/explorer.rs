# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Registers (or removes) the explorer.rs index helper as a logon scheduled task
    that runs elevated for the current user.

.DESCRIPTION
    explorer-rs-index-helper.exe reads the NTFS master file table and USN change
    journal, which requires administrator rights, and streams raw records to the
    unprivileged explorer-rs-indexd.exe over a named pipe that only the current
    user can open. See docs/decisions/0010-index-privilege-model.md.

    This script creates the scheduled task "\explorer-rs\index helper":
      - trigger:   at logon of the current user;
      - principal: the current user, interactive, "Run with highest privileges";
      - action:    <ExePath> (no arguments);
      - no time limit, restarts up to 3 times on failure.

    Without the helper, explorer-rs-indexd still works: it walks the folders
    and watches them for changes (slower first scan, more I/O).

    Safety:
      - Never run automatically. You run it, once, from an elevated PowerShell.
      - An elevated task must only run a binary that non-administrators cannot
        replace, otherwise any program running as you could gain admin rights
        by swapping the file. The script refuses (unless -Force) when the helper
        or its folder is writable by non-administrators. Install the binaries to
        a protected folder such as "C:\Program Files\explorer-rs".
      - -Uninstall removes the task and stops the helper. Nothing else is changed.

.PARAMETER ExePath
    Full path to explorer-rs-index-helper.exe. Defaults to the file next to this
    script's parent folder, or in the same folder as this script.

.PARAMETER Uninstall
    Remove the scheduled task and stop a running helper.

.PARAMETER StartNow
    Start the task right after registering it (otherwise it starts at next logon).

.PARAMETER Force
    Skip the confirmation prompt and the folder permission check.

.EXAMPLE
    # Elevated PowerShell:
    powershell -ExecutionPolicy Bypass -File .\scripts\install-index-helper.ps1 -ExePath "C:\Program Files\explorer-rs\explorer-rs-index-helper.exe" -StartNow

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\scripts\install-index-helper.ps1 -Uninstall
#>
#Requires -Version 5.1
#Requires -RunAsAdministrator
[CmdletBinding()]
param(
    [string]$ExePath,
    [switch]$Uninstall,
    [switch]$StartNow,
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$TaskPath = '\explorer-rs\'
$TaskName = 'index helper'
$UserId   = "$env:USERDOMAIN\$env:USERNAME"

function Stop-Helper {
    Get-Process -Name 'explorer-rs-index-helper' -ErrorAction SilentlyContinue |
        ForEach-Object {
            Write-Host "Stopping helper process $($_.Id)"
            Stop-Process -Id $_.Id -Force
        }
}

if ($Uninstall) {
    $task = Get-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -ErrorAction SilentlyContinue
    if ($null -eq $task) {
        Write-Host "No scheduled task '$TaskPath$TaskName' found. Nothing to do."
    } else {
        Stop-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -Confirm:$false
        Write-Host "Removed scheduled task '$TaskPath$TaskName'."
    }
    Stop-Helper
    Write-Host 'explorer-rs-indexd will fall back to walking folders after its next restart.'
    exit 0
}

if (-not $ExePath) {
    $candidates = @(
        (Join-Path (Split-Path -Parent $PSScriptRoot) 'explorer-rs-index-helper.exe'),
        (Join-Path $PSScriptRoot 'explorer-rs-index-helper.exe')
    )
    $ExePath = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $ExePath) {
        throw "explorer-rs-index-helper.exe not found next to this script; pass -ExePath."
    }
}
$ExePath = (Resolve-Path -LiteralPath $ExePath).Path
$Dir = Split-Path -Parent $ExePath
$Daemon = Join-Path $Dir 'explorer-rs-indexd.exe'
if (-not (Test-Path -LiteralPath $Daemon)) {
    Write-Warning "explorer-rs-indexd.exe is not in $Dir. The helper only serves the daemon from its own folder."
}

# --- Permission check: non-administrators must not be able to modify the binary or its folder.
$trusted = @(
    'NT AUTHORITY\SYSTEM',
    'BUILTIN\Administrators',
    'NT SERVICE\TrustedInstaller',
    'CREATOR OWNER',
    'APPLICATION PACKAGE AUTHORITY\ALL APPLICATION PACKAGES',
    'APPLICATION PACKAGE AUTHORITY\ALL RESTRICTED APPLICATION PACKAGES'
)
# Only bits that allow changing the file/folder (Modify and FullControl also
# contain read bits, so they cannot be used as a mask). 0x50000000 covers
# GENERIC_WRITE / GENERIC_ALL in unmapped ACEs.
$writeRights = [int64]([System.Security.AccessControl.FileSystemRights]'WriteData, AppendData, WriteExtendedAttributes, WriteAttributes, DeleteSubdirectoriesAndFiles, Delete, ChangePermissions, TakeOwnership') -bor 0x50000000
$problems = @()
# The daemon binary matters as much as the helper: the helper trusts its
# client by image path, so a writable explorer-rs-indexd.exe would let any
# non-elevated code obtain raw MFT data.
$checkItems = @($ExePath, $Dir)
if (Test-Path -LiteralPath $Daemon) { $checkItems += $Daemon }
foreach ($item in $checkItems) {
    $acl = Get-Acl -LiteralPath $item
    foreach ($rule in $acl.Access) {
        if ($rule.AccessControlType -ne 'Allow') { continue }
        $who = $rule.IdentityReference.Value
        if ($trusted -contains $who) { continue }
        if ((([int64]$rule.FileSystemRights) -band $writeRights) -ne 0) {
            $problems += "$item : $who has $($rule.FileSystemRights)"
        }
    }
}
if ($problems.Count -gt 0) {
    Write-Warning 'The helper binary or its folder can be modified by non-administrators:'
    $problems | ForEach-Object { Write-Warning "  $_" }
    Write-Warning 'An elevated task running such a file is a privilege-escalation risk.'
    Write-Warning 'Copy the binaries to a protected folder (e.g. C:\Program Files\explorer-rs) and use that path.'
    if (-not $Force) { throw 'Refusing to register the task. Use -Force only if you understand the risk.' }
}

Write-Host ''
Write-Host 'About to register an ELEVATED logon task:'
Write-Host "  Task:   $TaskPath$TaskName"
Write-Host "  User:   $UserId (run with highest privileges, only when logged on)"
Write-Host "  Action: $ExePath"
Write-Host 'The helper reads file names (not contents) from the NTFS master file table of fixed NTFS drives.'
if (-not $Force) {
    $answer = Read-Host 'Type YES to continue'
    if ($answer -ne 'YES') { Write-Host 'Aborted.'; exit 1 }
}

$action    = New-ScheduledTaskAction -Execute $ExePath -WorkingDirectory $Dir
$trigger   = New-ScheduledTaskTrigger -AtLogOn -User $UserId
$principal = New-ScheduledTaskPrincipal -UserId $UserId -LogonType Interactive -RunLevel Highest
$settings  = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit (New-TimeSpan -Seconds 0) `
    -MultipleInstances IgnoreNew `
    -RestartCount 3 `
    -RestartInterval (New-TimeSpan -Minutes 1)

Register-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName `
    -Action $action -Trigger $trigger -Principal $principal -Settings $settings `
    -Description 'explorer.rs: elevated NTFS MFT/USN reader for the file-name index (scripts/install-index-helper.ps1)' `
    -Force | Out-Null
Write-Host "Registered scheduled task '$TaskPath$TaskName'."

if ($StartNow) {
    Start-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName
    Start-Sleep -Seconds 2
    $p = Get-Process -Name 'explorer-rs-index-helper' -ErrorAction SilentlyContinue
    if ($p) { Write-Host "Helper running (pid $($p.Id))." } else { Write-Warning 'Helper did not start; check %LOCALAPPDATA%\explorer-rs\logs\index-helper.*.log' }
}
Write-Host 'Restart explorer-rs-indexd (or run "explorer-rs-indexd reindex") so it switches to the MFT source.'
Write-Host 'Remove with: install-index-helper.ps1 -Uninstall'
