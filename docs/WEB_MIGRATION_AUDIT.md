# MEB-Dönüştür — Web Migration Audit (Tauri desktop → browser + Rust server)

| | |
|---|---|
| Baseline | commit `9a7ae8d` "Checkpoint: MEB-Dönüştür foundation" |
| Nature | **Read-only audit.** No code, config, script or dependency was changed. Nothing was started or installed. |
| Method | Static reading of the repository. Every claim below cites a file/function. Where something was *not* verified it is listed in §19 (Unknowns). |
| Status of this document | Input for decisions (§19). It is not a design that has been approved. |

---

## 0. Executive summary (the findings that change the plan)

1. **The architecture is cleaner to migrate than the Tauri surface suggests.** The conversion core (`converter.rs`, `engines/*`, `native/*`, `speech/pipeline.rs`, `security/*`) is plain Rust with only two real Tauri couplings: `converter::APP_HANDLE` (progress `emit`) and `speech/commands.rs` (`AppHandle::emit`, `State`). Everything else Tauri-specific is in `lib.rs`, `commands.rs`, `security/fs_scope.rs` and the frontend call sites.
2. **The data model is path-based end to end, and that is the single largest redesign.** Every command takes a *client-supplied filesystem path* (`convert_file(inputPath, outputDir)`, `get_file_info(path)`, `speech_start_file_job(path)`). In a web app the browser has no server paths. This must become *upload → opaque file ID → job → download*. All of `security::path_validation` (input/output-dir validation) changes meaning, not just location.
3. **Concurrency model is single-user and process-global.** `RUNNING_PROCESSES`, `CANCELLED_JOBS` (`lazy_static` global `Mutex`es keyed by client-chosen `job_id`), `JobRegistry` (exactly one speech job app-wide), `TOOL_PATHS` cache, and a startup sweep that **deletes every job directory** (`security::temp::cleanup_stale_job_dirs`, `speech::job::cleanup_stale_in`). Each is correct for a desktop and wrong/dangerous for a multi-user server (cross-user cancellation by guessing/choosing a `job_id`; a second server instance or restart wiping live jobs).
4. **Office (LibreOffice) conversions cannot be cancelled today.** `converter::convert_document/spreadsheet/presentation` receive `job_id` but `let _ = job_id;` / never pass it to `engines::office::convert` → `process::run_with_timeout` (no cancel flag). Only a 600 s timeout (`office_manifest::CONVERT_TIMEOUT`) bounds it. The UI "cancel" for an Office job only resets UI state; the process keeps running. On a server this is a resource-leak and must be fixed in the job layer. Also, `run_with_timeout` kills only the direct child (documented in `process.rs` / `docs/OFFICE_ENGINE.md`), and does not drain stdout/stderr (pipe-deadlock risk, acknowledged in a comment).
5. **Two parallel process-launching systems exist.** The hardened `engines::process` (no shell, env deny-list, cwd = job dir, drained pipes, cancel+timeout) is used by Office, speech and audio-prep. The *legacy* `converter::run_command_with_job_id` / `run_ffmpeg_with_progress` is used for **video, audio conversion, ImageMagick, Ghostscript, Pandoc, 7-Zip** and resolves tools from **system PATH** (`tools::get_tool_path` → `which` + common install paths), has no timeout, no env scrubbing, no cwd pinning, and puts stdout/stderr into error strings returned to the UI. A web server must consolidate on the hardened path before exposing it to uploads.
6. **Ses Dikte's microphone path does not exist.** `SpeechDictationPage.tsx` states the mic tab is present but disabled and "nothing on this page ever calls getUserMedia"; `limits.rs` has `max_recording_secs` flagged `#[allow(dead_code)] // enforced by the microphone flow (Phase D)`. The file-upload path is complete and reusable almost as-is on the server side; browser recording is *new work* (MediaRecorder → upload), not a migration.
7. **PDF→DOCX is nothing but LibreOffice's `writer_pdf_import` followed by a DOCX export** (`reconstruction.rs::PdfToDocx::convert` → `office::convert(..., use_pdf_import_filter=true)`). There is **no Rust reconstruction code** — the premise "improve existing Rust reconstruction" has nothing to improve. The text-box explosion is an inherent property of that importer (§14). `PdfToXlsx`/`PdfToPptx` are explicit `FEATURE_NOT_IMPLEMENTED`.
8. **PDF→Office is not capability-gated in the UI**: `capabilityGating.ts::OFFICE_EXTENSIONS` excludes `pdf`, so a PDF input maps to no capability and the "partial quality" state (`ReconstructionQuality::Partial`) is never surfaced.
9. **Bundled engines are Windows x86-64 only** (`office_manifest::PINNED_ARCHITECTURE`, `.msi` administrative-install in `prepare-office-engine.ps1`, `soffice.exe`/`whisper-cli`/`ffmpeg.exe` expectations in `engine_id.rs`; `winreg`, `CREATE_NO_WINDOW`, `BELOW_NORMAL_PRIORITY_CLASS`, junction-based `ascii_link`). A Linux server needs different engine acquisition, manifests, process priority/limits, and a rewritten `ascii_link` (hard-link/copy is already the fallback).
10. **Positive findings worth keeping:** the native image pipeline's decompression-bomb limits (`MAX_DECODED_PIXELS = 40 MP`, `MAX_DIMENSION = 20 000`), `-protocol_whitelist file` + `-nostdin` in audio prep, `enclosed_name()` in ZIP extraction, UUID job dirs, `-dSAFER` for Ghostscript, structured path-free error types (`EngineError`, `SpeechError*`), a "no network" test (`speechNoNetwork.test.ts`), and manifest/SHA-256 verification of engines and model.

---

## 1. CURRENT ARCHITECTURE

### 1.1 Entry points
| Layer | Entry | Notes |
|---|---|---|
| Frontend | `index.html` → `src/main.tsx` → `<App/>` (`src/App.tsx`) | Vite 5 + React 19 + Tailwind 3 + Zustand 4. |
| Backend | `src-tauri/src/main.rs` (5 lines) → `localconvert_lib::run()` in `lib.rs` | `tauri::Builder` with `tauri_plugin_dialog` and `tauri_plugin_fs` only. |
| Crate | `src-tauri/Cargo.toml`: package `localconvert`, lib `localconvert_lib`, `crate-type = ["staticlib","cdylib","rlib"]` | Mobile-style crate types; `rlib` already makes the core reusable as a library. |

### 1.2 Routing
There is **no router**. `App.tsx` switches on store state: `activeView: "convert" | "dictation"` (`useStore.setActiveView`), plus `activeCategory`, and conditional modals (`SettingsModal`, `SystemStatusModal`, `ToolsSetupModal`) and overlays (lazy-loaded `PdfEditor` when `pdfEditorFile` is set; `VideoTrimmer` when `videoTrimmerFile` is set). Deep-linking, browser Back and per-URL state do not exist and would be new.

### 1.3 State management
`src/store/useStore.ts` (Zustand, ~1,220 lines): file list (`ConversionFile` with `status: pending|converting|completed|error`, `progress`, `outputPath`, `error`, `etaSecs`, `speed`, `previewUrl`), `activeConversions: Set<string>`, `settings` (incl. `outputDirectory`, `parallelProcessing`, `maxParallelConversions`, video/audio options), `capabilities`, `tools`, `gpuInfo`. **Persistence:** only the theme (`localStorage` keys `localconvert_theme_preference`, legacy `localconvert_theme`). No conversion history (removed; `system_status` reports `conversion_history_enabled: false`). The speech job state lives in `src/hooks/useSpeechJob.ts` (local `useState`), not in the store.

### 1.4 Tauri command boundary (registered in `lib.rs` `generate_handler!`)
`check_tools`, `detect_gpu`, `convert_file`, `cancel_conversion`, `get_file_info`, `trim_video`, `get_default_output_dir`, `get_image_preview`, `open_file_location`, `get_file_size_estimate`, `get_video_duration`, `get_video_thumbnail`, `get_video_metadata`, `get_hardware_encoders`, `register_context_menu`, `unregister_context_menu`, `get_startup_files`, `get_pdf_form_fields`, `fill_pdf_form_fields`, `get_pdf_text_blocks`, `edit_pdf_text_lopdf`, `authorize_fs_path`, `system_status`, `speech_get_status`, `speech_get_limits`, `speech_inspect_file`, `speech_start_file_job`, `speech_cancel_job`, `get_capabilities`, `office_engine_status`.

Unregistered but present in `commands.rs` (dead scaffolding, contain more path-taking code): `merge_pdfs`, `split_pdf`, `compress_pdf`, `rotate_pdf`, `add_watermark`, `pdf_to_images`, `images_to_pdf`, `resize_image`, `compress_image`, `crop_image`, `rotate_image`, `extract_audio`, `compress_video`, `ocr_pdf`, `extract_archive`, `create_archive`, `open_folder`, `apply_pdf_text_edits`, `get_pdf_info`, `search_replace_pdf_text`, `get_pdf_page_dimensions`, `get_supported_formats`, `get_video_metadata`(registered). The frontend has no UI for them (`lib.rs` comment says so).

### 1.5 Frontend ↔ backend communication
- **Request/response:** `invoke<T>(name, args)`; arguments are camelCase, serde maps to snake_case.
- **Push:** two event channels only: `conversion-progress` (`converter::emit_progress`, payload `ConversionProgress{job_id, progress, current_time_secs, total_duration_secs, speed, eta_secs, status}`) and `speech-job-update` (`speech::commands::UPDATE_EVENT`, payload `JobUpdate{job_id, state, elapsed_secs, progress_pct, transcript, error}`).
- **OS-level events:** `tauri://drag-enter|leave|drop` (`FileDropZone.tsx`, `SpeechDictationPage.tsx`, `App.tsx`).
- **Unusual:** direct JS filesystem access via `@tauri-apps/plugin-fs` in `PdfEditor.tsx` and `pdfSaveService.ts`; `convertFileSrc(path)` (asset protocol) in `VideoTrimmer.tsx`.

### 1.6 Conversion pipeline (`converter::convert_file`)
1. `security::path_validation::validate_input_file` (canonicalize, is-file, not inside app dir) and `validate_output_dir`.
2. `security::temp::JobTempDir::new()` → `<os temp>/localconvert/jobs/<uuid>/` (Drop-cleaned).
3. Category by extension (`tools::get_category_for_extension`) → `convert_video | convert_audio | convert_image | convert_document | convert_spreadsheet | convert_presentation | convert_archive | convert_vector | convert_font`.
4. Output is written into the job dir, verified `confirm_within(job.path(), produced)`, then `security::temp::move_into_place` (rename, copy fallback) to `<output_dir>/<stem>.<ext>`; if it would equal the input, `_converted` is appended. **Existing files at the destination are overwritten silently.**
5. Returns `ConversionResult{success, output_path, error, duration_ms}`. Errors are free-form strings, some containing raw tool stderr/stdout (`run_command_with_job_id`: `"{cmd} failed:\nStderr: …\nStdout: …"`).

Execution is **synchronous inside an `async` Tauri command** (`commands::convert_file` is `async` but calls blocking `do_convert` directly) — it occupies a Tauri async-runtime worker for the entire conversion. A server must use `spawn_blocking`/dedicated workers.

### 1.7 Engine invocation boundaries
| Engine | Where | Mechanism |
|---|---|---|
| Native image | `native/image/*` called from `converter::convert_image` | In-process (`image` 0.25, `tiff` 0.11). |
| LibreOffice | `engines/office.rs::convert` → `engines/process.rs::run_with_timeout` | `soffice --headless [--infilter=writer_pdf_import] -env:UserInstallation=file:///…/<job>/loffice_profile --convert-to <filter> --outdir <dir> <input>` |
| FFmpeg video/audio (conversion) | `converter.rs::convert_video/convert_audio` → `run_ffmpeg_with_progress` | Legacy path; `tools::get_tool_path("ffmpeg")` (PATH). Progress parsed from stderr `time=`/`speed=`; duration from `get_media_duration` (ffprobe). |
| FFmpeg/ffprobe (speech prep) | `engines/audio_prep.rs` | Bundled "minimal" ffmpeg (`EngineId::AudioFfmpeg`/`AudioFfprobe`), `-protocol_whitelist file -nostdin`, via `process::run_cancellable`. |
| whisper.cpp | `engines/speech.rs::WhisperCppEngine::transcribe` | `whisper-cli -m <model> -f norm.wav -l <lang> -t <n> -oj -np -pp -ng -sns -nth 0.6 -of transcript`; `run_cancellable(timeout: None, low_priority: true)`. |
| ImageMagick | `converter::convert_image` fallback, `convert_vector` (SVG), `get_image_preview` fallback | Legacy, PATH. Not bundled; capabilities `advanced_image_formats`, `heic_conversion`, `svg_rasterization` key off it. |
| Ghostscript | `convert_document` (PDF→png/jpg), `convert_vector` (eps/pdf), `merge/split/compress_pdf` (unregistered) | Legacy, PATH, `-dSAFER`, `-r300`. |
| Pandoc | `convert_document` (md/html/txt/rst/epub) | Legacy, PATH. |
| 7-Zip | `convert_archive` (all non-zip) | Legacy, PATH. |
| lopdf | `pdf_text_editor.rs` (1,023 lines) | In-process; `get_pdf_text_blocks`, `edit_pdf_text_lopdf`, form fields. |
| ZIP | `native/archive_zip.rs` | In-process (`zip` 2). |

