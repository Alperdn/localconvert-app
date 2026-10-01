# Ses Dikte — local speech engine

Ses Dikte (speech-to-text) runs **entirely on the user's device**. There is no
cloud API, no upload, no runtime download, no online fallback. This document
records what is bundled, how it is prepared and verified, and the policy
decisions behind it. Phase status: **B + C done** (engine, capability,
uploaded-audio transcription). Microphone (D), editor/export (E) and final
hardening (F) are still open.

## Pipeline

```
uploaded audio ─▶ validate (local path, extension, size)
              ─▶ ffprobe (has audio stream? duration)         [bundled FFmpeg build]
              ─▶ ffmpeg → 16 kHz mono PCM16 WAV in the job dir [bundled FFmpeg build]
              ─▶ exact duration + silence check (Rust)
              ─▶ SpeechEngine (whisper.cpp sidecar) → transcript
```

Engine identities (`src-tauri/src/engines/engine_id.rs`): `Speech`,
`AudioFfmpeg`, `AudioFfprobe`. The two FFmpeg ids are deliberately **not** the
legacy `Ffmpeg`/`Ffprobe`: the bundled build has no video codecs and must never
silently back video conversion. All three resolve **only** from the bundled
`engines/` directory (no PATH, no system tier, no env-var override).

## Bundled components (pinned)

| Component | Version | Source | SHA-256 |
|---|---|---|---|
| whisper.cpp CLI (`whisper-bin-x64.zip`) | v1.9.2 (commit `306c88f4`) | official `ggml-org/whisper.cpp` GitHub release asset | `49dcc16de826f20bd53d44f947a1ae49dfa81f86cad67a64d80820cb192d674a` |
| Model `ggml-small-q5_1.bin` (190,085,487 B) | HF revision `5359861c739e955e79d9a303bcbc70fb988958b1` | `huggingface.co/ggerganov/whisper.cpp` (MIT) | `ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb` |
| FFmpeg + ffprobe (minimal, LGPL) | 9.0.2 | built from the **official** `ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz` | source `8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e`; GPG-verified against release key `FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8` |

`ffmpeg.exe` `e10c5f20…942510`, `ffprobe.exe` `8370e2bb…9f6f` (full hashes in
`src-tauri/engines/ffmpeg/manifest.json`).

## Workflow (all build-time; nothing downloads at runtime)

| Step | Script |
|---|---|
| Build minimal FFmpeg from verified source (MSYS2 UCRT64, GCC 16.2.0) | `scripts/build-ffmpeg-engine.ps1` (+ `scripts/ffmpeg/build-ffmpeg.sh`) |
| Stage FFmpeg + manifest + LGPL notices | `scripts/prepare-ffmpeg-engine.ps1` |
| Stage whisper.cpp + model + manifest + notices | `scripts/prepare-speech-engine.ps1` |
| Release gates (independent of the Rust code) | `scripts/verify-ffmpeg-engine-before-build.ps1`, `scripts/verify-speech-engine-before-build.ps1` (chained into `npm run tauri:build:release`) |
| Synthetic test fixtures (dev only) | `scripts/generate-speech-fixtures.ps1` |

All scripts fail closed on any hash mismatch. MSYS2/MinGW are **build-time only**
dependencies of the developer machine.

### FFmpeg configuration (LGPL-2.1-or-later, no GPL/nonfree/version3, no external libs)

`--disable-everything --disable-autodetect --disable-network --disable-debug
--disable-ffplay --disable-avdevice --enable-static --extra-ldflags=-static
--enable-protocol=file --enable-demuxer=wav,mp3,aac,mov,flac,ogg
--enable-parser=… --enable-decoder=pcm_*,mp3float,mp3,aac,aac_latm,alac,flac,vorbis,opus
--enable-muxer=wav --enable-encoder=pcm_s16le --enable-filter=aresample,aformat,anull,anullsink,abuffer,abuffersink`
(full line in the manifest). Audited output: only the `file` protocol, only the
`wav` muxer and `pcm_s16le` encoder. `ffmpeg -buildconf` contains no gpl /
nonfree / version3. **Known limit:** without zlib the `mov` demuxer cannot read
compressed QuickTime headers (`cmov`), which modern `.m4a` files never use.

## Manifests

* `engines/speech/manifest.json` — schema 2: engine name/version/source/hash,
  `bin_files[]` (path, sha256, size — including the executable), a `runtime` block
  (name, version, source, deployment) with `runtime_files[]` (the app-local VC++ DLLs:
  path, sha256, size, version), and a `model`
  block (`id`, `file_relative_path`, `sha256`, `size_bytes`, `source_revision`,
  `multilingual`, `languages[]`). **The model id, hash and language capability come
  only from here**, so replacing small-q5_1 with medium-q5_0 / large-v3-turbo-q5_0
  is a change to `$Model` in `prepare-speech-engine.ps1` plus re-staging — no
  transcription code changes.
