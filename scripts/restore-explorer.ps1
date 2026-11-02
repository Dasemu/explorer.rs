# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Restores the per-user Windows shell after install-shell.ps1 (HKCU only, no admin needed).

.DESCRIPTION
    - If the backup key HKCU:\Software\explorer-rs exists and recorded a previous Shell value,
      that value is restored.
    - Otherwise (no backup, or no Shell value existed before) the HKCU Winlogon "Shell" value is
      deleted. The machine-wide HKLM default (explorer.exe) then applies. This is the safest default.
    - The backup key is removed afterwards.
    - With -StartExplorer, explorer-rs*.exe processes are stopped and explorer.exe is started now.

.PARAMETER StartExplorer
    Stop explorer-rs*.exe processes and start explorer.exe immediately.

.PARAMETER Force
    Skip the Y/N confirmation.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\restore-explorer.ps1 -StartExplorer
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [switch]$StartExplorer,
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$WinlogonKey = 'HKCU:\Software\Microsoft\Windows NT\CurrentVersion\Winlogon'
$BackupKey   = 'HKCU:\Software\explorer-rs'

foreach ($k in @($WinlogonKey, $BackupKey)) {
    if (-not $k.StartsWith('HKCU:\')) { throw "Refusing to touch non-HKCU key: $k" }
}

# --- Registry helpers -------------------------------------------------------
# Never use Get-ItemProperty / PSObject.Properties.Name to test for values: under
# Set-StrictMode that throws PropertyNotFoundStrict on keys that have no values.
function Get-RegValueInfo([string]$Path, [string]$Name) {
    $key = Get-Item -LiteralPath $Path -ErrorAction SilentlyContinue
    if ($null -eq $key) { return [pscustomobject]@{ Exists = $false; Value = $null } }
    if (@($key.GetValueNames()) -contains $Name) {
        return [pscustomobject]@{ Exists = $true; Value = $key.GetValue($Name, $null, 'DoNotExpandEnvironmentNames') }
    }
    [pscustomobject]@{ Exists = $false; Value = $null }
}

# Read the backup, if present.
$restoreValue = $null   # $null means: delete the value
$haveBackup = Test-Path -LiteralPath $BackupKey
# An existing key with no PreviousShellExisted value (e.g. left by a failed run) is
# harmless: it is treated as "no backup" and the Shell value is simply deleted.
$existedInfo = Get-RegValueInfo $BackupKey 'PreviousShellExisted'
if ($existedInfo.Exists -and ([int]$existedInfo.Value -eq 1)) {
    $prev = Get-RegValueInfo $BackupKey 'PreviousShell'
    if ($prev.Exists -and -not [string]::IsNullOrWhiteSpace([string]$prev.Value)) {
        $restoreValue = [string]$prev.Value
    }
}

Write-Host 'explorer.rs shell restore'
Write-Host "  Backup found : $($existedInfo.Exists)"
if ($null -ne $restoreValue) {
    Write-Host "  Action       : set HKCU Winlogon Shell back to: $restoreValue"
} else {
    Write-Host '  Action       : delete the HKCU Winlogon Shell value (system default explorer.exe applies)'
}
if ($StartExplorer) {
    Write-Host '  Also         : stop explorer-rs*.exe and start explorer.exe now'
}

if (-not $Force) {
    $answer = Read-Host 'Continue? [Y/N]'
    if ($answer -notmatch '^(?i)y(es)?$') {
        Write-Host 'Aborted. Nothing was changed.'
        return
    }
}

if ($null -ne $restoreValue) {
    if (-not (Test-Path -LiteralPath $WinlogonKey)) { New-Item -Path $WinlogonKey -Force | Out-Null }
    Set-ItemProperty -LiteralPath $WinlogonKey -Name 'Shell' -Value $restoreValue -Type String
    Write-Host 'Restored the previous Shell value.'
} else {
    if (Test-Path -LiteralPath $WinlogonKey) {
        if ((Get-RegValueInfo $WinlogonKey 'Shell').Exists) {
            Remove-ItemProperty -LiteralPath $WinlogonKey -Name 'Shell'
            Write-Host 'Deleted the HKCU Shell value.'
        } else {
            Write-Host 'No HKCU Shell value present; nothing to delete.'
        }
    } else {
        Write-Host 'No HKCU Winlogon key present; nothing to delete.'
    }
}

if ($haveBackup) {
    Remove-Item -LiteralPath $BackupKey -Recurse -Force
    Write-Host 'Removed the backup key HKCU:\Software\explorer-rs.'
}

if ($StartExplorer) {
    Get-Process -Name 'explorer-rs*' -ErrorAction SilentlyContinue | ForEach-Object {
        try { Stop-Process -Id $_.Id -Force } catch { Write-Warning "Could not stop $($_.Name): $($_.Exception.Message)" }
    }
    Start-Sleep -Milliseconds 500
    Start-Process -FilePath (Join-Path $env:WINDIR 'explorer.exe')
    Write-Host 'Started explorer.exe.'
}

Write-Host ''
Write-Host 'Done. Sign out and back in for the change to apply to the next session.' -ForegroundColor Green
