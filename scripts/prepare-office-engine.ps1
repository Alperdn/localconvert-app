<#
.SYNOPSIS
    Reproducibly prepares the bundled Office Engine (LibreOffice) resource
    tree consumed by src-tauri/src/engines/office_manifest.rs at runtime.

.DESCRIPTION
    This is a BUILD/DEVELOPMENT-time script only. LocalConvert never
    downloads anything at application runtime - see docs/OFFICE_ENGINE.md.

    Steps:
      1. Uses a PINNED LibreOffice version/architecture/source/hash
         (below - must match src-tauri/src/engines/office_manifest.rs's
         PINNED_VERSION/PINNED_ARCHITECTURE and docs/OFFICE_ENGINE.md).
      2. Downloads the official Windows x86-64 MSI ONLY from
         download.documentfoundation.org (the LibreOffice project's own
         distribution host).
      3. Verifies the downloaded file's SHA-256 against the pinned hash
         BEFORE doing anything else with it. Fails closed - deletes the
         download and exits non-zero - on any mismatch.
      4. Administratively installs the MSI into an isolated staging
         directory (msiexec /a - an "administrative install", i.e. an
         extraction into a target directory, not a real system install:
         nothing is registered, no Start Menu entries, no services).
      5. Copies exactly the program/ and share/ subtrees (plus license
         files) into src-tauri/engines/office/LibreOffice/.
      6. Writes src-tauri/engines/office/manifest.json from
         manifest.json.template with the verified hash and a build
         timestamp filled in.
      7. Copies LICENSE/NOTICE/credits files into
         THIRD_PARTY_NOTICES/LibreOffice/.

    Never falls back to a system-installed LibreOffice. Never proceeds
    past a hash mismatch. Safe to re-run - it is idempotent given the
    same pinned version/hash.

.PARAMETER SkipDownload
    If a matching, already-hash-verified installer is already sitting at
    -InstallerPath, skip re-downloading it.

.PARAMETER InstallerPath
    Where to place/look for the downloaded MSI. Defaults to a temp
    staging folder outside the repo.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/prepare-office-engine.ps1
#>

[CmdletBinding()]
param(
    [switch]$SkipDownload,
    [string]$InstallerPath = "$env:TEMP\localconvert-office-engine\LibreOffice_25.8.7_Win_x86-64.msi"
)

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------
# PINNED VERSION - keep in sync with:
#   - src-tauri/src/engines/office_manifest.rs (PINNED_VERSION/PINNED_ARCHITECTURE)
#   - docs/OFFICE_ENGINE.md
# ---------------------------------------------------------------------
$PinnedVersion      = "25.8.7"
$PinnedArchitecture = "x86_64"
$PinnedFileName     = "LibreOffice_${PinnedVersion}_Win_x86-64.msi"
$PinnedSourceUrl     = "https://download.documentfoundation.org/libreoffice/stable/$PinnedVersion/win/x86_64/$PinnedFileName"
$PinnedChecksumUrl   = "$PinnedSourceUrl.sha256"

# REQUIRED: fill this in from the official checksum file
# ($PinnedChecksumUrl) before running this script for real. Left empty
# deliberately so a stale/wrong hash can never be silently copy-pasted
# forward across a version bump - the script refuses to run without it.
$PinnedSha256 = "ecdb65e76f5e91dc198b8c8dce5b5d6e1eb12fea6023553e52b591afd10b619d"

$RepoRoot      = Resolve-Path (Join-Path $PSScriptRoot "..")
$EngineDir     = Join-Path $RepoRoot "src-tauri\engines\office"
$StagingDir    = Join-Path $EngineDir "_staging"
$TargetLoDir   = Join-Path $EngineDir "LibreOffice"
$NoticesDir    = Join-Path $RepoRoot "THIRD_PARTY_NOTICES\LibreOffice"
$ManifestTpl   = Join-Path $EngineDir "manifest.json.template"
$ManifestOut   = Join-Path $EngineDir "manifest.json"

function Fail($msg) {
    Write-Error $msg
    exit 1
}

if ([string]::IsNullOrWhiteSpace($PinnedSha256)) {
    Fail @"
PinnedSha256 is empty. Fetch and paste the official SHA-256 for
$PinnedFileName from $PinnedChecksumUrl into this script before running
it. This script deliberately fails closed rather than accepting an
unverified binary.
"@
}

New-Item -ItemType Directory -Force -Path (Split-Path $InstallerPath) | Out-Null

if (-not $SkipDownload -or -not (Test-Path $InstallerPath)) {
    Write-Host "Downloading $PinnedFileName from the official LibreOffice distribution host..."
    Invoke-WebRequest -Uri $PinnedSourceUrl -OutFile $InstallerPath -UseBasicParsing
}