### 1.8 Temp-file architecture
- **General jobs:** `<temp>/localconvert/jobs/<uuid>/` (`security::temp`).
- **Speech jobs:** `<temp>/MEB-Donusturucu/temp/speech/<uuid>/` (`speech::job::SpeechJobDir`; inspected-only calls also create one).
- **Archive conversion:** `<temp>/localconvert_archive_<epoch-millis>` — **not** a `JobTempDir`, not UUID, not Drop-guarded; two archive jobs started in the same millisecond collide, and a crash leaves it behind (the startup sweep does not cover this path).
- **Also:** `get_image_preview`/`get_video_thumbnail`/PDF editor create their own temp usage (not fully traced; see §19).
- Cleanup: Drop on every exit path + startup sweeps (`lib.rs::setup`). No TTL-based cleanup.

### 1.9 Capability detection
`capabilities.rs::compute_capabilities()` returns 16 `Capability{id, state, message}` with `state ∈ {AVAILABLE, ENGINE_MISSING, NOT_IMPLEMENTED, DISABLED_BY_POLICY}`: `image_conversion|resize|crop|rotate` (always available), `svg_rasterization`, `advanced_image_formats`, `heic_conversion` (ImageMagick present), `archive_operations`, `pdf_text_editing`, `pdf_structural_ops`, `office_to_pdf` (bundled Office self-check), `video_conversion|trimming`, `audio_extraction` (`resolver::is_available(EngineId::Ffmpeg)` — i.e. system FFmpeg, **not** the bundled minimal build), `speech_transcription` (manifest + hash + CPU-run self-check), `ocr` (`NOT_IMPLEMENTED`). The frontend never computes availability itself; `capabilityGating.ts` maps file category/extension → capability id and `convertFiles()` drops blocked files "defense in depth". This design is **directly reusable** as `GET /api/v1/capabilities`.

### 1.10 Error handling
- Engines: `engines/error.rs::EngineError` — stable codes (`OFFICE_ENGINE_NOT_AVAILABLE`, `OFFICE_TIMEOUT`, `FEATURE_NOT_IMPLEMENTED`, …), Display never includes paths/stderr (`technical_detail` kept separately). Test `technical_detail_never_appears_in_display`.
- Native image: `native/image/error.rs` with the same discipline.
- Speech: `engines/speech_error.rs` `SpeechErrorCode` (SCREAMING_SNAKE) with Turkish text → `SpeechErrorDto`.
- **Legacy converters:** `Result<_, String>`, raw tool output embedded. Inconsistent with the above; the UI shows `errorStr` straight from `String(error)` (`useStore.convertFiles` catch block).

### 1.11 Progress reporting
- FFmpeg conversions: parsed from stderr, `emit_progress` → `conversion-progress`. Only `ffmpeg` with a `job_id` gets this (`run_command_with_job_id`: `cmd == "ffmpeg" && job_id.is_some()`).
- Office, image, archive, Ghostscript, Pandoc: **no progress** (UI shows indeterminate/0 until done).
- Speech: 1 Hz ticker emits `elapsed_secs` + real whisper `progress =  NN%` parsed from stderr (`parse_progress`); `None` means indeterminate — "never faked" (explicit design rule in `useSpeechJob.ts`).

### 1.12 Cancellation
- Conversions: `cancel_conversion(job_id)` → `converter::kill_process`: inserts id into `CANCELLED_JOBS`, kills a child from `RUNNING_PROCESSES` if present. Legacy runner polls `is_cancelled` every 100 ms. **Not wired for Office** (see §0.4) or native image / in-process work. Note `RUNNING_PROCESSES` is **never inserted into** anywhere in `converter.rs` (grep: only declaration and the `kill_process` read), so `kill_process` effectively only sets the `CANCELLED_JOBS` flag; actual termination depends on each runner polling `is_cancelled` (100 ms in the legacy runner). In-process work (native image) never polls it.
- Speech: `JobRegistry.cancel(job_id)` flips a per-job `AtomicBool`; `process::run_cancellable` polls every 50 ms and kills+reaps. Cancelled state is returned as terminal `cancelled`.
- Frontend: store `cancelConversion` calls `invoke("cancel_conversion")` then **optimistically** resets the file to `pending` regardless of backend result.

### 1.13 Retry
- Conversions: `useStore.retryFile(id)` (client-side state reset; user re-runs). No server-side retry concept.
- Speech: UI "Yeniden dene"; backend frees the job slot *before* emitting the terminal state so the immediate restart is not refused as `SPEECH_BUSY` (comment in `run_job_thread`).

### 1.14 File picker / drag-drop / output / download
- Pick: `@tauri-apps/plugin-dialog` `open()` in `FileDropZone`, `FileList`, `ConversionPanel`, `SettingsModal`, `WatchFoldersModal`(unused), `SpeechDictationPage`, `useKeyboardShortcuts`. Returns absolute paths → `invoke("get_file_info",{path})`.
- Drop: Tauri window events (`tauri://drag-drop`) deliver OS paths; browser `dragover` handlers exist only for visuals.
- Output: `settings.outputDirectory` (default from `get_default_output_dir`); results are files in that folder; "open file location" = `open_file_location` (OS shell open). **There is no download concept.**
- Speech: result is a JSON transcript in memory delivered through the event; copy/save is UI-side (via dialog/fs; not fully traced).
- Window chrome: `Header.tsx` implements custom minimize/maximize/close via `getCurrentWindow()` (`decorations: false` in `tauri.conf.json`) and `data-tauri-drag-region`.

### 1.15 Dead or desktop-only code paths
`WatchFoldersModal.tsx`, `ScheduleModal.tsx` are **not imported anywhere** (grep: only `ToolsSetupModal` is imported, in `App.tsx`). `register_context_menu`/`unregister_context_menu` (`winreg`, Windows Explorer integration) is reachable from `SettingsModal`. `get_startup_files` reads `std::env::args()` (file-association launch).

---

## 2. TAURI DEPENDENCY INVENTORY

Legend: **A** must be removed · **B** must be replaced · **C** can remain internally but must be refactored · **D** UI-only / no longer needed.

### 2.1 npm packages
| Item | Where | Class | Action |
|---|---|---|---|
| `@tauri-apps/api` | ~20 files (see list below) | **B** | Replace with a thin `api/` client (fetch + EventSource). Remove dependency. |
| `@tauri-apps/plugin-dialog` | `ConversionPanel`, `FileDropZone`, `FileList`, `SettingsModal`, `WatchFoldersModal`, `SpeechDictationPage`, `PdfEditor`, `useKeyboardShortcuts` | **B** | `<input type=file>` / `showOpenFilePicker` fallback; `ask()` → in-app confirm modal; `save()` → browser download. |
| `@tauri-apps/plugin-fs` | `PdfEditor.tsx` (`readFile`,`writeFile`), `pdfSaveService.ts` | **B** | Fetch bytes from `GET /files/:id/content`; save = upload/`download`. |
| `@tauri-apps/cli` (devDep), `"tauri"` script, `tauri:build:release` script | `package.json` | **A** | Remove after cutover. |
| `vi.mock("@tauri-apps/…")` | `src/test/setup.ts` | **B** | Replace with API-client mock. |

Tauri call sites in `src/` (from grep): `App.tsx` (listen drop, `get_file_info`, `get_startup_files`, `trim_video`), `ConversionPanel` (`get_hardware_encoders`, dialog), `FileCard` (`get_file_size_estimate`, `open_file_location`), `FileDropZone`, `FileList`, `Header` (window), `ImagePreviewModal` (`get_image_preview`), `PdfEditor` (+`authorize_fs_path`, fs, save dialog), `pdfSaveService` (4 commands + fs), `SettingsModal` (`register/unregister_context_menu`, dialog), `speech/SpeechDictationPage` (invoke, listen, window `setFocus`, dialog `open/ask`), `SystemStatusModal` (`system_status`, `speech_get_status`), `VideoTrimmer` (`get_video_duration`, `get_video_thumbnail`, `convertFileSrc`), `useKeyboardShortcuts`, `useSpeechJob`, `useStore` (`get_image_preview`, `get_file_info`-like, `get_video_metadata`, `get_video_thumbnail`, `check_tools`, `get_capabilities`, `detect_gpu`, `convert_file`, `cancel_conversion`, `get_default_output_dir`, `listen("conversion-progress")`).

### 2.2 Frontend API classification
| API | Class | Replacement |
|---|---|---|
| `invoke("convert_file")` | B | `POST /api/v1/jobs` + SSE |
| `invoke("cancel_conversion")` | B | `POST /jobs/:id/cancel` |
| `invoke("get_file_info")` | B | response of `POST /files` (server-side probe) |
| `invoke("get_image_preview")` | B | `GET /files/:id/preview?max=` (server resize) — or client-side `createObjectURL` for small local previews |
| `invoke("get_video_thumbnail" / "get_video_duration" / "get_video_metadata")` | B | included in upload probe result; thumbnail via `GET /files/:id/thumbnail` |
| `invoke("get_file_size_estimate")` | B/C | `POST /api/v1/estimate` or drop |
| `invoke("get_default_output_dir")`, `outputDirectory` setting | D | Remove; output is a server-side result downloaded by the browser |
| `invoke("open_file_location")` | D | Replace by "Download" |
| `invoke("get_startup_files")` | D | Remove (file association launch) |
| `invoke("register_context_menu")`/`unregister…` | D / A (Rust) | Remove UI + `winreg` + commands |
| `invoke("authorize_fs_path")` + `security/fs_scope.rs` | A | Concept disappears |
| `invoke("check_tools")` / `detect_gpu` / `get_hardware_encoders` | B/D | `GET /capabilities` (admin-level detail only; GPU controls hidden for web — see §13) |
| `invoke("system_status")` | B | `GET /api/v1/privacy-status` (values must be re-derived; some claims change — §12) |
| `invoke("speech_*")` | B | `/api/v1/speech/*` or generic jobs of kind `speech` |
| `invoke("get_pdf_form_fields" … "edit_pdf_text_lopdf")` | B | `POST /jobs` kinds `pdf_text_edit`, `pdf_form_fill`; or `GET /files/:id/pdf/text-blocks` |
| `invoke("trim_video")` | B | job kind `video_trim` |
| `listen("conversion-progress")`, `listen("speech-job-update")` | B | SSE |
| `listen("tauri://drag-*")` | B | DOM `dragenter/dragover/drop` + `DataTransfer.files` |
| `getCurrentWindow()` (min/max/close/setFocus/drag region) | D | Delete window controls; keep a plain header |
| `convertFileSrc` | B | `<video src="/api/v1/files/:id/stream">` with Range support, or `URL.createObjectURL(file)` |

### 2.3 Rust
| Item | Where | Class | Notes |
|---|---|---|---|
| `tauri` 2 (+`protocol-asset`, `custom-protocol`) | `Cargo.toml` | **A** | Server crate drops it. |
| `tauri-build` (build-dep), `build.rs` | `src-tauri/build.rs` | **A** | |
| `tauri-plugin-dialog`, `tauri-plugin-fs` | `Cargo.toml`, `lib.rs` | **A** | |
| `#[tauri::command]` wrappers (~50) | `commands.rs`, `speech/commands.rs`, `capabilities.rs`, `engines/office_manifest.rs::office_engine_status` | **B** | Become axum/HTTP handlers calling the same inner functions. |
| `AppHandle`/`APP_HANDLE`/`emit` | `converter.rs` (`set_app_handle`, `emit_progress`), `speech/commands.rs` | **B/C** | Replace with a `ProgressSink` trait/channel owned by the job. |
| `tauri::State<JobRegistry>` | `speech/commands.rs` | **C** | Becomes shared server state; registry must support N jobs, per-user. |
| `tauri::async_runtime::spawn_blocking` | `speech/commands.rs` | **C** | `tokio::task::spawn_blocking` (identical semantics). |
| `app.path().app_data_dir()` | `lib.rs::setup` | **B** | Server data root from config/env. |
| `security/fs_scope.rs` (`FsExt`, `asset_protocol_scope`) | | **A** | |
| `src-tauri/capabilities/default.json`, `gen/schemas/*`, permissions (`core:window:*`, `dialog:*`, `fs:allow-read-file/write-file`) | | **A** | |
| `tauri.conf.json` (window, CSP, assetProtocol, bundle, resources) | | **A**, but its **CSP intent** must be re-expressed as HTTP headers (§12). |
| `bundle.resources` `engines/{office,speech,ffmpeg}/**/*` | | **B** | Engines move to a server install prefix. |
| `bundle.windows.*`, icons, `createUpdaterArtifacts:false` | | **A/D** | |
| Updater | `lib.rs` comment: plugin removed; `createUpdaterArtifacts:false` | **D** | Nothing to remove except the config key; server updates are an ops matter (§10). |
| `engines/resolver.rs::bundled_root()` = `current_exe().parent()/engines` | | **C** | Must become configured install prefix (`ENGINES_ROOT`), still compile-time-fixed or admin-config-only, never request-influenced (keep that invariant). |
| `engines/resolver.rs` test-only `CARGO_MANIFEST_DIR` branch | | **C** | Keep for tests. |
| `winreg`, `register_context_menu` | `Cargo.toml`, `commands.rs` | **A** | |
| `open` crate, `open_file_location`/`open_folder` | | **A** | Server must never "open" anything. |
| `dirs` crate, `get_default_output_dir` | | **A/D** | |
| `std::env::args()` startup files | | **A** | |
| `CREATE_NO_WINDOW`, `BELOW_NORMAL_PRIORITY_CLASS` | `process.rs`, `converter.rs`, `commands.rs` | **C** | Windows-only; Linux equivalent is `nice`/cgroups/`setpriority`. |
| `ascii_link` (junction/hard-link/copy) | `engines/ascii_link.rs` | **C** | Exists because Windows ANSI code page breaks non-ASCII paths. Largely unnecessary on Linux/UTF-8 but harmless: server-generated names are ASCII UUIDs anyway. |
| Free-disk query | `speech/limits.rs::free_disk_bytes` (`#[cfg(windows)]`; `None` elsewhere → check skipped) | **C** | Must be implemented for Linux (`statvfs`) — otherwise the disk guard silently disappears on a Linux server. |
| `lazy_static` globals `RUNNING_PROCESSES`, `CANCELLED_JOBS`, `TOOL_PATHS` | `converter.rs`, `tools.rs` | **B** | Per-job state in a job manager. |

