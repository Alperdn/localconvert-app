//! Job model: state machine, snapshot (the single source of truth a client
//! sees - via `GET /jobs/:id` or SSE) and the create-job request.
//!
//! ```text
//! queued ──► running ──► completed
//!   │           │   └──► failed
//!   │           └─ cancel ─► cancel_requested ─► cancelled
//!   └─ cancel ─► cancelled
//! ```
//! Terminal states (`completed`, `failed`, `cancelled`) are immutable.

use crate::ids::{FileId, JobId};
use crate::session::Owner;
use crate::storage::JobPaths;
use meb_core::image::NativeImageFormat;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    CancelRequested,
    Cancelled,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Completed | JobState::Failed | JobState::Cancelled
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobResultInfo {
    /// Suggested download name (display only).
    pub output_name: String,
    pub size: u64,
    pub content_type: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobSnapshot {
    pub job_id: String,
    pub kind: &'static str,
    pub file_id: String,
    pub output_format: &'static str,
    pub state: JobState,
    /// Real, stage-based progress only; `null` = not yet known.
    pub progress_pct: Option<u8>,
    pub error: Option<JobErrorBody>,
    pub result: Option<JobResultInfo>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    /// Monotonic per job; lets a reconnecting client order snapshots.
    pub seq: u64,
}

/// Options accepted for `kind: "convert"`. Unknown fields are rejected so a
/// client cannot smuggle engine parameters (paths, codecs, GPU, ...).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConvertOptions {
    pub quality: Option<u8>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateJobRequest {
    pub kind: String,
    pub file_id: String,
    pub output_format: String,
    #[serde(default)]
    pub options: Option<ConvertOptions>,
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A job. Created only by the create-job handler; reachable from a client
/// id only through `Registry::find_job` (owner-checked).
pub struct Job {
    pub id: JobId,
    owner: Owner,
    pub file_id: FileId,
    pub display_stem: String,
    pub source_format: NativeImageFormat,
    pub target: NativeImageFormat,
    pub options: ConvertOptions,
    pub paths: JobPaths,
    state_tx: watch::Sender<JobSnapshot>,
    cancel: AtomicBool,
    deleted: AtomicBool,
    finished_at: Mutex<Option<Instant>>,
}

pub enum Outcome {
    Completed { size: u64 },
    Failed { code: &'static str },
    Cancelled,
}

impl Job {
    // Every field is required and set exactly once, by the create-job
    // handler; a builder would only add ceremony here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: JobId,
        owner: Owner,
        file_id: FileId,
        display_stem: String,
        source_format: NativeImageFormat,
        target: NativeImageFormat,
        options: ConvertOptions,
        paths: JobPaths,
    ) -> Arc<Job> {
        let now = now_ms();
        let snapshot = JobSnapshot {
            job_id: id.to_string(),
            kind: "convert",
            file_id: file_id.to_string(),
            output_format: target.canonical_extension(),
            state: JobState::Queued,
            progress_pct: None,
            error: None,
            result: None,
            created_at_ms: now,
            updated_at_ms: now,
            seq: 0,
        };
        let (state_tx, _) = watch::channel(snapshot);
        Arc::new(Job {
            id,
            owner,
            file_id,
            display_stem,
            source_format,
            target,
            options,
            paths,
            state_tx,
            cancel: AtomicBool::new(false),
            deleted: AtomicBool::new(false),
            finished_at: Mutex::new(None),
        })
    }

    pub(crate) fn owner(&self) -> &Owner {
        &self.owner
    }

    pub fn snapshot(&self) -> JobSnapshot {
        self.state_tx.borrow().clone()
    }

    pub fn state(&self) -> JobState {
        self.state_tx.borrow().state
    }

    pub fn subscribe(&self) -> watch::Receiver<JobSnapshot> {
        self.state_tx.subscribe()
    }

    pub(crate) fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }

    pub fn is_cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Applies `f` unless the job is already terminal; bumps `seq`.
    fn update(&self, f: impl FnOnce(&mut JobSnapshot) -> bool) -> bool {
        self.state_tx.send_if_modified(|s| {
            if s.state.is_terminal() {
                return false;
            }
            let changed = f(s);
            if changed {
                s.seq += 1;
                s.updated_at_ms = now_ms();
            }
            changed
        })
    }

    /// queued -> running. False if the job was cancelled while queued.
    pub(crate) fn try_start(&self) -> bool {
        self.update(|s| {
            if s.state == JobState::Queued {
                s.state = JobState::Running;
                s.progress_pct = Some(0);
                true
            } else {
                false
            }
        })
    }

    pub(crate) fn set_progress(&self, pct: u8) {
        self.update(|s| {
            if s.state == JobState::Running && s.progress_pct.is_none_or(|p| pct > p) {
                s.progress_pct = Some(pct.min(100));
                true
            } else {
                false
            }
        });
    }

    /// Cancel request. A queued job is cancelled immediately (the worker
    /// will see it and never start it); a running job moves to
    /// `cancel_requested` and the engine stops at its next checkpoint.
    /// Terminal jobs are unaffected. Returns the resulting state.
    pub fn request_cancel(&self) -> JobState {
        self.cancel.store(true, Ordering::SeqCst);
        let mut finished = false;
        self.update(|s| match s.state {
            JobState::Queued => {
                s.state = JobState::Cancelled;
                s.progress_pct = None;
                finished = true;
                true
            }
            JobState::Running => {
                s.state = JobState::CancelRequested;
                true
            }
            _ => false,
        });
        if finished {
            self.mark_finished();
        }
        self.state()
    }

    pub(crate) fn finish(&self, outcome: Outcome) {
        let display_stem = self.display_stem.clone();
        let target = self.target;
        let same_format = self.source_format == target;
        self.update(|s| {
            match outcome {
                Outcome::Completed { size } => {
                    s.state = JobState::Completed;
                    s.progress_pct = Some(100);
                    let stem = if same_format {
                        format!("{display_stem}_donusturuldu")
                    } else {
                        display_stem
                    };
                    s.result = Some(JobResultInfo {
                        output_name: format!("{stem}.{}", target.canonical_extension()),
                        size,
                        content_type: target.mime_type(),
                    });
                }
                Outcome::Failed { code } => {
                    s.state = JobState::Failed;
                    s.progress_pct = None;
                    s.error = Some(JobErrorBody {
                        code: code.to_string(),
                        message: crate::error::job_error_message(code).to_string(),
                    });
                }
                Outcome::Cancelled => {
                    s.state = JobState::Cancelled;
                    s.progress_pct = None;
                }
            }
            true
        });
        self.mark_finished();
    }

    fn mark_finished(&self) {
        if let Ok(mut f) = self.finished_at.lock() {
            f.get_or_insert_with(Instant::now);
        }
    }

    pub(crate) fn finished_at(&self) -> Option<Instant> {
        self.finished_at.lock().ok().and_then(|f| *f)
    }

    pub(crate) fn mark_deleted(&self) {
        self.deleted.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_deleted(&self) -> bool {
        self.deleted.load(Ordering::SeqCst)
    }

    /// Path of the published output (exists only once completed).
    pub(crate) fn output_path(&self) -> std::path::PathBuf {
        self.paths
            .out_dir
            .join(format!("result.{}", self.target.canonical_extension()))
    }
}
