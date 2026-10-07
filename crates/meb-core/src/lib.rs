//! MEB-Dönüştür conversion core.
//!
//! Everything in this crate is free of Tauri, HTTP, identity and storage
//! policy (see docs/WEB_ARCHITECTURE_PROPOSAL.md §L): it knows how to
//! convert a file it is given, inside a directory it is given, and how to
//! report capabilities. It never decides *where* files live or *who* owns
//! them - the desktop app (`src-tauri`) and the web server (`meb-server`)
//! each own that policy and call into this crate.
//!
//! Migrated so far:
//! - `image`: the native raster pipeline (moved from
//!   `src-tauri/src/native/image`, plus cooperative cancellation hooks and
//!   upload-time probing).
//! - `format`: what a file IS - the shared `SourceFormat` vocabulary
//!   (image/PDF/Office) and container sniffing from magic bytes.
//! - `document`: structural validation of an untrusted PDF or Office file,
//!   the document-side counterpart of `image::probe_file`.
//! - `capabilities`: the shared capability types and the native-image
//!   capability entries.

pub mod capabilities;
pub mod document;
pub mod format;
pub mod image;