---

## 3. TARGET WEB ARCHITECTURE

```
Browser (React SPA, Turkish UI, light/dark)
   │  HTTPS (TLS terminated at reverse proxy or the server)
   ▼
Reverse proxy (optional but recommended: TLS, auth, body-size, rate limit, headers)
   │  HTTP/1.1 (+SSE)
   ▼
Rust API server  ── auth middleware (pluggable) ── request validation ── routes
   │
   ├── Upload service  (streaming to per-user quarantine area)
   ├── Job manager     (queue, state machine, per-user + global limits, events)
   │       └── Worker pool (blocking threads / child processes)
   │               └── Engine adapters (native image · LibreOffice · FFmpeg · whisper.cpp · lopdf/zip · [gs/pandoc/7z/magick: see §7])
   ├── Workspace store (disk, per-user, per-job dirs, TTL janitor)
   └── Capability service (cached self-checks)
```

### 3.1 Framework evaluation (no premature choice)
| Option | Fit | Pros | Cons for this project |
|---|---|---|---|
| **axum** (tokio/hyper/tower) | Strong | Native SSE (`axum::response::sse`), multipart + streaming bodies, tower middleware (limits, timeouts, tracing, auth layers), very large ecosystem, same async runtime already in use (`tokio` full is already a dependency) | Needs deliberate handling of blocking engine calls (`spawn_blocking`). |
| actix-web | Good | Mature, fast, built-in multipart/SSE via crates | Different runtime ergonomics (actor heritage); less alignment with existing tokio usage. |
| rocket | Adequate | Ergonomic | Smaller middleware ecosystem for streaming/limits; async maturity historically lagging. |
| poem / salvo | Possible | Light, SSE built-in | Smaller communities → long-term maintenance risk in an institutional setting. |
| warp | Possible | Composable filters | Effectively in maintenance mode relative to axum. |

**Assessment:** axum is the best fit *technically* (tokio already present, tower layers map 1:1 onto the controls in §6 and §12, SSE built in). It is a recommendation, not a decision: the genuine selection criteria are institutional (is the framework approved/vetted for MEB use; supply-chain policy for crates) and are listed in §19. The core design below is framework-neutral.

