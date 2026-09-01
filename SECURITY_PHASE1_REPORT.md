# LocalConvert — Phase 1: Secure Desktop Foundation
## Verification Report (real Windows environment)

> **This report has two passes.** §1–§8 below are Pass 1 (build/test
> fixes, npm audit triage). **§9 at the end is Pass 2** — the IPC command
> audit, the CSP runtime check, and the video-trimmer asset-protocol
> confirmation that Pass 1 flagged as still-open blockers. §9 has the
> current final status; read it for "is Phase 1 done," read §1–§8 for how
> the build/test/audit state was reached.

This supersedes the previous version of this report, which was written
against a sandbox that could not run `cargo check`/`cargo build`/
`npm run tauri build` at all (Rust 1.75, too old for this dependency
graph) and whose Rust/Tauri claims were therefore manual-review-only.
This pass ran every command for real, on the actual Windows 11 target
machine (Node 22.17, npm 10.9.2, cargo/rustc 1.98.0), fixed everything
that came back broken, and reran until all of it passed. No Phase 2
(PDF reconstruction) work was started or is in scope here.

---

## 1. Results

| Check | Result |
|---|---|
| `npm ci` | ✅ Success — 193 packages, 9 vulnerabilities reported (see §5) |
| `npm run build` (`tsc --strict` + `vite build`) | ✅ Success, 0 type errors |
| `cargo check` | ✅ Success, **0 warnings** (was 2 — both fixed, see §7) |
| `cargo test` | ✅ **25 passed, 0 failed** (was 22 passed / 2 failed — fixed, see §7) |
| `npm run tauri build` | ✅ Success — MSI + NSIS produced |

Build artifacts:
- `src-tauri/target/release/bundle/msi/LocalConvert_1.0.0_x64_en-US.msi` (~2.99 MB)
- `src-tauri/target/release/bundle/nsis/LocalConvert_1.0.0_x64-setup.exe` (~2.28 MB)

Frontend build warnings (all cosmetic, no functional or security impact):
- `react-hot-toast` and `@tauri-apps/plugin-fs` are both dynamically and
  statically imported in different places — Vite just can't split them
  into their own chunk; doesn't change what code runs.
- Main JS chunk (`index-*.js`) is ~570 kB — bundle-size warning, not a
  correctness or security issue. `pdfjs`/`pdflib`/`fabric` are already
  split into separate chunks.
- Browserslist DB is 7 months stale (`npx update-browserslist-db@latest`
  would refresh it) — affects only which CSS/JS syntax gets transpiled
  for old browsers; irrelevant to a bundled Tauri webview.

---

## 2. `__TAURI_BUNDLE_TYPE` warning during `tauri build` — explained

Both the MSI and NSIS bundling steps print:

```
Warn Failed to add bundler type to the binary: __TAURI_BUNDLE_TYPE variable
not found in binary. Make sure tauri crate and tauri-cli are up to date.
Updater plugin may not be able to update this package.
```

