//! Runtime, per-file scope grants.
//!
//! Two frontend features need to read/write a file directly from
//! JavaScript rather than through a Tauri command, and Tauri gates both
//! behind an explicit scope the frontend cannot expand on its own:
//!
//!   - The PDF editor calls `@tauri-apps/plugin-fs`'s `readFile`/
//!     `writeFile` directly (see `PdfEditor.tsx`) to load and save PDF
//!     bytes it manipulates client-side with pdf-lib/fabric.js.
//!   - The video trimmer plays a preview via `convertFileSrc(path)`
//!     (see `VideoTrimmer.tsx`), which reads through Tauri's `asset://`
//!     protocol.
//!
//! The capabilities file used to grant this by putting `$HOME/**` in
//! `fs:scope` - i.e. "the frontend may read or write literally any file
//! under the user's home directory, forever, from page load." That is
//! the exact kind of unrestricted grant this phase is meant to remove.
//!
//! Tauri v2's own recommended alternative (see
//! https://v2.tauri.app/security/) is to keep the static scope minimal
//! and extend it at *runtime*, in Rust, for exactly the file the user
//! has already explicitly chosen (native file dialog, drag-and-drop) -
//! after that path has been through `path_validation`. That's what this
//! module does: one file at a time, validated first, nothing implicitly
//! recursive or forever-standing beyond that single path.
//!
//! Both scope systems are extended together because a caller shouldn't
//! need to know which underlying Tauri subsystem a given frontend call
//! happens to go through.

use std::path::Path;
use tauri::{AppHandle, Manager};
use tauri_plugin_fs::FsExt;

/// Grants read/write access to exactly `path` for both the fs plugin
/// (`readFile`/`writeFile` from JS) and the asset protocol
/// (`convertFileSrc`). Idempotent - safe to call more than once for the
/// same path.
///
/// Callers must validate `path` first (see `security::path_validation`);
/// this function does not re-validate, it only extends scope.
pub fn authorize_file(app: &AppHandle, path: &Path) -> Result<(), String> {
    app.fs_scope()
        .allow_file(path)
        .map_err(|e| format!("Failed to authorize file access: {}", e))?;

    // `asset_protocol_scope()` is provided by `tauri`'s own
    // `protocol-asset` Cargo feature, which `src-tauri/Cargo.toml`
    // enables unconditionally on the `tauri` dependency (it's needed for
    // the video trimmer's `convertFileSrc` preview). That's a different
    // thing from a feature *of this crate* - there is no `protocol-asset`
    // entry in this crate's own `[features]` table, so a
    // `#[cfg(feature = "protocol-asset")]` guard here would always
    // evaluate false ("unexpected cfg condition value", and silently
    // dead code: the video preview's asset-protocol grant would never
    // actually run). Since the method is unconditionally available given
    // how the dependency is declared, call it unconditionally too.
    app.asset_protocol_scope()
        .allow_file(path)
        .map_err(|e| format!("Failed to authorize asset-protocol access: {}", e))?;

    Ok(())
}
