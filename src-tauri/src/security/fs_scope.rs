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

    // The asset-protocol scope is only compiled in when the
    // `protocol-asset` Cargo feature is enabled (it is - see
    // src-tauri/Cargo.toml - because the video trimmer's preview player
    // needs it). Feature-gate defensively anyway so this module keeps
    // compiling if that feature is ever toggled off.
    #[cfg(feature = "protocol-asset")]
    {
        let _ = app.asset_protocol_scope().allow_file(path);
    }

    Ok(())
}
