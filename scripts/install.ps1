# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Installs explorer.rs per user, WITHOUT replacing the Windows shell (no admin, HKCU only).

.DESCRIPTION
    Copies explorer-rs*.exe, *.pdb, config.example.toml, LICENSE, *.md and the scripts folder to a
    stable per-user location (default: %LOCALAPPDATA%\Programs\explorer-rs). Creates
    %APPDATA%\explorer-rs\config.toml from config.example.toml if it does not exist yet.

    Optionally adds an HKCU Run value so explorer.rs starts in dev mode (alongside explorer.exe)
    at sign-in, and/or starts it now. It never changes the Windows shell; use install-shell.ps1
    for that.

.PARAMETER Destination
    Install folder. Default: %LOCALAPPDATA%\Programs\explorer-rs

.PARAMETER Autostart
    Add HKCU\Software\Microsoft\Windows\CurrentVersion\Run value 'explorer-rs' = "<dest>\explorer-rs.exe" --dev

.PARAMETER StartNow
    Start "explorer-rs.exe --dev" after copying.

.PARAMETER Force
    Skip the Y/N confirmation.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\scripts\install.ps1 -Autostart -StartNow
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Destination = (Join-Path $env:LOCALAPPDATA 'Programs\explorer-rs'),
    [switch]$Autostart,
    [switch]$StartNow,
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
if (-not $RunKey.StartsWith('HKCU:\')) { throw "Refusing to touch non-HKCU key: $RunKey" }

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

# Locate the source folder: the script's parent folder (zip layout) or the script's own folder.
$source = $null
foreach ($c in @((Split-Path -Parent $scriptDir), $scriptDir)) {
    if ($c -and (Test-Path -LiteralPath (Join-Path $c 'explorer-rs.exe') -PathType Leaf)) { $source = $c; break }
}
if (-not $source) {
    throw 'explorer-rs.exe not found next to the scripts folder or next to this script.'
}
$source = (Resolve-Path -LiteralPath $source).ProviderPath

New-Item -ItemType Directory -Path $Destination -Force | Out-Null
$Destination = (Resolve-Path -LiteralPath $Destination).ProviderPath.TrimEnd('\')

if ($source.TrimEnd('\') -ieq $Destination) {
    throw "Source and destination are the same folder ($Destination). Run this from the unzipped folder."
}

$configDir    = Join-Path $env:APPDATA 'explorer-rs'
$configFile   = Join-Path $configDir 'config.toml'
$configSample = Join-Path $source 'config.example.toml'
$destExe      = Join-Path $Destination 'explorer-rs.exe'

Write-Host 'explorer.rs per-user install (the Windows shell is NOT changed)'
Write-Host "  From        : $source"
Write-Host "  To          : $Destination"
Write-Host "  Config      : $configFile (created only if missing)"
Write-Host "  Autostart   : $(if ($Autostart) { 'yes, dev mode at sign-in (HKCU Run)' } else { 'no' })"
Write-Host "  Start now   : $(if ($StartNow) { 'yes (--dev)' } else { 'no' })"

if (-not $Force) {
    $answer = Read-Host 'Continue? [Y/N]'
    if ($answer -notmatch '^(?i)y(es)?$') {
        Write-Host 'Aborted. Nothing was changed.'
        return
    }
}

# Stop running copies, but only those running from the destination folder.
$running = @(Get-Process -Name 'explorer-rs*' -ErrorAction SilentlyContinue | Where-Object {
    $p = $null
    try { $p = $_.Path } catch { $p = $null }
    $p -and $p.StartsWith($Destination + '\', [System.StringComparison]::OrdinalIgnoreCase)
})
foreach ($proc in $running) {
    try { Stop-Process -Id $proc.Id -Force } catch { Write-Warning "Could not stop $($proc.Name): $($_.Exception.Message)" }
}
if ($running.Count -gt 0) { Start-Sleep -Milliseconds 500 }

# Copy files.
foreach ($pattern in @('explorer-rs*.exe', '*.pdb', 'config.example.toml', 'LICENSE', '*.md')) {
    foreach ($f in @(Get-ChildItem -LiteralPath $source -Filter $pattern -File -ErrorAction SilentlyContinue)) {
        Copy-Item -LiteralPath $f.FullName -Destination $Destination -Force
    }
}
$srcScripts = Join-Path $source 'scripts'
if (-not (Test-Path -LiteralPath $srcScripts -PathType Container)) { $srcScripts = $scriptDir }
$dstScripts = Join-Path $Destination 'scripts'
New-Item -ItemType Directory -Path $dstScripts -Force | Out-Null
foreach ($f in @(Get-ChildItem -LiteralPath $srcScripts -File -ErrorAction SilentlyContinue)) {
    Copy-Item -LiteralPath $f.FullName -Destination $dstScripts -Force
}
# Files copied from a downloaded zip may carry the "from the internet" mark; clear it.
foreach ($f in @(Get-ChildItem -LiteralPath $Destination -Recurse -File -ErrorAction SilentlyContinue)) {
    try { Unblock-File -LiteralPath $f.FullName -ErrorAction SilentlyContinue } catch { }
}
Write-Host "Copied files to $Destination."

# Default config.
if (-not (Test-Path -LiteralPath $configFile)) {
    if (Test-Path -LiteralPath $configSample -PathType Leaf) {
        New-Item -ItemType Directory -Path $configDir -Force | Out-Null
        Copy-Item -LiteralPath $configSample -Destination $configFile
        Write-Host "Created $configFile from config.example.toml."
    } else {
        Write-Warning 'config.example.toml not found; no config was created (built-in defaults apply).'
    }
} else {
    Write-Host "Keeping existing config: $configFile"
}

# Optional autostart in dev mode.
if ($Autostart) {
    if (-not (Test-Path -LiteralPath $RunKey)) { New-Item -Path $RunKey -Force | Out-Null }
    Set-ItemProperty -LiteralPath $RunKey -Name 'explorer-rs' -Value ('"{0}" --dev' -f $destExe) -Type String
    Write-Host 'Added HKCU Run value explorer-rs (dev mode at sign-in).'
}

# Optional start.
if ($StartNow) {
    Start-Process -FilePath $destExe -ArgumentList '--dev' -WorkingDirectory $Destination
    Write-Host 'Started explorer-rs.exe --dev.'
}

Write-Host ''
Write-Host 'Done.' -ForegroundColor Green
Write-Host "  Program : $destExe"
Write-Host "  Config  : $configFile"
Write-Host "  Logs    : $(Join-Path $env:LOCALAPPDATA 'explorer-rs')"
Write-Host '  Run it  : explorer-rs.exe --dev   (quit: right-click empty taskbar area -> Quit explorer.rs)'
Write-Host '  Replace the shell (VM first!): scripts\install-shell.ps1 in the install folder, see docs\INSTALL.md'
Write-Host '  Uninstall:'
Write-Host "    powershell -ExecutionPolicy Bypass -File `"$(Join-Path $dstScripts 'uninstall.ps1')`""
