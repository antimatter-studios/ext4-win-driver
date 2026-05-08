<#
.SYNOPSIS
  Build the ext4-win-driver installer (MSI + Burn bootstrapper EXE).

.DESCRIPTION
  Two-stage WiX 4 build:
    1. `wix build Product.wxs`   ->  ext4-win-driver-<ver>.msi
    2. `wix build Bundle.wxs`    ->  ext4-win-driver-<ver>-Setup.exe
       (chains WinFsp's MSI ahead of ours)

  Stage 2 needs the WinFsp redistributable MSI in `installer\redist\`.
  If it's missing the script downloads it from the WinFsp GitHub release
  pinned by $WinFspVersion / $WinFspUrl below and verifies SHA256 against
  $WinFspSha256 (sourced from WinFsp's release notes).

  By default both artefacts are produced. Pass -MsiOnly to skip the
  bundle stage (e.g. for SCCM/Intune deployments that push WinFsp
  separately).

  Prerequisites:
    - WiX 4+ -- `dotnet tool install --global wix` (or winget WiXToolset.WiX).
    - Extensions:
        wix extension add WixToolset.Util.wixext
        wix extension add WixToolset.BootstrapperApplications.wixext  # bundle UI (WiX 7 -- was Bal in WiX 4)
    - Internet access on first build (to fetch WinFsp), or pre-populated
      installer\redist\winfsp-<ver>.msi.

.PARAMETER ExePath
  Path to the release ext4.exe. Required.

.PARAMETER Version
  Override the version. Default: read from ../Cargo.toml.

.PARAMETER OutputDir
  Output directory. Default: <repo>\dist.

.PARAMETER MsiOnly
  Skip the Burn bundle stage; produce only the MSI.

.PARAMETER Arch
  Target architecture for the MSI + bundle. Must match the embedded
  ext4.exe:
    x64    -- built with target `x86_64-pc-windows-msvc`   (Machine 0x8664)
    arm64  -- built with target `aarch64-pc-windows-gnullvm` (Machine 0xAA64)
  A mismatch (e.g. arm64 ext4.exe in an x64-templated MSI) installs but
  silently fails on the wrong host CPU. The script PE-sniffs $ExePath at
  startup and warns on disagreement. Default: x64.

.EXAMPLE
  installer\build.ps1 -ExePath target\release\ext4.exe

.EXAMPLE
  installer\build.ps1 -ExePath target\release\ext4.exe -Arch arm64
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ExePath,

    [Parameter()]
    [string]$Version,

    [Parameter()]
    [string]$OutputDir,

    [Parameter()]
    [switch]$MsiOnly,

    [Parameter()]
    [ValidateSet('x64','arm64')]
    [string]$Arch = 'x64'
)

$ErrorActionPreference = 'Stop'

# ----------------------------------------------------------------------------
# WinFsp redistributable pin.
#
# Bump these together when a new WinFsp release ships:
#   1. Update $WinFspVersion (4-part version -- matches the MSI's
#      ProductVersion, used by Bundle.wxs DetectCondition).
#   2. Update $WinFspMsiName + $WinFspUrl from the GitHub release page
#      https://github.com/winfsp/winfsp/releases.
#   3. Update $WinFspSha256 -- copy from the release notes' "SHA256" block.
#
# Leaving $WinFspSha256 empty disables verification (DEV ONLY -- never ship
# a bundle built that way; CI must hard-fail on empty).
# ----------------------------------------------------------------------------
$WinFspVersion = '2.1.25156'
$WinFspMsiName = 'winfsp-2.1.25156.msi'
$WinFspUrl     = "https://github.com/winfsp/winfsp/releases/download/v2.1/$WinFspMsiName"
$WinFspSha256  = '073a70e00f77423e34bed98b86e600def93393ba5822204fac57a29324db9f7a'

# ----------------------------------------------------------------------------
# Resolve paths. Script can be invoked from anywhere -- anchor to its dir.
# ----------------------------------------------------------------------------
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot  = Split-Path -Parent $scriptDir
$cargoToml = Join-Path $repoRoot 'Cargo.toml'
$redistDir = Join-Path $scriptDir 'redist'

if (-not (Test-Path $ExePath)) {
    throw "ExePath not found: $ExePath"
}
$ExePath = (Resolve-Path $ExePath).Path

# ----------------------------------------------------------------------------
# PE-sniff $ExePath and warn if its IMAGE_FILE_HEADER.Machine disagrees with
# -Arch. Layout: bytes [0x3C..0x3F] = e_lfanew (offset to PE header). At
# e_lfanew is "PE\0\0" (4 bytes), then IMAGE_FILE_HEADER whose first field
# (2 bytes, LE) is Machine. 0x8664 = x64, 0xAA64 = arm64.
# ----------------------------------------------------------------------------
try {
    $fs = [IO.File]::OpenRead($ExePath)
    try {
        $buf = New-Object byte[] 4096
        $n = $fs.Read($buf, 0, $buf.Length)
        if ($n -ge 0x40 -and $buf[0] -eq 0x4D -and $buf[1] -eq 0x5A) {
            $eLfanew = [BitConverter]::ToInt32($buf, 0x3C)
            if ($eLfanew -gt 0 -and ($eLfanew + 6) -lt $n -and
                $buf[$eLfanew] -eq 0x50 -and $buf[$eLfanew+1] -eq 0x45) {
                $machine = [BitConverter]::ToUInt16($buf, $eLfanew + 4)
                $detected = switch ($machine) {
                    0x8664  { 'x64'   }
                    0xAA64  { 'arm64' }
                    default { ('unknown(0x{0:X4})' -f $machine) }
                }
                if ($detected -ne $Arch) {
                    Write-Warning "ExePath PE Machine = $detected but -Arch = $Arch. The MSI will install but the binary will not run on the target CPU."
                }
            }
        }
    } finally { $fs.Close() }
} catch {
    Write-Warning "PE-header sniff of $ExePath failed: $_"
}

# ----------------------------------------------------------------------------
# Pull version out of Cargo.toml unless caller supplied one.
# ----------------------------------------------------------------------------
if (-not $Version) {
    if (-not (Test-Path $cargoToml)) {
        throw "Cargo.toml not found at $cargoToml -- pass -Version explicitly."
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
        throw "Could not read version from $cargoToml -- pass -Version explicitly."
    }
}

if (-not $OutputDir) {
    $OutputDir = Join-Path $repoRoot 'dist'
}
if (-not (Test-Path $OutputDir)) {
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}
$msiOut    = Join-Path $OutputDir "ext4-win-driver-$Version-$Arch.msi"
$bundleOut = Join-Path $OutputDir "ext4-win-driver-$Version-$Arch-Setup.exe"

# ----------------------------------------------------------------------------
# Stage LICENSE next to the WiX sources (referenced by relative path from
# both Product.wxs and Bundle.wxs).
# ----------------------------------------------------------------------------
$licenseSrc = Join-Path $repoRoot 'LICENSE'
$licenseDst = Join-Path $scriptDir 'LICENSE'
if (Test-Path $licenseSrc) {
    Copy-Item -Force $licenseSrc $licenseDst
} elseif (-not (Test-Path $licenseDst)) {
    Set-Content -Path $licenseDst -Value 'GPL-3.0 -- see https://www.gnu.org/licenses/gpl-3.0.html'
}

# ----------------------------------------------------------------------------
# Locate `wix`.
# ----------------------------------------------------------------------------
$wix = Get-Command wix -ErrorAction SilentlyContinue
if (-not $wix) {
    throw @"
WiX 4+ not found. Install with:
  dotnet tool install --global wix
  wix extension add WixToolset.Util.wixext
  wix extension add WixToolset.BootstrapperApplications.wixext
or via winget: winget install WiXToolset.WiX
"@
}

Write-Host "WiX:        $($wix.Source)"
Write-Host "Version:    $Version"
Write-Host "Arch:       $Arch"
Write-Host "ExePath:    $ExePath"
Write-Host "OutputDir:  $OutputDir"
Write-Host "MsiOnly:    $MsiOnly"

# ============================================================================
# Stage 1 -- build the MSI.
# ============================================================================
Push-Location $scriptDir
try {
    Write-Host ""
    Write-Host "[1/2] Building MSI..."
    & wix build `
        -ext WixToolset.Util.wixext `
        -d "Version=$Version" `
        -d "ExePath=$ExePath" `
        -arch $Arch `
        -out $msiOut `
        Product.wxs
    if ($LASTEXITCODE -ne 0) {
        throw "wix build (MSI) failed with exit code $LASTEXITCODE"
    }
    Write-Host "Built: $msiOut"
} finally {
    Pop-Location
}

if ($MsiOnly) {
    Write-Host ""
    Write-Host "MsiOnly set -- skipping bundle. Done."
    return
}

# ============================================================================
# Stage 2 -- fetch WinFsp MSI (if missing), then build the Burn bundle.
# ============================================================================
if (-not (Test-Path $redistDir)) {
    New-Item -ItemType Directory -Path $redistDir -Force | Out-Null
}
$winFspMsi = Join-Path $redistDir $WinFspMsiName

if (-not (Test-Path $winFspMsi)) {
    Write-Host ""
    Write-Host "[2/2] WinFsp MSI not cached; downloading..."
    Write-Host "      $WinFspUrl"
    # TLS 1.2 -- older PS defaults break on github.com.
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $WinFspUrl -OutFile $winFspMsi -UseBasicParsing
}

# SHA256 verification -- refuse to ship an unverified bundle.
if ($WinFspSha256) {
    $actual = (Get-FileHash -Algorithm SHA256 $winFspMsi).Hash.ToLowerInvariant()
    $expected = $WinFspSha256.ToLowerInvariant()
    if ($actual -ne $expected) {
        Remove-Item -Force $winFspMsi
        throw "WinFsp MSI SHA256 mismatch.`n  expected: $expected`n  actual:   $actual`nRemoved cached file; re-run after verifying $WinFspUrl."
    }
    Write-Host "WinFsp SHA256 verified."
} else {
    Write-Warning "WinFspSha256 is empty -- bundle will be built without integrity check. DO NOT RELEASE."
}

Push-Location $scriptDir
try {
    Write-Host ""
    Write-Host "[2/2] Building bundle..."
    # Bundle UI extension was renamed in WiX 7: the old WixToolset.Bal.wixext
    # NuGet now ships as WixToolset.BootstrapperApplications.wixext. The bal:
    # xmlns/element names in Bundle.wxs are unchanged -- only the package +
    # -ext arg moved.
    & wix build `
        -ext WixToolset.Util.wixext `
        -ext WixToolset.BootstrapperApplications.wixext `
        -d "Version=$Version" `
        -d "Ext4Msi=$msiOut" `
        -d "WinFspMsi=$winFspMsi" `
        -d "WinFspVersion=$WinFspVersion" `
        -arch $Arch `
        -out $bundleOut `
        Bundle.wxs
    if ($LASTEXITCODE -ne 0) {
        throw "wix build (bundle) failed with exit code $LASTEXITCODE"
    }
    Write-Host "Built: $bundleOut"
} finally {
    Pop-Location
}

Write-Host ""
Write-Host "Artefacts:"
Write-Host "  MSI:    $msiOut"
Write-Host "  Setup:  $bundleOut   (ship this to end users)"
