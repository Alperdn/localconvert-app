//! Native (pure-Rust, in-process) replacements for external-tool duties,
//! migrated one at a time per the V1 dependency architecture.
//! - ZIP - see `archive_zip.rs`. Everything else archive-shaped
//!   (7z/tar/tgz/tbz2) still goes through the external `7z` tool in
//!   `converter::convert_archive`.
//! - Raster images (JPEG/PNG/WebP/BMP/GIF/TIFF) - see `image/`. SVG
//!   rasterization and HEIC/AVIF/PSD still go through ImageMagick or are
//!   not implemented - see `native::image` module docs.

pub mod archive_zip;
pub mod image;
