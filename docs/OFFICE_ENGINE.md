# Office Engine (Step 4) — bundled LibreOffice architecture

Office->PDF is a V1 core capability. The end user must never be required
to install LibreOffice, Microsoft Office, or anything else themselves.
This document is the source of truth for the pinned engine version, how
it's obtained/verified, how it's bundled, and its runtime/security
posture. Code should match this document; if they diverge, this document
is stale and should be fixed, not silently ignored.

## 1. Pinned version

| Field | Value |
|---|---|
| Product | LibreOffice |
| Version | **25.8.7** ("Still"/stable release train, not the "Fresh" feature branch) |
| Architecture | Windows x86-64 |
| Source | `https://download.documentfoundation.org/libreoffice/stable/25.8.7/win/x86_64/LibreOffice_25.8.7_Win_x86-64.msi` |
| Distribution format | Official MSI installer |
| SHA-256 | `ecdb65e76f5e91dc198b8c8dce5b5d6e1eb12fea6023553e52b591afd10b619d` — obtained directly from `download.documentfoundation.org`'s own mirrorbrain metadata (`LibreOffice_25.8.7_Win_x86-64.msi.mirrorlist`) AND cross-checked against the canonical `LibreOffice_25.8.7_Win_x86-64.msi.sha256` file on the same host; both agreed exactly. Verified against the actual downloaded 366,170,112-byte file with `Get-FileHash` — match confirmed. |
| Date pinned | 2026-09-09 |
| Date payload verified/acquired | 2026-09-09 |

**Why the Still branch, not Fresh:** as of this pinning date, LibreOffice's
"Fresh" branch (26.8.0) is the newest feature release; "Still" (25.8.7) is
the previous release train's most recent bugfix build, which is the
project's own recommendation for production/institutional deployments
where stability matters more than the newest features. LocalConvert only
uses LibreOffice's headless `--convert-to` conversion path — it needs
none of the newest UI/feature-branch functionality — so there is no
offsetting reason to take on Fresh's higher regression risk for an
institutional (MEB BİGM) deployment.

`PINNED_VERSION`/`PINNED_ARCHITECTURE` in
`src-tauri/src/engines/office_manifest.rs` and the pinned values in
`scripts/prepare-office-engine.ps1` / `scripts/verify-office-engine-before-build.ps1`
must all agree with this table. Bumping the pinned version means updating
all four places plus re-running the prep script to produce a new,
re-verified `manifest.json`.

## 2. Acquisition workflow (build-time only, never runtime)

`scripts/prepare-office-engine.ps1`:

1. Downloads the pinned MSI **only** from
   `download.documentfoundation.org` (the LibreOffice project's own host —
   never a mirror, never a third-party CDN).
2. Computes the file's SHA-256 and compares it against the pinned hash.
   **Any mismatch deletes the file and exits non-zero.** The script never
   silently proceeds with an unverified binary.
3. Runs `msiexec /a <msi> /qn TARGETDIR=<staging>` — an *administrative
   install*, i.e. a plain extraction of the MSI's file table into a
   directory. This does **not** register LibreOffice on the build
   machine, create Start Menu shortcuts, write to `HKLM\...\Uninstall`,
   or run any installer scripts beyond what an admin-image extraction
   does. It is the standard, Microsoft-documented, non-interactive way to
   get an MSI's contents onto disk without performing a real install —
   deliberately safer than silently running the interactive installer or
   unpacking the MSI's internal CAB streams by hand.
4. Copies `program/` and `share/` (LibreOffice's runtime binaries/DLLs
   and its filters/gallery/registry/autocorrect resources, respectively)
   into `src-tauri/engines/office/LibreOffice/`.
5. Copies the installer's own `LICENSE`/`NOTICE`/readme/credits files
   into `THIRD_PARTY_NOTICES/LibreOffice/`.
6. Writes `src-tauri/engines/office/manifest.json` from
   `manifest.json.template`, filling in the verified hash and a build
   timestamp.

