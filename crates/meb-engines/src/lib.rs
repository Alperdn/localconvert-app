//! Engine identity, resolution policy and sandboxed process execution for
//! the external tools MEB-Dönüştür can invoke.
//!
//! Shared verbatim by the desktop app (`src-tauri`) and the web server
//! (`crates/meb-server`) so that both reach an external engine through
//! exactly one code path, with exactly one set of invariants:
//!
//! - `engine_id` - the fixed, closed set of engines. No function anywhere
//!   turns a client- or frontend-supplied string into an `EngineId`.
//! - `resolver` - the *mechanism* of the bundled -> configured -> system
//!   resolution order, plus the containment check that keeps a bundled
//!   lookup inside its engines root. The *policy* (which roots, which
//!   system scanner, which engines may fall back to a system install)
//!   belongs to each consumer and is handed in as a `ResolutionPolicy` of
//!   plain `fn` pointers - it is therefore fixed at compile time and
//!   cannot be redirected at runtime.
//! - `process` - the only sanctioned way to turn a `ResolvedEngine` into a
//!   running process: no shell, argument arrays only, controlled cwd,
//!   denylisted environment, cancellable with a wall-clock timeout.
//! - `error` - the structured, path-free error model.
//! - `bundle` - SHA-256 verification of the files inside a bundled engine
//!   directory.

pub mod bundle;
pub mod engine_id;
pub mod error;
pub mod process;
pub mod resolver;

pub use engine_id::EngineId;
pub use error::{EngineError, EngineErrorKind};
pub use resolver::{EngineTier, ResolutionPolicy, ResolvedEngine};