* `engines/ffmpeg/manifest.json` — engine id, FFmpeg version, architecture,
  source URL + hash, exact configure flags, per-binary hashes, license mode.

Validation (`engines/bundle.rs`, `speech_manifest.rs`, `ffmpeg_manifest.rs`): every
listed file is re-hashed (cached by size+mtime); manifest paths may not escape the
engine dir; the executable must itself be hashed; a manifest claiming a non-LGPL
mode or carrying `--enable-gpl/-nonfree/-version3` is refused. `available` also
requires that the engine **actually starts** on this machine
(`whisper-cli --version` must load a CPU backend).

## Capability model

`speech_transcription` is computed by `capabilities.rs` from
`engines::speech::report()`:

| Backend status | Capability state | UI label |
|---|---|---|
| `AVAILABLE` | `AVAILABLE` | Hazır |
| `ENGINE_MISSING`, `MODEL_MISSING`, `ENGINE_INVALID` (hash/manifest), `RUNTIME_MISSING`, `AUDIO_PREP_UNAVAILABLE` | `ENGINE_MISSING` | Bileşen eksik |
| `CPU_UNSUPPORTED` | `NOT_IMPLEMENTED` | Henüz desteklenmiyor |

## CPU requirements (measured, bundled build, 34 s clip, 4 threads)

The build ships ggml CPU backends that are selected at load time. **No hard AVX/AVX2
requirement** — any x86-64 CPU runs it — but speed depends on the backend:
x64 baseline / SSE4.2 ≈ 68.6 s (2.0× slower than real time), AVX ≈ 24.7 s (0.73×),
AVX2 ≈ 8.1 s (0.24×). Without AVX2 the page shows a Turkish "may be slower" note.
Windows on ARM64 (no ARM build is shipped) reports "Henüz desteklenmiyor".

## Limits (initial development defaults — **not final production limits**)

`src-tauri/src/speech/limits.rs` (`DEFAULT_LIMITS`), boundary-tested:

| Limit | Value | Rationale |
|---|---|---|
| Input file | 500 MB | bounds read/decode cost |
| Audio duration | 3 h | practicality on school-class CPUs |
| Microphone recording | 2 h | ≈ 230 MB of 16 kHz PCM16 |
| Concurrent jobs | 1 | CPU-bound; RAM |
| Free disk | source + normalized WAV (32 KB/s) + 256 MB workspace | conservative |

## Security notes

* No shell anywhere: `Command::new(resolved_exe)` + `args` array; cwd = job dir.
* The frontend supplies only a picked file path and a language code that the
  manifest declares. It never supplies an executable, model path or engine name.
* Input paths: absolute, local, not UNC, not a URL, allow-listed extension,
  canonicalized; original filenames are display-only (jobs use UUID dirs and
  generated names). FFmpeg additionally gets `-protocol_whitelist file`, a
  `file:` prefix, `-map 0:a:0 -vn -sn -dn`, and `-t <limit+1 s>` (output is
  bounded even if a container lies about its duration).
* **Non-ASCII paths (root cause + fix).** `whisper-cli.exe` gets an ANSI-narrowed argv from the
  CRT and opens its model with a UTF-8-only loader, so *any* non-ASCII character in a path it is
  handed fails - including Turkish `ç ş ğ ı ö ü İ` on a Turkish Windows (cp1254); Cyrillic/Arabic/CJK are
  also mangled to `?`. The launcher (`CreateProcessW`) and the file APIs are fine: `ffmpeg`/`ffprobe` open
  the same paths, and whisper works with relative ASCII names in a non-ASCII cwd. Fix
  (`engines/ascii_link.rs`): children never receive a non-ASCII path. Each job runs with `cwd = <UUID job
  dir>`; the user's file is hard-linked (else cancel-aware copied) to `input.<ext>`; the model is exposed as
  `m/<file>` through a directory junction to the engine's `models/` dir (no admin, no 190-570 MB copy;
  fallbacks: hard link, then copy). The original path/name is display metadata only. Cleanup detaches the
  junction first so only the link can ever be removed (tested: the engine's model survives job cleanup and
  the stale-dir sweep). The old 8.3 short-path workaround was removed.
* Environment: `GGML_BACKEND_PATH`, `FFREPORT` (plus the existing denylist) are
  stripped before every spawn. Whisper runs at below-normal priority.
* Cancellation kills exactly the child the job spawned and reaps it; the job dir
  (normalized WAV, JSON output) is removed on success, failure and cancel, and
  stale dirs are swept at startup.
* Network audit: no network crate in `Cargo.toml` (tested); no bundled binary
  imports a networking DLL (PE import scan); live `netstat` sampling showed 0
  sockets for `whisper-cli` and `ffmpeg`; the webview CSP `connect-src` remains
  `'self' ipc: http://ipc.localhost` (tested).

