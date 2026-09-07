# src-tauri/CLAUDE.md

Backend-specific guidance. Loads only when working under `src-tauri/`. See the root `CLAUDE.md` for project-wide conventions.

## CRITICAL: Migration Context

This project was originally built on **Tauri v1** and is being migrated to **Tauri v2**. All new code MUST use Tauri v2 APIs and patterns. When you encounter v1 patterns, migrate them to v2. Key differences:

### Tauri v2 Migration Rules

1. **No allowlist** — The v1 `tauri > allowlist` in `tauri.conf.json` is REMOVED. Use the **capabilities/permissions** system instead. Create `src-tauri/capabilities/default.json` with explicit permission grants.

2. **Plugins replace core APIs** — These are now separate crates and npm packages:
   - `tauri::api::process` → `tauri-plugin-shell` (crate) + `@tauri-apps/plugin-shell` (npm)
   - `tauri::api::fs` → `tauri-plugin-fs` (crate) + `@tauri-apps/plugin-fs` (npm)
   - `tauri::api::dialog` → `tauri-plugin-dialog` (crate) + `@tauri-apps/plugin-dialog` (npm)
   - `tauri::api::path` → `tauri-plugin-fs` or `tauri::path` (built-in)
   - `tauri::api::shell::open` → `tauri-plugin-opener` (crate) + `@tauri-apps/plugin-opener` (npm)
   - Updater → `tauri-plugin-updater` (crate) + `@tauri-apps/plugin-updater` (npm)
   - Notification → `tauri-plugin-notification` (crate) + `@tauri-apps/plugin-notification` (npm)

3. **JS import changes:**
   - `@tauri-apps/api/tauri` → `@tauri-apps/api/core`
   - `@tauri-apps/api/window` → `@tauri-apps/api/webviewWindow`
   - `invoke()` is now from `@tauri-apps/api/core`
   - `Window` type → `WebviewWindow` type
   - All plugin APIs use `@tauri-apps/plugin-<name>` packages

4. **Rust API changes:**
   - `tauri::Window` → `tauri::WebviewWindow`
   - `Manager::get_window()` → `Manager::get_webview_window()`
   - `tauri::api::process::Command` → `tauri_plugin_shell::process::Command` via `ShellExt`
   - Plugin registration in `lib.rs` via `.plugin(tauri_plugin_shell::init())` etc.
   - Commands use `tauri::command` attribute (unchanged) but handler registration syntax may differ

5. **Config structure changes (`tauri.conf.json`):**
   - `tauri > windows` → `app > windows`
   - `tauri > bundle` → `bundle`
   - `tauri > security` → `app > security`
   - `tauri > cli` → `plugins > cli`
   - `tauri > updater` → `plugins > updater` AND `bundle > updater`
   - `tauri > allowlist` → REMOVED (use capabilities)
   - Add `"identifier"` at root level

6. **Capabilities file** — Create `src-tauri/capabilities/default.json`:
   ```json
   {
     "identifier": "default",
     "description": "Default capabilities for LocalConvert",
     "windows": ["main"],
     "permissions": [
       "core:default",
       "shell:allow-execute",
       "shell:allow-open",
       "shell:allow-spawn",
       "shell:allow-stdin-write",
       "shell:allow-kill",
       "dialog:allow-open",
       "dialog:allow-save",
       "dialog:allow-message",
       "dialog:allow-ask",
       "fs:default",
       "fs:allow-read",
       "fs:allow-write",
       "fs:allow-exists",
       "fs:allow-mkdir",
       "fs:allow-remove",
       "fs:allow-rename",
       "fs:allow-copy-file",
       "fs:allow-stat",
       "fs:allow-readdir",
       "notification:default",
       "notification:allow-notify",
       "updater:default",
       "opener:default"
     ]
   }
   ```

7. **Sidecar / External Binary changes** — In v1, external binaries were defined in the allowlist. In v2, use the shell plugin permissions. The `externalBin` config stays in `bundle` but execution goes through `tauri_plugin_shell`.

## Auto-Updater — REMOVED (Phase 1: Secure Desktop Foundation)

This project previously auto-checked for updates on every startup and, if
one was found, silently downloaded, installed, and relaunched with no user
confirmation (`@tauri-apps/plugin-updater` + `@tauri-apps/plugin-process`,
called from `App.tsx`'s startup effect). That has been **deliberately
removed** — see `SECURITY_PHASE1_REPORT.md` in the repo root for the full
rationale — because this app must not make network requests during normal
use and must never install something the user didn't explicitly ask for.

`tauri-plugin-updater` and `tauri-plugin-process` are no longer dependencies
at all (removed from `Cargo.toml`, `package.json`, plugin registration in
`lib.rs`, and the `bundle.updater`/`plugins.updater` blocks in
`tauri.conf.json`), specifically so there's no code path left that can do
this even by accident.

**Do not re-add this by following old instructions or examples** (including
ones that might resemble what used to be here). If update checking becomes
a real requirement again, it needs to be:
- Explicit and user-initiated (a "Check for updates" button), never
  automatic on startup.
- A visible confirmation step before anything downloads or installs.
- Re-evaluated against the same no-silent-network-request principle as
  everything else in this codebase, not just dropped back in.

## Backend Conventions

- Rust error handling: `Result<T, String>` pattern for all commands
- Rust edition 2021
- All Tauri commands are async
- Platform-specific code uses `#[cfg(target_os = "...")]` — NEVER use runtime OS checks when compile-time checks work
- When adding new Tauri commands, ALWAYS also add the corresponding permission in `src-tauri/capabilities/default.json`
- When adding new plugins, register them in BOTH `lib.rs` (Rust side) AND install the npm package (JS side)