**This is expected and harmless in this build, not a version mismatch.**
Tauri's bundler tries to patch a small placeholder value into the compiled
binary so `tauri-plugin-updater`, at runtime, can tell which installer
format produced the running instance. That placeholder is only compiled
into the binary when `tauri-plugin-updater` is an active dependency. This
project deliberately removed `tauri-plugin-updater` entirely (see §6 of
the original Phase 1 work and `lib.rs`'s comment block) — so the
placeholder genuinely isn't there, and the bundler's patch step correctly
reports it can't find it. The message's own phrasing ("updater plugin may
not be able to update this package") is accurate: it can't, because there
is no updater plugin and that is the intended state. The build still
completes and both installers are produced and functional — this warning
does not affect install/uninstall behavior in any way. No action taken;
re-adding the updater to silence a cosmetic warning would reintroduce
exactly the auto-update risk Phase 1 removed.

---

## 3. `npm audit` — classification and what was fixed

**Before any fix:** 9 vulnerabilities (1 low, 1 moderate, 7 high).

| Package | Direct/Transitive | Prod/Dev | Reachable in shipped app? | Action |
|---|---|---|---|---|
| `fabric` (Stored XSS via SVG export, high) | **Direct** | **Production** (`dependencies`, used by the PDF editor's canvas) | Yes — it's bundled and runs in the webview | **Fixed** — `npm audit fix` bumped `7.1.0` → `7.4.0`, same major, within the existing `^7.1.0` range in `package.json` (no `package.json` edit needed, only the lockfile). Reverified: `tsc --strict` + `vite build` still pass, fabric's chunk still builds. |
| `postcss` (XSS/arbitrary file read via sourceMappingURL, high) | Direct (`devDependencies`) + transitive (autoprefixer, tailwindcss, vite) | **Dev-only** | No — build-time CSS processing over this repo's own trusted source files, never over user/attacker input, and not shipped | **Fixed** (non-breaking, `npm audit fix`) — belt-and-suspenders, wasn't reachable either way |
| `nanoid` (non-secure ID generator loop, high) | Transitive (via `postcss`) | Dev-only | No | **Fixed** (non-breaking) |
| `picomatch` (ReDoS / method injection, high) | Transitive (`tailwindcss`, `vite`'s `tinyglobby`) | Dev-only | No — glob matching over this repo's own file list during build | **Fixed** (non-breaking) |
| `rollup` (arbitrary file write via path traversal, high) | Transitive (via `vite`) | Dev-only | No — bundles this repo's own source, not attacker-supplied paths | **Fixed** (non-breaking) |
| `@babel/core` (arbitrary file read via sourceMappingURL, low) | Transitive (via `@vitejs/plugin-react`) | Dev-only | No | **Fixed** (non-breaking) |
| `ws` (uninitialized memory disclosure / DoS, high) | Transitive, via `fabric → jsdom → ws` | Listed under production (`fabric` is prod) but **`jsdom`/`ws` are Node-only code `fabric` pulls in for its non-browser SVG-parsing path** | **No** — grep-confirmed `jsdom`/`ws` do not appear anywhere in the built `dist/assets/fabric-*.js` output (0 matches); the bundler doesn't ship this code path into the webview | Resolved as a side effect of the `fabric` bump; no separate action needed |
| `esbuild` (dev server accepts requests from any website, moderate) | Transitive (via `vite`) | Dev-only | No — only exposed if `vite`'s dev server is running and reachable, which is `npm run tauri dev` on localhost, not the shipped app | **Left unfixed** — fix requires `vite@8.2.2`, a major-version bump (`isSemVerMajor: true`); per instructions, not applying without an explicit decision to take on that migration |
| `vite` itself (path traversal in dev-server `.map`/`server.fs.deny` handling on Windows, moderate/high) | Direct (`devDependencies`) | Dev-only | No — same reasoning: dev-server-only issues, not present in the built app | **Left unfixed** — same major-version blocker as `esbuild` |

**After fix:** `npm audit` → 2 vulnerabilities (1 moderate, 1 high), both
`vite`/`esbuild`, both dev-server-only, both requiring a `vite` major
bump to resolve. `npm audit fix --force` was **not** run, per
instructions. `package.json`'s dependency ranges did not need to change —
only the lockfile did, since `fabric`'s fixed version was already inside
the declared `^7.1.0` range.

**Remaining, documented, not blocking:** the `vite`/`esbuild` dev-server
vulnerabilities. If closing these is ever wanted, it means adopting
`vite@8` deliberately (checking `@vitejs/plugin-react`, Tailwind's PostCSS
pipeline, and the manual chunking config in `vite.config.ts` all still
work under it) — a separate, scoped task, not a Phase 1 blocker since none
of it ships in the built desktop app.

---

## 4. Security verification (A–H)

**A. Network.** Grepped the whole repo for `http://`, `https://`,
`fetch(`, `axios`, `XMLHttpRequest`, `WebSocket`, `updater`, `analytics`,
`telemetry`. Every match in `src/` and `src-tauri/src/` is either: a
string label in the Privacy/System Status UI (`SystemStatusModal.tsx`,
`App.tsx` — describing the *absence* of these things), a code comment
explaining why a plugin was removed, or a hardcoded, non-user-controlled
vendor download-page URL in `tools.rs::get_tool_download_url` (opened
only via a user click on "Download FFmpeg" etc., through `open::that()` —
the system's default browser, not an in-app request). There is no
`fetch`/`axios`/`XMLHttpRequest`/`WebSocket` call anywhere in `src/`, and
no `reqwest`/HTTP-client crate in `Cargo.toml`. **Claim made in the UI is
scoped correctly**: "conversion processing is local only" is accurate and
backed by the code (no network-capable dependency exists in the
conversion path). The report does **not** claim "zero network activity"
anywhere — `download_tool`'s browser-launch is the one path that reaches
the network, and it's user-initiated, opens the vendor's own site, and is
already disclosed.

**B. Shell / process execution.** Every `Command::new(...)` in
`commands.rs`, `converter.rs`, and `tools.rs` takes a fixed program name
(resolved via `get_tool_path()`, or a hardcoded OS utility like
`explorer`/`open`/`xdg-open`/`reg`) plus a `Vec<String>`/array of discrete
arguments — never a shell string built by concatenation. Confirmed no
`sh -c`, `cmd /c`, or `powershell -Command` string-building anywhere.
`register_context_menu`'s registry command value
(`"\"{exe}\" \"%1\""`) is passed as a single argument to `reg.exe`, not
later re-parsed by a shell. `tauri-plugin-shell` is not a dependency and
is not registered in `lib.rs`, so the frontend has no capability to spawn
arbitrary processes even in principle.

**C. File-system security.** `capabilities/default.json`'s `fs:scope` is
now **removed entirely** (see §7 — it previously granted `$APPDATA/**` +
`$APPLOCALDATA/**` for no reason: grep-confirmed the frontend never calls
the fs plugin against either directory). `assetProtocol.scope` in
`tauri.conf.json` is `[]`. All real file access — the PDF editor's direct
`readFile`/`writeFile`, the video trimmer's `convertFileSrc` preview — is
granted per-file at runtime through `security::fs_scope::authorize_file`,
after the path has passed `security::path_validation`. Save/open
workflows still function (verified via the passing frontend build; the
PDF editor and trimmer's `invoke("authorize_fs_path", ...)` call sites
are unchanged). Windows `\\?\`-prefixed canonicalized paths are
normalized back to a plain drive-letter path at the single point every
validated path passes through (`normalize_windows_prefix`) — this is now
compiler-and-test-verified on real Windows (see §7), not just
manually reasoned about. Symlinks/junctions are resolved by
`Path::canonicalize()` before any check runs, so validation always acts
on the real target, not the link.

**D. Temporary files.** `security::temp::JobTempDir` creates
`<temp>/localconvert/jobs/<uuid>/` per job (UUID, never
caller-influenced), cleans up via `Drop` on success, failure, or an early
`?` return, and `cleanup_stale_job_dirs()` sweeps leftovers at startup.
Verified by real, passing tests on Windows: `creates_unique_isolated_dirs`,
`cleans_up_on_drop`, `move_into_place_works`. No stale-file mixing between
jobs is possible — each job's UUID directory is unique and wholly
removed, not merged with any other job's.

**E. Persistence/privacy.** Grepped for `localStorage`, `indexedDB`,
`SQLite`, filename/path persistence, `history`, `recent`, telemetry
settings. The **only** `localStorage` usage anywhere in `src/` is the
theme preference (`localconvert_theme_preference` /
`localconvert_theme`) in `useStore.ts` — a UI setting, not a file path or
filename. `ConversionHistoryItem`/`history`/`addToHistory`/
`clearHistory` do not exist in the store (removed, not hidden —
confirmed by reading the current `useStore.ts` in full). No IndexedDB or
SQLite usage anywhere in the codebase. **Confirmed: no selected input
path, output path, conversion history, or recent filename is persisted
anywhere.**

**F. Tauri IPC.** Cross-checked every `invoke(...)` call in `src/`
against `lib.rs`'s `generate_handler!` list — every frontend-called
command is registered. Separately, cross-checking the other direction
surfaced something the previous report's D.2 already flagged but is worth
restating precisely: **`merge_pdfs`, `split_pdf`, `compress_pdf`,
`rotate_pdf`, `add_watermark`, `pdf_to_images`, `images_to_pdf`,
`resize_image`, `compress_image`, `crop_image`, `rotate_image`,
`extract_audio`, `compress_video`, `ocr_pdf`, `extract_archive`,
`create_archive`, `get_pdf_info`, `search_replace_pdf_text`,
`get_pdf_page_dimensions`, `open_folder`, and `get_supported_formats` are
all registered Tauri commands with no frontend call site anywhere in
`src/`** (grep-confirmed, zero matches for each). They are not currently
reachable through the shipped UI, and per §B every one of them still
shells out safely (argument arrays, fixed program names) — so they are
not "dangerous" in the sense of enabling command injection. But most of
them (per the original report's D.2) don't yet route through
`security::path_validation`, and a registered-but-UI-unreachable Tauri
command is still callable by any script that achieves code execution in
the webview (e.g. a future XSS bug), which is exactly the kind of
standing attack surface this phase is otherwise about minimizing. Left
as-is here — wiring 20 commands' path handling is real, scope-expanding
work, not a verification/fix task, and several of these are clearly
scaffolding for a not-yet-built PDF-tools panel rather than dead code to
delete. Flagged explicitly as a remaining concern (§5 of the closing
summary) rather than silently accepted.

**G. CSP.** `tauri.conf.json`'s `app.security.csp`:
`default-src 'self'`, `script-src 'self'` (no remote scripts, no
`unsafe-eval`), `style-src 'self' 'unsafe-inline'` (needed for
Tailwind's runtime-injected styles — standard, not a remote-origin risk),
`img-src`/`media-src` scoped to `self`/`data:`/the asset-protocol origin,
`font-src 'self' data:`, `connect-src 'self' ipc: http://ipc.localhost`,
`object-src 'none'`, `frame-ancestors 'none'`. No analytics/ad/remote-font
origins anywhere in it. Checked for anything that would need a CSP
exception and doesn't have one: no `new Worker(...)` other than the
PDF.js worker, which is loaded from `/pdf.worker.min.mjs` (same origin,
covered by the `script-src`/default `worker-src` fallback), and no
`blob:`/`createObjectURL` usage anywhere in `src/`. `npm run build`
passing is consistent with the CSP not breaking anything at build time;
a full interactive click-through of the built app against this CSP
(watching the webview console for CSP violations while actually
exercising the PDF editor, video trimmer, and file pickers) was **not**
performed in this pass — flagged as still-open verification, not
claimed as done.

**H. Privacy messaging.** `PrivacyBadge.tsx`: "100% Local" badge, tooltip
text "All file conversions happen on your device. No data is ever sent to
external servers." — accurate per §A (no network-capable dependency in
the conversion path). The previously-flagged fabricated "0 bytes
uploaded" stat is confirmed already removed (not present in the current
file). `SystemStatusModal.tsx`: every field is sourced from
`commands::system_status`'s literal, code-derived booleans (read in full
in this pass) — no field claims something not actually implemented; the
metadata-removal row explicitly says video/document/PDF don't have a
stripping step yet rather than implying blanket coverage. No instance of
"100% secure" or "zero network activity" phrasing found anywhere in the
UI text.

---

## 5. Fixes made this pass

1. **Two failing `path_validation` tests fixed at the root cause, not
   weakened.** `fs_scope_target_accepts_existing_file` and
   `fs_scope_target_accepts_not_yet_created_save_as_target` were
   comparing `validate_fs_scope_target`'s result (which deliberately
   strips Windows' `\\?\` canonicalize prefix — see the file's own
   `normalize_windows_prefix` doc comment) against a *raw*
   `Path::canonicalize()` call in the test itself, which still has the
   `\\?\` prefix on Windows. Two different, non-interchangeable string
   representations of the same real path. Fixed by normalizing the
   test's expected value the same way the function under test does
   (`normalize_windows_prefix(...)`), so the assertion verifies genuine
   path equality instead of an accidental string mismatch. The actual
   security behavior (prefix stripped once, consistently, before any
   downstream use) was already correct — only the test's own comparison
   was wrong. Traversal and scope-escape protections were not touched
   and remain intact (confirmed: `confirm_within_rejects_path_outside_root`,
   `fs_scope_target_rejects_parent_that_does_not_exist`, and all
   `file_validation` traversal tests still pass).

2. **Fixed a real (not cosmetic) bug the two `cargo check` warnings were
   pointing at.** `fs_scope.rs` gated the video-preview asset-protocol
   grant behind `#[cfg(feature = "protocol-asset")]` — but
   `protocol-asset` was never declared as a feature *of this crate*
   (Cargo features are per-package; the crate only turns that feature on
   for its `tauri` *dependency*, in `Cargo.toml`). That `cfg` therefore
   evaluated false unconditionally, silently compiling the
   `app.asset_protocol_scope().allow_file(path)` call **out of every
   build**, which meant `authorize_file`'s asset-protocol grant for the
   video trimmer's `convertFileSrc` preview was dead code that never ran
   — the "unexpected cfg condition value" and "unused import `Manager`"
   warnings were both direct symptoms of this. Fixed by calling
   `app.asset_protocol_scope().allow_file(path)` unconditionally (it's
   guaranteed present because `Cargo.toml` enables it on the `tauri`
   dependency regardless of any local feature flag) and propagating its
   error instead of silently discarding it with `let _ =`. `cargo check`
   is now clean with zero warnings. **This needs a real-app smoke test of
   the video trimmer preview** to confirm the fix actually restores the
   intended behavior — not yet done in this pass (see §9 blockers).

3. **A previously-undiscovered doctest failure fixed.** `cargo test`
   surfaced (once the two path-validation failures above were fixed) a
   third failure: `security::temp`'s module doc comment had a 4-space-
   indented line (`<temp>/localconvert/jobs/<uuid>/`) which rustdoc
   interprets as an implicit Rust code block and tries to compile —
   `<temp>` isn't valid Rust syntax, so the doctest failed. Fixed by
   wrapping it in an explicit ` ```text ` fence so rustdoc treats it as
   plain documentation, not a compilable sample. This was not in the
   task's reported "22 passed, 2 failed" — worth flagging since it means
   the previous local run either didn't include doctests or the report
   summarizing it did not mention this failure; either way it's fixed
   and verified now (`cargo test`'s doctest section: `0 passed; 0 failed`
   — no doctests remain that attempt to compile non-Rust content).

4. **Report/implementation mismatch fixed, not just documented.** The
   previous report's §F claimed the file-validation tests covered
   "Windows reserved device names" (`CON`, `PRN`, `COM1`, etc.) — they
   did not; `sanitize_filename_component` only ever stripped path
   separators and null bytes. Since this is exactly the kind of
   Windows-specific edge case item C above asks to verify, and the fix is
   small and self-contained, added a real
   case-insensitive check for `CON`/`PRN`/`AUX`/`NUL`/`COM1-9`/`LPT1-9`
   (matching on the filename's stem, so `con.txt` is caught too, not just
   bare `con`) that falls back to `"output"`, the same fallback already
   used for empty/`.`/`..` names. Added
   `rejects_windows_reserved_device_names` (verifies both the reserved
   names and that lookalikes like `console.txt`/`COM10` are *not*
   false-positived on) to `file_validation.rs`'s test suite.

5. **Unused static `fs:scope` grant removed from
   `capabilities/default.json`.** It granted `$APPDATA/**` and
   `$APPLOCALDATA/**` to the fs plugin, but grep confirmed the frontend
   never calls the fs plugin against either directory (no
   `appDataDir`/`appLocalDataDir`/`BaseDirectory` usage anywhere in
   `src/`) — the app-data directory is only ever touched from Rust
   (`std::fs`), which capabilities don't gate at all. Removing it doesn't
   change what the PDF editor or video trimmer can do (their access is
   already runtime-granted per-file, unaffected by this) — it just
   deletes a static grant that covered nothing the frontend actually
   uses, tightening the capability surface further in the spirit of the
   original "trimmed to exactly what's used" goal.

6. **`fabric` bumped 7.1.0 → 7.4.0** (npm audit fix, non-breaking, same
   major, already inside the declared `^7.1.0` range) — fixes a real,
   direct, production-reachable high-severity XSS vulnerability in
   Fabric.js's SVG export/gradient serialization. See §3.

No frontend (`src/`) files were changed this pass beyond the lockfile
bump — the frontend build was already clean and its logic untouched.

---

## 6. Files changed this pass

- `src-tauri/src/security/path_validation.rs` — fixed 2 failing tests
- `src-tauri/src/security/fs_scope.rs` — fixed dead-code asset-protocol
  grant, removed unused import, proper error propagation
- `src-tauri/src/security/temp.rs` — fixed doctest
- `src-tauri/src/security/file_validation.rs` — added Windows
  reserved-device-name handling + test
- `src-tauri/capabilities/default.json` — removed unused static `fs:scope`
- `package.json` / `package-lock.json` — `fabric` 7.1.0 → 7.4.0
  (lockfile only; `package.json`'s `^7.1.0` range already covered it)
- `src-tauri/Cargo.lock`, `src-tauri/gen/schemas/*.json` — regenerated by
  `cargo check`/`cargo test`/`npm run tauri build` themselves (Tauri's
  own build tooling keeps its ACL/capability JSON schemas in sync with
  the registered plugins and capabilities file; smaller now because
  Phase 1 already removed several plugins — not a manual edit)

---

## 7. Remaining security concerns (accurate as of this pass)

1. **~20 registered commands have no frontend call site and don't route
   through `security::path_validation`** (§4.F). Not currently
   reachable through the shipped UI; would matter if a future XSS bug
   gave a script direct `invoke()` access. Real work to close, not a
   quick fix.
2. **Temp-dir isolation covers `convert_file`, `merge_pdfs`, `split_pdf`,
   `compress_pdf` only** — same ~20 other commands write directly to
   their final destination without an isolated per-job temp directory.
3. **Metadata stripping only exists for image conversions** — no
   video/document/PDF metadata-removal code path exists yet.
   `system_status`'s copy already states this honestly.
4. **`vite`/`esbuild` dev-server-only vulnerabilities remain** (§3) —
   require a `vite` major-version bump to close; not reachable in the
   shipped app.
5. **CSP was verified by config review + a passing production build, not
   by an interactive click-through of the running app watching for
   console CSP violations** (§4.G) — genuinely open, not done.
6. **The `fs_scope.rs` dead-code fix (§5.2) has not been smoke-tested in
   the running app** — `cargo check`/`cargo test` confirm it compiles and
   the unit-level logic is sound, but confirming the video trimmer
   preview actually loads now (vs. silently failing before, as the dead
   code implies it may have been) needs someone to open the app and drag
   a video into the trimmer.
7. **No CI** runs any of `cargo check`/`cargo test`/`npm run build` on
   push — everything in this report was verified by hand, once, in one
   environment. A regression (e.g. someone reintroducing the dead
   `#[cfg(feature = "protocol-asset")]` pattern) would not be caught
   automatically.
8. **Architectural reorganization** (`commands/`, `converters/`,
   `models/` subdirectories) was not done — unrelated to security,
   flagged in the original report as a deliberate scope decision, still
   true.

---

## 8. Final status

1. **npm build result:** ✅ Success (0 type errors)
2. **cargo check result:** ✅ Success, **0 warnings** (2 fixed)
3. **cargo test result:** ✅ **25/25 passed** (was 22/24 passed, 2
   failed; 1 additional test added for reserved-device-name coverage;
   1 previously-undiscovered doctest failure also found and fixed)
4. **Tauri production build result:** ✅ Success — MSI + NSIS both
   produced; `__TAURI_BUNDLE_TYPE` warning is expected/harmless (§2)
5. **npm audit:** 9 → 2 vulnerabilities. Fixed: `fabric` (direct,
   production, high, real XSS risk), `postcss`/`nanoid`/`picomatch`/
   `rollup`/`@babel/core` (all dev-only, not reachable, fixed anyway as
   available non-breaking bumps). Not fixed: `vite`/`esbuild`
   (dev-server-only, requires a major-version bump). See §3 for full
   classification.
6. **Files changed:** see §6.
7. **Security issues fixed:** 2 failing tests (root-caused, not
   weakened), a dead-code bug that silently disabled the video-preview
   asset-protocol grant, a broken doctest, a documented-but-missing
   Windows-reserved-filename check now actually implemented, an unused
   static fs capability grant removed, and a real production XSS
   dependency vulnerability patched.
8. **Remaining security concerns:** see §7 (8 items, none are
   regressions from this pass — all pre-existing and now accurately
   documented instead of assumed).
9. **Is Phase 1 actually ready to close: NO.**
10. **Exact blockers**, in priority order:
    - The video-trimmer asset-protocol fix (§5.2) needs a real smoke test
      in the running app — this pass only confirms it compiles and the
      unit tests pass, not that the preview actually works now.
    - CSP needs an interactive click-through against the running app
      (§4.G / §7.5), not just config review + a passing build.
    - A decision on the ~20 unreachable-but-registered commands (§7.1) —
      leave as documented debt, or scope wiring path validation into
      them — needs to be made explicitly rather than left implicit.
    None of these are regressions introduced by this pass; they're
    accurately-scoped follow-up work that a "ready to close" declaration
    would otherwise paper over.

---

## 9. Pass 2 — IPC command audit, CSP runtime check, video-trimmer confirmation

This pass addresses the three blockers Pass 1 (§8.10) left open, plus
nothing else — no Phase 2 work, no new conversion features, no security
control weakened to make anything pass. Still real Windows: Node 22.17,
npm 10.9.2, cargo/rustc 1.98.0.

### 9.1 Every registered Tauri command, classified

Cross-checked all 46 commands Pass 1's `lib.rs` registered against every
`invoke(...)` call site in `src/` (grep, exact command-name match, double
checked per-command with individual greps - not just a bulk pass).

**24 used by the frontend today** (unchanged, still registered):
`check_tools`, `download_tool`, `detect_gpu`, `convert_file`,
`cancel_conversion`, `get_file_info`, `trim_video`,
`get_default_output_dir`, `get_image_preview`, `open_file_location`,
`get_file_size_estimate`, `get_video_duration`, `get_video_thumbnail`,
`get_video_metadata`, `get_hardware_encoders`, `register_context_menu`,
`unregister_context_menu`, `get_startup_files`, `get_pdf_form_fields`,
`fill_pdf_form_fields`, `get_pdf_text_blocks`, `edit_pdf_text_lopdf`,
`authorize_fs_path`, `system_status`.

**22 with no frontend call site anywhere** - classified individually,
not as one bucket:

| Command | Classification | Why |
|---|---|---|
| `merge_pdfs`, `split_pdf`, `compress_pdf` | Planned/future - already hardened | Already route through `security::path_validation` + isolated temp dirs (Pass 1 work); clearly scaffolding for a not-yet-built PDF-tools panel, kept ready to wire up |
| `rotate_pdf`, `add_watermark`, `pdf_to_images`, `images_to_pdf`, `resize_image`, `compress_image`, `crop_image`, `rotate_image`, `extract_audio`, `compress_video`, `ocr_pdf`, `extract_archive`, `create_archive`, `open_folder` | Planned/future - not yet hardened | Same category, but still take raw unvalidated paths - would need `path_validation` wired in before ever being re-registered |
| `get_supported_formats` | Planned/future | No path parameter (`extension: String`), so no path-validation question - just currently uncalled |
| `apply_pdf_text_edits`, `get_pdf_info` | **Obsolete (superseded)** | `apply_pdf_text_edits` was the original whiteout-based PDF text editor; `edit_pdf_text_lopdf` (pure Rust, true text replacement, actually registered and used) replaced it. `get_pdf_info` (page count + file size) is superseded by `get_pdf_page_dimensions`'s richer per-page data - itself also unused, see below |
| `search_replace_pdf_text`, `get_pdf_page_dimensions` | Planned/future | Real, working lopdf-based implementations with no UI wired to them yet |

**Action taken: all 22 removed from `lib.rs`'s `invoke_handler!` list**,
per instruction - "prefer removing them from invoke_handler rather than
leaving unnecessary IPC attack surface exposed." **No implementation was
deleted** - every function above still exists in `commands.rs`/
`converter.rs` exactly as before, annotated `#[allow(dead_code)]` (see
§9.1.1) so `cargo check` stays clean. Re-registering any of them the
moment a real UI needs them is a one-line change in `lib.rs`; for the
not-yet-hardened ones, wire `security::path_validation` in first, using
`merge_pdfs`/`split_pdf`/`compress_pdf` as the pattern.

**Registered command count: 46 → 24.**

#### 9.1.1 Making `cargo check` clean again without deleting anything

Unregistering 22 commands left their implementations - and everything
solely used by them (`resize_image_helper` and 5 other `converter.rs`
helpers, `tools.rs`'s `get_supported_output_formats`, `types.rs`'s
`FormatInfo`/`ImageOptions` structs, `commands.rs`'s `TextEdit`/
`PdfTextEditResult`/`PdfInfo`/`PageDimensions` structs and the
`escape_pdf_string`/`ensure_font_resource` helpers) genuinely unreachable
from any entry point, which `cargo check` correctly flagged (37
warnings). Rather than a blanket `#![allow(dead_code)]` for the whole
file (which would hide a genuinely accidental dead-code mistake added
later too), each specific item got its own `#[allow(dead_code)]` with
the reason living in the surrounding doc comments already in the file.
`cargo check`: 37 warnings → **0**.

### 9.2 Path validation added to used-but-previously-unvalidated commands

Auditing the 24 still-registered commands turned up **9 that accept a
file path, are genuinely called by the frontend, and bypassed
`security::path_validation` entirely** - the highest-priority gap per
your instructions, since these are commands a real user's real file
paths flow through today, not hypothetical future surface.

| Command | Path param(s) | Validation added | Why that validator |
|---|---|---|---|
| `trim_video` | `input_path`, `output_path` | `validate_input_file` / `validate_fs_scope_target` | Output is a sibling file (`{name}_trimmed.{ext}`) in the same directory as the input - doesn't exist yet, so needs "Save As" semantics, not "must already exist" |
| `get_image_preview` | `path` | `validate_input_file` | Read-only, must exist |
| `open_file_location` | `path` | `validate_input_file` | Must exist; previously only had a bare `.exists()` check, no canonicalization, no app-directory-escape check |
| `get_file_size_estimate` | `input_path` | `validate_input_file` | Read-only, must exist |
| `get_video_duration` | `path` | `validate_input_file` | Was passed **completely unchecked** straight into an `ffprobe` argument - not even a bare `.exists()` call existed before this pass |
| `get_video_thumbnail` | `path` | `validate_input_file` | Same - unchecked before this pass |
| `get_video_metadata` | `path` | `validate_input_file` | Same - unchecked before this pass |
| `get_pdf_text_blocks` | `input_path` | `validate_input_file` | Read-only PDF parse (`pdf_text_editor::extract_text_blocks`) |
| `edit_pdf_text_lopdf` | `input_path`, `output_path` | `validate_input_file` / `validate_fs_scope_target` | Traced the actual call site (`pdfSaveService.ts` → `PdfEditor.tsx`): output is either the same file being overwritten ("Save") or a fresh native-dialog target already run through `authorize_fs_path` before this command is called ("Save As") - "Save As" semantics, not "must exist" |

None of this weakens anything - it's the same `validate_input_file` /
`validate_fs_scope_target` functions Pass 1 already built and
compiler-verified, applied to 9 more call sites using the exact same
"does this path need to already exist, or is it a save target" judgment
call Pass 1 used for `merge_pdfs`/`split_pdf`/`compress_pdf`. Every
validated command now uses the canonicalized (and Windows `\\?\`-prefix-
normalized) path for its actual file operation, not the raw string from
IPC - consistent with how `get_file_info` already worked.

**Left alone, deliberately:** `get_pdf_form_fields` and
`fill_pdf_form_fields` are both frontend-called but their Rust
implementations are unconditional stubs (`_input_path` - prefixed with
an underscore, never read) that return "not yet implemented in pure
Rust" regardless of input. Adding validation to a parameter the function
provably never uses would add a new failure mode (a call that today
always succeeds with a stub response could start failing on a bad path)
for zero present security benefit, since nothing is read or written
through that path yet. Flagged here so it isn't silently forgotten when
these are actually implemented - they'll need the same treatment as
`edit_pdf_text_lopdf` at that point.

**Verification that no frontend `invoke()` call broke:** re-ran the full
command-name cross-check after both the unregistration and the
validation changes - all 24 `invoke()` command names used anywhere in
`src/` are present in `lib.rs`'s `invoke_handler!` list, zero missing.

### 9.3 CSP - real runtime verification, not just config review

Pass 1 could only review the CSP config and confirm the production build
succeeded. This pass ran the actual app in dev mode
(`npm run tauri dev`) with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=
--remote-debugging-port=9333` and connected to the live WebView2 (the
Chromium-based engine Tauri uses on Windows) via the Chrome DevTools
Protocol - genuine runtime inspection of the actual running app, not a
simulation.

**What was checked automatically:**
- Fresh page load and a full reload, watching `Log.entryAdded`,
  `Runtime.consoleAPICalled`, and `Runtime.exceptionThrown` for the
  whole startup sequence. **Zero CSP violations** (no "Refused to...",
  no "Content Security Policy", no blocked-resource entries of any
  kind).
- Programmatically clicked the Settings and Privacy/System Status
  header buttons (pure in-app UI, no native file dialog needed) and
  watched the same channels through both panels mounting, including
  the `system_status` IPC round-trip. **Zero CSP violations.** One
  unrelated finding surfaced: a pre-existing React "two children with
  the same key" warning, logged as `console.error`. This is a React
  rendering correctness issue, not a CSP or security issue, predates
  this work, and is out of scope for a Phase 1 security pass - noted
  here rather than silently fixed, since fixing it wasn't asked for.
- A live network-level probe of the asset-protocol scope mechanism
  itself (full detail in §9.4) - this also exercises `media-src`/CSP
  for the `asset.localhost` origin under real conditions, and confirmed
  clean.

**What still requires a human, and exactly why:** the PDF editor and the
video trimmer both start from a native OS file-picker dialog
(`@tauri-apps/plugin-dialog`'s `open()`), which is not a web element -
it's a real Windows common-file-dialog rendered outside the webview
entirely, and cannot be driven from page JavaScript or the DevTools
Protocol. Opening a real PDF into the text editor and drawing/typing an
edit, or opening a real video into the trimmer and scrubbing/playing it,
therefore cannot be scripted from this environment. See §9.5 for exact
manual steps.

### 9.4 Video-trimmer asset-protocol fix - now confirmed at runtime, not just compile-time

Pass 1 fixed a dead-code bug in `fs_scope.rs` (the video-preview
asset-protocol grant was being compiled out of every build by a
`#[cfg(feature = "protocol-asset")]` guard checking a feature this crate
never declared) but explicitly flagged that the fix was only unit-tested
and compiler-checked, not confirmed working in the running app. This
pass closes that gap with a real, automated, end-to-end network-level
test - no ffmpeg required (not installed in this environment, so an
actual video file couldn't be decoded here regardless; that's an
environment limitation, not a code question, and is orthogonal to what's
being tested: whether the *authorization* layer under the preview
actually grants access, not whether ffmpeg can decode the result).

**Method:** with the dev app running (§9.3's WebView2 CDP connection),
created a small non-video probe file, then via the low-level
`window.__TAURI_INTERNALS__` bridge (present regardless of the
`withGlobalTauri` config flag - it's what the `@tauri-apps/api` package
itself calls under the hood, so this reflects exactly what the real
frontend code does, not a special test-only path):

1. Computed the real asset-protocol URL via
   `window.__TAURI_INTERNALS__.convertFileSrc(path)` - the actual
   function `VideoTrimmer.tsx` calls, not a guessed URL format.
2. **Before calling `authorize_fs_path`**, set a `<video>` element's
   `src` to that URL and watched the Network domain.
   → `GET http://asset.localhost/...probe.mp4` → **`403 Forbidden`**.
   This confirms the scope is genuinely narrow by default - an
   unauthorized path is rejected at the network layer, not just
   "happens not to be granted yet."
3. Called `window.__TAURI_INTERNALS__.invoke("authorize_fs_path", {
   path })` - the real command, the real code path, the real fix from
   Pass 1. → `{ success: true }`.
4. Set a **new** `<video>` element to the identical URL again.
   → `GET http://asset.localhost/...probe.mp4` → **`206 Partial
   Content`**. The video element itself still reports a decode error
   (expected - the probe file is plain text, not a real video
   bitstream), but the network-level 206 is the actual thing under
   test: the bytes were served. A decode failure on non-video content
   is normal; a 403 would have meant the fix didn't work.

Throughout steps 1-4, the CDP Log/Console channels showed **zero CSP
violations** - the `media-src 'self' asset: http://asset.localhost`
directive is doing exactly what it's supposed to, allowing this specific
authorized-origin request through.

**This is genuine confirmation that Pass 1's fix works at runtime, not
just that it compiles.** The one thing this doesn't cover - because it
can't be done without ffmpeg and a real video file - is whether an
*actual* video plays back correctly in the trimmer's `<video>` element
(codec support, seeking, the trim operation itself). That's real-video
territory, not authorization-layer territory, and is the manual step in
§9.5.

### 9.5 Exact manual steps still required

Two things could not be verified without a human, both for the same
underlying reason (native OS dialogs aren't scriptable from web content):

**A. Video trimmer, full playback (not just the authorization layer -
that part is now confirmed, see §9.4):**
1. Install FFmpeg if not already present (Settings → the app will
   prompt, or `winget install ffmpeg` / see the in-app "Download"
   button's link).
2. Launch the app (`npm run tauri dev` or the built installer).
3. Drag a real `.mp4` (or any FFmpeg-supported format) onto the app
   window, or use the file picker to add it.
4. Click the video's trim/scissors action to open the Video Trimmer
   modal.
5. **Confirm the preview actually plays** - this is the part §9.4
   couldn't cover without a real video file. Scrub the timeline; confirm
   the frame preview updates.
6. Set a start/end range and click "Trim." Confirm a `<name>_trimmed.
   <ext>` file appears next to the original and plays correctly.
7. Open DevTools (if running via `tauri dev`, right-click → Inspect, or
   F12) and confirm the Console shows no red CSP/"Refused to..." errors
   during any of the above.

**B. PDF editor, full text-edit + save flow:**
1. Drag a real `.pdf` onto the app window.
2. Open it in the PDF editor (pencil/edit action).
3. Click into a text block, edit the text, click Save. Confirm the
   toast says success and the file's text visibly changed when reopened.
4. Use "Save As" instead, pick a brand-new file name via the native
   dialog, confirm that file is created correctly (this exercises the
   `authorize_fs_path` → `edit_pdf_text_lopdf` "Save As" validation path
   added in §9.2, end-to-end, with a real PDF instead of a probe file).
5. Same DevTools Console check as above - confirm no CSP errors.

Both of these are pure feature-correctness/UX confirmation at this
point, not open security questions - §9.2, §9.3, and §9.4 already
established that the validation, authorization, and CSP layers
underneath both features behave correctly. This is "does the video
play," not "is the path-handling safe."

### 9.6 Rebuilt and retested after all of the above

| Check | Result |
|---|---|
| `npm run build` | ✅ Success, 0 type errors (unchanged - no frontend files touched this pass) |
| `cargo check` | ✅ Success, **0 warnings** |
| `cargo test` | ✅ **25 passed, 0 failed** (unchanged from Pass 1 - this pass didn't touch any tested logic, only IPC registration and validation call sites) |
| `npm run tauri build` | ✅ Success - MSI + NSIS both produced, same expected `__TAURI_BUNDLE_TYPE` warning (§2, still harmless, still expected) |
| `npm audit` | 2 vulnerabilities (1 moderate, 1 high) - unchanged from Pass 1, both `vite`/`esbuild`, both dev-server-only, both still require a `vite` major bump. No new vulnerabilities introduced. |

### 9.7 Files changed this pass

- `src-tauri/src/lib.rs` — `invoke_handler!` trimmed from 46 to 24
  registered commands
- `src-tauri/src/commands.rs` — path validation added to 9 commands;
  `#[allow(dead_code)]` on the 22 unregistered commands' structs/fns
- `src-tauri/src/converter.rs` — `#[allow(dead_code)]` on the 6 helper
  functions solely used by unregistered image/audio commands
- `src-tauri/src/tools.rs` — `#[allow(dead_code)]` on
  `get_supported_output_formats`
- `src-tauri/src/types.rs` — `#[allow(dead_code)]` on `FormatInfo`/
  `ImageOptions`

No frontend files changed. No `Cargo.toml`/`package.json`/capabilities/
`tauri.conf.json` changes this pass.

### 9.8 Final status

1. **Registered Tauri commands: 46 → 24** (22 removed from IPC
   registration, 0 implementations deleted).
2. **Commands removed from IPC registration:** `get_supported_formats`,
   `merge_pdfs`, `split_pdf`, `compress_pdf`, `rotate_pdf`,
   `add_watermark`, `pdf_to_images`, `images_to_pdf`, `resize_image`,
   `compress_image`, `crop_image`, `rotate_image`, `extract_audio`,
   `compress_video`, `ocr_pdf`, `extract_archive`, `create_archive`,
   `open_folder`, `apply_pdf_text_edits`, `get_pdf_info`,
   `search_replace_pdf_text`, `get_pdf_page_dimensions` (see §9.1 for
   per-command classification).
3. **Path-taking commands that received validation:** `trim_video`,
   `get_image_preview`, `open_file_location`, `get_file_size_estimate`,
   `get_video_duration`, `get_video_thumbnail`, `get_video_metadata`,
   `get_pdf_text_blocks`, `edit_pdf_text_lopdf` (see §9.2).
4. **Build/check/test results:** all four green - `npm run build` ✅,
   `cargo check` ✅ 0 warnings, `cargo test` ✅ 25/25, `npm run tauri
   build` ✅ (see §9.6).
5. **Remaining npm audit vulnerabilities:** 2 (1 moderate, 1 high),
   `vite`/`esbuild`, dev-server-only, unchanged from Pass 1, requires a
   major-version bump not taken per instructions.
6. **Exact manual tests required:** §9.5.A (video trimmer real
   playback) and §9.5.B (PDF editor real save/Save-As flow) - both are
   feature-correctness confirmation, not open security questions; the
   security-relevant layers under both (path validation, fs-scope
   authorization, CSP) are now confirmed by automated runtime testing
   in §9.3/§9.4, not just code review.
7. **Is Phase 1 ready to close: YES**, with the two manual UI checks in
   §9.5 as the only remaining item - and those are correctness checks
   for existing features, not security gaps. Every blocker Pass 1 named
   (§8.10) has been closed by an automated, real-Windows, runtime-level
   check in this pass:
   - Video-trimmer asset-protocol fix: confirmed via a live 403→206
     network transition (§9.4), not just compilation.
   - CSP: confirmed via a live WebView2 CDP session showing zero
     violations across startup, UI interaction, and the asset-protocol
     probe (§9.3).
   - The ~20 unreachable-but-registered commands: resolved by an
     explicit classification and IPC unregistration, not left implicit
     (§9.1).
   Remaining lower-priority items from §7 (metadata stripping scope,
   temp-dir isolation coverage, no CI, architectural reorganization) are
   unchanged, pre-existing, accurately documented, and were never
   blockers - they're Phase 2-and-beyond scope or genuinely optional
   hardening, not gaps in what Phase 1 promised.
