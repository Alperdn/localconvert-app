<#
.SYNOPSIS
    BUILD-TIME ONLY. Stages the minimal LGPL audio FFmpeg/ffprobe (built by
    scripts/build-ffmpeg-engine.ps1) into src-tauri/engines/ffmpeg/ and writes
    its manifest + license notices.

.DESCRIPTION
    Fails closed:
      * the build record's hashes must match the binaries being staged;
      * the recorded configure line must contain no GPL/nonfree/version3 flag;
      * `ffmpeg -version` / `ffprobe -version` must run and report the pinned version.
    Output layout (engines/ffmpeg/):
      bin/ffmpeg.exe, bin/ffprobe.exe, manifest.json
    Notices go to THIRD_PARTY_NOTICES/FFmpeg/ (license texts, build record,
    -version/-buildconf output, and the LGPL source offer).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/prepare-ffmpeg-engine.ps1
#>
[CmdletBinding()]
param([string]$BuildDir = "C:\ffmpeg-engine-build")
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$outDir = Join-Path $BuildDir "out"
$buildSh = Get-Content (Join-Path $PSScriptRoot "ffmpeg\build-ffmpeg.sh") -Raw

function Get-Pinned($name) {
    if ($buildSh -match "(?m)^$name=`"([^`"]+)`"") { return $Matches[1] }
    throw "pinned value $name not found in build-ffmpeg.sh"
}
$version = Get-Pinned "FFMPEG_VERSION"
$srcSha  = Get-Pinned "FFMPEG_SHA256"
$tarball = "ffmpeg-$version.tar.xz"
$srcUrl  = "https://ffmpeg.org/releases/$tarball"

$record = @{}
Get-Content (Join-Path $outDir "build-record.txt") | ForEach-Object {
    $i = $_.IndexOf('=')
    if ($i -gt 0) { $record[$_.Substring(0, $i)] = $_.Substring($i + 1) }
}

$ffmpeg  = Join-Path $outDir "bin\ffmpeg.exe"
$ffprobe = Join-Path $outDir "bin\ffprobe.exe"
$hFf = (Get-FileHash $ffmpeg -Algorithm SHA256).Hash.ToLower()
$hFp = (Get-FileHash $ffprobe -Algorithm SHA256).Hash.ToLower()
if ($hFf -ne $record.ffmpeg_sha256 -or $hFp -ne $record.ffprobe_sha256) { throw "Binaries do not match build-record.txt. STOP." }
foreach ($bad in @("--enable-gpl", "--enable-nonfree", "--enable-version3")) {
    if ($record.configure_flags.Contains($bad)) { throw "Build configuration contains $bad - not an LGPL-only build. STOP." }
}
$verOut = & $ffmpeg -hide_banner -version | Select-Object -First 1
if ($verOut -notmatch ("ffmpeg version " + [regex]::Escape($version))) { throw "ffmpeg reports '$verOut', expected $version" }
$probeOut = & $ffprobe -hide_banner -version | Select-Object -First 1
if ($probeOut -notmatch ("ffprobe version " + [regex]::Escape($version))) { throw "ffprobe version mismatch: $probeOut" }

$dest = Join-Path $repo "src-tauri\engines\ffmpeg"
if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
New-Item -ItemType Directory -Force "$dest\bin" | Out-Null
Copy-Item $ffmpeg, $ffprobe "$dest\bin\"

$manifest = [ordered]@{
    schema_version        = 1
    engine_id             = "ffmpeg"
    ffmpeg_version        = $version
    architecture          = "x86_64"
    source_url            = $srcUrl
    source_archive_sha256 = $srcSha
    configure_flags       = $record.configure_flags
    ffmpeg_sha256         = $hFf
    ffprobe_sha256        = $hFp
    license_mode          = "LGPL-2.1-or-later"
    required_files        = @(
        [ordered]@{ path = "bin/ffmpeg.exe";  sha256 = $hFf; size = (Get-Item $ffmpeg).Length },
        [ordered]@{ path = "bin/ffprobe.exe"; sha256 = $hFp; size = (Get-Item $ffprobe).Length }
    )
    build_date            = $record.build_date
    compiler              = $record.compiler
    build_environment     = "MSYS2 UCRT64 ($($record.msys2_runtime)), $($record.nasm), $($record.make)"
}
[System.IO.File]::WriteAllText("$dest\manifest.json", ($manifest | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding($false)))

$notices = Join-Path $repo "THIRD_PARTY_NOTICES\FFmpeg"
New-Item -ItemType Directory -Force $notices | Out-Null
foreach ($f in "COPYING.LGPLv2.1", "COPYING.LGPLv3", "LICENSE.md", "ffmpeg-version.txt", "ffmpeg-buildconf.txt", "ffprobe-version.txt", "build-record.txt") {
    Copy-Item (Join-Path $outDir $f) $notices -Force
}
$offer = @(
    "FFmpeg $version - LGPL source offer (MEB-Donusturucu, Ses Dikte audio preprocessing)",
    "",
    "The bundled ffmpeg.exe / ffprobe.exe are built from the unmodified official",
    "FFmpeg $version source release with an LGPL-2.1-or-later configuration (no",
    "--enable-gpl, --enable-nonfree or --enable-version3, no external libraries):",
    "",
    "  Source:        $srcUrl",
    "  Signature:     $srcUrl.asc",
    "                 (FFmpeg release signing key FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8)",
    "  SHA-256:       $srcSha",
    "  Configuration: see ffmpeg-buildconf.txt / build-record.txt in this folder",
    "  Build script:  scripts/ffmpeg/build-ffmpeg.sh (in this project's repository)",
    "",
    "The corresponding source, the exact configure line and the build script are",
    "available on request from the distributor of this application. The libraries",
    "are statically linked; to rebuild a modified version, run",
    "scripts/build-ffmpeg-engine.ps1 against a modified source tree."
)
$offer | Set-Content (Join-Path $notices "SOURCE_OFFER.txt") -Encoding utf8
Write-Host "FFmpeg staged: $dest"
