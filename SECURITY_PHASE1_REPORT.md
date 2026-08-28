# LocalConvert — Phase 1: Secure Desktop Foundation
## Implementation Report

This covers exactly the scope requested: turning the existing Tauri + React
+ Rust desktop converter into an offline-first, privacy-first foundation.
No Phase 2 (PDF reconstruction) work was started.

A note on process: this sandbox's Rust toolchain (1.75, Dec 2023) is too
old for this project's current dependency graph, which now requires
edition2024 (stabilized in Rust 1.85). See section D and F for exactly what
that means for verification and what you need to run first.

---

## A. Modified files

**Rust / backend**
- `src-tauri/Cargo.toml` — removed unused/security-relevant dependencies
- `src-tauri/src/lib.rs` — plugin registration, startup cleanup hook
- `src-tauri/src/commands.rs` — validation wired into key commands, two new commands
- `src-tauri/src/converter.rs` — `convert_file` router: validation + isolated temp dir
- `src-tauri/src/types.rs` — `PrivacyStatus` struct; `preserve_metadata` default flipped
- `src-tauri/src/security/` **(new)** — `mod.rs`, `path_validation.rs`, `file_validation.rs`, `temp.rs`, `fs_scope.rs`
- `src-tauri/tauri.conf.json` — CSP added, fs/asset-protocol scope trimmed, updater config removed
- `src-tauri/capabilities/default.json` — trimmed to exactly what's used

**Frontend**
- `src/App.tsx` — auto-updater flow removed, History→Privacy panel swap, palette fixes
- `src/components/Header.tsx` — History button → Privacy & System Status button
- `src/components/HistoryModal.tsx` — **deleted**
- `src/components/SystemStatusModal.tsx` **(new)** — Privacy/System Status panel
- `src/components/PrivacyBadge.tsx` — removed a fabricated "0 bytes uploaded" stat
- `src/components/PdfEditor/PdfEditor.tsx` — authorizes file paths before direct fs access
- `src/components/SettingsModal.tsx` — Dark/Light/System theme selector
- `src/store/useStore.ts` — history state removed; theme-preference resolution added
- `tailwind.config.js` — navy palette (not black), MEB red accent
- `package.json` / `package-lock.json` — unused plugin packages removed
- `CLAUDE.md` — auto-updater section replaced with a removal notice (see D)

---

## B. Summary of security changes

**1. Silent auto-update removed entirely, not just disabled.**
`App.tsx` previously ran `check()` → `update.downloadAndInstall()` →
`relaunch()` on every startup with no user prompt. `tauri-plugin-updater`
and `tauri-plugin-process` are no longer dependencies at all — removed from
`Cargo.toml`, `package.json`, `lib.rs` plugin registration, and the
`bundle.updater`/`plugins.updater` blocks in `tauri.conf.json`. There is no
code path left that can do this, not just an unused one.

**2. Centralized path validation** (`security::path_validation`).
`validate_input_file` / `validate_output_dir` / `validate_fs_scope_target`
canonicalize and check every path before it's used — input must be an
existing regular file, output directories must exist and be directories,
neither may resolve inside the app's own install directory. Also
normalizes Windows' `\\?\` canonicalize prefix at the source (previously
only 4 of the LibreOffice-backed converters stripped this locally; now
every validated path is consistently clean everywhere).

Wired into: `get_file_info` (the one function every file passes through),
`convert_file`, `merge_pdfs`, `split_pdf`, `compress_pdf`, and the new
`authorize_fs_path` command. See section H for what isn't covered yet.

**3. Isolated per-job temp directories** (`security::temp::JobTempDir`).
Each job gets `<temp>/localconvert/jobs/<uuid>/`. External tools write
there, never directly into the user's real output folder; the finished
file is moved into place only after success (rename, with copy+delete
fallback across drives/volumes). Cleanup runs via `Drop` — success,
failure, or an early return via `?` all trigger it — plus a
`cleanup_stale_job_dirs()` sweep at startup for anything a crashed
previous run left behind. Wired into `convert_file`, `merge_pdfs`,
`split_pdf`, `compress_pdf`.

