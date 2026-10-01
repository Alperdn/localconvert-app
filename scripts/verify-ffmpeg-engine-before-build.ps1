<#
.SYNOPSIS
    Release gate: verifies src-tauri/engines/ffmpeg/ against its own manifest.json
    (independently of the Rust code) and runs both tools. Exits non-zero on any problem.
#>
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dir = Join-Path $root "src-tauri\engines\ffmpeg"
function Fail($m) { Write-Error "FFmpeg engine verification FAILED: $m"; exit 1 }
if (-not (Test-Path "$dir\manifest.json")) { Fail "manifest.json missing - run scripts/prepare-ffmpeg-engine.ps1" }
$m = Get-Content "$dir\manifest.json" -Raw | ConvertFrom-Json
if ($m.engine_id -ne "ffmpeg") { Fail "wrong engine_id" }
if ($m.license_mode -ne "LGPL-2.1-or-later") { Fail "license_mode is not LGPL-2.1-or-later" }
foreach ($bad in "--enable-gpl", "--enable-nonfree", "--enable-version3") {
    if ($m.configure_flags.Contains($bad)) { Fail "configure_flags contain $bad" }
}
foreach ($f in $m.required_files) {
    $p = Join-Path $dir $f.path
    if (-not (Test-Path $p)) { Fail "missing $($f.path)" }
    if ((Get-FileHash $p -Algorithm SHA256).Hash.ToLower() -ne $f.sha256) { Fail "hash mismatch: $($f.path)" }
}
foreach ($t in "ffmpeg", "ffprobe") {
    $v = (& "$dir\bin\$t.exe" -hide_banner -version | Out-String)
    if ($LASTEXITCODE -ne 0 -or $v -notmatch [regex]::Escape($m.ffmpeg_version)) { Fail "$t did not report version $($m.ffmpeg_version)" }
}
if (-not (Test-Path (Join-Path $root "THIRD_PARTY_NOTICES\FFmpeg\SOURCE_OFFER.txt"))) { Fail "LGPL source-offer notice missing" }
Write-Host "FFmpeg engine OK ($($m.ffmpeg_version), $($m.license_mode))"
