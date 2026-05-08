<#
.SYNOPSIS
  Mount an ext4 image as a Windows drive letter via WinFsp.

.DESCRIPTION
  Thin PowerShell wrapper around `ext4.exe mount`. Accepts an optional
  image path; if omitted, opens an Open-File dialog so the user can pick
  one. Picks the first free drive letter (Z:..D:) and runs the mount in
  the foreground -- Ctrl-C unmounts.

  Installed alongside ext4.exe in `%ProgramFiles%\ext4-win-driver\`.

.PARAMETER ImagePath
  Path to an ext4 image (.img) or whole-disk image. Optional.

.PARAMETER DriveLetter
  Force a specific drive letter (e.g. 'X:'). Optional. Default: first free.

.PARAMETER Part
  1-indexed partition number for whole-disk images. Optional.

.EXAMPLE
  Mount-Ext4.ps1 C:\images\rootfs.img
  Mount-Ext4.ps1 -ImagePath disk.img -DriveLetter Y: -Part 1
  Mount-Ext4.ps1                                # opens file picker
#>

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$ImagePath,

    [Parameter()]
    [string]$DriveLetter,

    [Parameter()]
    [int]$Part
)

$ErrorActionPreference = 'Stop'

# ----------------------------------------------------------------------------
# Locate ext4.exe. Installed copy lives next to this script in Program Files;
# fall back to PATH for dev runs.
# ----------------------------------------------------------------------------
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$exeNextToScript = Join-Path $scriptDir 'ext4.exe'

if (Test-Path $exeNextToScript) {
    $ext4Exe = $exeNextToScript
} else {
    $cmd = Get-Command ext4.exe -ErrorAction SilentlyContinue
    if (-not $cmd) {
        Write-Error "ext4.exe not found next to this script ($scriptDir) or on PATH."
        exit 1
    }
    $ext4Exe = $cmd.Source
}

# ----------------------------------------------------------------------------
# If no image path was supplied, pop the Open-File dialog.
# ----------------------------------------------------------------------------
if (-not $ImagePath) {
    Add-Type -AssemblyName System.Windows.Forms
    $dlg = New-Object System.Windows.Forms.OpenFileDialog
    $dlg.Title = 'Select an ext4 image to mount'
    $dlg.Filter = 'Disk images (*.img;*.iso;*.bin;*.raw)|*.img;*.iso;*.bin;*.raw|All files (*.*)|*.*'
    $dlg.CheckFileExists = $true
    if ($dlg.ShowDialog() -ne [System.Windows.Forms.DialogResult]::OK) {
        Write-Host 'Cancelled.'
        exit 0
    }
    $ImagePath = $dlg.FileName
}

if (-not (Test-Path $ImagePath)) {
    Write-Error "Image not found: $ImagePath"
    exit 1
}

# ----------------------------------------------------------------------------
# Pick a drive letter if the user didn't supply one. Walk Z..D and grab the
# first letter that isn't currently a PSDrive.
# ----------------------------------------------------------------------------
function Get-FreeDriveLetter {
    $used = @(Get-PSDrive -PSProvider FileSystem | ForEach-Object { $_.Name.ToUpper() })
    foreach ($c in [char[]]'ZYXWVUTSRQPONMLKJIHGFED') {
        if ($used -notcontains "$c") { return "${c}:" }
    }
    throw 'No free drive letter available (D..Z all in use).'
}

if (-not $DriveLetter) {
    $DriveLetter = Get-FreeDriveLetter
}

# Normalise: 'X' -> 'X:'.
if ($DriveLetter -notmatch ':$') { $DriveLetter += ':' }

# ----------------------------------------------------------------------------
# Run the mount in the foreground. Ctrl-C in the console unmounts.
# ----------------------------------------------------------------------------
$args = @('mount', $ImagePath, '--drive', $DriveLetter)
if ($PSBoundParameters.ContainsKey('Part')) {
    $args += @('--part', "$Part")
}

Write-Host "Mounting $ImagePath at $DriveLetter ..."
Write-Host "Ctrl-C in this window to unmount."
Write-Host ""

& $ext4Exe @args
exit $LASTEXITCODE
