//! The external engines this server can execute.
//!
//! Every one of them reaches its tool through `meb_engines`: the engine is
//! resolved once, at startup, by `meb_engines::resolver` (bundled next to
//! the server binary first, then `PATH`), and every process is started by
//! `meb_engines::process` - no shell, argument arrays only, the job's own
//! work directory as cwd, a denylisted environment, and a wall-clock
//! timeout with kill-and-reap on cancel.
//!
//! Nothing here uses the legacy `src-tauri/src/converter.rs` launcher, and
//! nothing here accepts a client string: a runner is handed an already
//! validated `JobSpec` plus server-built paths, and turns that into a fixed
//! argument vector.
//!
//! An engine that does not resolve on this machine is not registered for
//! any job kind (see `runner::RunnerRegistry::production`), so the API
//! refuses those kinds with `UNSUPPORTED_CONVERSION` instead of accepting
//! jobs that could only ever fail.

pub mod ghostscript;
pub mod image_optimize;
pub mod libreoffice;
pub mod lopdf_ops;
pub mod tesseract;

pub use ghostscript::GhostscriptRunner;
pub use image_optimize::ImageOptimizeRunner;
pub use libreoffice::LibreOfficeRunner;
pub use lopdf_ops::LopdfRunner;
pub use tesseract::TesseractRunner;
