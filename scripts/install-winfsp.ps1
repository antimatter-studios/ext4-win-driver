# Installs WinFsp from Chocolatey and fails unless it is actually installed.
#
# `choco install winfsp` exits 0 having installed nothing when the feed
# cannot serve the package ("Unable to find package 'winfsp'", "installed
# 0/0 packages"). The job then fails much later, in winfsp-sys's build
# script, with "WinFsp installation directory not found", which reads like a
# code problem (#49). This script retries a feed error, then looks for
# WinFsp exactly where winfsp-sys does -- the InstallDir value under
# HKLM\SOFTWARE\WOW6432Node\WinFsp or HKLM\SOFTWARE\WinFsp -- and fails the
# step itself when the headers are not there.

$ErrorActionPreference = 'Continue'

function Find-WinFsp {
    foreach ($key in 'HKLM:\SOFTWARE\WOW6432Node\WinFsp', 'HKLM:\SOFTWARE\WinFsp') {
        $dir = (Get-ItemProperty -Path $key -Name InstallDir -ErrorAction SilentlyContinue).InstallDir
        if ($dir -and (Test-Path (Join-Path $dir 'inc\winfsp\winfsp.h'))) {
            return $dir
        }
    }
    return $null
}

$attempts = 4
for ($attempt = 1; $attempt -le $attempts; $attempt++) {
    choco install winfsp --yes --no-progress
    $status = $LASTEXITCODE
    $dir = Find-WinFsp
    if ($status -eq 0 -and $dir) {
        Write-Host "WinFsp is installed at $dir"
        exit 0
    }
    Write-Host "attempt $attempt of ${attempts}: choco exited $status and WinFsp's headers were not found"
    if ($attempt -lt $attempts) {
        Start-Sleep -Seconds (15 * $attempt)
    }
}

Write-Host "::error::WinFsp is not installed after $attempts attempts; the Chocolatey feed may be failing to serve it"
exit 1
