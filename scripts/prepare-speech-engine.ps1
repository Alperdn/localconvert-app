<#
.SYNOPSIS
    BUILD/DEVELOPMENT-time only. Stages the bundled Speech Engine
    (whisper.cpp CLI + one ggml model) into src-tauri/engines/speech/.

.DESCRIPTION
    MEB-Donusturucu never downloads anything at application runtime. This
    script is the ONLY place the speech engine or model is fetched.

    Pinned inputs (all hashes verified BEFORE anything is extracted/copied;
    any mismatch deletes the download and exits non-zero):
      * whisper.cpp v1.9.2 official release asset whisper-bin-x64.zip
        (github.com/ggml-org/whisper.cpp release assets)
      * ggml-small-q5_1.bin from Hugging Face repo ggerganov/whisper.cpp at
        an EXACT commit revision (never "main")

    Output layout (engines/speech/):
      bin/            whisper-cli.exe + its DLLs (only files listed in $RequiredBinFiles)
      models/         <model file>
      manifest.json   generated - see src-tauri/src/engines/speech_manifest.rs
      (licenses go to THIRD_PARTY_NOTICES/whisper.cpp/)

    To switch model later (medium-q5_0, large-v3-turbo-q5_0): change ONLY the
    $Model block below (id, file, revision, sha256, size, languages). The
    Rust transcription code reads everything from manifest.json.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/prepare-speech-engine.ps1
#>
[CmdletBinding()]
param(
    [string]$CacheDir = "$env:TEMP\meb-speech-engine"
)
$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot

# ---------------- PINNED: engine ----------------
$Engine = @{
    version = "v1.9.2"
    url     = "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.2/whisper-bin-x64.zip"
    file    = "whisper-bin-x64.zip"
    sha256  = "49dcc16de826f20bd53d44f947a1ae49dfa81f86cad67a64d80820cb192d674a"
    commit  = "306c88f4d1286aec1bf96e544632897886af5501"
}
# ---------------- PINNED: model (swap here only) ----------------
$Model = @{
    id        = "ggml-small-q5_1"
    file      = "ggml-small-q5_1.bin"
    repo      = "ggerganov/whisper.cpp"
    revision  = "5359861c739e955e79d9a303bcbc70fb988958b1"
    sha256    = "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb"
    size      = 190085487
    multilingual = $true
    languages = @("tr", "auto")
    license   = "MIT"
}
$Model.url = "https://huggingface.co/$($Model.repo)/resolve/$($Model.revision)/$($Model.file)"
# ---------------- PINNED: app-local VC++ runtime (see the staging block below) ----------------
$Runtime = @{
    name        = "Microsoft Visual C++ 2015-2022 Runtime (VC143)"
    fileVersion = "14.44.35211.0"
    redistFolder = "14.44.35112"
    source      = "Visual Studio 2022 Community 17.14 - VC\Redist\MSVC\14.44.35112\x64 (Distributable Code list: https://aka.ms/vs/17/redist.txt)"
    files       = @(
        @{ name = "msvcp140.dll";     sub = "Microsoft.VC143.CRT";    sha256 = "0f885b509a685d2bbfa652fed26b5fb31d88fbdab0a978c641d1c7b8aa460aa9" },
        @{ name = "vcruntime140.dll";   sub = "Microsoft.VC143.CRT";  sha256 = "d5e4d9a3e835fa679450145d6a7d94e36573a509317111904d9b3712c30d9066" },
        @{ name = "vcruntime140_1.dll"; sub = "Microsoft.VC143.CRT";  sha256 = "1f2d41c4aa5db0bc33ebf7b66d72943a817d7ce6cbe880502a9403823633093f" },
        @{ name = "vcomp140.dll";     sub = "Microsoft.VC143.OpenMP"; sha256 = "55aba23cdcd6484fbb06f4155b8ca75adfce7a881f10afd0c49457165e677164" }
    )
}
# Files copied from the release zip into bin/ (anything else is ignored).
$RequiredBinFiles = @("whisper-cli.exe")   # DLLs are discovered and added below

function Get-Pinned($url, $dest, $sha256, $size) {
    if (Test-Path $dest) {
        if ((Get-FileHash $dest -Algorithm SHA256).Hash.ToLower() -eq $sha256) { return }
        Remove-Item $dest -Force
    }
    New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
    Write-Host "Downloading $url"
    & curl.exe -fL --retry 3 -o $dest $url
    if ($LASTEXITCODE -ne 0) { Remove-Item $dest -Force -ErrorAction SilentlyContinue; throw "download failed" }
    $got = (Get-FileHash $dest -Algorithm SHA256).Hash.ToLower()
    if ($got -ne $sha256) {
        Remove-Item $dest -Force
        throw "SHA-256 MISMATCH for $(Split-Path $dest -Leaf): expected $sha256 got $got. STOP."
    }
    if ($size -and (Get-Item $dest).Length -ne $size) { Remove-Item $dest -Force; throw "SIZE MISMATCH" }
}

