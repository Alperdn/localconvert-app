//! Native (pure-Rust, in-process) replacements for external-tool duties,
//! migrated one at a time per the V1 dependency architecture.
//! - ZIP - see `archive_zip.rs`. Everything else archive-shaped
//!   (7z/tar/tgz/tbz2) still goes through the external `7z` tool in
//!   `converter::convert_archive`.
//! - Raster images (JPEG/PNG/WebP/BMP/GIF/TIFF) - see `image/`. SVG
//!   rasterization and HEIC/AVIF/PSD still go through ImageMagick or are
//!   not implemented - see `native::image` module docs.
//!
//! The image pipeline itself now lives in the Tauri-independent
//! `meb-core` crate (`crates/meb-core/src/image`) so the web server can use
//! the exact same engine. It is re-exported here unchanged, so every
//! existing `crate::native::image::...` call site keeps working.

pub mod archive_zip;
pub use meb_core::image;
