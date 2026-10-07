# Track B — Generalizing the web job system beyond image conversion

Goal: `meb-server` executes every conversion category the desktop app does —
PDF operations, Office conversion, image optimization — instead of only
raster image conversion.

The constraint throughout: external engines go on the unified process runner
of `WEB_ARCHITECTURE_PROPOSAL.md` §E. The legacy `src-tauri/src/converter.rs`
launcher, `commands::hidden_command` and `tools::get_tool_path` are never
reused by the server.

---

## B1 — Job kinds and the engine seam ✅ done

The job system was hardcoded to image conversion: `Job` carried
`source_format`/`target: NativeImageFormat` and image-only `ConvertOptions`,
`RunRequest` carried `target` + `options`, the worker built `result.<ext>`
from `job.target` and sniffed image magic bytes, `AppState` held exactly one
runner, and `kind` had to be the literal `"convert"`.

**`spec.rs` (new)** is now the only place a client string becomes an
operation:

- `JobKind` — the closed set of published operations, with `wire()`/`parse()`
  and an `arity()`.
- `JobSpec` — one variant per kind holding that kind's *validated* work.
  Everything the kind-agnostic layers need is a method on it:
  `output_extension()`, `output_mime()`, `output_name()`, `validate_output()`.
- `CreateJobRequest` — strict top-level parsing, with `options` kept opaque
  and then parsed into each kind's own `deny_unknown_fields` type. No engine
  parameter (path, codec, filter, GPU flag) has a representation.

**`runner.rs`** — `RunRequest` carries `inputs: &[PathBuf]` + `spec`.
`RunnerRegistry` maps kind → runner; **a kind with no runner is refused at
create time with `UNSUPPORTED_CONVERSION`**, before any job, workspace or
work exists. That is how a kind whose engine is missing stays off the API
instead of failing every job it accepts.

`jobs.rs` and `worker.rs` now contain no format-specific code at all.

## B2 — Formats, inputs and the API surface ✅ done

**`meb-core::format` (new)** — `SourceFormat` (image / PDF / Office),
`OfficeFormat`, `FormatCategory`, and `Container` sniffing from magic bytes.
A name's extension is a *claim*; the container narrows it; a probe decides.

**`meb-core::document` (new)** — the document-side counterpart of
`image::probe_file`. A short prefix cannot tell DOCX from XLSX from ODT (all
three are ZIPs), so:

- OOXML is identified by the part that defines it (`word/document.xml`,
  `xl/workbook.xml`, `ppt/presentation.xml`) via the ZIP central directory —
  the same evidence LibreOffice uses.
- OpenDocument is identified by its `mimetype` member.
- PDF is checked for the `%PDF-` header and a `%%EOF` trailer. This is a
  well-formedness floor, **not** a damage guarantee; page count and
  encryption need a real parser and are left to the engine.

Uploads admit PDFs and the six Office formats. Legacy OLE `.doc`/`.xls`/
`.ppt` are deliberately not admitted. `FileRecord.format` is a
`SourceFormat`; `width`/`height` are `Option<u32>` (null for documents
rather than a made-up zero).

**Multi-input** — a request names `file_id` *or* `file_ids`; the count is
checked against the kind's arity (`MAX_INPUTS = 50`). Inputs are staged under
server-built positional names (`in/source-00.pdf`).

**Kinds published** — `convert` (image, incl. optimization), `pdf_merge`,
`pdf_split`, `pdf_compress`, `pdf_rotate`, `pdf_watermark`, `pdf_ocr`,
`office_convert`. Every option that reaches an engine is a closed enum
(`PdfQuality`, `PdfRotation`, `OcrLanguage`) or a bounded, sanitized value
(watermark text, opacity, page lists). Only `convert` has a runner; the other
seven are refused as above, and `GET /capabilities` *derives* its answer from
the registry, so it cannot claim an engine the server does not have.

---

## B3 — `exec`: the one process executor ⬜ next

Per §E.1, evolved from `src-tauri/src/engines/process.rs`, whose invariants it
keeps: no shell, argument arrays, the executable only from the resolver, cwd
is the job's `work/`, both pipes drained on threads, cancel flag polled,
child killed and reaped.

1. `ExecSpec` / `ExecEnd` / `ExecResult` as sketched in §E.1.
2. `Arg` (§E.3): `Flag` is a literal; `Value` goes through a per-engine
   checker; `Input`/`Output` are server-built relative paths, so they can
   never begin with `-`.
3. Env **allowlist** (§E.4) replacing the current denylist: `env_clear()`,
   then `PATH`, `TEMP`/`TMP`/`TMPDIR` → `work/tmp`, `HOME`/`USERPROFILE` →
   `work/home`, `SystemRoot`.
4. Engine resolution restricted to the **bundled tier** under a configured
   `ENGINES_ROOT`; no system fallback, no PATH scanning. Manifest
   verification at startup; size/mtime re-check at spawn.

**Known gap to decide:** §E.6 wants a Windows Job Object per spawn
(`CREATE_SUSPENDED` + `AssignProcessToJobObject` +
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), which needs `windows-sys` and a small
platform module, because `std::process::Command` cannot create suspended
processes. Until then the executor kills only the direct child — the same
residual risk `src-tauri/src/engines/process.rs` and `docs/OFFICE_ENGINE.md`
already document for `soffice`. This should be an explicit decision, not an
omission.

## B4 — The document runners ⬜

One `ConversionRunner` per kind, on top of `exec`, registered in
`RunnerRegistry::production()` only when its engine verifies — which is what
flips the capability to `AVAILABLE` and makes the kind postable.

| Kind | Engine | Notes |
|---|---|---|
| `office_convert` | LibreOffice | `--headless` + per-job `-env:UserInstallation` profile, seeded from a read-only template that disables macros, update checks and link updates (§E.8). The profile race is a real documented pitfall, not hypothetical. |
| `pdf_merge`, `pdf_compress`, `pdf_rotate` | Ghostscript | `-dSAFER -dBATCH -dNOPAUSE -sDEVICE=pdfwrite`, quality preset from `PdfQuality`. |
| `pdf_split` | Ghostscript + `zip` | Per-page render, then one ZIP (the output contract is already single-valued). Needs the page cap enforced against the real page count. |
| `pdf_watermark` | undecided | The desktop `add_watermark` does **not** watermark — it runs a no-op Ghostscript pass and silently falls back to copying the file. Do not port it. Either an overlay via a PostScript prologue or a `lopdf` content-stream append; pick one before wiring the runner. |
| `pdf_ocr` | Tesseract | Needs an `EngineId`, a manifest, and the traineddata for `OcrLanguage`. Rasterize → OCR → re-assemble a PDF with a text layer; the assembly step is the open design question. |

## B5 — Remaining ⬜

- Per-category upload caps (§I.2). One `max_upload_bytes` is shared today.
- Wall-clock timeouts per kind; the in-process image engine is still bounded
  only by its pixel limits.
- PDF page count / encryption detection at upload, if a clearer error than
  the engine's is wanted.
- Frontend: consume `capabilities.kinds` instead of assuming the kind set.