$zip = Join-Path $CacheDir $Engine.file
$mdl = Join-Path $CacheDir $Model.file
Get-Pinned $Engine.url $zip $Engine.sha256 $null
Get-Pinned $Model.url  $mdl $Model.sha256 $Model.size

$dest = Join-Path $repoRoot "src-tauri\engines\speech"
if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
New-Item -ItemType Directory -Force "$dest\bin", "$dest\models" | Out-Null

$ex = Join-Path $CacheDir "extract"
if (Test-Path $ex) { Remove-Item $ex -Recurse -Force }
Expand-Archive $zip $ex -Force
$cli = Get-ChildItem $ex -Recurse -Filter "whisper-cli.exe" | Select-Object -First 1
if (-not $cli) { throw "whisper-cli.exe not found in release zip" }
$srcBin = $cli.DirectoryName
# SDL2.dll (live-audio capture for whisper's stream demos) and parakeet.dll
# (a different model family) are NOT needed by whisper-cli for file
# transcription - verified by running it without them - so they are not
# shipped: less attack surface, no audio-capture code in the bundle.
$ExcludedDlls = @("SDL2.dll", "parakeet.dll")
Get-ChildItem $srcBin -File | Where-Object { ($_.Name -eq "whisper-cli.exe") -or ($_.Extension -eq ".dll" -and $_.Name -notin $ExcludedDlls) } |
    ForEach-Object { Copy-Item $_.FullName "$dest\bin\" }
Copy-Item $mdl "$dest\models\$($Model.file)"

$binFiles = Get-ChildItem "$dest\bin" -File | Sort-Object Name | ForEach-Object {
    @{ path = "bin/$($_.Name)"; sha256 = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(); size = $_.Length }
}

# ---------------- App-local Visual C++ runtime ----------------
# whisper-cli/whisper.dll/ggml*.dll import the VC++ runtime. End users must not
# be asked to install the Redistributable, so the exact DLLs are deployed
# app-local next to the engine. Source: the Visual Studio 2022 redist folder
# (Microsoft's "Distributable Code": files under VC\redist may be copied and
# distributed unmodified with your program - https://aka.ms/vs/17/redist.txt).
# NOT taken from System32 or any third party; NOT extracted from
# VC_redist.x64.exe (that installer's own license forbids bundling it).
$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) { throw "Visual Studio (vswhere) not found - needed as the official source of the runtime DLLs." }
$vsPath = & $vswhere -latest -products * -property installationPath
$redistBase = Join-Path $vsPath "VC\Redist\MSVC\$($Runtime.redistFolder)\x64"
if (-not (Test-Path $redistBase)) { throw "Pinned redist folder $($Runtime.redistFolder) not found under $vsPath" }

$runtimeEntries = @()
foreach ($f in $Runtime.files) {
    $src = Join-Path $redistBase "$($f.sub)\$($f.name)"
    if (-not (Test-Path $src)) { throw "missing $src" }
    $h = (Get-FileHash $src -Algorithm SHA256).Hash.ToLower()
    if ($h -ne $f.sha256) { throw "SHA-256 MISMATCH for runtime $($f.name): expected $($f.sha256) got $h. STOP." }
    $sig = Get-AuthenticodeSignature $src
    if ($sig.Status -ne "Valid" -or $sig.SignerCertificate.Subject -notmatch "Microsoft") { throw "$($f.name): Authenticode signature is not a valid Microsoft signature." }
    $ver = (Get-Item $src).VersionInfo.FileVersion
    if ($ver -ne $Runtime.fileVersion) { throw "$($f.name): version $ver, expected $($Runtime.fileVersion)" }
    Copy-Item $src "$dest\bin\$($f.name)"
    $runtimeEntries += [ordered]@{ path = "bin/$($f.name)"; sha256 = $h; size = (Get-Item $src).Length; version = $ver }
}

# Prove the set is exactly what the engine needs: every VC++-family import of
# every staged binary must be one of the staged runtime DLLs, and vice versa.
$dumpbin = Get-ChildItem (Join-Path $vsPath "VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe") | Sort-Object FullName | Select-Object -Last 1
if (-not $dumpbin) { throw "dumpbin.exe not found (needed to derive the required runtime DLL set)." }
$needed = @{}
Get-ChildItem "$dest\bin" -File | Where-Object { $_.Extension -in ".exe", ".dll" -and ($Runtime.files.name -notcontains $_.Name) } | ForEach-Object {
    foreach ($line in (& $dumpbin.FullName /nologo /dependents $_.FullName)) {
        $l = $line.Trim().ToLower()
        if ($l -match '^(msvcp|vcruntime|vcomp|concrt|vccorlib|vcamp|mfc|msvcr)\S*\.dll$') { $needed[$l] = 1 }
    }
}
$have = @($Runtime.files.name | ForEach-Object { $_.ToLower() }) | Sort-Object
$want = @($needed.Keys) | Sort-Object
if (($have -join ",") -ne ($want -join ",")) { throw "Runtime set mismatch. Engine needs: $($want -join ', '); staged: $($have -join ', '). STOP." }
Write-Host "VC++ runtime staged app-local: $($have -join ', ') (v$($Runtime.fileVersion))"