## Manual acceptance tests

Phase C scope (mic tests arrive with Phase D):

1. Upload a WAV / MP3 / M4A / AAC / FLAC / OGG with Turkish speech → text appears, timestamps toggle works.
2. Upload a 10+ minute recording → UI stays responsive; "Dikte ediliyor… Geçen süre" advances; cancel works.
3. Silence-only file → "Konuşma algılanamadı."; no transcript.
4. Music-only file → no fabricated paragraph.
5. Corrupt MP3, zero-byte file, video-only `.m4a`, `.txt`/`.mp4` → Turkish error, no crash.
6. Cancel mid-normalization and mid-transcription → returns to "İptal edildi"; Task Manager shows no `whisper-cli`/`ffmpeg`; `%TEMP%\MEB-Donusturucu\temp\speech\` empty.
7. Retry after failure → works and starts a fresh job.
8. Disconnect the network → transcription still works.
9. Settings → system status shows "Konuşma Tanıma: Hazır / Motor / Model / Dil".
10. Rename/remove the model file → capability becomes "Bileşen eksik"; Start is disabled; no fallback.
11. Put the audio under `C:\Çalışmalar\Öğretmen\Ses Kayıtları\` (and a Cyrillic/Arabic folder) and install the app under a non-ASCII folder → transcription works and the text matches an ASCII-path run.
12. On a clean Windows VM/Sandbox with NO Visual C++ Redistributable installed → the engine starts (runtime is app-local); delete one `vc*140.dll` from `engines/speech/bin` → "Bileşen eksik" with the Turkish reinstall message.

## Known risks

* **VC++ runtime - resolved (app-local).** The engine imports `msvcp140.dll`, `vcruntime140.dll`,
  `vcruntime140_1.dll` and `vcomp140.dll` (OpenMP) - exactly this set (derived with `dumpbin /dependents`
  and enforced by `prepare-speech-engine.ps1`). They are staged unmodified in `engines/speech/bin/` from the
  Visual Studio 2022 redist folder (`VC\Redist\MSVC\14.44.35112\x64`, file version 14.44.35211.0, all
  Microsoft-signed), hash-pinned in the script, listed with SHA-256 in `manifest.json` (`runtime_files`),
  verified by the capability self-check (missing -> `SPEECH_RUNTIME_MISSING`, tampered -> invalid), and
  documented in `THIRD_PARTY_NOTICES/Microsoft-VC-Runtime/`. **License basis:** Microsoft's Distributable
  Code list (https://aka.ms/vs/17/redist.txt) allows copying files under `VC\redist` *unmodified, with your
  program*, subject to the license terms of the Visual Studio edition they were obtained from (this build
  used Visual Studio Community 2022). The DLLs are deliberately NOT taken from `VC_redist.x64.exe`: that
  installer's own license forbids bundling it with an application. **Have legal confirm** that the release
  builds are made under a Visual Studio license that grants the Distributable Code right for an institutional
  distribution (Community has organisation-size/usage conditions).
  Verified on this machine: a live `whisper-cli` loads all four from the app folder (none from System32),
  and poisoning the app-local `vcruntime140.dll` breaks the engine (0xC000012F). Not yet verified on a
  machine that truly lacks the redistributable (no Windows Sandbox/VM available here): run manual test 12.
  UCRT (`api-ms-win-crt-*`) is part of Windows 10+, which Tauri/WebView2 already require.
* **Accuracy.** small-q5_1 measured **≈14 % WER** on 5 real Turkish clips
  (94 words, FLEURS, CC-BY-4.0; not committed). Proper nouns and foreign names are
  the main errors. The synthetic-TTS fixtures are a pipeline test, not an accuracy
  benchmark: the Windows "Tolga" voice scored **59-94 % WER** with this model
  (measured; it is largely unintelligible to it), so never judge the engine by them.
  medium-q5_0 / large-v3-turbo-q5_0 should be benchmarked before locking the V1 model.
  Reproduce with `MEB_HUMAN_SPEECH_DIR=<folder of clip + .txt reference pairs>`
  and `cargo test --lib measure_word_error_rate -- --nocapture`.
* Silence and music: silence never reaches Whisper (energy pre-check). Music-only
  audio DID make Whisper emit the subtitle-credit hallucination "Altyazı M.K."
  during acceptance testing; `clean_segments` now drops known credit strings
  everywhere, drops stock sign-offs ("İzlediğiniz için teşekkürler", "Abone olun")
  only when they are the entire transcript (so a real closing line survives), and
  collapses repetition loops. Result: `SPEECH_NO_SPEECH_DETECTED`. Other
  hallucinations may exist; the filter is a denylist, not a guarantee.
