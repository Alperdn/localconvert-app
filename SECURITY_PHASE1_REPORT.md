# LocalConvert — Phase 1: Secure Desktop Foundation
## Verification Report (real Windows environment)

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
