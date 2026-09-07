//! Centralized engine resolution, process spawning, and structured
//! dependency errors for external tools LocalConvert can invoke.
//!
//! See `resolver.rs` for the resolution policy (bundled -> configured ->
//! system), `process.rs` for the single sanctioned way to spawn a
//! resolved engine, `error.rs` for the structured error model, and
//! `office.rs` for the first engine fully migrated onto this layer.
//!
//! `EngineId` is a fixed, closed enum (`engine_id.rs`) - there is no
//! function anywhere in this module, or called by a Tauri command, that
//! turns a frontend-supplied string into an `EngineId`.

pub mod engine_id;
pub mod error;
pub mod office;
pub mod process;
pub mod resolver;

pub use engine_id::EngineId;