### 3.2 Layers
- **API layer:** thin handlers; translate HTTP ↔ internal `JobRequest`; no engine logic. Versioned `/api/v1`.
- **Request validation:** JSON schema/typed `serde` structs with `deny_unknown_fields`; **whitelist** option fields (today `ConversionOptions` accepts `customFfmpegParams`-adjacent settings from the client; see §12 — free-form ffmpeg params must not be accepted); enforce numeric ranges; output format must be in a server-side matrix keyed by input kind (today `tools::get_supported_output_formats` is a good basis).
- **Authentication boundary:** one middleware that yields `Principal{user_id, org_id, roles}` or rejects. Handlers never read credentials. Detail in §9.
- **Authorization:** every resource (`file`, `job`, `workspace`) carries `owner_id`; every handler checks `resource.owner == principal.user_id` (or admin role) *after* resolving an ID, and returns **404** (not 403) for foreign IDs to avoid existence oracles.
- **IDs:** `file_id`, `job_id` = UUIDv4 from a CSPRNG (`uuid` v4 already used), generated **server-side only**. Today the client chooses `job_id` (it passes the file's UI id) — that must be reversed.
- **Workspaces:** `<DATA_ROOT>/users/<user_hash>/<workspace_uuid>/{in,work,out}/`. One workspace per job (cheap deletion, no cross-job leakage); an uploaded file stays under `users/<hash>/uploads/<file_uuid>/` until it is consumed or expires, then is hard-linked/copied into the job's `in/`.
- **Job lifecycle (state machine):** `uploaded → queued → preparing → running → (finalizing) → completed | failed | cancelled | expired`. Terminal states are immutable. Retry creates a **new job** referencing the same `file_id` (or the same options) — never mutates the old one — which also matches how the UI behaves (`retryFile`).
- **Worker concurrency:** a global semaphore per resource class (see §7): `cpu_heavy` (Office, video, whisper) with small fixed capacity; `light` (native image) higher. Plus per-user limits (e.g. max N queued + M running). Queue is bounded; overflow → `429`/`503` with `Retry-After`. Whisper capacity is explicitly 1–2 (current code: 1, `DEFAULT_LIMITS.max_concurrent_jobs`; and it uses `available_parallelism()-1` threads clamped 1..8 — **two concurrent jobs would oversubscribe**, so the scheduler must budget threads, not only job count).
- **Cancellation:** every job owns a `CancelToken` (`Arc<AtomicBool>` — the pattern already used by `speech`). All engine adapters must accept it. Cancel kills the **process group/tree** (Linux: `setsid` + `kill(-pgid)`; Windows: Job Object). Current code kills only the direct child.
- **Progress updates:** job-scoped event bus (`tokio::sync::broadcast`/`watch`) → SSE (§5). Adapters report through a `ProgressSink` replacing `APP_HANDLE.emit`.
- **Result download:** only via `GET /jobs/:id/download` after authz; server-generated `Content-Disposition` filename (sanitized original stem + extension from server-side matrix), `X-Content-Type-Options: nosniff`, `Content-Type` from output kind, never from the file.
- **Cleanup:** (a) Drop guards (already exist) on every exit path; (b) TTL janitor (new): delete workspaces/uploads older than configured TTL after last access, and immediately after download if "delete-on-download" policy is on; (c) disk-watermark janitor evicts oldest terminal jobs when free space < threshold.
- **Failure recovery:** at start, reconcile the on-disk workspace tree against an in-memory/embedded job table. Because there is **no persistent conversion history** requirement, the job table can be in-memory; on restart, in-flight jobs are marked `failed(SERVER_RESTARTED)` and their directories removed. **Important:** replace the current "delete everything under `jobs/`" startup sweep with "delete directories not owned by a live job of *this* instance", otherwise multiple instances sharing a temp root destroy each other's work.
- **Startup cleanup:** run janitor once before binding the listener; verify engine manifests (existing `check_dir` functions) and expose results via `/capabilities`; refuse to start (or start degraded, flagged) if `DATA_ROOT` is not on a quota-limited volume (config decision).
- **Shutdown:** stop accepting new jobs; send cancel to all running jobs; wait bounded time; kill process groups; remove workspaces; `SIGTERM`/`Ctrl-C` handler via axum graceful shutdown. Because `panic = "abort"` is set in `Cargo.toml [profile.release]`, **a panic in any handler/worker aborts the whole server** (all users' jobs); in a server this should be reconsidered (§17).

---

## 4. API DESIGN (proposal only — nothing implemented)

Conventions: JSON; errors `{ "error": { "code": "SCREAMING_SNAKE", "message_tr": "...", "request_id": "..." } }` reusing the existing code vocabulary (`EngineError`, `SpeechErrorCode`, image error codes); never include paths/stderr in responses (existing discipline). All endpoints require authentication except `/healthz`. `404` for non-owned IDs.

| Endpoint | Purpose / Request / Response | Validation | Errors |
|---|---|---|---|
| `GET /api/v1/capabilities` | Returns `Capability[]` exactly as `compute_capabilities()` (same ids/states). Response also `limits` (max upload, max duration, formats matrix). Cached (self-checks hash the whisper model — expensive; `speech_get_status` already moved it to `spawn_blocking`). | none | `401` |
| `POST /api/v1/files` | `multipart/form-data` (one file per request, or `application/octet-stream` + `X-File-Name`). Streams to `uploads/<file_id>/original.bin` (server-chosen name). Response `201 {file_id, name_display, size, extension_claimed, category, probe:{duration?, resolution?, codec?, subtitles?, pages?}, expires_at}`. Replaces `get_file_info`, `speech_inspect_file`, `get_video_metadata`. | Size cap enforced while streaming (not after); extension allow-list; **content sniffing** (magic bytes) compared with claimed extension; count/quota per user; probe runs in a sandboxed engine call with timeout. | `401`, `413 UPLOAD_TOO_LARGE`, `415 UNSUPPORTED_TYPE`, `422 CONTENT_MISMATCH`, `429 QUOTA_EXCEEDED`, `507 INSUFFICIENT_STORAGE` |
| `GET /api/v1/files/:id/thumbnail` / `/preview?max=` | Server-rendered thumbnail/preview (replaces `get_image_preview`, `get_video_thumbnail`). Return `image/png|jpeg` bytes, not base64 data URL. | `max` clamped (existing 200/1920 usage) | `404`, `422` |
| `DELETE /api/v1/files/:id` | Remove an uploaded file not yet consumed. `204`. | owner | `404` |
| `POST /api/v1/jobs` | Body `{ kind: "convert"|"video_trim"|"pdf_text_edit"|"pdf_form_fill"|"speech", inputs:[file_id…], output_format?, options:{…whitelisted}, language? }`. Response `202 {job_id, state:"queued", position}`. Replaces `convert_file`, `trim_video`, `speech_start_file_job`, PDF edit commands. | Server-side matrix `(category, ext) → allowed outputs` (from `tools.rs`); capability must be `AVAILABLE` (server enforces what the client only "defense-in-depth" enforces today); typed options with ranges; reject unknown fields; language in manifest `languages`. | `400 INVALID_REQUEST`, `404 FILE_NOT_FOUND`, `409 FILE_ALREADY_CONSUMED`, `422 FEATURE_NOT_IMPLEMENTED/ENGINE_MISSING`, `429 TOO_MANY_JOBS`, `503 QUEUE_FULL` |
| `GET /api/v1/jobs/:id` | Snapshot `{job_id, kind, state, progress_pct|null, eta_secs?, speed?, elapsed_secs, error?, result?:{name, size, content_type}, transcript?}` (polling fallback & reconnect source of truth). | owner | `404` |
| `GET /api/v1/jobs/:id/events` | SSE stream (§5). `event:` types `state`, `progress`, `heartbeat`, `result`, `error`. Supports `Last-Event-ID`. | owner | `404` |
| `POST /api/v1/jobs/:id/cancel` | Idempotent. `202 {state}` ; no effect on terminal jobs (`200` with current state). | owner | `404` |
| `POST /api/v1/jobs/:id/retry` | Creates a new job from the original request while the source file is still retained. `202 {job_id}`. | owner; source not expired; original job in `failed|cancelled` | `404`, `409 NOT_RETRYABLE`, `410 SOURCE_EXPIRED` |
| `GET /api/v1/jobs/:id/download` | Streams the output (supports `Range`). For `speech`: `?format=txt|srt|json` rendered on demand from stored transcript. | owner; state must be `completed` | `404`, `409 NOT_READY`, `410 EXPIRED` |
| `DELETE /api/v1/jobs/:id` | Cancels if running, removes workspace & output immediately. `204`. | owner | `404` |
| `GET /api/v1/privacy-status` | Replacement for `system_status`; values re-derived for server (§12). | | |
| `POST /api/v1/speech/jobs` *(or generic job kind `speech`)* | Body `{file_id | recording_id, language}`. Same semantics. | language ∈ manifest; duration/size limits (`SpeechLimits`) | as above plus `SPEECH_BUSY`, `SPEECH_RESOURCE_LIMIT`, `SPEECH_INPUT_UNSUPPORTED`, `SPEECH_NO_SPEECH_DETECTED`, … (existing codes) |
| `POST /api/v1/speech/recordings` *(Phase 6+)* | Chunked/streamed mic upload (`audio/webm;codecs=opus`, `audio/ogg`, `audio/mp4`) with hard duration/size cap (`max_recording_secs` = 2 h exists, unused). | MIME + sniff + caps | as `/files` |
| `GET /healthz`, `GET /readyz` | Liveness / readiness (engines verified, disk OK). | none | |

Notes: (a) `jobId` generated by the server fixes the current cross-user cancel weakness; the client’s per-file UI id is a separate local concept. (b) Batch UX is preserved client-side (queue of independent jobs, `maxParallelConversions` becomes a *request* the server may throttle). (c) A "zip of results" endpoint is optional and must reuse the archive size limits.

---

## 5. PROGRESS AND CANCELLATION

| | Polling | Server-Sent Events | WebSocket |
|---|---|---|---|
| Latency/fit for progress | Poor at low interval cost; wasteful with many jobs | Native push; text; auto-reconnect with `Last-Event-ID` | Push + bidirectional |
| Complexity | Lowest | Low (axum has `Sse`) | Higher (framing, ping/pong, auth over upgrade, proxies) |
| Proxy/enterprise friendliness | Best | Good (plain HTTP; needs `proxy_buffering off`, long read timeout) | Often blocked/terminated by corporate proxies/WAFs |
| Auth reuse | Same as REST | Same cookies/headers as REST (not custom headers via `EventSource` — token must be a cookie or use `fetch` streaming) | Handshake auth only |
| Need for client→server messages | n/a | None (commands use REST) | Not needed here |
| Browser connection limit | None | HTTP/1.1: 6 per origin — **mitigate with one multiplexed stream** | n/a |

**Recommendation: SSE as the primary channel + `GET /jobs/:id` snapshot as the reconnect/fallback source of truth.** The app is server→client for progress and uses ordinary REST for commands (cancel/retry), which is exactly SSE's sweet spot; it mirrors the current Tauri `emit` model one-to-one; and it survives enterprise proxies better than WebSocket. To respect the 6-connection limit with "multiple simultaneous jobs", provide a **per-user multiplexed stream** `GET /api/v1/events` (events tagged with `job_id`) rather than one stream per job; per-job `/jobs/:id/events` remains for simple clients. Server sends a heartbeat comment every ~15 s; client falls back to polling `GET /jobs/:id` every few seconds if the stream drops (and always re-fetches the snapshot on reconnect, since the existing frontend already tolerates "events arriving before the id is known" — `useSpeechJob` pattern).

Semantic rules to preserve from the desktop app: progress is `null` (indeterminate) unless the engine reports it (don't invent percentages); terminal states are final; cancel is idempotent and results in `cancelled` (not `failed`); cancelled jobs never leave output.

---

## 6. FILE STORAGE / PRIVACY MODEL

### 6.1 Lifecycle
```
upload ──► quarantine ──► validate ──► job workspace ──► engine ──► output validation ──► download ──► cleanup
          (stream,       (size, magic,  (UUID dir,        (no shell, (size cap, type      (authz,      (Drop + TTL +
           byte cap)      probe, bomb    own LO profile,   timeout,    sniff, within-       one-time or   janitor +
                          checks)        links not copies) cancel)     workspace check)     TTL window)   startup reconcile)
```
1. **Upload:** stream to `uploads/<user>/<file_uuid>/original` with a counting writer that aborts at `MAX_UPLOAD_BYTES`; original name stored as *display metadata only* (the codebase already follows this: `InputInfo.file_name` "never used to build a path"; `speech` stages `input.<ext>` under a generated name).
2. **Validation:** extension allow-list (+ per-category sizes), magic-byte check vs. claimed type, structural limits (images: existing 40 MP / 20 000 px; audio: duration ≤ 3 h / size ≤ 500 MB; archives: see below; PDFs: page count/size caps; Office: ZIP-container inspection for OOXML bombs and external links — §7).
3. **Workspace:** one UUID dir per job with `in/`, `work/`, `out/`; created `0700` owned by the service account; engine cwd = workspace (already enforced by `process::build_command_ex`).
4. **Conversion:** adapters write only inside `work/` then `out/`; after finish, `confirm_within(workspace, produced)` (exists) + **size cap on output** (new) + **content sniff** (new).
5. **Download:** streamed from `out/`; response headers prevent sniffing/inline execution: `Content-Disposition: attachment`, `X-Content-Type-Options: nosniff`.
6. **Cleanup:** on terminal state + configurable retention (suggest short TTL; "delete after first successful download" optional), plus janitor.

### 6.2 Threat → control matrix (what must be built)
| Threat | Control |
|---|---|
| Path traversal / arbitrary FS access | No client path ever reaches the FS API. Clients only hold UUIDs. Keep `sanitize_filename_component`, `confirm_within`, canonicalization as defense-in-depth. Remove `validate_output_dir` semantics (no client-chosen output dir). |
| Filename injection | Server-generated names; display name sanitized on output (`Content-Disposition` RFC 6266/5987 encoding; strip CR/LF/quotes; Turkish characters encoded with `filename*`). |
| Cross-user access | `owner_id` on every record; per-user directory; 404 on mismatch; no listing endpoints across users; no predictable IDs (UUIDv4); separate `users/<hash(user_id)>` (don’t embed raw identifiers in paths). |
| Symlink attacks | Create files with `O_NOFOLLOW`/`create_new`; never follow symlinks inside user-controlled content; **ZIP/7z extraction must reject symlink entries** (`enclosed_name()` blocks `..`/absolute but the current extractor writes whatever `by_index` yields; 7-Zip `x` can create symlinks — currently unrestricted on Linux). Workspaces owned by service account with no shared writable parent. |
| Stale temp files | Drop guards + TTL janitor + startup reconcile; **fix archive temp path** (`localconvert_archive_<millis>`) to use the same workspace system. |
| Unbounded disk | Per-user quota (bytes + file count), global high-watermark, `MAX_UPLOAD_BYTES`, `MAX_OUTPUT_BYTES`, free-space precheck (implement `free_disk_bytes` for Linux; existing speech logic is a good template: `required_disk_bytes`). Ideally `DATA_ROOT` on its own filesystem/quota. |
| Unbounded upload size | Proxy limit + server streaming cap + `Content-Length` pre-check; reject early. |
| Malicious archives / decompression bombs | Cap entries, total uncompressed bytes, ratio, nesting depth, path length; reject symlinks/devices; extract in workspace only; timeout. **Current `extract_zip` has no size/count/ratio limit** and `create_zip` reads each file fully into memory (`read_to_end`). 7-Zip path has none either. |
| Malicious PDFs | Ghostscript with `-dSAFER` (present) and also `-dNOPAUSE -dBATCH`; add `-dPARANOIDSAFER` where possible, resolution/page caps (currently `-r300` on arbitrary page counts), timeout; lopdf parsing limits; never execute embedded JS/launch actions. |
| Hostile Office documents | LibreOffice headless with a fresh profile per job (already), **no macro execution** (profile `MacroSecurityLevel` = very high / disable), no external link/update-on-load, run as unprivileged user, network-egress denied (OS firewall/namespace), memory/CPU/time limits, kill process *tree*, cap input size and ZIP-container expansion. |
| Media parser abuse (ffmpeg/ffprobe) | Audio prep already uses `-protocol_whitelist file`. **Video path (legacy) does not** → must add `-protocol_whitelist file,pipe` (or `file`), `-nostdin`, resource limits, reject playlists/`concat` demuxer/HLS/`subfile` inputs (SSRF/LFI via crafted `.m3u8`/`.sdp`). Use the minimal build for everything. |
| Resource exhaustion | Semaphores, per-user job caps, wall-clock timeout per engine (add for FFmpeg/gs/pandoc/7z; none today), memory ceiling via cgroups/`ulimit -v`, thread budget for whisper. |
| Information leakage | Do not return raw stderr/paths (legacy converters do today). Redact in logs. |

### 6.3 Quotas / limits to define (values are decisions, §19)
Max upload size per file and per category; max files per request; per-user concurrent jobs; global concurrency per engine class; per-user storage quota; retention TTL; max output size; max audio duration (currently 3 h) and recording duration (currently 2 h); max page counts; ZIP limits.

---

## 7. ENGINE MIGRATION AUDIT

### A. Native Rust image engine (`native/image/*`)
| Aspect | Finding |
|---|---|
| Invocation | In-process: `native::image::convert_file(input, output, target, &NativeConvertOptions{width,height,quality,png_compression_level})`. Formats JPEG/PNG/WebP/BMP/GIF/TIFF (`image` features). |
| Discovery | N/A. |
| I/O | Path in → path out (inside job dir). |
| Security | `decode.rs` checks dimensions before decoding (`MAX_DIMENSION 20 000`, `MAX_DECODED_PIXELS 40 M`), rejects multi-page TIFF (`tiff` IFD check), metadata stripped by default (`preserve_metadata` default false). Structured `ImageNativeError`. |
| Timeout / cancel | **None** (in-process, uninterruptible once started). |
| Cleanup | Job dir Drop. |
| Web changes | Keep as-is; run in `spawn_blocking`; add cooperative cancel checks between decode/transform/encode; add an output-size cap. WebP encoding quality: `image` 0.25 WebP encoder is lossless-only, so "WebP/AVIF quality" for the future Imagify feature needs `libwebp`/`ravif`-class crates (§16). |
| Risks | CPU/memory: a 40 MP RGBA decode ≈ 160 MB per concurrent job → budget concurrency by memory (e.g. 4 concurrent ≈ 640 MB + working copies for resize/rotate). Panics in decoders (`panic = "abort"` kills the server) → wrap in `catch_unwind` or isolate in a subprocess. |
| Concurrency | Safe (pure functions); bound by semaphore. |

### B. LibreOffice (`engines/office*.rs`, `reconstruction.rs`)
| Aspect | Finding |
|---|---|
| Invocation | `soffice --headless [--infilter=writer_pdf_import] -env:UserInstallation=<job>/loffice_profile --convert-to <filter> --outdir <dir> <input>` via `process::run_with_timeout`. |
| Discovery | `resolver::resolve(EngineId::Office)`: bundled `<exe dir>/engines/office/LibreOffice/program/soffice[.exe]`; system fallback only if env `LOCALCONVERT_ALLOW_SYSTEM_OFFICE_FALLBACK=1`. `office_manifest::self_check` validates manifest/version (pinned **25.8.7**)/architecture (**x86_64**)/required dirs. |
| Security checks | No shell, env deny-list (`ENV_DENYLIST`), cwd = job dir, per-job profile (avoids lock-file races), input canonicalization. **No macro policy configuration, no network block, no memory limit, no process-tree kill.** |
| Timeout | 600 s (`CONVERT_TIMEOUT`). |
| Cancellation | **Absent** (see §0.4). |
| Cleanup | Job dir Drop (profile lives inside it — good: every conversion cold-starts a ~200–500 MB profile-less LO; slow). |
| Web changes | (1) add cancel token + tree kill; (2) harden profile (disable macros, no update checks, no external links, no Java, disable crash-report/online features) via `registrymodifications.xcu` seeded per job; (3) run under an unprivileged account with no network; (4) re-acquire the Linux build (the `.msi` admin-image method is Windows-only; Linux uses distro `libreoffice-core` packages or TDF `.deb`/`.rpm`) and rewrite the manifest/self-check for it; (5) consider a **warm pool** (`soffice --accept` + UNO) or `unoserver` to avoid cold start — this trades isolation for latency and is a design decision; (6) drain stdout/stderr. |
| Resource risk | Each soffice: 300 MB–2+ GB RAM for large spreadsheets/presentations; CPU spikes; can hang on malformed files (timeout is the only guard). |
| Concurrency | Per-job profile fixes the lock-file race, but concurrency of 2–4 is a realistic ceiling per host; start-up cost multiplies. Needs memory-based admission control. |

### C. FFmpeg / ffprobe
| Aspect | Finding |
|---|---|
| Invocation | Two systems. Speech prep: hardened (`audio_prep`, minimal build, `-protocol_whitelist file`, `-nostdin`, cancellable, timeouts). Conversion/trim/thumbnail/duration: legacy (`converter.rs`, `commands.rs` using `hidden_command`/`Command::new`). |
| Discovery | Speech: bundled `engines/ffmpeg` (pinned **9.0.2**, `REQUIRED_LICENSE_MODE = "LGPL-2.1-or-later"`, validated by `ffmpeg_manifest::check_dir`). Legacy: `tools::get_tool_path` (PATH + common paths, cached in `TOOL_PATHS`). Capability `video_conversion` etc. checks `EngineId::Ffmpeg` (system), **not** the bundled minimal build — and the bundled build is audio-only, so video would still need a full FFmpeg (licensing/codec decisions open: H.264/AAC encoders are GPL/non-free-adjacent depending on build). |
| Security | Legacy: none beyond arg arrays. `customFfmpegParams` exists in `Settings` (free-form user ffmpeg params) — must never reach a server. GPU/hardware encoders (`detect_gpu`, `useGpu`, `gpuEncoder`) are client-selected today. |
| Timeout | None for video/audio conversion (only audio-prep has). |
| Cancellation | Works (flag + kill) for ffmpeg path; direct child only. |
| Cleanup | Job dir Drop. |
| Web changes | Single hardened runner for all FFmpeg calls; wall-clock timeout proportional to duration; `-nostdin`, protocol whitelist, `-max_alloc`/`-threads` budget, `-t`/`-fs` output caps; reject container types that reference external resources; ignore client GPU choice (server policy decides; no GPU on most servers); progress parsing reused as `ProgressSink`. |
| Resource risk | Video transcoding is the heaviest CPU consumer; unbounded input resolution/duration → CPU & disk exhaustion; output can be orders of magnitude larger than input. |
| Concurrency | Existing UI forces videos sequential (`videoFiles` loop in `convertFiles`) because "CPU/GPU intensive" — server must enforce this, not the client. |

### D. whisper.cpp (`engines/speech.rs`, `speech/*`)
| Aspect | Finding |
|---|---|
| Invocation | `whisper-cli` args shown in §1.7; JSON output `transcript.json` parsed by `parse_whisper_json`; `-ng` forces CPU; model SHA-256 verified via manifest (`speech_manifest`, `SCHEMA_VERSION 2`); model/engine run from ASCII-exposed links in the job dir (`ascii_link::expose_model`). |
| Discovery | Bundled `engines/speech/bin` + `engines/speech/models`; test-only repo-root override; no runtime override (documented: "no runtime override, env var or setting"). |
| Security | Extension allow-list (`wav mp3 m4a aac flac ogg`), 500 MB cap, 3 h cap, free-disk check, reject UNC/URL paths, authoritative duration from decoded WAV, silence gate before whisper (prevents hallucination), `ENV_DENYLIST` includes `GGML_BACKEND_PATH`, cwd = job dir, low priority. |
| Timeout | `None` (bounded by the duration limit and user cancel). On a server also add a wall-clock cap proportional to audio length. |
| Cancellation | Good (`AtomicBool`, 50 ms poll, kill+reap). Tree kill advisable. |
| Cleanup | `SpeechJobDir` Drop; startup sweep (global delete). |
| Web changes | Replace single-slot `JobRegistry` with scheduler slots (thread budgeting); remove `AppHandle`; keep `pipeline::run_file_job` unchanged (it is Tauri-free by design, per its module doc); replace path input with workspace-staged file; implement Linux `free_disk_bytes`; low priority via `nice`; build/acquire Linux `whisper-cli` + model (Windows binaries are what is bundled). |
| Resource risk | CPU-bound: `threads = available_parallelism-1 (1..8)` per job; model RAM; a 3 h file takes a long time (real-time factor not recorded in the code; see `docs/SPEECH_ENGINE.md` and `scripts/bench/` for measurements — not re-read in this audit). |
| Concurrency | Must be 1–2 concurrent per host unless cores are partitioned; queue with visible position. |

### E. PDF-related
| Capability | Implementation | Server notes |
|---|---|---|
| PDF → images | Ghostscript (`png16m`/`jpeg`, `-r300`) via legacy runner; **single output file path** though gs may produce one page (`-sOutputFile=<single>` will overwrite per page unless `%d` is used — not verified at runtime) | Needs page cap, DPI cap, timeout, `-dSAFER`+`-dPARANOIDSAFER`; ensure `%d` pattern + zip of pages. |
| PDF → DOCX/ODT/RTF/HTML/TXT/EPUB | LibreOffice `writer_pdf_import` (§14) | See §14. |
| PDF → XLSX/PPTX | `FEATURE_NOT_IMPLEMENTED` | Keep disabled. |
| PDF text editing | `pdf_text_editor.rs` (lopdf), commands `get_pdf_text_blocks`, `edit_pdf_text_lopdf` | In-process parsing of untrusted PDFs: needs size/page limits and panic isolation. Frontend `PdfEditor` uses `pdfjs-dist`, `pdf-lib`, `fabric` client-side (works in browsers unchanged). |
| PDF merge/split/compress/rotate/watermark | Unregistered commands (Ghostscript/lopdf) | Not user-reachable now; classify DEFER. |
| Form fields | `get_pdf_form_fields`/`fill_pdf_form_fields` | Re-route as jobs or sync endpoints with limits. |
| OCR | `ocr_pdf` unregistered; capability `ocr = NOT_IMPLEMENTED`; `EngineId::Tesseract` exists | DEFER. |
| Script | `scripts/pdf_text_edit.py` exists (python); `tools.rs` probes python on PATH | Python dependency on server must be avoided/justified; not on the registered path (verify, §19). |

### F. Archive / compression
Native ZIP (`native/archive_zip.rs`) for zip→zip; 7-Zip for all else (`7z x`, `7z a`, `-y`). Temp dir not a `JobTempDir`; no limits; no timeout; `-y` overwrite-all; 7-Zip `x` honors symlinks and absolute-ish paths per its own policy. **Highest risk engine on a server** (§6.2). Web changes: unify onto workspace; limits (entries, ratio, total bytes, depth); reject symlinks/special files; no password-protected archives (hang/prompt risk — `-y` doesn’t answer password prompts; stdin must be closed); use `-p` empty? (decision); timeout; streaming zip creation (no `read_to_end`); consider replacing 7z with pure-Rust crates (`tar`, `flate2`, `sevenz-rust`) as the code comment itself proposes.

### G. Other executables / libraries
| Tool | Where | Notes |
|---|---|---|
| ImageMagick (`magick`) | AVIF/HEIC/PSD/SVG fallback, previews | **ImageMagick has a long CVE history and a policy.xml; it must run with a locked-down policy (disable `MVG`, `MSL`, `URL`, `HTTPS`, `EPHEMERAL`, `@*` path reads, PS/PDF coders) or be replaced** (SVG → `resvg`, AVIF → `ravif`/`libavif`, HEIC → `libheif`). Env deny-list includes `MAGICK_CONFIGURE_PATH`. |
| Ghostscript (`gs`) | PDF→image, EPS/PDF | `-dSAFER` present. Ghostscript has had repeated sandbox-escape CVEs; keep current, no network, tight limits. |
| Pandoc | md/html/txt/rst/epub | Run with `--sandbox` (disables file/network access from filters/includes); the current call lacks it. Raw HTML input can reference local files/URLs. |
| 7-Zip | archives | See F. |
| Python | PDF script probe | Avoid. |
| Tesseract | `EngineId` exists, capability NOT_IMPLEMENTED | Future. |
| fontforge | not used; `convert_font` is `fs::copy` with extension change (**this produces mislabeled output, not a conversion**) | Classify REMOVE or mark NOT_IMPLEMENTED. |

---

## 8. SES DIKTE WEB MIGRATION

### 8.1 Target flow mapping
```
Browser mic (getUserMedia) ─► MediaRecorder (webm/opus | ogg/opus | mp4/aac) ─► HTTPS upload (chunked)
      ─► server stores in workspace ─► ffprobe/ffmpeg → 16 kHz mono PCM16 WAV (audio_prep::normalize)
      ─► energy/silence gate ─► whisper-cli ─► transcript JSON ─► SSE/poll ─► browser
```

### 8.2 What exists vs. what is new
| Piece | Status | Reuse |
|---|---|---|
| `speech/pipeline.rs::run_file_job_in` (validate→probe→normalize→silence gate→transcribe) | Complete, Tauri-free, unit/integration tested (`speech/tests.rs`, 965 lines) | **Reuse as-is** (replace `validate_source` path input with a staged workspace file; keep extension allow-list as defense-in-depth *plus* add content sniffing). |
| `audio_prep.rs`, `audio_wav.rs` | Complete (probe, normalize with `-t` cap, WAV header parse, energy analysis) | Reuse; **extend `ACCEPTED_EXTENSIONS` / sniff list for browser recording containers** (`webm`, `ogg` is present, `mp4`/`m4a` present, `opus`, `weba`). `ACCEPTED_EXTENSIONS = ["wav","mp3","m4a","aac","flac","ogg"]` currently lacks `webm`/`opus`; **the minimal bundled ffmpeg may not include a Matroska/WebM demuxer or Opus decoder** (the build script `scripts/ffmpeg/*` defines what is compiled in; not verified here) → check before promising Chrome/Firefox recordings. |
| `engines/speech.rs`, manifest, error types | Complete | Reuse; Linux binaries/model needed. |
| `speech/limits.rs` | Complete incl. recording limit (unused) | Reuse; `max_recording_secs` finally gets used. `max_concurrent_jobs = 1` global → per-host capacity setting. |
| `speech/job.rs::JobRegistry` | Single-slot | **Replace** with scheduler. |
| `speech/commands.rs` | Tauri glue + 1 Hz ticker thread | **Rewrite** as handler + `ProgressSink`; keep `TranscriptDto`, `JobUpdate`, `LimitsDto` shapes. |
| `useSpeechJob.ts` | Event-driven hook with careful race handling (events before id returned; discard; unmount cancel) | **Mostly reusable**: swap `invoke`/`listen` for the API client + SSE; keep the state machine and race handling. |
| `SpeechDictationPage.tsx` | File picker via dialog, drag via Tauri events, window focus, `ask()` confirm | **Adapt**: `<input type=file accept=…>`, DOM drag/drop, in-app confirm, remove `setFocus`. Tests (`SpeechDictationPage.test.tsx`, 51 tests) mock Tauri; will need a client mock. |
| `utils/speech.ts`, `types/speech.ts` | Types/helpers | Reuse (rename event constant). |
| `speechNoNetwork.test.ts` | Asserts no network use in speech frontend code | **Must be rewritten**: the web client *must* make same-origin HTTP calls; the invariant becomes "no cross-origin requests / no third-party hosts". |
| Microphone capture | **Not implemented** (mic tab disabled; i18n keys `sourceMic` exist) | **New**: `getUserMedia` + `MediaRecorder`, permission UX, secure-context requirement, recording UI (timer, level, stop), upload, 2 h cap, discard. |

### 8.3 Specific issues
- **Secure context:** `getUserMedia` works only on HTTPS (or `localhost`). Internal deployments with plain HTTP or self-signed/unsigned certs will have mic disabled. Requires proper TLS/internal CA.
- **Permissions-Policy** header must allow `microphone=(self)` (and the Tauri CSP `media-src … asset:` goes away).
- **Formats:** Safari records `audio/mp4`, Chrome/Firefox `audio/webm;codecs=opus` or `audio/ogg`. Server-side normalization (ffmpeg) is the right place to unify; **client-side WAV encoding** (AudioWorklet → 16 kHz PCM) is an alternative that sidesteps ffmpeg-demuxer support, at the cost of bigger uploads and more client code.
- **Long recordings:** upload in chunks (or a single `fetch` with a `ReadableStream` on supporting browsers) and finalize; handle network loss (resume or fail clearly). 2 h of webm/opus is small (tens of MB) — much smaller than the 230 MB PCM figure in `limits.rs`.
- **Live (streaming) dictation is out of scope:** whisper-cli is a batch tool (reads whole WAV, JSON at the end). The current UX is "record → transcribe". Streaming would need `whisper-server`/`stream` or chunked re-invocation; not an incremental change.
- **Timestamps:** segment-level (`start_ms`, `end_ms`) from `-oj`; `timestamps_available` flag already drives UI visibility. Reusable. (Word-level not available.)
- **Cancellation/retry/temp files:** as §1.12/§1.13; retry needs the staged input retained until job TTL (today the user's original file is untouched, so retry is free; on a server, retention is a policy decision — privacy vs. UX).
- **CPU:** one job ≈ `threads` cores for the duration; a host with 8 cores supports ~1 concurrent job at the current `-t` choice; multi-user dictation needs a queue and honest "sıradasınız: N" UX.
- **Transcript delivery:** transcripts are sensitive text; deliver only to the owner; do not log content; delete at TTL; export `txt/srt/json` generated on demand.
- **Turkish:** engine languages come from the manifest (`supports_language`); all user-visible errors already have Turkish text in `speech_error.rs`.

---

## 9. AUTHENTICATION / MEB ENVIRONMENT

**Evidence check:** the repository contains **no** authentication code, no SSO/LDAP/OIDC configuration, no user model and no mention of a specific MEB identity provider. Nothing below assumes any MEB infrastructure exists.

### 9.1 Pluggable boundary
```
Request ─► [AuthLayer: Authenticator trait] ─► Principal { subject, display_name?, org_unit?, roles[], auth_source } ─► handlers
```
Possible authenticators (all behind one trait/config switch, none implemented now):
| Mode | How it plugs in | Considerations |
|---|---|---|
| **Reverse-proxy authentication** (e.g. proxy injects `X-Remote-User` after SSO) | Server trusts identity headers *only* from configured proxy source (mTLS or loopback/UNIX socket + allow-listed CIDR) | Simplest integration with whatever the institution already runs; **header spoofing is the main risk** — the server must be unreachable except via the proxy and must strip inbound copies of those headers. |
| **OIDC** (authorization-code + PKCE) | Server (or BFF) performs login, issues a session cookie (`HttpOnly; Secure; SameSite=Lax/Strict`) | Needs IdP discovery; session store; logout; token refresh. Cookie-based sessions also make SSE work without custom headers. |
| **SAML 2.0** | Usually via proxy/IdP gateway rather than in the Rust app | Mature in public-sector estates; Rust SAML libraries are less mature than OIDC ones → prefer terminating at the proxy. |
| **LDAP/AD bind** | Server verifies credentials against directory | Server sees passwords (undesirable); needs TLS to directory; account lockout/rate limiting. Prefer Kerberos/SPNEGO or proxy-based if AD is used. |
| **Local accounts / API key** | For pilots and for service integrations | Not for production at scale. |

### 9.2 Model
- **Identity → ownership:** `owner_id` = stable, opaque subject identifier (not email) on every file/job; used to derive the per-user storage directory by hash.
- **Authorization:** roles at minimum `user`, `admin` (capabilities/diagnostics, quota override, force-delete); optional per-feature entitlements (e.g. speech enabled only for certain org units) — the existing `DISABLED_BY_POLICY` capability state is a natural carrier.
- **Tenant/organization boundary:** if multiple units (e.g. directorates, schools) share one deployment, add `org_id` to the principal, namespaces, quotas and audit; isolation is logical (same host) unless separate instances are deployed — a decision (§19).
- **CSRF:** with cookie sessions, state-changing endpoints need CSRF protection (SameSite + `Origin` check or token); with bearer headers it doesn't apply.
- **Audit logging (privacy-sensitive):** log *metadata only* — timestamp, principal id, action, job kind, size class, outcome code, duration, request id. **Never** log file names, contents, transcripts, or full IPs unless policy requires (KVKK considerations — legal review needed; the repo already says "legal review required" for third-party notices, indicating institutional compliance review is expected). Log retention and access must be defined; logs can themselves become the "persistent history" the requirements forbid.
- **Privacy implications:** the privacy story shifts from "nothing leaves your computer" to "nothing leaves the organization's server". `system_status`/`PrivacyStatus` claims and UI texts (`SystemStatusModal`, help text, README, `locales/*.ts`) must be rewritten honestly: files *are* uploaded to a server (under organization control), admins can in principle read them, and TLS is required in transit. At-rest encryption of `DATA_ROOT` is a deployment option.

---

## 10. DEPLOYMENT OPTIONS (no decision)

| Criterion | A. Windows Server | B. Linux server | C. Docker/container | D. Hybrid/reverse-proxy front |
|---|---|---|---|---|
| Operational complexity | Medium; matches the current dev/build environment (all scripts are PowerShell, `.msi`) | Medium; best tooling for services (systemd, cgroups) | Medium-high initially; best reproducibility | Adds a component; standard in institutions |
| LibreOffice | **Works today** (pinned 25.8.7 Win x64 build; `soffice.exe` flow validated) | Needs new acquisition/manifest/self-check; well-supported; fonts (incl. Turkish metric-compatible fonts like Liberation/Carlito) must be installed deliberately | Same as Linux; fonts & profile dirs baked into image; good isolation | Neutral |
| FFmpeg | Works (BtbN-style or own build; scripts exist) | Native, easiest; package managers/own build | Same | Neutral |
| whisper.cpp | Windows build/model bundled today (`engines/speech`) | Build from pinned source; AVX/AVX2 detection already modelled (`SPEECH_CPU_UNSUPPORTED`) | Same; CPU flags depend on host | Neutral |
| Rust server | Fine (tokio/hyper on Windows) | Best-supported | Fine (static musl is awkward with C deps → use glibc image) | Neutral |
| File permissions | NTFS ACLs; weaker per-process sandbox primitives | POSIX modes, users/groups, `umask`, mount options (`noexec,nosuid,nodev` on `DATA_ROOT`) | Mounts + non-root user + read-only rootfs | Neutral |
| Process isolation | Job Objects (kill tree, memory/CPU limits), low-box/AppContainer is complex | namespaces/cgroups/seccomp, `bubblewrap`, `systemd-run --scope` with `MemoryMax`, `PrivateNetwork=yes` | Cgroups + `--network none` for engine containers; per-job ephemeral containers possible | n/a |
| Updates | Windows Update/maintenance windows; manual engine swaps | Packages/CI image rebuilds | Image rebuild & rollout (best for pinned versions) | Proxy updated independently |
| Monitoring | Event Log/Perf counters | journald/Prometheus node exporter | Container metrics/logs | Proxy metrics |
| Notes | All current scripts and bundled binaries are Windows-specific; **lowest port effort** | Highest long-term fit for sandboxing | Eases engine pinning, hardening, and reproducible builds | Needed in any case for TLS/SSO unless the app handles it |

These are not mutually exclusive (e.g. Linux + container behind a reverse proxy). Selecting one requires MEB/IT constraints that are not in the repository (§19).

---

## 11. FRONTEND MIGRATION

| Area | Finding | Action |
|---|---|---|
| Nearly unchanged | `Sidebar`, `WelcomeScreen`, `FileCard` (UI parts), `PresetsSelector`, `ImageComparePreview`, `ConversionPanel` (UI), `SettingsModal` (non-desktop sections), `SystemStatusModal` (UI), `speech/*` (UI), `locales/{tr,en,index}.ts`, `index.css`/Tailwind theming, `utils/capabilityGating.ts` (+tests), `types/*`, `PdfEditor/*` (pdfjs-dist, pdf-lib, fabric are browser libraries) | Minimal edits |
| Need API adapter | `useStore.ts` (`convertFiles`, `cancelConversion`, `loadCapabilities`, `checkTools`, previews/thumbnails/metadata), `useSpeechJob.ts`, `SpeechDictationPage` (status/limits/inspect/start), `SystemStatusModal`, `ImagePreviewModal`, `VideoTrimmer`, `pdfSaveService.ts`, `PdfEditor.tsx` | Introduce `src/api/` (client, SSE manager, error mapping) and swap call sites; keep types |
| Must disappear | `Header` window controls + `data-tauri-drag-region`; context-menu section in `SettingsModal`; output-directory picker; "open file location"; `get_startup_files` effect in `App.tsx`; `tauri://` listeners; watch-folders & schedule UI (already unmounted); `ToolsSetupModal` (concept of installing local tools; verify content); hardware-acceleration / GPU / custom FFmpeg params UI | Delete or hide behind capability |
| File input | Replace dialog `open()` with `<input type="file" multiple accept>` + hidden trigger; `File` objects replace `FileInfo.path` (store model changes: `path` → `fileId` after upload; keep `File` handle for retry before upload) | Store refactor |
| Drag & drop | Already has DOM drag handlers for visuals (`FileDropZone.tsx:101`); add `onDrop` using `DataTransfer.files`; folder drops via `webkitGetAsEntry` only if desired | Small |
| Upload UX | New states: `uploading` (progress via `XMLHttpRequest.upload` or `fetch` streams), `queued`, plus failure/resume | Extend `ConversionFile.status` (currently `pending|converting|completed|error`) |
| Download | Completed file card gets "İndir" (anchor to `/jobs/:id/download`); batch “download all” (zip) optional | New |
| Progress | `EventSource` manager (single multiplexed stream), reconnect + snapshot refetch; replace `listen("conversion-progress")` | New hook |
| Cancellation | Same UX; make it *authoritative* (reflect server state rather than optimistic reset) | Adjust |
| Error handling | Map `{code}` → Turkish strings (pattern exists in `utils/speech.ts`/`locales`); stop displaying raw `String(error)`; handle `401` (re-login), `413`, `429`, network offline | Extend |
| Previews | Local image previews can use `URL.createObjectURL(file)` before upload (no server round trip; improves privacy/latency); server thumbnails for video | Optimize |
| VideoTrimmer | Uses `convertFileSrc` for playback → `URL.createObjectURL(file)` locally (best) or ranged streaming endpoint | Adapt |
| PdfEditor | Reads/writes via fs plugin; uses client libs; server-side lopdf commands for text blocks/edit | Adapt to bytes via fetch; decide whether editing is client-only (pdf-lib) or server (lopdf) — duplicate approaches exist today |
| Microphone | New component | New |
| Responsive | Existing Tailwind responsive layout; remove `minWidth/minHeight` window assumptions (`tauri.conf.json` 900×600); verify small screens/touch | Verify |
| Light/dark theme | `localStorage` theme preference + `prefers-color-scheme` ("system") already implemented in store | Keep |
| Turkish localization | `locales/tr.ts` / `en.ts`; backend Turkish strings in `speech_error.rs` | Keep; add API/auth/upload strings; fix privacy copy |
| Tests | `src/test/setup.ts` mocks `@tauri-apps/*`; 112 tests currently pass | Replace mocks with API-client mock; most tests become adapters only |
| Build | Vite `devUrl` 1420; `dist/` served by Tauri | Serve `dist/` as static files from the Rust server or CDN/proxy; set CSP via HTTP headers (current CSP `connect-src 'self' ipc: http://ipc.localhost` → `connect-src 'self'`) |
| Misc | Sound chime via Web Audio (browser-native); `react-hot-toast` (fine); `localStorage` for theme (fine; avoid storing file metadata) | Keep |

---

## 12. SECURITY GAP ANALYSIS

| Area | Current protection | Web deployment impact | New risk | Required future control |
|---|---|---|---|---|
| Path validation | `path_validation::validate_input_file/validate_output_dir` (canonicalize, regular file, not in app dir); `sanitize_filename_component`; `confirm_within(job_dir, produced)` | Client paths vanish; trust boundary moves to upload | Traversal via multipart filename/ZIP entries; ID guessing | Server-generated names/IDs; `create_new`, `O_NOFOLLOW`; keep `confirm_within`; never derive FS paths from request data |
| File validation | Extension allow-lists (speech), image dimension limits, `tiff` multi-page rejection, size limit (speech 500 MB) | Files are now fully attacker-controlled & anonymous-ish | Extension/content mismatch; polyglots; oversized files | Magic-byte sniffing, per-category size caps, structural pre-checks, quarantine until validated |
| Temp directories | UUID `JobTempDir`, Drop cleanup, startup sweep | Multi-user, long-running process, multiple instances | Stale files; cross-instance sweep deletes live data; archive temp dir not UUID/guarded (`localconvert_archive_<ms>`) | Workspace manager with owner, TTL janitor, instance-aware reconcile; fix archive path; `0700` perms; `noexec` mount |
| Engine process execution | No shell, arg arrays, env deny-list, cwd job dir (hardened path); legacy path lacks all but arg arrays | Engines now parse hostile input from untrusted users | Engine RCE → server compromise; kill-tree gap; no sandbox | Run engines as dedicated low-priv user; namespaces/seccomp/bubblewrap or per-job container; tree kill; resource limits; consolidate on one runner |
| Command injection | Arg arrays only; no `sh -c`; `build_command_never_touches_a_shell` test | Same | Option injection via filenames starting with `-` (e.g. `-oFoo`) or crafted option values (e.g. `customFfmpegParams`, `-sDEVICE=` composition) | Server-generated filenames; `--` terminators where supported; strict typed option whitelist; remove free-form params |
| Resource exhaustion | Speech limits (size/duration/disk/1 job); image pixel caps; Office 600 s timeout | Many users, many jobs | CPU/RAM/disk DoS; zip/pdf/xml bombs; unbounded video | Global + per-user semaphores & quotas, timeouts for *every* engine, cgroup memory ceilings, queue bounds, output caps |
| Upload abuse | n/a (local files) | New attack surface | Slowloris, huge bodies, many small files, storage fill | Streaming caps, request timeouts, rate limiting, per-user quotas, early reject by `Content-Length`, proxy limits |
| Authentication | None (local single user) | Required | Unauthenticated access to all conversion capability | Pluggable auth layer (§9); deny by default |
| Authorization | None | Required | IDOR on job/file IDs; **client-chosen `job_id` allows cancelling others’ jobs today** | Owner checks, 404 on foreign IDs, server-generated IDs |
| Cross-user isolation | OS user boundary | Shared process & disk | Data leakage via shared dirs, caches (`TOOL_PATHS`, LO profile), logs | Per-user dirs, per-job LO profile (already), no shared caches of user data, scrubbed logs |
| Downloads | Files written to chosen folder | Browser downloads | Content sniffing/XSS via served HTML/SVG outputs; header injection via filename; path leak in `Content-Disposition` | `attachment`, `nosniff`, server-chosen content-type, sanitized/encoded filename, separate origin or sandbox CSP for downloads, never serve user HTML/SVG inline |
| Cleanup | Drop + startup sweep | Concurrent users | Orphaned outputs, disk fill, privacy retention | TTL, delete-on-download option, janitor, shutdown handling |
| SSRF | No network code (reqwest removed; `speechNoNetwork` test) | Server has network reach to internal services | Documents/media/SVG/HTML that fetch URLs: LibreOffice external links, FFmpeg HLS/`http` protocols (legacy path lacks whitelist), ImageMagick `url:`, Pandoc `--sandbox` absent, SVG external refs | Deny all outbound traffic for engine users (firewall/netns); protocol whitelists; disable remote resources in LO profile; Pandoc `--sandbox`; ImageMagick policy |
| Outbound network control | "No network" guarantee is by absence of code | Server and engines may egress | Data exfiltration; C2 after engine exploit | Egress deny-by-default at host/container; allow only IdP/logging endpoints for the API process |
| Malicious documents | LO headless w/ separate profile; PDF import | Hostile OOXML/ODF/RTF/PDF from any user | Macro execution, external-link fetch, parser bugs, LO hang | Macro/links disabled in seeded profile, sandboxing, timeouts, patch cadence tracking (LO pinned at 25.8.7: needs update policy) |
| Archive attacks | `enclosed_name()` for ZIP; nothing for 7z | Same | Zip bomb, symlink, hardlink, path escape via 7z, nested bombs, password prompts | Limits (entries/ratio/bytes/depth), reject special files, extraction in workspace, pure-Rust extractors, timeout |
| Logging / privacy | No persistent history; errors path-free in engine layer; no telemetry (`PrivacyStatus`) | Servers log by default (proxy, access, app) | Filenames/IPs/transcripts leaking into logs; “no history” claim becomes false if logs retain metadata | Log-minimization policy, structured logs without content/filenames, retention limits, access control; correct UI privacy claims |
| Web-specific | CSP in `tauri.conf.json`; Tauri IPC allow-list | Standard web threats | XSS, CSRF, clickjacking, CORS misconfig, session fixation | Strict CSP header, `frame-ancestors 'none'`, CSRF controls, no wildcard CORS, security headers, dependency audit for npm/crates |
| Process crash domain | `panic = "abort"` | One panic kills all users’ work | Availability | Isolate untrusted-input parsing in subprocesses or `catch_unwind` + `panic=unwind` for the server profile |

---

## 13. FEATURES THAT CHANGE STATUS

| Feature | Class | Rationale |
|---|---|---|
| Image conversion (JPEG/PNG/WebP/BMP/GIF/TIFF) | **KEEP** (engine) / **REFACTOR** (I/O) | Native Rust; only the path/IPC layer changes. |
| Image resize / crop / rotate | **KEEP / REFACTOR** | `resize_image_helper` etc. exist; resize is within `convert_image`; crop/rotate commands are *unregistered* today (no UI) — expose only when the UI is wired. |
| SVG / AVIF / HEIC / PSD | **REPLACE** (long-term) / **DEFER** | Depend on unbundled ImageMagick; hardened replacement needed on server; capabilities currently `ENGINE_MISSING` without it. |
| Office conversion (DOCX/XLSX/PPTX/ODT/ODS/ODP/RTF/DOC/XLS/PPT → PDF & inter-format) | **REFACTOR** | LO engine reusable; add cancel, hardening, Linux build, resource governance. |
| PDF → DOCX/ODT/RTF/HTML/TXT/EPUB | **REPLACE / REDESIGN** | See §14: today it is raw LO import; quality unsuitable for table-heavy documents. Until redesigned, label “experimental”. |
| PDF → XLSX / PPTX | **DEFER** (stays `FEATURE_NOT_IMPLEMENTED`) | No implementation. |
| PDF → images | **REFACTOR** | Ghostscript; output multiplicity, DPI/page caps. |
| PDF editor (text blocks, form fill) | **REFACTOR** or **DEFER** | Client libs reusable; server lopdf commands need limits. Note recent commit `fde3c92` "hide broken PDF editor features" — current UI exposure not fully verified (§19). |
| PDF merge/split/compress/rotate/watermark | **DEFER** | Unregistered scaffolding. |
| OCR | **DEFER** | `NOT_IMPLEMENTED`. |
| Video/audio conversion | **REFACTOR** | Move onto hardened runner; server-owned policy; licensing review for codecs. |
| Video trimming (`trim_video`) | **REFACTOR** | Same; UI uses local preview. |
| GPU/hardware encoders, `customFfmpegParams` | **REMOVE** (from web UI) | Server policy; free-form params are an injection vector. |
| Ses Dikte (file) | **KEEP / REFACTOR** | Core reusable. |
| Ses Dikte (microphone) | **DEFER → NEW** | Not implemented today. |
| Archive/compression | **REFACTOR (high risk)** | See §6/§7F. |
| Font "conversion" | **REMOVE** (or mark not implemented) | `convert_font` just copies. |
| SAS support | **DEFER** | §15. |
| Image optimization (Imagify-like) | **DEFER** | §16. |
| Recent operations/history | **REMOVE** (stay removed) | Requirement: no persistent history; already removed in the client. Server job table must be ephemeral. |
| Capability checks | **KEEP / REFACTOR** | Exposed via API; add admin-only detail; never leak paths (`office_engine_status` already path-free). |
| System status / privacy status | **REFACTOR** | Values change meaning (§9, §12). |
| Help UI / Tools setup modal | **REFACTOR / REMOVE** | “Install tools” guidance is meaningless for end users. |
| Settings: output directory, context menu, watch folders, scheduling, startup files, window chrome | **REMOVE** | Desktop-only. |
| Updater | **REMOVE** (nothing active) | Already removed; delete `createUpdaterArtifacts` config with Tauri. |
| Keyboard shortcuts (`useKeyboardShortcuts`) | **KEEP / REFACTOR** | Uses dialog `open()` → file input; watch for browser-reserved combos. |
| Completion sound, themes, i18n | **KEEP** | Browser-native. |

---

## 14. PDF → DOCX SPECIAL AUDIT

### 14.1 Where it is implemented
- Routing: `converter.rs::convert_document` — `input_ext == "pdf"` and `output ∈ {docx, doc, odt, rtf, html, txt, epub}` → `reconstruction::PdfToDocx::convert(input, output, filter, job_dir)`; `md` → same with `txt`.
- `reconstruction.rs::PdfToDocx::convert` → `engines::office::convert(input, output, filter, use_pdf_import_filter = true, job_dir)`.
- `engines/office.rs::convert` → `soffice --headless --infilter=writer_pdf_import -env:UserInstallation=… --convert-to docx --outdir … <input.pdf>`.
- `ReconstructionQuality::Partial` constant is declared and `#[allow(dead_code)]` — **not consumed by the UI**. `capabilityGating.ts::OFFICE_EXTENSIONS` (doc, docx, odt, rtf, xls, xlsx, ods, ppt, pptx, odp) does not contain `pdf`, so `capabilityIdForFile` returns `null` for a PDF input: **PDF→DOCX is not capability-gated in the UI at all**, even though it needs the Office engine (the backend returns `OFFICE_ENGINE_NOT_AVAILABLE` only after the attempt).

### 14.2 How the output DOCX is reconstructed
There is **no custom reconstruction in the repository**. LibreOffice's `writer_pdf_import` filter converts the PDF through its Draw-based PDF import (via `pdfium`/internal `xpdf`-derived parser): each text *portion* becomes a positioned text frame, vector graphics become drawing shapes, and the document is then exported to DOCX via the Writer DOCX filter. No post-processing of the DOCX exists (no table detection, no paragraph merging, no style inference, no font mapping, no cleanup pass).

### 14.3 Why a 21-page, landscape, table-heavy PDF yields thousands of text boxes / drawing objects
(Explanation of the mechanism; the specific 21-page file is not in the repository, so its exact numbers were not reproduced here.)
1. A PDF stores *positioned glyph runs* and *stroked/filled paths*; it has no table, row, cell, paragraph or reading-order objects.
2. The importer therefore creates **one text frame per run/line fragment** to preserve absolute position. A table cell with 3 lines of text becomes 3+ frames; a table of 15 columns × 40 rows × 21 pages can produce thousands.
3. Table borders and shading are separate **line/rectangle drawing objects** (typically one per cell edge), producing another multiplicative set of shapes.
4. Page geometry (landscape) is preserved by anchoring frames to the page rather than flowing text, so Word sees floating objects, not content.
5. Result: structurally "faithful-looking", but not editable as tables/paragraphs; large DOCX, slow open in Word, text reflow impossible. Turkish text adds failure modes: characters from subset-embedded fonts without proper ToUnicode maps become wrong glyphs; ligature/diacritic handling (`ş ğ ı İ`) depends on font embedding; font substitution on the server changes metrics (frame overflow).
6. The default LO path for scanned PDFs yields images only (the `reconstruction.rs` docs already acknowledge this).

### 14.4 Known weaknesses
No table reconstruction; no paragraph/heading semantics; text frames instead of flowing text; absolute positioning; fidelity depends on fonts installed on the server; no OCR for scans; no validation of output quality; no per-job quality signal (the `Partial` enum isn’t surfaced); runs through the same cold-start LO path (slow) with the 600 s timeout.

### 14.5 Options
| Criterion | A. Improve "existing Rust reconstruction" | B. Mature server-side PDF→DOCX tool/library | C. Hybrid semantic reconstruction |
|---|---|---|---|
| Premise check | **There is no Rust reconstruction to improve**; A means *writing one from scratch* on top of `lopdf` (or `pdfium`/`mupdf` bindings) | e.g. tools in the `pdf2docx` family (Python/PyMuPDF), commercial SDKs, or LO with tuned filters/options | Layout analysis + table detection + DOCX writer (semantic), falling back to B/LO when confidence is low |
| Fidelity | Potentially best *if* heavily invested; high risk of long tail | Moderate–good on simple docs; table-heavy varies by tool | Best achievable for target documents; quality depends on detectors |
| Tables | Must implement ruling-line + whitespace-alignment table detection, spanning cells, nested tables | Some tools have table extraction (line/stream based), imperfect on merged cells | Explicit table model; can validate (row/col counts, text coverage) and fall back |
| Turkish text | Full control of Unicode mapping; must handle ToUnicode/subset fonts | Depends on the library’s font/CMap handling; test needed | Same as A, plus fallback |
| Layout preservation | You decide: flow vs. anchored | Typically blends | Mix: flow for body, anchored for figures |
| Maintainability | **Highest burden** (own PDF layout engine in Rust) | Depends on dependency health | High (two pipelines + routing) |
| Server resource use | Low-medium (in-process, no LO cold start) | Python/native runtime; moderate | Moderate |
| Licensing | Your code; underlying PDF lib licensing matters (`lopdf` MIT already; pdfium BSD-3; **MuPDF is AGPL/commercial; PyMuPDF AGPL**; `pdf2docx` is GPL-3 — verify) | **Must be vetted**: AGPL/GPL components change distribution obligations; commercial SDKs cost & lock-in | Inherits constituents’ licenses |
| Offline operation | Yes | Yes if self-hosted (no cloud API) | Yes |
| Privacy | Best (in-process) | Good if local; **reject any cloud-API product** (violates requirements) | Good |

**Assessment (not a decision):** keeping the current LO import as a labelled "basic/experimental" path while evaluating B on a *corpus of real MEB documents* (including the 21-page example, which should be added as a private regression asset) is the least risky first step; C is the likely end state if B is insufficient on tables, because table-heavy institutional PDFs are the stated pain point. A as literally worded ("improve existing Rust reconstruction") has no starting point in the codebase. License screening of B candidates is mandatory before evaluation work begins. The deciding artifacts are an evaluation corpus and acceptance metrics (table cell accuracy, text coverage, Turkish character fidelity, object count in DOCX, open-time in Word) — none exist in the repository today.

---

## 15. FUTURE SAS SUPPORT

*No SAS support exists in the repository.* (grep finds no SAS parser, no format entries; nothing is claimed.)

| Format | What it is | Offline server-side parser availability (general knowledge; **verify before relying**) |
|---|---|---|
| **SAS7BDAT** | Proprietary binary dataset file produced by Base SAS. No official spec; community reverse-engineered | Open-source readers exist in multiple ecosystems (e.g. ReadStat/`haven`, `pandas.read_sas`, Rust crates of varying maturity). Typical issues: compression variants (RLE/RDC), encodings (Turkish `cp1254`/`latin5` vs UTF-8), very large files, label/format metadata. Feasible offline. |
| **XPT** | SAS Transport (v5 / v8) — a *published* exchange format (FDA submissions use v5); also CPORT/XPORT variants exist | Well documented; pure-library parsers are straightforward (v5 IBM-float quirks, 8-char names/40-char labels limits). Most tractable of the three. |
| **SASHDAT** | SAS Viya/CAS in-memory table **export** format (`.sashdat`, distributed block layout, often accompanied by HDFS/`CASHDAT` conventions) | Closely tied to SAS Viya/CAS; I am not aware of a mature, complete standalone parser; may require SAS tooling (CAS actions, `saspy`/SWAT against a Viya server) → **likely not realistically available as a standalone offline parser** in this architecture without a SAS runtime or vendor SDK. Treat as high-risk/unsupported until proven. |

Architecture implications:
- SAS is a **data-conversion kind**, not a document kind: new `Capability` ids (e.g. `sas_import`, `sas_export`), new job kind `data_convert` with its own limits (row counts, memory).
- It belongs in the **job system as an engine adapter** (`engines::sas`) running in the same sandboxed worker model; large datasets must be **streamed** (chunked read → CSV/Parquet/XLSX writer) rather than loaded in memory; row/column and output-size caps are mandatory (an XLSX output has a hard 1,048,576-row limit that must produce a clear error).
- Metadata (labels, formats, encoding) mapping is a product decision (what to preserve in CSV vs XLSX vs Parquet).
- Output formats probably CSV/XLSX/(Parquet/JSON); Turkish encoding handling must be explicit.
- Licensing: ReadStat is MIT; verify any Rust crate; never embed SAS software.

---

## 16. FUTURE IMAGE OPTIMIZATION (Imagify-like)

Fits as **new job kind `image_optimize`** reusing `native/image` (decode → transform → encode) and the job/worker/progress model — not as a variant of "convert".
- **Compression & quality controls:** per-format encoders (JPEG: quality, progressive, chroma subsampling; PNG: lossless/quantized palettes; WebP lossy/lossless; AVIF speed/quality). Current `image` 0.25 build has WebP **lossless only** and no AVIF encoder (`default-features = false, features = ["jpeg","png","webp","bmp","gif","tiff"]`) → needs additional codec dependencies (licensing/supply-chain vetting: e.g. libwebp, `ravif`/`libavif`, `mozjpeg`/`oxipng`-class tools).
- **Resize:** existing `transform.rs` (resize/crop/rotate) reusable; add max-dimension policy and aspect lock.
- **Metadata stripping:** already the default for images (`preserve_metadata=false`); needs EXIF orientation applied before stripping (verify current behavior), and ICC profile policy.
- **Before/after size:** natural job result fields `{input_bytes, output_bytes, saved_pct}` + optional `GET /files/:id/preview` comparison (`ImageComparePreview.tsx` exists for UI).
- **Batch:** a parent "batch" record grouping N jobs (priority below interactive jobs), per-user caps, and zip download.
- **Limits:** keep the 40 MP / 20 000 px caps; memory-aware concurrency; cancellation points between stages.
- **Location in architecture:** `light` resource class (fast, high concurrency), capability id `image_optimization`.

---

## 17. ARCHITECTURAL RISKS

### CRITICAL
| # | Risk | Reason | Affected | Mitigation |
|---|---|---|---|---|
| C1 | Engines parse untrusted uploads with no sandbox | Engine exploit = host compromise; currently run as the interactive user with full FS/network | `engines/process.rs`, `converter.rs` legacy runner | Dedicated low-priv user, namespaces/seccomp/bubblewrap or per-job containers, no network, FS allow-list, resource limits |
| C2 | Path-based data model & global process state | Client chooses paths and `job_id`; global `RUNNING_PROCESSES`/`CANCELLED_JOBS` | `commands.rs`, `converter.rs`, `security/path_validation.rs`, `speech/job.rs` | Upload/ID model; per-job state; server-generated IDs; owner checks |
| C3 | No authN/Z, no per-user isolation | Greenfield but security-critical | whole server | Pluggable auth layer + ownership model before any non-localhost exposure |
| C4 | Archive handling has no bomb/symlink/limit controls, uses non-guarded temp | Direct DoS/LFI vector | `converter::convert_archive`, `native/archive_zip.rs`, 7-Zip calls | Dedicated safe-extraction module with limits; reject special files; workspace; timeouts |
| C5 | SSRF/LFI via media/document features | Server can reach internal network; legacy FFmpeg lacks protocol whitelist; Pandoc/ImageMagick/LO external refs | `converter.rs` (video/audio/pandoc/magick), LO profile | Egress deny, protocol whitelists, policy files, Pandoc `--sandbox`, LO profile hardening |

### HIGH
| # | Risk | Reason | Affected | Mitigation |
|---|---|---|---|---|
| H1 | Office jobs not cancellable; timeouts only 600 s; no tree kill | Resource leak, zombie soffice children | `converter.rs`, `engines/office.rs`, `process.rs` | Cancel token into `office::convert`; process groups/Job Objects |
| H2 | Two process-launch systems; legacy has no timeout/env scrubbing/cwd and leaks stderr to users | Inconsistent security posture | `converter.rs::run_command_with_job_id`, `run_ffmpeg_with_progress`, `commands.rs` | Consolidate onto `engines::process` |
| H3 | Windows-only engine bundles and tooling | No Linux path for LO/ffmpeg/whisper; scripts are PowerShell/MSI | `engine_id.rs`, `office_manifest.rs`, `scripts/*.ps1`, `ascii_link.rs`, `limits.rs::free_disk_bytes` | Platform abstraction + new acquisition scripts + manifests per OS; implement Linux disk query |
| H4 | Upload/disk exhaustion; no quotas | New | n/a | Quotas, streaming caps, janitor |
| H5 | `panic = "abort"` + in-process parsers (image, lopdf, zip) | One bad file kills all users’ jobs | `Cargo.toml`, `native/*`, `pdf_text_editor.rs` | `panic=unwind` + `catch_unwind` at job boundary, or subprocess isolation |
| H6 | PDF→DOCX quality (user-visible) | Table-heavy docs unusable | `reconstruction.rs`, `office.rs` | §14 evaluation; label as experimental meanwhile |
| H7 | Startup sweeps delete all job dirs | Destroys concurrent instances’ jobs | `security::temp::cleanup_stale_job_dirs`, `speech::job::cleanup_stale_in` | Instance-owned workspace registry |
| H8 | Licensing of redistributed/served components | FFmpeg (LGPL mode pinned; codecs), whisper.cpp model license, LO MPL, Ghostscript AGPL, MuPDF/PyMuPDF AGPL, 7-Zip LGPL+unRAR | `THIRD_PARTY_NOTICES/*`, scripts | Legal review (the repo already flags this); note server-side use of AGPL software can trigger network-use source obligations |

### MEDIUM
| # | Risk | Reason | Affected | Mitigation |
|---|---|---|---|---|
| M1 | Whisper thread oversubscription / long jobs block capacity | CPU-bound | `engines/speech.rs`, scheduler | Thread budgeting, queue with ETA |
| M2 | LO cold-start latency / memory per job | UX & capacity | `engines/office.rs` | Admission control; evaluate warm pool |
| M3 | Browser mic requires HTTPS; format zoo (webm/opus/mp4) vs minimal ffmpeg demuxers | Feature may silently not work | `audio_prep.rs`, deployment | Verify build config; client-side WAV encoding fallback |
| M4 | Stale privacy claims in UI/README | Misleading users | `locales/*.ts`, `SystemStatusModal`, `README.md`, `system_status` | Rewrite copy; legal/UX review |
| M5 | Frontend store coupled to `path` | Large refactor | `useStore.ts`, components | Introduce `fileId` model & API client early |
| M6 | Test suite coupled to Tauri mocks | Churn | `src/test/setup.ts`, speech tests | Adapter-level mocks |
| M7 | `CONVERT_TIMEOUT`/limits are constants | Server needs configuration | `office_manifest.rs`, `limits.rs` | Config layer with validated ranges |
| M8 | Output overwrite semantics (`move_into_place` overwrites) | n/a on server (unique job outputs) | `security/temp.rs` | Not needed in new model |

### LOW
| # | Risk | Reason | Affected | Mitigation |
|---|---|---|---|---|
| L1 | Dead code (unregistered commands, `WatchFoldersModal`, `ScheduleModal`) | Clutter | `commands.rs`, components | Delete during migration |
| L2 | `convert_font` is a fake conversion | Misleading | `converter.rs` | Remove/mark unsupported |
| L3 | `rust-version = "1.70"` stale for current deps/tokio ecosystems | Build friction | `Cargo.toml` | Update MSRV |
| L4 | Crate named `localconvert` / identifiers `com.localconvert.desktop` | Branding | `Cargo.toml`, config | Rename in server crate |
| L5 | Temp dir naming inconsistent (`localconvert/jobs` vs `MEB-Donusturucu/temp/speech`) | Ops | `security/temp.rs`, `speech/job.rs` | Single data root |

---

## 18. PROPOSED MIGRATION PHASES

*(Adjusted to what the repository actually contains. Each phase ends green on tests and leaves the desktop build untouched until cutover is decided.)*

- **Phase 0 — Decisions & audit (this document).** Resolve §19 items that block design: auth mode, OS/deployment, limits, retention, licensing stance, PDF→DOCX strategy.
- **Phase 1 — Workspace refactor (no HTTP yet).** Extract a Tauri-free core crate (`converter-core`): move `converter`, `engines`, `native`, `security`, `speech`, `tools`, `types`, `capabilities` logic behind traits (`ProgressSink`, `CancelToken`, `WorkspaceProvider`). Remove `APP_HANDLE`, global `RUNNING_PROCESSES`/`CANCELLED_JOBS`. Desktop keeps working via a thin Tauri adapter during the transition. *Reason first: it de-risks everything else and keeps tests meaningful.*
- **Phase 2 — Server skeleton + job model.** HTTP framework, config, health, structured error type, auth trait with a dev stub, workspace manager, job manager (state machine, queue, semaphores, cancel tokens, SSE bus), janitor, graceful shutdown. Capabilities endpoint.
- **Phase 3 — Upload pipeline.** Streaming upload, size caps, magic-byte sniffing, probe (ffprobe/image/pdf/audio), quotas, download endpoint with safe headers.
- **Phase 4 — Frontend adapter layer.** `src/api/` client + SSE manager; replace Tauri calls in the store and hooks; upload UX; download UX; remove window chrome and desktop-only settings; replace test mocks.
- **Phase 5 — Native image engine on server.** First vertical slice end-to-end (browser → upload → job → download) using the safest engine; stress-test limits & cleanup.
- **Phase 6 — Hardened process runner + Office engine.** Single runner (tree kill, cancel, timeout, limits, sandbox); LibreOffice Linux/Windows acquisition, profile hardening (macros/links off), cancellation, concurrency/admission control.
- **Phase 7 — FFmpeg/media.** Move video/audio onto the hardened runner; protocol whitelist; timeouts/caps; progress via `ProgressSink`; licensing/codec decision.
- **Phase 8 — Ses Dikte.** Port file flow (mostly reuse), scheduler slots/thread budget, Linux engine+model, then **new** mic capture (MediaRecorder), formats, HTTPS requirement, UX.
- **Phase 9 — Archives & PDF tooling hardening.** Safe extraction module, limits, pure-Rust formats; Ghostscript/Pandoc/ImageMagick policies; PDF editor server side decisions.
- **Phase 10 — PDF→DOCX strategy.** Build evaluation corpus + metrics; evaluate B (license-screened) and C; ship improved or clearly-labelled path.
- **Phase 11 — Security hardening & verification.** Threat-model review, fuzz/abuse tests (bombs, polyglots, SSRF probes, symlinks, path tricks), resource-exhaustion tests, dependency/supply-chain audit, header/CSP review, penetration test.
- **Phase 12 — Authentication/authorization integration.** Real IdP/proxy integration, roles, audit logging, privacy copy rewrite, KVKK/legal review.
- **Phase 13 — Deployment & operations.** Packaging (service/container), TLS, proxy, monitoring, backup (none needed for user data), patch/update procedure for LO/FFmpeg/whisper/model, runbooks.
- **Phase 14 — Optional features.** SAS data conversion, Imagify-like optimization, OCR, additional PDF tools.
- **Cutover/retirement of Tauri** after parity and acceptance testing (removal list in §2).

---

## 19. FINAL REPORT ITEMS

### 19.1 Files inspected (read in full or in the cited ranges)
`CLAUDE.md`, `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json`, `src-tauri/src/lib.rs`, `commands.rs` (headers + command list + `get_startup_files`, `system_status`, `get_supported_formats`), `converter.rs` (core router, document/spreadsheet/presentation/vector/font/archive converters, process runners, progress/cancel), `reconstruction.rs`, `capabilities.rs` (compute + head), `types.rs` (struct list), `tools.rs` (path resolution), `engines/{office,process,resolver}.rs`, `engines/{office,ffmpeg,speech}_manifest.rs` (constants/signatures), `engines/{engine_id,speech,audio_prep}.rs` (excerpts), `speech/{commands,job,pipeline,limits}.rs`, `security/{temp,fs_scope,path_validation,file_validation}.rs`, `native/archive_zip.rs`, `native/image/{decode,mod}.rs` (limits), `src/store/useStore.ts` (types, `convertFiles`, `cancelConversion`, settings, theme init), `src/hooks/useSpeechJob.ts`, `src/components/Header.tsx`, `FileDropZone.tsx` (Tauri events), `src/utils/capabilityGating.ts`, `src/main.tsx`; full grep of all `@tauri-apps`/`invoke`/`listen`/`convertFileSrc` call sites in `src/`; `docs/OFFICE_ENGINE.md` (first sections).

### 19.2 Files changed
**Only `docs/WEB_MIGRATION_AUDIT.md`** (this file). See the git status shown in the session output.

### 19.3 Assumptions
- Target is an institutional (MEB) internal deployment; "no persistent conversion history" means no *user-visible* history and no retained user content beyond a short TTL.
- Engines continue to be the same family (LibreOffice, FFmpeg, whisper.cpp) as in the repository.
- General-knowledge statements about third-party tools (licenses, SAS formats, CVE history, browser recording formats) are not verified against current sources in this audit.

### 19.4 Unknowns (not verified in this audit)
1. Exact remaining reachable UI for PDF editor features after commit `fde3c92` ("hide broken PDF editor features").
2. Which demuxers/decoders the bundled minimal FFmpeg actually contains (e.g. WebM/Opus) — see `scripts/ffmpeg/*` and `scripts/build-ffmpeg-engine.ps1` (not read).
3. Measured whisper real-time factor on target hardware (`docs/SPEECH_ENGINE.md`, `scripts/bench/` not read in this audit).
4. Temp usage in `get_image_preview`, `get_video_thumbnail`, PDF editor paths (partially traced).
5. Whether `-sOutputFile=<single file>` for multi-page PDF→PNG yields only the last page (Ghostscript behavior with no `%d`; not run).
6. Content of `ToolsSetupModal`, `SettingsModal` sections, `FileCard` details beyond grep.
7. The 21-page landscape PDF itself (not in the repo) — object counts in §14 are explained mechanistically, not measured.
8. MEB’s identity infrastructure, network zoning, OS standards, hardware (cores/RAM/GPU), TLS/CA situation, expected concurrent users — nothing in the repo.
9. Real size/format distribution of MEB documents.
10. Whether `legal`/KVKK review has defined retention/logging rules.

### 19.5 Decisions needed before implementation
1. **Deployment OS/model** (Windows Server vs Linux vs container; sandboxing level; per-job container or process sandbox).
2. **Authentication mode** (reverse-proxy SSO vs OIDC vs SAML vs LDAP) and tenant model (single org vs multiple units).
3. **Retention & privacy policy:** TTL for uploads/outputs/transcripts, delete-on-download, audit log content/retention, at-rest encryption.
4. **Limits:** upload size per category, quotas, concurrency, durations, page counts, archive limits.
5. **Capacity targets:** concurrent users/jobs and hardware; whisper capacity policy.
6. **Framework choice** (axum recommended on technical grounds) and crate-vetting policy.
7. **Engine policy:** keep Windows bundles vs. build Linux ones; LibreOffice warm-pool vs cold start; replace ImageMagick/7-Zip/Pandoc/Ghostscript or run under strict policies; codec licensing for video.
8. **PDF→DOCX strategy** (A/B/C) and the evaluation corpus/metrics; license stance on AGPL/GPL tools.
9. **Mic dictation scope:** record-then-transcribe only vs. live streaming; client WAV encoding vs server demux.
10. **Panic strategy** for the server build (`abort` vs `unwind`/subprocess isolation).
11. **Scope of the desktop app** after the pivot (retire vs maintain), and how to treat `src-tauri` history.
12. **Whether PDF editor and other currently-hidden features are in scope** for web v1.
