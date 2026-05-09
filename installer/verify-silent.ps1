#requires -Version 5.1
<#
.SYNOPSIS
  Assert that Setup.exe /quiet completes without a GUI prompt.

.DESCRIPTION
  winget reviewers run the bundle with /quiet during the unattended-install
  validation pass. If Burn's hyperlinkLicense theme blocks waiting for a
  click, validation fails with License-Blocks-Install /
  Validation-Unattended-Failed. This script catches that locally + in CI:

    1. Launches Setup.exe with /quiet /norestart under a hard timeout.
    2. Treats "still running past TimeoutSeconds" as a UI-blocked failure.
    3. Accepts exit 0 (success) or 3010 (reboot required) as PASS.
    4. Asserts ext4.exe is present at the expected install path.

  Side-effect: leaves ext4-win-driver + WinFsp installed on the host. Run on
  ephemeral CI runners or a Windows VM you don't mind dirtying.
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$SetupPath,

  [int]$TimeoutSeconds = 240,

  [string]$InstallDir = (Join-Path $env:ProgramFiles 'ext4-win-driver')
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $SetupPath)) {
  throw "Setup.exe not found at: $SetupPath"
}

$logPath = Join-Path ([System.IO.Path]::GetDirectoryName((Resolve-Path $SetupPath))) 'verify-silent.log'
if (Test-Path $logPath) { Remove-Item $logPath -Force }

Write-Host "verify-silent: launching '$SetupPath /quiet /norestart' (timeout ${TimeoutSeconds}s, log: $logPath)"
$proc = Start-Process -FilePath $SetupPath `
  -ArgumentList '/quiet', '/norestart', '/log', $logPath `
  -PassThru

if (-not $proc.WaitForExit($TimeoutSeconds * 1000)) {
  try { $proc.Kill() } catch {}
  if (Test-Path $logPath) {
    Write-Host "--- last 60 lines of $logPath ---"
    Get-Content $logPath -Tail 60
  }
  throw "Setup.exe /quiet did not exit within ${TimeoutSeconds}s -- likely showing a GUI prompt. winget would flag this as License-Blocks-Install / Validation-Unattended-Failed."
}

$exit = $proc.ExitCode
Write-Host "verify-silent: Setup.exe exited with code $exit"

# 0 = success; 3010 = ERROR_SUCCESS_REBOOT_REQUIRED -- both pass for /norestart.
if ($exit -ne 0 -and $exit -ne 3010) {
  if (Test-Path $logPath) {
    Write-Host "--- last 80 lines of $logPath ---"
    Get-Content $logPath -Tail 80
  }
  throw "Setup.exe /quiet returned exit code $exit (expected 0 or 3010)."
}

$ext4 = Join-Path $InstallDir 'ext4.exe'
if (-not (Test-Path $ext4)) {
  throw "Setup.exe reported success but '$ext4' is missing -- install incomplete."
}

Write-Host "verify-silent: PASS -- '$ext4' present, exit $exit, no GUI hang."