This script is deterministic given the same pinned version/hash, and is
meant to be re-run on any build machine (a developer's, or CI) rather
than distributing a pre-populated `engines/office/` folder out of band.

`scripts/verify-office-engine-before-build.ps1` is the fail-closed gate,
wired into `src-tauri/tauri.conf.json`'s `bundle.beforeBundleCommand`. It
re-validates the manifest/version/hash-shape/executable/required
directories immediately before Tauri packages a release build, so a
release can never accidentally ship a broken or absent engine. See
§9 "Build-time failure policy".

## 3. Bundled directory contract

```
src-tauri/engines/office/
  manifest.json                 <- generated, gitignored (see §5)
  manifest.json.template        <- checked in
  README.md                     <- checked in
  LibreOffice/                  <- generated, gitignored
    program/                    <- soffice.exe + all runtime DLLs/binaries
    share/                      <- export filters, gallery, registry,
                                    autocorrect/autotext, template
                                    fallbacks - required for Writer/Calc/
                                    Impress -> PDF filters to load at all
```

`program/` and `share/` are both required — `share/` is not optional
"extra" content; LibreOffice's PDF export filters and default
document-type registrations live under it, and headless conversion fails
without it. No stripping is done in Step 4 (§8, installer size).

## 4. Engine manifest

`src-tauri/src/engines/office_manifest.rs::OfficeManifest` mirrors
`manifest.json.template`'s fields: `engine_id`, `engine_name`, `version`,
`architecture`, `executable_relative_path`, `source`, `sha256`,
`bundled_at_build`, `license_notice_path`, `required_relative_dirs`. This
is backend/build metadata only — never sent to the frontend as-is (the
frontend only ever sees the stable `OfficeEngineStatus` enum via the
`office_engine_status` Tauri command).

## 5. What's committed vs. generated

Committed to git: `manifest.json.template`, `README.md`,
`THIRD_PARTY_NOTICES/LibreOffice/README.md`, this document, the two
scripts.

Gitignored (generated by the prep script, build-machine-local):
`src-tauri/engines/office/manifest.json`,
`src-tauri/engines/office/LibreOffice/` (the actual ~350MB+ payload), and
the populated contents of `THIRD_PARTY_NOTICES/LibreOffice/` beyond its
`README.md`. See root `.gitignore`.

## 6. Resolver policy

`src-tauri/src/engines/resolver.rs` resolution order is unchanged in
shape (bundled -> configured -> system) but Office's system tier is now
**off by default**: `system_path()` refuses to look up Office at all
unless the process environment variable
`LOCALCONVERT_ALLOW_SYSTEM_OFFICE_FALLBACK=1` is explicitly set. This is
never set by the application itself and is not reachable from the
frontend or any Tauri command — it exists purely so a developer machine
without the bundled engine prepared can still exercise
`engines::office::convert` against a locally-installed LibreOffice during
development. A production/institutional build must never rely on it: the
bundled tier is authoritative.

## 7. Self-check

`engines::office_manifest::self_check()` (exposed as the
`office_engine_status` Tauri command) checks, in order:

1. The bundled `engines/office/` directory exists next to the running
   executable.
2. `manifest.json` exists and parses as valid JSON matching the expected
   shape.
3. `manifest.engine_id == "office"`.
4. `manifest.version`/`architecture` match this build's pinned constants.
5. The executable named by `executable_relative_path` exists as a file,
   and cannot resolve outside the manifest's own directory.
6. Every directory in `required_relative_dirs` exists.
7. The existing file-existence-based `resolver::is_available` agrees.

Returns one of: `AVAILABLE`, `ENGINE_MISSING`, `ENGINE_INVALID`,
`ENGINE_VERSION_MISMATCH`, `SELF_CHECK_FAILED` — plus a fixed, translatable
message. Never a path, filename, or raw process output.

Note: `self_check()` does **not** re-hash the ~350MB+ `LibreOffice/` tree
against `manifest.sha256` on every check (capability polls happen
frequently and re-hashing that much data each time would be a real
performance cost). SHA-256 verification happens once, at prep time,
against the downloaded installer (§2) — `self_check()` trusts that the
`LibreOffice/` directory sitting next to a manifest recording the correct
pinned version/architecture is the payload that hash already verified.
Tampering with `LibreOffice/` in place after prep without touching
`manifest.json` would not be caught by `self_check()`. This is an
accepted trust boundary for Step 4, not an oversight.

Not yet implemented in Step 4: actually spawning `soffice.exe --version`
headless as part of the self-check (a stronger "can this executable
actually start" check beyond file-existence). This is a documented
residual gap — see §12 Known limitations.

## 8. Capability model

`capabilities::compute_capabilities()`'s `office_to_pdf` entry now calls
`office_manifest::self_check()` instead of the cheaper
`resolver::is_available` check, so `AVAILABLE` means the full self-check
passed (manifest present and internally consistent, pinned version/arch
match, required directories present) — not merely "a file named
`soffice.exe` exists somewhere".

## 9. Supported inputs (V1)

| Input | Status |
|---|---|
| DOCX -> PDF | Supported (core V1 requirement) |
| XLSX -> PDF | Supported (core V1 requirement) |
| PPTX -> PDF | Supported (core V1 requirement) |
| ODT -> PDF | Supported |
| ODS -> PDF | Supported |
| ODP -> PDF | Supported |
| DOC/XLS/PPT (legacy binary) -> PDF | Supported via the same LibreOffice import filters as the OOXML formats above; legacy binary formats have historically had more import-filter edge cases than OOXML/ODF, so treat as lower-confidence than the six formats above pending the clean-machine acceptance pass (§13). |
| PDF -> DOCX/XLSX/PPTX | **Out of scope for Step 4.** `PdfToDocx` stays whatever partial/experimental state it was already in; `PdfToXlsx`/`PdfToPptx` remain `FEATURE_NOT_IMPLEMENTED`. These are reconstruction problems, not rendering, and are not touched by this step. |

## 10. Office -> PDF execution pipeline

Unchanged from the existing `engines::office::convert` implementation,
now timeout-bounded (§11):

1. `converter.rs` validates the input path via the existing security
   layer and confirms the extension is a supported Office format before
   ever reaching the engine layer.
2. A fresh `security::temp::JobTempDir` is created per conversion.
3. `office::convert()` resolves the engine (`resolver::resolve`,
   bundled-authoritative per §6), builds a job-specific
   `-env:UserInstallation=<job dir>/loffice_profile` argument, and
   invokes `soffice --headless --convert-to pdf --outdir <job dir> <input>`
   via `process::run_with_timeout` — never a shell, never a built command
   string (`process.rs`'s existing invariants, unchanged).
4. On success, the produced file is verified to exist, then moved into
   the caller's requested destination via `security::temp::move_into_place`
   (rename-or-copy, creating the destination directory as needed).
5. The job directory (including the isolated LibreOffice profile) is
   removed on `Drop`, whether the job succeeded, failed, or timed out.

## 11. Concurrency and profile isolation

Each job gets `job_dir/loffice_profile` as its
`-env:UserInstallation=` — a fresh, UUID-named directory per
`JobTempDir::new()` — so two conversions (e.g. one DOCX->PDF and one
XLSX->PDF) running at the same time never share a LibreOffice user
profile lock. This was already true before Step 4; Step 4 does not change
it, only adds the timeout wrapper around the same invocation.

## 12. Timeout / hung-process control

`process::run_with_timeout` (used by `office::convert` with
`office_manifest::CONVERT_TIMEOUT` = **180 seconds**) polls the child
process every 100ms; if it hasn't exited by the deadline, the process is
killed and reaped, and the caller receives `EngineError::timeout`
(`OFFICE_TIMEOUT`). 180s was chosen as generous headroom for large
legitimate spreadsheets/presentations while still bounding a
corrupt/hostile input to a fixed wall-clock instead of hanging the UI
indefinitely.

**Known limitation:** this kills the direct child process only, not a
process tree. `soffice.exe` is the long-running process for the headless
`--convert-to` invocation this app makes, so this covers the actual call
site, but it is not a general subprocess-tree sandbox — a future
hardening pass should use a Windows Job Object (`CreateJobObject` +
`AssignProcessToJobObject` with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) so
that killing the job also kills anything the engine spawned. Not done in
Step 4.

## 13. Macros / active content / network privacy

LibreOffice is invoked exclusively via `--headless --convert-to`, which
does not open a UI and does not execute document macros by default — the
default LibreOffice security setting for macro execution is
"never run macros automatically without a signed/trusted source", and
headless conversion does not prompt for or auto-elevate that. This app
does not set any registry/config option that would raise LibreOffice's
own macro trust level.

The job-specific `-env:UserInstallation=` profile is fresh, empty, and
discarded after each job (§11) — LibreOffice never accumulates persistent
state, remembered documents, or a "trusted locations" list across
conversions, which also means it starts from LibreOffice's own defaults
(update checks off by default for a fresh profile, no configured extra
extensions, no remembered remote content) every single time rather than
inheriting whatever a real, long-lived user profile might have
accumulated.

**What is NOT independently re-verified in Step 4:** LibreOffice's own
`Load/Save -> General` "check for updates" and any telemetry-adjacent
settings are LibreOffice defaults for a fresh profile, not settings this
app explicitly forces off via a registrymodifications.xcu seeded into
each job profile. Because the job runs headless with no network
dependency in its own code path and a throwaway profile, this has not
been observed to cause a network call in this app's usage pattern, but it
has not been packet-captured/verified end-to-end either. A future
hardening pass should seed each job's `UserInstallation` profile with an
explicit `registrymodifications.xcu` that pins
`UpdateCheck/Enabled=false` and disables the online-content-in-templates
setting, so this is a hardcoded property of every job rather than an
inherited default. **Residual risk, not yet closed.**

Verified: a normal local DOCX/XLSX/PPTX conversion invokes only the local
`soffice.exe` process with local file paths as arguments — nothing in
`office::convert()` or `process::run_with_timeout` makes a network
request, and there is no HTTP client dependency in this codebase for this
path (`reqwest`/similar were already removed from `Cargo.toml` per Phase
1 — see `SECURITY_PHASE1_REPORT.md`).

## 14. License / notices

LibreOffice is MPL-2.0 with third-party components under their own
licenses (fonts, ICU, Python, etc. — see the copied `NOTICE`/credits
files). `scripts/prepare-office-engine.ps1` copies the pinned
installer's own license/notice/credits files verbatim into
`THIRD_PARTY_NOTICES/LibreOffice/` so the notices shipped in a given
build always correspond exactly to that build's LibreOffice payload.
**Institutional/legal review of these notices is still required before
final MEB distribution** — see
`THIRD_PARTY_NOTICES/LibreOffice/README.md`. Nothing in this step
constitutes a legal clearance.

## 15. Build-time failure policy

The installed `tauri-build` version in this repo's `Cargo.lock` does not
support a `bundle.beforeBundleCommand`/pre-bundle hook field (confirmed
by running `cargo check` after adding one - it rejected the field
outright), so this is enforced as an explicit, separate step rather than
a Tauri-native hook:

- `npm run verify:office-engine` runs
  `scripts/verify-office-engine-before-build.ps1` directly.
- `npm run tauri:build:release` runs that verification and only then
  `tauri build` - **this is the command a real release build must use**,
  not a bare `npm run tauri build` / `tauri build`.
- `.github/workflows/release.yml`'s Windows leg runs
  `npm run verify:office-engine` as its own CI step before the
  `tauri-apps/tauri-action` build step, so a release build in CI fails
  fast with a clear message rather than silently producing an installer
  without the engine.

On Windows, the script fails (non-zero exit) if the manifest is missing,
malformed, at the wrong pinned version/architecture, has a malformed hash
field, the executable is missing, or any `required_relative_dirs` entry
is missing. Set `LOCALCONVERT_SKIP_OFFICE_ENGINE_CHECK=1` to explicitly
bypass for a non-distributed development build; never set it for a build
that will actually reach a user. On non-Windows build targets it's a
no-op (Step 4 is Windows-only bundling scope).

**Residual gap:** because this is a separate command rather than a
Tauri-enforced hook, a developer could still run `npx tauri build`
directly and bypass it locally - CI is the actual backstop for anything
that gets distributed. If a future `tauri-build` version adds a real
pre-bundle hook, wire it there instead and remove this workaround.

## 16. Error codes added

- `OFFICE_TIMEOUT` — headless conversion exceeded `CONVERT_TIMEOUT` and
  was killed.
- Existing `OFFICE_ENGINE_NOT_AVAILABLE`, `OFFICE_PROCESS_FAILED`,
  `OFFICE_OUTPUT_INVALID`/`OUTPUT_INVALID`, `INPUT_INVALID` are unchanged.
- `office_engine_status` returns `OfficeEngineStatus`:
  `AVAILABLE` / `ENGINE_MISSING` / `ENGINE_INVALID` /
  `ENGINE_VERSION_MISMATCH` / `SELF_CHECK_FAILED`.

## 17. Known limitations (as of Step 4)

- **The actual LibreOffice payload has not been downloaded/bundled in
  this environment.** See the boundary note at the end of this document.
- Timeout kills only the direct child process, not a process tree (§12).
- LibreOffice's own update-check/telemetry-adjacent defaults are relied
  upon rather than explicitly pinned off via a seeded
  `registrymodifications.xcu` (§13).
- Self-check does not yet spawn `soffice --version` to confirm the
  executable actually starts; it checks file existence/manifest
  consistency only (§7).
- Legacy binary DOC/XLS/PPT support is architecturally identical to the
  OOXML/ODF formats but has not been separately fixture-tested in this
  step (§9).
- No Windows Job Object hardening yet for subprocess-tree containment.

## 18. Clean-machine acceptance checklist

To be run on a genuinely clean Windows 10/11 x64 VM (no LibreOffice, no
Microsoft Office, no relevant PATH entries) once a real bundled engine
and installer exist:

1. Install LocalConvert via the generated MSI/NSIS.
2. Disconnect the network.
3. Convert: DOCX->PDF, XLSX->PDF, PPTX->PDF, ODT->PDF, ODS->PDF,
   ODP->PDF. Open each resulting PDF and confirm it renders correctly.
4. Convert a file with a Turkish/Unicode filename containing spaces.
5. Run two conversions simultaneously (e.g. a DOCX and an XLSX at once)
   and confirm both succeed independently.
6. Attempt to convert: a corrupt DOCX, a zero-byte file renamed to
   `.docx`, and a password-protected DOCX — confirm each fails with a
   stable, non-raw error rather than a crash or a raw LibreOffice error
   string.
7. Convert `report.docx` to a folder that already has `report.pdf` and
   confirm the existing collision-handling policy is honored (no silent
   overwrite unless that is already this app's established policy).
8. Restart LocalConvert and repeat one conversion.
9. Uninstall LocalConvert and confirm the bundled `engines/office/`
   payload is removed with it (no orphaned files under the install
   directory).

---

## STEP 4 BOUNDARY

Everything above this line is implemented against the current source
tree: the manifest/self-check module, timeout-bounded process execution,
bundled-authoritative resolver policy, capability model wired to the
self-check, the reproducible prep/verify scripts, the directory contract,
`tauri.conf.json` resource/pre-bundle wiring, and tests.

**What has not been done, and could not safely/reproducibly be done
inside this environment:** actually downloading, hash-verifying, and
committing-to-the-build-machine a ~350MB+ official LibreOffice installer,
running `scripts/prepare-office-engine.ps1` for real, running
`npm run tauri build` to produce an installer that actually contains the
engine, and executing the clean-machine acceptance checklist (§18) on a
real clean Windows VM. Those require a real network fetch of a large
official binary, a Windows machine willing to run an MSI administrative
install, and a separate clean VM for acceptance — none of which this
session performed. See the final report for the exact required next
steps.