$rtNotices = Join-Path $repoRoot "THIRD_PARTY_NOTICES\Microsoft-VC-Runtime"
New-Item -ItemType Directory -Force $rtNotices | Out-Null
$rtLines = @(
    "Microsoft Visual C++ 2015-2022 Runtime (VC143) - app-local deployment",
    "",
    "The Ses Dikte speech engine (whisper.cpp, MIT) is built with Microsoft Visual C++ and needs the",
    "following runtime DLLs, deployed unmodified beside it in engines/speech/bin/ so that end users do",
    "not have to install the Visual C++ Redistributable:",
    ""
)
foreach ($e in $runtimeEntries) { $rtLines += ("  {0}  v{1}  {2} bytes  SHA-256 {3}" -f $e.path, $e.version, $e.size, $e.sha256) }
$rtLines += @(
    "",
    "Source:   $($Runtime.source)",
    "Redist folder version: $($Runtime.redistFolder)   File version: $($Runtime.fileVersion)",
    "Signed:   Microsoft Corporation (Authenticode valid, verified at staging time)",
    "",
    "License basis: Microsoft's 'Distributable Code for Visual Studio 2022' list (https://aka.ms/vs/17/redist.txt):",
    "  'Subject to the License Terms for the software, you may copy and distribute with your program any of",
    "   the files within the following folder and its subfolders except as noted below. You may not modify",
    "   these files.  [VisualStudioFolder]\VC\redist'",
    "The files are distributed UNMODIFIED. Any redistribution is subject to the Microsoft Software License",
    "Terms of the Visual Studio edition used to obtain them; see also the notices in the Visual Studio",
    "installation (Licenses\1033\ThirdPartyNotices.txt).",
    "",
    "These files are NOT covered by this project's MIT license."
)
$rtLines | Set-Content (Join-Path $rtNotices "NOTICE.txt") -Encoding utf8

$notices = Join-Path $repoRoot "THIRD_PARTY_NOTICES\whisper.cpp"
New-Item -ItemType Directory -Force $notices | Out-Null
$lic = Get-ChildItem $ex -Recurse -Include "LICENSE*", "COPYING*", "NOTICE*" -File -ErrorAction SilentlyContinue
foreach ($l in $lic) { Copy-Item $l.FullName $notices -Force }
if (-not (Test-Path "$notices\LICENSE")) {
    & curl.exe -fsSL -o "$notices\LICENSE" "https://raw.githubusercontent.com/ggml-org/whisper.cpp/$($Engine.commit)/LICENSE"
}
@"
Whisper model weights (OpenAI) are MIT licensed; ggml conversion by ggerganov.
Model: $($Model.id)  Source: huggingface.co/$($Model.repo) @ $($Model.revision)  License: $($Model.license)
"@ | Set-Content "$notices\MODEL_NOTICE.txt" -Encoding utf8

$manifest = [ordered]@{
    schema_version = 2
    engine_id      = "speech"
    engine_name    = "whisper.cpp"
    engine_version = $Engine.version
    architecture   = "x86_64"
    source         = $Engine.url
    source_archive_sha256 = $Engine.sha256
    source_commit  = $Engine.commit
    license        = "MIT"
    executable_relative_path = "bin/whisper-cli.exe"
    bin_files      = @($binFiles)
    runtime = [ordered]@{
        name         = $Runtime.name
        version      = $Runtime.fileVersion
        source       = $Runtime.source
        license_notice_path = "THIRD_PARTY_NOTICES/Microsoft-VC-Runtime"
        deployment   = "app-local (unmodified, beside the engine executable)"
    }
    runtime_files  = @($runtimeEntries)
    model = [ordered]@{
        id = $Model.id; file_relative_path = "models/$($Model.file)"
        sha256 = $Model.sha256; size_bytes = $Model.size
        source = $Model.url; source_revision = $Model.revision
        multilingual = $Model.multilingual; languages = $Model.languages; license = $Model.license
    }
    license_notice_path = "THIRD_PARTY_NOTICES/whisper.cpp"
    bundled_at_build = (Get-Date).ToUniversalTime().ToString("o")
}
# UTF-8 WITHOUT BOM (the Rust loader also strips a BOM defensively)
[System.IO.File]::WriteAllText("$dest\manifest.json", ($manifest | ConvertTo-Json -Depth 6), (New-Object System.Text.UTF8Encoding($false)))
Write-Host "Speech engine staged at $dest"
