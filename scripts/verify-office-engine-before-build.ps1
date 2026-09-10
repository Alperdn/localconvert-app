<#
.SYNOPSIS
    Fail-closed pre-bundle check: refuses to let a release build proceed
    if the Office Engine resources it claims to ship are missing or
    invalid.

.DESCRIPTION
    Wired into src-tauri/tauri.conf.json as `bundle.beforeBundleCommand`
    (release packaging only - dev/`tauri dev` does not call this). If the
    manifest is missing, the executable is missing, the pinned
    version/hash don't match, or a required resource directory is
    missing, this exits non-zero and Tauri's build stops - so a
    production installer can never silently ship an "Office-enabled" UI
    backed by nothing.

    Development bypass: set LOCALCONVERT_SKIP_OFFICE_ENGINE_CHECK=1 to
    allow a build to proceed without the engine (e.g. iterating on
    unrelated UI/installer changes). Never set this for a build that will
    actually be distributed - see docs/OFFICE_ENGINE.md.
#>

[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

if ($env:LOCALCONVERT_SKIP_OFFICE_ENGINE_CHECK -eq "1") {
    Write-Warning "LOCALCONVERT_SKIP_OFFICE_ENGINE_CHECK=1 - skipping Office Engine validation. This build must not be distributed."
    exit 0
}

# $IsWindows only exists under PowerShell 6+ (pwsh); Windows PowerShell
# 5.1 (the default on every Windows machine, incl. this repo's dev
# environment) only ever runs on Windows, so treat "not defined" as
# "yes, Windows" rather than misreading $null as "not Windows".
$onWindows = $true
if (Get-Variable -Name IsWindows -ErrorAction SilentlyContinue) {
    $onWindows = $IsWindows
}
if (-not $onWindows) {
    Write-Host "Non-Windows build target - bundled Office Engine is Windows x86-64 only (Step 4 scope). Skipping validation."
    exit 0
}

$RepoRoot    = Resolve-Path (Join-Path $PSScriptRoot "..")
$EngineDir   = Join-Path $RepoRoot "src-tauri\engines\office"
$ManifestPath = Join-Path $EngineDir "manifest.json"

$PinnedVersion      = "25.8.7"
$PinnedArchitecture = "x86_64"

function Fail($msg) {
    Write-Error @"
Office Engine validation FAILED: $msg

This build was going to ship an installer claiming Office->PDF support
without a valid bundled engine behind it. Run:

    powershell -ExecutionPolicy Bypass -File scripts/prepare-office-engine.ps1

to populate src-tauri/engines/office/ first, or set
LOCALCONVERT_SKIP_OFFICE_ENGINE_CHECK=1 for a deliberate non-distributed
dev build.
"@
    exit 1
}

if (-not (Test-Path $ManifestPath)) {
    Fail "manifest.json missing at $ManifestPath"
}

try {
    $manifest = Get-Content $ManifestPath -Raw | ConvertFrom-Json
} catch {
    Fail "manifest.json is not valid JSON: $_"
}

if ($manifest.engine_id -ne "office") {
    Fail "manifest.json engine_id is not 'office'."
}
if ($manifest.version -ne $PinnedVersion) {
    Fail "manifest.json version '$($manifest.version)' does not match pinned version '$PinnedVersion'."
}
if ($manifest.architecture -ne $PinnedArchitecture) {
    Fail "manifest.json architecture '$($manifest.architecture)' does not match pinned architecture '$PinnedArchitecture'."
}
if ([string]::IsNullOrWhiteSpace($manifest.sha256) -or $manifest.sha256.Length -ne 64) {
    Fail "manifest.json sha256 field is missing or malformed."
}

$exePath = Join-Path $EngineDir $manifest.executable_relative_path
if (-not (Test-Path $exePath)) {
    Fail "Expected executable not found at $exePath"
}

foreach ($rel in $manifest.required_relative_dirs) {
    $dirPath = Join-Path $EngineDir $rel
    if (-not (Test-Path $dirPath -PathType Container)) {
        Fail "Required resource directory missing: $dirPath"
    }
}

Write-Host "Office Engine validated: version $($manifest.version) ($($manifest.architecture)), sha256 $($manifest.sha256)"
exit 0
