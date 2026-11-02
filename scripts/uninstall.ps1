# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Removes the per-user explorer.rs install made by install.ps1 (HKCU only, no admin needed).

.DESCRIPTION
    - Refuses to continue if the HKCU Winlogon Shell value points into the install folder
      (run restore-explorer.ps1 first).
    - Removes the HKCU Run value 'explorer-rs' if present.
    - Stops explorer-rs*.exe processes running from the install folder.
    - Deletes the install folder.
    - Keeps %APPDATA%\explorer-rs (config) and %LOCALAPPDATA%\explorer-rs (logs) unless -RemoveData.

.PARAMETER Destination
    Install folder. Default: %LOCALAPPDATA%\Programs\explorer-rs

.PARAMETER RemoveData
    Also delete the config folder and the log folder.

.PARAMETER Force
    Skip the Y/N confirmation.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\uninstall.ps1
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Destination = (Join-Path $env:LOCALAPPDATA 'Programs\explorer-rs'),
    [switch]$RemoveData,
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$WinlogonKey = 'HKCU:\Software\Microsoft\Windows NT\CurrentVersion\Winlogon'
$RunKey      = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
foreach ($k in @($WinlogonKey, $RunKey)) {
    if (-not $k.StartsWith('HKCU:\')) { throw "Refusing to touch non-HKCU key: $k" }
}

function Get-RegValueInfo([string]$Path, [string]$Name) {
    $key = Get-Item -LiteralPath $Path -ErrorAction SilentlyContinue
    if ($null -eq $key) { return [pscustomobject]@{ Exists = $false; Value = $null } }
    if (@($key.GetValueNames()) -contains $Name) {
        return [pscustomobject]@{ Exists = $true; Value = $key.GetValue($Name, $null, 'DoNotExpandEnvironmentNames') }
    }
    [pscustomobject]@{ Exists = $false; Value = $null }
}

$Destination = $Destination.TrimEnd('\')
if (-not (Test-Path -LiteralPath $Destination -PathType Container)) {
    Write-Host "Nothing installed at $Destination."
    return
}
$Destination = (Resolve-Path -LiteralPath $Destination).ProviderPath.TrimEnd('\')

# Safety: refuse deleting something that is clearly not an explorer.rs install.
if (-not (Test-Path -LiteralPath (Join-Path $Destination 'explorer-rs.exe') -PathType Leaf)) {
    throw "$Destination does not contain explorer-rs.exe; refusing to delete it."
}

# Refuse if the shell points into the destination.
$shell = Get-RegValueInfo $WinlogonKey 'Shell'
if ($shell.Exists -and ([string]$shell.Value).IndexOf($Destination, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
    Write-Host "The HKCU Winlogon Shell value still points into $Destination :" -ForegroundColor Red
    Write-Host "  $($shell.Value)"
    Write-Host 'Run restore-explorer.ps1 first (see docs\RECOVERY.md), then run this script again.'
    return
}

$configDir = Join-Path $env:APPDATA 'explorer-rs'
$logDir    = Join-Path $env:LOCALAPPDATA 'explorer-rs'

Write-Host 'explorer.rs uninstall'
Write-Host "  Delete folder : $Destination"
Write-Host "  Run value     : $(if ((Get-RegValueInfo $RunKey 'explorer-rs').Exists) { 'remove explorer-rs' } else { '(none)' })"
Write-Host "  Config + logs : $(if ($RemoveData) { "delete $configDir and $logDir" } else { 'keep' })"

if (-not $Force) {
    $answer = Read-Host 'Continue? [Y/N]'
    if ($answer -notmatch '^(?i)y(es)?$') {
        Write-Host 'Aborted. Nothing was changed.'
        return
    }
}

if ((Get-RegValueInfo $RunKey 'explorer-rs').Exists) {
    Remove-ItemProperty -LiteralPath $RunKey -Name 'explorer-rs'
    Write-Host 'Removed HKCU Run value explorer-rs.'
}

$running = @(Get-Process -Name 'explorer-rs*' -ErrorAction SilentlyContinue | Where-Object {
    $p = $null
    try { $p = $_.Path } catch { $p = $null }
    $p -and $p.StartsWith($Destination + '\', [System.StringComparison]::OrdinalIgnoreCase)
})
foreach ($proc in $running) {
    try { Stop-Process -Id $proc.Id -Force } catch { Write-Warning "Could not stop $($proc.Name): $($_.Exception.Message)" }
}
if ($running.Count -gt 0) { Start-Sleep -Milliseconds 500 }

# If this script runs from inside the install folder, step out so the folder can be deleted.
Set-Location -LiteralPath $env:USERPROFILE
Remove-Item -LiteralPath $Destination -Recurse -Force
Write-Host "Deleted $Destination."

if ($RemoveData) {
    foreach ($d in @($configDir, $logDir)) {
        if (Test-Path -LiteralPath $d) {
            Remove-Item -LiteralPath $d -Recurse -Force
            Write-Host "Deleted $d."
        }
    }
} else {
    Write-Host "Kept config ($configDir) and logs ($logDir). Use -RemoveData to delete them."
}

Write-Host ''
Write-Host 'Done.' -ForegroundColor Green
