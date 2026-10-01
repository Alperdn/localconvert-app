<#
.SYNOPSIS
    Release gate: verifies src-tauri/engines/speech/ against its own manifest.json
    (independently of the Rust code) and confirms whisper-cli starts.
    Exits non-zero on any problem.
#>
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dir = Join-Path $root "src-tauri\engines\speech"
function Fail($m) { Write-Error "Speech engine verification FAILED: $m"; exit 1 }
if (-not (Test-Path "$dir\manifest.json")) { Fail "manifest.json missing - run scripts/prepare-speech-engine.ps1" }
$m = Get-Content "$dir\manifest.json" -Raw | ConvertFrom-Json
if ($m.engine_id -ne "speech") { Fail "wrong engine_id" }
foreach ($f in $m.bin_files) {
    $p = Join-Path $dir $f.path
    if (-not (Test-Path $p)) { Fail "missing $($f.path)" }
    if ((Get-FileHash $p -Algorithm SHA256).Hash.ToLower() -ne $f.sha256) { Fail "hash mismatch: $($f.path)" }
}
# App-local VC++ runtime: present, hash-matching, Microsoft-signed.
if (-not $m.runtime_files -or $m.runtime_files.Count -eq 0) { Fail "manifest declares no runtime_files" }
foreach ($f in $m.runtime_files) {
    $rp = Join-Path $dir $f.path
    if (-not (Test-Path $rp)) { Fail "missing runtime $($f.path)" }
    if ((Get-FileHash $rp -Algorithm SHA256).Hash.ToLower() -ne $f.sha256) { Fail "runtime hash mismatch: $($f.path)" }
    $sig = Get-AuthenticodeSignature $rp
    if ($sig.Status -ne "Valid" -or $sig.SignerCertificate.Subject -notmatch "Microsoft") { Fail "runtime $($f.path) is not validly Microsoft-signed" }
}
if (-not (Test-Path (Join-Path $root "THIRD_PARTY_NOTICES\Microsoft-VC-Runtime\NOTICE.txt"))) { Fail "VC++ runtime notice missing" }
$mp = Join-Path $dir $m.model.file_relative_path
if (-not (Test-Path $mp)) { Fail "model file missing" }
if ((Get-Item $mp).Length -ne $m.model.size_bytes) { Fail "model size mismatch" }
if ((Get-FileHash $mp -Algorithm SHA256).Hash.ToLower() -ne $m.model.sha256) { Fail "model hash mismatch" }
if (-not $m.model.languages -or $m.model.languages.Count -eq 0) { Fail "model declares no languages" }
# whisper-cli logs "load_backend: ..." to stderr; that must not be treated as a PowerShell error.
$ErrorActionPreference = "Continue"
$out = & "$dir\$($m.executable_relative_path)" --version 2>&1 | Out-String
$ErrorActionPreference = "Stop"
if ($out -notmatch "loaded CPU backend") { Fail "whisper-cli did not load a CPU backend on this machine" }
if (-not (Test-Path (Join-Path $root "THIRD_PARTY_NOTICES\whisper.cpp\LICENSE"))) { Fail "license notice missing" }
Write-Host "Speech engine OK ($($m.engine_name) $($m.engine_version), model $($m.model.id))"
