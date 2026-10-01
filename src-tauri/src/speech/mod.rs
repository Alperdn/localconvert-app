//! Ses Dikte: local, offline speech-to-text.
//!
//! - `limits`   - resource-limit policy constants + boundary checks
//! - `job`      - job-scoped temp dirs, stale sweep, single-slot registry
//! - `pipeline` - uploaded-audio pipeline (validate -> probe -> normalize -> transcribe)
//! - `commands` - the thin Tauri IPC surface
//!
//! Engines live in `crate::engines` (`speech`, `audio_prep`, ...). Nothing here
//! talks to the network; there is no cloud fallback.

pub mod commands;
pub mod job;
pub mod limits;
pub mod pipeline;

#[cfg(test)]
mod tests;
