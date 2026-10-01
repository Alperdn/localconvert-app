<#
.SYNOPSIS
    DEV-TIME ONLY. Generates local, synthetic audio fixtures for the Ses Dikte
    tests into src-tauri/tests/fixtures/speech/generated/ (git-ignored).

.DESCRIPTION
    - Turkish speech is synthesized with the Windows OneCore voice
      "Microsoft Tolga" (tr-TR) - it is TEXT-TO-SPEECH output, not a human
      recording, so nothing personal is ever stored or committed. Real-speech
      accuracy still needs a human recording (manual acceptance test A).
    - Container variants (MP3/M4A/AAC/FLAC/OGG) are encoded with PyAV (a
      dev-only Python venv), used ONLY to make test inputs. It is NOT the
      shipped FFmpeg (that build has no encoders on purpose); encoding with
      an independent implementation makes the shipped decoder's test meaningful.
    - Silence / music-like / no-audio-stream fixtures are generated in
      speech_fixture_encode.py.

    Requires: Windows 10+ with the tr-TR OneCore voice, and a venv at
    ~/fxenv with `pip install av`.
#>
[CmdletBinding()]
param(
    [int]$LongMinutes = 10
)
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$out = Join-Path $repo "src-tauri\tests\fixtures\speech\generated"
New-Item -ItemType Directory -Force $out | Out-Null

# ---- Turkish TTS via WinRT ----
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$null = [Windows.Media.SpeechSynthesis.SpeechSynthesizer, Windows.Media.SpeechSynthesis, ContentType = WindowsRuntime]
$null = [Windows.Storage.Streams.DataReader, Windows.Storage.Streams, ContentType = WindowsRuntime]
$asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' })[0]
function Await($op, $type) { $t = $asTask.MakeGenericMethod($type).Invoke($null, @($op)); $t.Wait(-1) | Out-Null; $t.Result }

function New-TurkishWav([string]$text, [string]$path) {
    $synth = New-Object Windows.Media.SpeechSynthesis.SpeechSynthesizer
    $voice = [Windows.Media.SpeechSynthesis.SpeechSynthesizer]::AllVoices | Where-Object { $_.Language -eq 'tr-TR' } | Select-Object -First 1
    if (-not $voice) { throw "No tr-TR OneCore voice installed." }
    $synth.Voice = $voice
    $stream = Await ($synth.SynthesizeTextToStreamAsync($text)) ([Windows.Media.SpeechSynthesis.SpeechSynthesisStream])
    $reader = New-Object Windows.Storage.Streams.DataReader($stream)
    Await ($reader.LoadAsync([uint32]$stream.Size)) ([uint32]) | Out-Null
    $bytes = New-Object byte[] $stream.Size
    $reader.ReadBytes($bytes)
    [IO.File]::WriteAllBytes($path, $bytes)
}

$sentence = "Merhaba, bugün toplantımızın ilk gündem maddesi siber güvenlik çalışmalarıdır. Öğrencilerimizin kişisel verilerini korumak için yeni önlemler alacağız."
$raw = Join-Path $out "tts_raw.wav"
New-TurkishWav $sentence $raw

# Encoding + non-speech fixtures: PyAV (dev-only venv), see speech_fixture_encode.py
$py = Join-Path $env:USERPROFILE "fxenv\Scripts\python.exe"
if (-not (Test-Path $py)) { throw "Fixture venv missing: python -m venv ~/fxenv ; ~/fxenv/Scripts/pip install av" }
$para = ($sentence + " ") * 3
$chunk = Join-Path $out "tts_chunk.wav"
New-TurkishWav $para $chunk
& $py (Join-Path $PSScriptRoot "speech_fixture_encode.py") $out $raw $chunk $LongMinutes
if ($LASTEXITCODE -ne 0) { throw "fixture encoding failed" }
Remove-Item $chunk, $raw -Force
Get-ChildItem $out | Select-Object Name, Length | Format-Table -AutoSize | Out-String | Write-Host
