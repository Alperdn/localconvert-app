//! Centralized engine resolution, process spawning, and structured
//! dependency errors for external tools LocalConvert can invoke.
//!
//! See `resolver.rs` for this app's resolution policy (bundled ->
//! configured -> system), `meb_engines::process` for the single sanctioned
//! way to spawn a resolved engine, `meb_engines::error` for the structured
//! error model, and `office.rs` for the first engine fully migrated onto
//! this layer.
//!
//! `EngineId` is a fixed, closed enum (`meb_engines::engine_id`) - there is no
//! function anywhere in this module, or called by a Tauri command, that
//! turns a frontend-supplied string into an `EngineId`.

#[cfg(test)]
mod acceptance;
pub mod ascii_link;
pub mod audio_prep;
pub mod audio_wav;
pub mod ffmpeg_manifest;
pub mod office;
pub mod office_manifest;
pub mod resolver;
pub mod speech;
pub mod speech_error;
pub mod speech_manifest;

// `engine_id`, `error`, `process` and `bundle` now live in the shared
// `meb-engines` crate, so the web server reaches an external engine through
// the same code path (and the same invariants) as the desktop app. They are
// re-exported here under their original module paths: every `super::error`,
// `super::process`, `crate::engines::bundle` ... call site in this module
// keeps working unchanged, and `resolver` below is still this app's own
// policy over the shared mechanism.
pub use meb_engines::{bundle, engine_id, error, process};

pub use meb_engines::EngineId;

#[cfg(test)]
mod speech_dir_tests;
