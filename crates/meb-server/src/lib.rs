//! MEB-Dönüştür web server - first vertical slice (local development).
//!
//! Browser -> upload -> server `file_id` -> job -> server `job_id` ->
//! native image engine (`meb_core::image`) -> validated output ->
//! ownership-checked download. See docs/WEB_ARCHITECTURE_PROPOSAL.md.
//!
//! Module boundaries:
//! - `api`: HTTP handlers only; no storage paths, no engine code.
//! - `session`: the ONLY place an `Owner` can be created (opaque cookie).
//! - `registry`: the ONLY way to reach a file/job from a client-supplied
//!   id; every lookup is owner-scoped.
//! - `storage`: the ONLY place filesystem paths are built (typed ids only).
//! - `runner`: the engine boundary (`ConversionRunner`). In-process engines
//!   implement it directly; external engines (LibreOffice, FFmpeg,
//!   whisper.cpp) will implement it on top of the unified process runner in
//!   a later phase - never the legacy `converter.rs` launcher.
//! - `worker`: job execution, concurrency limits, panic isolation, cleanup.
//! - `janitor`: TTL expiry of files/jobs/sessions.
//!
//! Authentication is intentionally NOT implemented in this phase; a session
//! cookie provides per-browser ownership isolation only.

pub mod api;
pub mod config;
pub mod error;
pub mod ids;
pub mod janitor;
pub mod jobs;
pub mod names;
pub mod registry;
pub mod runner;
pub mod session;
pub mod storage;
pub mod worker;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub use config::Config;
pub use runner::{ConversionRunner, NativeImageRunner};

/// Shared server state. Cheap to clone (one `Arc`).
#[derive(Clone)]
pub struct AppState(Arc<AppInner>);

pub struct AppInner {
    pub config: Config,
    pub storage: storage::Storage,
    pub registry: registry::Registry,
    pub sessions: session::SessionStore,
    pub limiter: worker::Limiter,
    /// Bounds uploads being streamed at the same time (`max_concurrent_
    /// uploads`). A stalled upload therefore ties up one slot at most, and
    /// only until its idle timeout fires.
    pub uploads: tokio::sync::Semaphore,
    pub runner: Arc<dyn ConversionRunner>,
    /// Directories whose deletion failed (e.g. a file still open by a
    /// download on Windows). Retried by the janitor.
    pub pending_deletions: Mutex<Vec<PathBuf>>,
}

impl std::ops::Deref for AppState {
    type Target = AppInner;
    fn deref(&self) -> &AppInner {
        &self.0
    }
}

impl AppState {
    /// Removes a server-owned directory tree, queueing it for a janitor
    /// retry if the OS refuses right now. Never touches anything outside
    /// the data root (enforced by `Storage::remove_tree`).
    pub(crate) fn remove_tree_or_defer(&self, path: &Path) {
        if let Err(e) = self.storage.remove_tree(path) {
            tracing::warn!(error = %e, "deferred workspace cleanup");
            if let Ok(mut pending) = self.pending_deletions.lock() {
                pending.push(path.to_path_buf());
            }
        }
    }
}

#[derive(Debug)]
pub enum StartupError {
    Storage(storage::StorageError),
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartupError::Storage(e) => write!(f, "storage: {e}"),
        }
    }
}

/// A constructed server: state plus router. Used by `main.rs` and by the
/// integration tests (which drive the real router in-process).
pub struct App {
    pub state: AppState,
}

impl App {
    pub fn new(config: Config) -> Result<App, StartupError> {
        Self::with_runner(config, Arc::new(NativeImageRunner))
    }

    /// Same as `new`, with an explicit engine runner (tests inject runners
    /// that block, panic or fail to exercise the job lifecycle).
    pub fn with_runner(
        config: Config,
        runner: Arc<dyn ConversionRunner>,
    ) -> Result<App, StartupError> {
        let storage = storage::Storage::open(&config.data_root).map_err(StartupError::Storage)?;
        let state = AppState(Arc::new(AppInner {
            sessions: session::SessionStore::new(config.max_sessions),
            limiter: worker::Limiter::new(config.worker_permits, config.max_running_per_session),
            uploads: tokio::sync::Semaphore::new(config.max_concurrent_uploads.max(1)),
            registry: registry::Registry::default(),
            storage,
            runner,
            pending_deletions: Mutex::new(Vec::new()),
            config,
        }));
        Ok(App { state })
    }

    pub fn router(&self) -> axum::Router {
        api::router(self.state.clone())
    }

    /// One janitor pass (expiry + deferred deletions), synchronously.
    pub fn sweep(&self) {
        janitor::sweep(&self.state);
    }

    pub fn data_root(&self) -> &Path {
        self.state.storage.root()
    }

    /// Requests cancellation of every non-terminal job (shutdown path).
    pub fn cancel_all(&self) {
        self.state.registry.cancel_all();
    }
}
