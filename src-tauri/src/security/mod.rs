//! Phase 1 - Secure Desktop Foundation.
//!
//! Centralized security primitives used by the Tauri commands that touch
//! the filesystem or spawn an external conversion tool:
//!
//!   - `path_validation` - `validate_input_file`, `validate_output_dir`,
//!     `validate_fs_scope_target`, `confirm_within`. The single place that
//!     decides whether a path is safe to read from or write into. Every
//!     path arriving from the frontend is untrusted input.
//!   - `file_validation`  - `sanitize_filename_component`, the lower-level
//!     helper `path_validation` and `temp` both build on.
//!   - `temp`             - `JobTempDir`: per-job isolated temporary
//!     directories with automatic cleanup on both success and failure
//!     (via `Drop`), plus `move_into_place` for the atomic-ish final
//!     commit into the user's real output directory, and
//!     `cleanup_stale_job_dirs` for crash-leftover cleanup at startup.
//!   - `fs_scope`         - `authorize_file`: grants the frontend narrow,
//!     per-file access (instead of a broad static `$HOME/**`-style grant)
//!     to the fs plugin and the asset protocol, for the two features that
//!     need it (the PDF editor and the video trimmer preview).
//!
//! None of this is decorative: every function here is called from
//! `commands.rs` / `converter.rs`. See the security report in the project
//! root for exactly which commands call into this module today and which
//! don't yet (documented as follow-up work rather than silently assumed).

pub mod file_validation;
pub mod fs_scope;
pub mod path_validation;
pub mod temp;