Write-Host "Verifying SHA-256..."
$actualHash = (Get-FileHash -Path $InstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()
$expectedHash = $PinnedSha256.ToLowerInvariant()

if ($actualHash -ne $expectedHash) {
    Remove-Item -Force $InstallerPath -ErrorAction SilentlyContinue
    Fail "SHA-256 mismatch for $PinnedFileName. Expected $expectedHash, got $actualHash. Refusing to use this file - it has been deleted."
}
Write-Host "SHA-256 verified: $actualHash"

# ---------------------------------------------------------------------
# Administrative (extraction-only) install into an isolated staging dir.
# This does NOT register LibreOffice on the build machine, create Start
# Menu entries, or touch the registry beyond what msiexec /a itself
# requires for the extraction. Never run without /a.
# ---------------------------------------------------------------------
if (Test-Path $StagingDir) {
    Remove-Item -Recurse -Force $StagingDir
}
New-Item -ItemType Directory -Force -Path $StagingDir | Out-Null

Write-Host "Extracting MSI via administrative install into staging dir..."
$msiArgs = @(
    "/a", "`"$InstallerPath`"",
    "/qn",
    "TARGETDIR=`"$StagingDir`""
)
$proc = Start-Process -FilePath "msiexec.exe" -ArgumentList $msiArgs -Wait -PassThru -NoNewWindow
if ($proc.ExitCode -ne 0) {
    Fail "msiexec administrative install failed with exit code $($proc.ExitCode)."
}

# The administrative image layout nests the actual LibreOffice tree a
# couple of levels down (typically
# <staging>\PFiles64\LibreOffice\...). Locate program\soffice.exe rather
# than hardcoding the exact nesting, which has shifted across releases.
$sofficeCandidate = Get-ChildItem -Path $StagingDir -Recurse -Filter "soffice.exe" -ErrorAction SilentlyContinue |
    Where-Object { $_.Directory.Name -eq "program" } |
    Select-Object -First 1

if (-not $sofficeCandidate) {
    Fail "Could not locate program\soffice.exe anywhere under the extracted staging tree. Administrative install layout may have changed - inspect $StagingDir manually before proceeding."
}

$ExtractedLoRoot = $sofficeCandidate.Directory.Parent.FullName  # .../LibreOffice
Write-Host "Found extracted LibreOffice tree at: $ExtractedLoRoot"

# ---------------------------------------------------------------------
# Copy program/ and share/ into the resource tree the app expects.
# Correctness over size in Step 4 - do not strip files here.
# ---------------------------------------------------------------------
if (Test-Path $TargetLoDir) {
    Remove-Item -Recurse -Force $TargetLoDir
}
New-Item -ItemType Directory -Force -Path $TargetLoDir | Out-Null

foreach ($sub in @("program", "share", "presets", "Fonts")) {
    $src = Join-Path $ExtractedLoRoot $sub
    if (-not (Test-Path $src)) {
        Fail "Expected subdirectory '$sub' missing from extracted LibreOffice tree at $src."
    }
    Write-Host "Copying $sub ..."
    Copy-Item -Path $src -Destination (Join-Path $TargetLoDir $sub) -Recurse -Force
}

# `presets/` is NOT optional seed content - it's the template LibreOffice
# copies from when bootstrapping a brand-new `-env:UserInstallation=`
# profile on first run. Without it, headless conversion fails immediately
# with "User installation could not be completed" (exit code 77) - this
# was confirmed by hand during Step 4 payload acceptance, not assumed.
#
# `System64/` (present alongside `program`/`share` in the administrative
# image, not nested under either) holds the MSVC redistributable DLLs
# (vcruntime140.dll, msvcp140*.dll, concrt140.dll, vccorlib140.dll) a
# real MSI install would register system-wide via a merge module. A
# clean target machine has no guarantee those are already present system-
# wide, so they're copied directly into `program/` (the directory
# containing soffice.exe, which Windows always searches first when
# resolving a process's DLL imports) instead, keeping the bundle
# self-contained.
$system64Src = Join-Path $ExtractedLoRoot "System64"
if (-not (Test-Path $system64Src)) {
    Fail "Expected 'System64' (bundled MSVC runtime DLLs) missing from extracted LibreOffice tree at $system64Src."
}
Write-Host "Merging System64 runtime DLLs into program/ ..."
Copy-Item -Path (Join-Path $system64Src "*.dll") -Destination (Join-Path $TargetLoDir "program") -Force

# ---------------------------------------------------------------------
# License/notice files -> THIRD_PARTY_NOTICES/LibreOffice/
# ---------------------------------------------------------------------
New-Item -ItemType Directory -Force -Path $NoticesDir | Out-Null
Get-ChildItem -Path $ExtractedLoRoot -File |
    Where-Object { $_.Name -match "(?i)^(LICENSE|NOTICE|readme|credits|third.?party)" } |
    ForEach-Object {
        Copy-Item -Path $_.FullName -Destination (Join-Path $NoticesDir $_.Name) -Force
    }

# ---------------------------------------------------------------------
# manifest.json
# ---------------------------------------------------------------------
if (-not (Test-Path $ManifestTpl)) {
    Fail "Missing $ManifestTpl - the checked-in manifest template."
}
$manifest = Get-Content $ManifestTpl -Raw | ConvertFrom-Json
$manifest.version = $PinnedVersion
$manifest.architecture = $PinnedArchitecture
$manifest.source = $PinnedSourceUrl
$manifest.sha256 = $actualHash
$manifest.bundled_at_build = (Get-Date).ToUniversalTime().ToString("o")
$manifestJson = $manifest | ConvertTo-Json -Depth 5
# Windows PowerShell 5.1's `-Encoding utf8` writes a UTF-8 BOM, which
# serde_json (office_manifest.rs::load_manifest) will not skip on its
# own accord. Write explicitly without a BOM instead of relying on the
# Rust side to strip it.
[System.IO.File]::WriteAllText($ManifestOut, $manifestJson, (New-Object System.Text.UTF8Encoding($false)))

Remove-Item -Recurse -Force $StagingDir -ErrorAction SilentlyContinue

Write-Host ""
Write-Host "Office Engine prepared successfully:"
Write-Host "  Version:      $PinnedVersion ($PinnedArchitecture)"
Write-Host "  SHA-256:      $actualHash"
Write-Host "  Resource dir: $TargetLoDir"
Write-Host "  Manifest:     $ManifestOut"
Write-Host ""
Write-Host "Next: run 'npm run verify:office-engine', then 'npm run tauri:build:release'."
