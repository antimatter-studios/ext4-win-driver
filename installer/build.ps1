<#
.SYNOPSIS
  Build the ext4-win-driver MSI installer.

.DESCRIPTION
  Wraps `wix build` (WiX 4+). Reads the version from Cargo.toml unless
  -Version is passed. Copies the supplied ext4.exe and the project LICENSE
  into the installer/ working area so the WiX source's relative paths
  resolve, then emits an MSI under -Output (default
  dist/ext4-win-driver-<version>.msi).

  Prerequisites:
    - WiX 4+ — `dotnet tool install --global wix` (or winget WiXToolset.WiX).
    - WiX UI / Util extensions — `wix extension add WixToolset.UI.wixext`
      and `wix extension add WixToolset.Util.wixext`.

.PARAMETER ExePath
  Path to the release ext4.exe. Required.

.PARAMETER Version
  Override the version. Default: read from ../Cargo.toml.

.PARAMETER Output
  MSI output path. Default: dist/ext4-win-driver-<version>.msi.

.EXAMPLE
  installer\build.ps1 -ExePath target\release\ext4.exe
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ExePath,

    [Parameter()]
    [string]$Version,

    [Parameter()]
    [string]$Output
)

$ErrorActionPreference = 'Stop'

# ----------------------------------------------------------------------------
# Resolve paths. Script can be invoked from anywhere — anchor to its dir.
# ----------------------------------------------------------------------------
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot  = Split-Path -Parent $scriptDir
$cargoToml = Join-Path $repoRoot 'Cargo.toml'

if (-not (Test-Path $ExePath)) {
    throw "ExePath not found: $ExePath"
}
$ExePath = (Resolve-Path $ExePath).Path

# ----------------------------------------------------------------------------
# Pull version out of Cargo.toml (`version = "x.y.z"` in [package]) unless
# the caller supplied one explicitly.
# ----------------------------------------------------------------------------
if (-not $Version) {
    if (-not (Test-Path $cargoToml)) {
        throw "Cargo.toml not found at $cargoToml — pass -Version explicitly."
    }
    $inPackage = $false
    foreach ($line in Get-Content $cargoToml) {
        if ($line -match '^\s*\[package\]\s*$') { $inPackage = $true; continue }
        if ($line -match '^\s*\[') { $inPackage = $false; continue }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            $Version = $Matches[1]
            break
        }
    }
    if (-not $Version) {
        throw "Could not read version from $cargoToml — pass -Version explicitly."
    }
}

# ----------------------------------------------------------------------------
# Default output path.
# ----------------------------------------------------------------------------
if (-not $Output) {
    $Output = Join-Path $repoRoot "dist\ext4-win-driver-$Version.msi"
}
$outDir = Split-Path -Parent $Output
if (-not (Test-Path $outDir)) {
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null
}

# ----------------------------------------------------------------------------
# Stage LICENSE next to Product.wxs (the WiX source references it by relative
# path). If the repo doesn't have a LICENSE yet, write a placeholder so the
# build doesn't fail outright — the MSI will ship whatever's there.
# ----------------------------------------------------------------------------
$licenseSrc = Join-Path $repoRoot 'LICENSE'
$licenseDst = Join-Path $scriptDir 'LICENSE'
if (Test-Path $licenseSrc) {
    Copy-Item -Force $licenseSrc $licenseDst
} elseif (-not (Test-Path $licenseDst)) {
    Set-Content -Path $licenseDst -Value 'GPL-3.0 — see https://www.gnu.org/licenses/gpl-3.0.html'
}

# ----------------------------------------------------------------------------
# Locate `wix`. Prefer the dotnet global tool; fall back to PATH.
# ----------------------------------------------------------------------------
$wix = Get-Command wix -ErrorAction SilentlyContinue
if (-not $wix) {
    throw @"
WiX 4+ not found. Install with:
  dotnet tool install --global wix
  wix extension add WixToolset.UI.wixext
  wix extension add WixToolset.Util.wixext
or via winget: winget install WiXToolset.WiX
"@
}

Write-Host "WiX:      $($wix.Source)"
Write-Host "Version:  $Version"
Write-Host "ExePath:  $ExePath"
Write-Host "Output:   $Output"

# ----------------------------------------------------------------------------
# Invoke `wix build`. -d sets WiX preprocessor variables consumed by
# Product.wxs as $(var.Name).
# ----------------------------------------------------------------------------
Push-Location $scriptDir
try {
    & wix build `
        -ext WixToolset.Util.wixext `
        -d "Version=$Version" `
        -d "ExePath=$ExePath" `
        -arch x64 `
        -out $Output `
        Product.wxs
    if ($LASTEXITCODE -ne 0) {
        throw "wix build failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

Write-Host ""
Write-Host "Built: $Output"
