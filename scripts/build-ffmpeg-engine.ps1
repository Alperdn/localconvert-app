<#
.SYNOPSIS
    BUILD-TIME ONLY. Builds the minimal LGPL FFmpeg/ffprobe from the pinned
    official source release using MSYS2 UCRT64 (see scripts/ffmpeg/build-ffmpeg.sh
    for the pinned version, SHA-256, signing-key fingerprint and configure flags).

.DESCRIPTION
    MSYS2/MinGW are build-time dependencies of the developer machine ONLY.
    They are never required on end-user machines. Output goes to -WorkDir
    (outside the repo); scripts/prepare-ffmpeg-engine.ps1 stages it.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/build-ffmpeg-engine.ps1
#>
[CmdletBinding()]
param(
    [string]$WorkDir = "C:\ffmpeg-engine-build",
    [string]$Msys2Root = "C:\msys64"
)
$ErrorActionPreference = "Stop"
$bash = Join-Path $Msys2Root "usr\bin\bash.exe"
if (-not (Test-Path $bash)) { throw "MSYS2 not found at $Msys2Root (build-time dependency)." }
$script = (Resolve-Path (Join-Path $PSScriptRoot "ffmpeg\build-ffmpeg.sh")).Path
$env:MSYSTEM = "UCRT64"
$env:CHERE_INVOKING = "1"
if ($script -match '\s' -or $WorkDir -match '\s') { throw "Paths must not contain spaces." }
$fwdScript = $script -replace '\\', '/'
$fwdWork = $WorkDir -replace '\\', '/'
$cmd = 'bash $(cygpath -u ' + $fwdScript + ') $(cygpath -u ' + $fwdWork + ')'
& $bash -lc $cmd
if ($LASTEXITCODE -ne 0) { throw "FFmpeg build failed (exit $LASTEXITCODE). Nothing was staged." }
Write-Host "Build output: $WorkDir\out"