**4. Runtime, per-file scope grants replace the static `$HOME/**` grant**
(`security::fs_scope`). The PDF editor's direct `readFile`/`writeFile` and
the video trimmer's `convertFileSrc` preview used to be authorized by a
blanket `fs:scope: ["$HOME/**"]` — the frontend could read or write
anywhere under the user's home directory, indefinitely, from page load.
This is now `[]` (empty) in `tauri.conf.json`'s `assetProtocol.scope` and
trimmed to `$APPDATA/**`/`$APPLOCALDATA/**` in the fs capability. Instead,
`get_file_info` and the new `authorize_fs_path` command grant access to
exactly one already-validated file at a time, via
`app.fs_scope().allow_file()` / `app.asset_protocol_scope().allow_file()`
(the pattern Tauri's own docs recommend for this exact situation).

**5. Content Security Policy added** (`tauri.conf.json`). Previously
`null` (unset). Now a real, restrictive policy: `default-src 'self'`,
no remote scripts, `img-src`/`media-src` scoped to `self`/`data:`/the
asset protocol origin, `connect-src` scoped to `self`/the IPC origin,
`object-src 'none'`, `frame-ancestors 'none'`.

**6. Capabilities trimmed to exactly what's called.** Removed
`shell:allow-execute`, `shell:allow-spawn`, `shell:allow-stdin-write`,
`shell:allow-kill`, `shell:allow-open` — grep-verified that
`@tauri-apps/plugin-shell` is never imported anywhere in `src/`. All
external tool execution already went through Rust `std::process::Command`
with argument arrays, never a shell string — this was true before Phase 1
too, the capability grant was simply unused. Also removed unused
`fs:default`/`fs:allow-read`/`fs:allow-write`/`fs:allow-exists`/
`fs:allow-mkdir`/`fs:allow-remove`/`fs:allow-rename`/`fs:allow-copy-file`
(the frontend only ever calls `readFile`/`writeFile`) and the entire
`notification:*` grant (see item 8).

**7. No arbitrary shell strings anywhere** — confirmed, not just assumed.
Every `Command::new(...)` call in the codebase uses a fixed program name
resolved via `get_tool_path()` plus a `Vec<String>` of discrete arguments.
The two `-c` occurrences (Ghostscript page rotation, ffmpeg `-c copy`) are
fixed literals selected from a small match, not interpolated user text.
`register_context_menu` already used `HKEY_CURRENT_USER` (no admin
required) and passes the registry command value as a single `reg.exe`
argument, not something later shell-parsed.

**8. Unused capabilities removed for consistency, not just the ones named
in the brief.** Grep-verified `tauri-plugin-notification` (registered, but
`@tauri-apps/plugin-notification` never imported — completion feedback
uses a Web Audio API chime instead) and `tauri-plugin-opener` are unused;
removed both, along with genuinely-unused Cargo dependencies `reqwest`,
`futures-util`, `zip`, `chrono` (an HTTP client sitting unused in an
offline app's dependency tree is exactly the kind of thing this phase is
about — even though nothing in the code called it).

**9. Conversion history removed entirely**, not hidden. `useStore.ts`'s
`ConversionHistoryItem` (which stored `inputPath`, `inputName`,
`outputPath` — full file paths) and the `history`/`addToHistory`/
`clearHistory` state are gone. `HistoryModal.tsx` is deleted. Nothing
replaced it with a quieter version — there is no in-memory or persisted
log of filenames/paths at all.

**10. Metadata stripped by default** — for images. Both
`ConversionOptions::preserve_metadata` (Rust default) and
`settings.preserveMetadata` (frontend default) flipped from `true` to
`false`. This is honestly scoped: the only converter that currently checks
this flag is `convert_image`. Video/document/PDF conversions have no
metadata-stripping code path yet at all — see section H.

**11. `system_status` command** — implements item 16's suggested command
and backs the new Privacy & System Status panel (item 25) and an updated
`PrivacyBadge` tooltip. Every field is a fact derived from what's actually
registered/wired in this build (e.g. `automatic_updates_enabled: false`
because the updater plugin isn't registered, not because a flag says so).
The metadata field is named `metadata_removal_default_for_images`, not a
blanket claim, and the panel copy says explicitly that video/document/PDF
don't have this yet.

**12. Privacy claims made honest, not just added.** The existing
"100% Local" badge had a hardcoded, unmeasured "0 bytes uploaded" line —
removed; nothing in the codebase counts bytes, so it wasn't a real stat.

---

## C. Permissions removed

| Removed | Reason |
|---|---|
| `shell:allow-execute`, `shell:allow-spawn`, `shell:allow-stdin-write`, `shell:allow-kill`, `shell:allow-open` | Unused by frontend; all tool execution is Rust-side already |
| `tauri-plugin-shell` (plugin registration + crate + npm package) | Same — nothing used it |
| `tauri-plugin-updater`, `tauri-plugin-process` (+ npm equivalents) | Backed the removed silent auto-update flow |
| `tauri-plugin-opener` | Grep-verified unused |
| `tauri-plugin-notification` (+ npm equivalent) | Grep-verified unused; completion sound uses Web Audio instead |
| `fs:scope: ["$HOME/**", "$DOWNLOAD/**", "$DOCUMENT/**", "$DESKTOP/**", "$TEMP/**", "$RESOURCE/**"]` | Replaced with `[]`/minimal + runtime per-file grants |
| `fs:default`, `fs:allow-read`, `fs:allow-write`, `fs:allow-exists`, `fs:allow-mkdir`, `fs:allow-remove`, `fs:allow-rename`, `fs:allow-copy-file` | Frontend only calls `readFile`/`writeFile` |
| `assetProtocol.scope: ["**"]` | Replaced with `[]` + runtime per-file grants |
| `bundle.updater` / `plugins.updater` config (endpoints, pubkey) | Updater removed |
| `reqwest`, `futures-util`, `zip`, `chrono` (Cargo deps) | Grep-verified unused anywhere in `src-tauri/src` |

Kept: `dialog:*` (native pickers, actively used), `fs:allow-read-file` /
`fs:allow-write-file` (PDF editor, now scoped per-file at runtime instead
of statically), `core:window:*` (window chrome controls).

---

## D. Remaining security concerns

1. **`cargo check`/`cargo build` could not be run to completion in this
   environment** — the sandbox's Rust 1.75 toolchain is too old for the
   project's current dependency graph (multiple transitive crates now
   require edition2024, stabilized in Rust 1.85). Pinning a couple of
   leaf dependencies down worked (`notify-rust`, `home`), but the chain
   bottoms out at `tauri-utils` itself requiring a `time` version that
   needs edition2024 — meaning `tauri` would need to be downgraded several
   minor versions to fully resolve here, which risked introducing API
   differences I couldn't verify blind. **Run `cargo check` in your real
   dev environment as the literal first step before anything else** — see
   section F for exactly what was and wasn't independently verified.

2. **~18 other commands still take raw, unvalidated paths.** `rotate_pdf`,
   `add_watermark`, `pdf_to_images`, `images_to_pdf`, `resize_image`,
   `compress_image`, `crop_image`, `rotate_image`, `trim_video`,
   `extract_audio`, `compress_video`, `ocr_pdf`, `extract_archive`,
   `create_archive`, `apply_pdf_text_edits`, `fill_pdf_form_fields`,
   `edit_pdf_text_lopdf`, `search_replace_pdf_text` were deliberately left
   untouched rather than edited without the ability to compile-check them.
   See section H.

3. **The asset-protocol/fs-plugin runtime grant relies on
   `app.asset_protocol_scope()` and `FsExt::fs_scope()`**, which I
   confirmed exist in Tauri v2's public API via docs.rs source and an
   official skill doc, but could not compile against the real crate here.
   If `cargo check` reports these methods don't exist on your resolved
   Tauri version, the fallback is a narrower static `assetProtocol.scope`
   covering just common media folders (a regression for files outside
   those folders, but still far narrower than `["**"]`).

4. **Full architectural reorganization from item 21 was not done.**
   `commands.rs` (now ~1,900 lines) and `converter.rs` (~1,600 lines)
   were not split into `commands/` and `converters/` subdirectories; there
   is no `models/` directory. Only the new `security/` module was added in
   the suggested layout, since that's genuinely new capability. Splitting
   already-working files into new ones is pure-reorganization risk I
   can't compile-verify, and directly conflicts with "do not perform a
   massive rewrite if unnecessary" — flagging this as a deliberate scope
   decision rather than an oversight.

5. **No automated tests exist for this project** (confirmed true before
   Phase 1 too — CLAUDE.md says so explicitly). The new `security` module
   has 21 unit tests, verified passing against a real compiler in
   isolation (see section F), but there's no CI wiring anything into a
   test run yet.

6. **`download_tool` still opens external URLs in the system browser**
   (via the `open` crate, for "here's where to download FFmpeg" links).
   This is user-initiated (a button click), opens the *vendor's* official
   page, and is not a request the app itself makes — but it's worth
   knowing about if "must not make network requests" gets interpreted more
   strictly than "during startup or conversion."

---

## E. Build / run instructions

```bash
# Frontend — verified working in this environment (Node 22, npm 10)
npm install
npm run build          # tsc --strict, then vite build

# Rust / Tauri — NOT verified end-to-end here (see D.1). Needs Rust 1.85+
# (or whatever your real toolchain resolves the current dependency graph
# to) and, on Linux, libwebkit2gtk-4.1-dev + libgtk-3-dev + friends.
cargo check             # run this FIRST — fix anything it reports
npm run tauri dev       # dev mode
npm run tauri build     # production build (Windows NSIS/MSI is the target per item 27)
```

---

## F. Tests performed

**Frontend — real, complete verification.**
`npm install` + `npm run build` (`tsc` in strict mode — `noUnusedLocals`,
`noUnusedParameters` — then Vite production build) passed with **zero
errors**, twice, after the last two rounds of edits. This is genuine
end-to-end verification for every frontend change: no broken imports, no
type errors, no unused leftover state across ~500KB of bundled components.
(It also caught a real bug: `App.tsx` importing the by-then-deleted
`HistoryModal` — fixed before this report.)

**Security module core — real, compiler-verified, in isolation.**
`path_validation.rs`, `file_validation.rs`, and `temp.rs` don't depend on
any Tauri types, so I compiled and tested them in a standalone throwaway
Cargo project (just the `uuid` crate as a dependency) against this
sandbox's Rust 1.75. **21/21 tests passed**, covering: filename
sanitization against traversal (`../../evil.txt`), Windows reserved
device names, accepting real files/rejecting missing or directory paths,
`validate_output_dir` rejecting a file passed as a directory, the
"Save As" not-yet-created-file case, containment checks (`confirm_within`
accepting nested paths / rejecting paths outside root), unique per-job
temp dirs, `Drop`-based cleanup actually removing the directory, and
`move_into_place` performing a real file move.

**Rust/Tauri glue code — manual review only, not compiled.** `lib.rs`,
`commands.rs`, `converter.rs`, `fs_scope.rs`, and the `authorize_file`
call sites were checked by hand against exact signatures read directly
from the source (not assumed), including verifying — via direct
inspection, not sampling — that all 9 category converters in
`converter.rs` genuinely return the exact `output` path they were given
(the invariant `convert_file`'s temp-dir substitution depends on).
**This was not run through a real compiler.** Do not treat it as verified
until `cargo check` confirms it in a real environment.

**Manual/static checks:** grep-verified every claim about what's used vs.
unused (shell/updater/opener/notification/reqwest/etc.) rather than
assuming; confirmed `register_context_menu` uses `HKEY_CURRENT_USER` (no
admin); confirmed both `-c` occurrences in the codebase are fixed literals
in an argument array, not shell-interpolated strings; JSON-validated
`tauri.conf.json`, `capabilities/default.json`, and `package.json`.

---

## G. Conversions / features that still work

Everything that worked before Phase 1, as far as static analysis and the
passing frontend build can confirm — nothing in the conversion routing,
category dispatch, or external-tool argument construction was changed,
only wrapped:

- The full video/audio/image/document/spreadsheet/presentation/
  archive/vector/font routing in `convert_file` — same external tools,
  same arguments, same category dispatch logic.
- PDF merge, split, compress (now with validation + isolated temp dirs).
- The PDF editor (text editing, form fields, annotations) — now
  authorizes file access per-file instead of relying on a blanket
  `$HOME/**` grant.
- The video trimmer preview (`convertFileSrc`) — same mechanism, now
  scoped per-file via the same runtime grant as the PDF editor's files.
- Tool detection, GPU encoder detection, context menu registration
  (Windows), drag-and-drop, file-association startup files.
- Light/Dark theme (visually reworked to navy/MEB red — see below) plus
  the new System option, with live OS-scheme updates.
- The existing "100% Local" privacy badge (copy simplified, no
  functional change).

**Visually changed, functionally equivalent:** dark mode no longer uses
near-black (`#09090b`/`#000000`) — it's a navy ramp built from the
suggested `#07182B`/`#091D33`/`#102A46`/`#173A5E`/`#29445F` values. The
primary accent (buttons, active states, glow effects) is MEB red
(`#E30A17`) instead of violet/indigo. Per-category file-type badge colors
(e.g. the violet "ebook" badge) were deliberately left alone — recoloring
every category badge is a UI-redesign decision beyond what this phase
asked for, not a security or theme-infrastructure concern.

---

## H. Conversions / work that need Phase 2 (or before it)

- **Path validation is not universal yet.** The ~18 commands listed in
  section D.2 still take raw strings. Extending
  `security::path_validation` to them is mechanical (the pattern is now
  established) but real work, and needs to happen with compiler feedback
  available — attempting it blind here risked more than it was worth.
- **Temp-dir isolation likewise covers only `convert_file`, `merge_pdfs`,
  `split_pdf`, `compress_pdf`.** The other PDF/image/video/archive
  commands still write directly to their final destination.
- **Metadata stripping only exists for image conversions.** No
  video/document/PDF metadata-removal code path exists at all yet, so
  "metadata removal default: enabled" (item 13) is only true for one
  category — the Privacy panel says this explicitly rather than
  overclaiming.
- **Architectural reorganization** (`commands/`, `converters/`,
  `models/` subdirectories per item 21) not done — see D.4.
- **Windows-specific runtime behavior is unverified**: this was developed
  and reviewed on Linux; the `HKEY_CURRENT_USER` registry calls, the
  `\\?\` prefix normalization, and the NSIS/MSI bundle targets all need a
  real Windows smoke test before institutional deployment.
- **No automated CI** runs `cargo check`/`npm run build`/the security
  module's tests on push — worth adding given how much of this report's
  confidence rests on manual verification in one environment.

Phase 2 (PDF reconstruction) was not started, per the brief.
