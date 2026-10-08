//! Job execution.
//!
//! - Concurrency: a per-session semaphore (`max_running_per_session`) and a
//!   global one (`worker_permits`, the `light` class) must both be held
//!   before a job leaves `queued`.
//! - Isolation: the engine runs on a blocking thread inside
//!   `catch_unwind`. A panic fails only that job; the server keeps running.
//! - Validation: an engine "success" is accepted only if the declared output
//!   is a regular file inside `work/`, non-empty, within `max_output_bytes`,
//!   and sniffs as the format the job's `JobSpec` promised. Only then is it
//!   moved to `out/`. The check itself is the spec's (`output_matches`), so
//!   this module stays free of per-format knowledge.
//! - Cleanup: `in/` and `work/` are always removed; the whole job directory
//!   is removed on failure, cancellation or deletion.
//!
//! Timeouts: the native image engine runs in-process and cannot be killed
//! mid-call; its runtime is bounded by the engine's pixel limits. Wall-clock
//! timeouts with process-tree kill arrive with the external-process runner.

use crate::jobs::{Job, Outcome};
use crate::runner::{ConversionRunner, JobControl, RunError, RunRequest};
use crate::storage::is_regular_file_within;
use crate::AppState;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub struct Limiter {
    global: Arc<Semaphore>,
    per_owner: Mutex<HashMap<String, Arc<Semaphore>>>,
    per_owner_permits: usize,
}

impl Limiter {
    pub fn new(global_permits: usize, per_owner_permits: usize) -> Self {
        Limiter {
            global: Arc::new(Semaphore::new(global_permits.max(1))),
            per_owner: Mutex::new(HashMap::new()),
            per_owner_permits: per_owner_permits.max(1),
        }
    }

    async fn acquire(
        &self,
        owner_key: &str,
    ) -> Option<(OwnedSemaphorePermit, OwnedSemaphorePermit)> {
        let owner_sem = {
            let mut map = self.per_owner.lock().ok()?;
            map.entry(owner_key.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(self.per_owner_permits)))
                .clone()
        };
        let owner_permit = owner_sem.acquire_owned().await.ok()?;
        let global_permit = self.global.clone().acquire_owned().await.ok()?;
        Some((owner_permit, global_permit))
    }

    /// Drops idle per-owner semaphores (janitor).
    pub(crate) fn forget_idle(&self, owner_key: &str) {
        if let Ok(mut map) = self.per_owner.lock() {
            if map
                .get(owner_key)
                .is_some_and(|s| s.available_permits() == self.per_owner_permits)
            {
                map.remove(owner_key);
            }
        }
    }
}

/// Resolves as soon as the job is terminal - which is what a cancel (or a
/// delete, which cancels) does to a job that is still queued.
async fn until_terminal(job: &Job) {
    let mut rx = job.subscribe();
    loop {
        if rx.borrow_and_update().state.is_terminal() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Runs one job to a terminal state. Spawned by the create-job handler.
///
/// `runner` is resolved for the job's kind by the create-job handler, so a
/// job only ever reaches a worker with an engine that handles it.
pub(crate) async fn run_job(
    state: AppState,
    job: Arc<Job>,
    runner: Arc<dyn ConversionRunner>,
    inputs: Vec<std::path::PathBuf>,
) {
    // A job cancelled while queued must not sit in the permit queue behind
    // other sessions' work: it is already terminal, so stop waiting and
    // tear it down now.
    let permits = tokio::select! {
        biased;
        _ = until_terminal(&job) => {
            cleanup(&state, &job);
            return;
        }
        permits = state.limiter.acquire(job.owner().key()) => permits,
    };
    if permits.is_none() {
        job.finish(Outcome::Failed {
            code: "INTERNAL_ERROR",
        });
        cleanup(&state, &job);
        return;
    }

    if !job.try_start() {
        // Cancelled (or deleted) between getting the permit and starting.
        cleanup(&state, &job);
        return;
    }

    let max_output = state.config.max_output_bytes;
    let job_for_thread = job.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        execute(runner.as_ref(), &job_for_thread, &inputs, max_output)
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!(job_id = %job.id, error = %e, "job thread failed");
        Outcome::Failed {
            code: "INTERNAL_ERROR",
        }
    });
    drop(permits);

    let label = match &outcome {
        Outcome::Completed { .. } => "completed",
        Outcome::Failed { code } => code,
        Outcome::Cancelled => "cancelled",
    };
    tracing::info!(job_id = %job.id, outcome = label, "job finished");
    job.finish(outcome);
    cleanup(&state, &job);
}

fn execute(
    runner: &dyn ConversionRunner,
    job: &Job,
    inputs: &[std::path::PathBuf],
    max_output: u64,
) -> Outcome {
    let output = job.paths.work_dir.join(job.output_file_name());
    let progress = |pct: u8| job.set_progress(pct);
    let control = JobControl {
        cancel: job.cancel_flag(),
        progress: &progress,
    };
    let request = RunRequest {
        inputs,
        output: &output,
        spec: &job.spec,
    };

    // Measured BEFORE the engine runs, while the inputs are certainly
    // still staged: the inputs of a job are deleted as soon as it ends.
    let source_size: u64 = inputs
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();

    let result = catch_unwind(AssertUnwindSafe(|| runner.run(&request, &control)));
    match result {
        Err(_) => {
            tracing::error!(job_id = %job.id, "engine panicked; job failed, server unaffected");
            Outcome::Failed {
                code: "INTERNAL_ERROR",
            }
        }
        Ok(Err(RunError::Cancelled)) => Outcome::Cancelled,
        Ok(Err(RunError::Failed { code, detail })) => {
            tracing::info!(job_id = %job.id, code, detail = %detail, "engine reported failure");
            Outcome::Failed { code }
        }
        Ok(Ok(())) => {
            // A cancel that arrived after the engine's last checkpoint still
            // wins as long as nothing has been published yet.
            if job.is_cancel_requested() {
                return Outcome::Cancelled;
            }
            publish_output(job, &output, max_output, source_size)
        }
    }
}

fn publish_output(job: &Job, produced: &Path, max_output: u64, source_size: u64) -> Outcome {
    let invalid = |why: &str| {
        tracing::warn!(job_id = %job.id, reason = why, "output rejected");
        Outcome::Failed {
            code: "OUTPUT_VALIDATION_FAILED",
        }
    };
    if !is_regular_file_within(&job.paths.work_dir, produced) {
        return invalid("not a regular file inside the work dir");
    }
    let size = match std::fs::metadata(produced) {
        Ok(m) => m.len(),
        Err(_) => return invalid("unreadable"),
    };
    if size == 0 || size > max_output {
        return invalid("size out of bounds");
    }
    if let Err(reason) = job.spec.validate_output(produced) {
        return invalid(reason);
    }
    let destination = job.output_path();
    if std::fs::rename(produced, &destination).is_err() {
        return invalid("could not publish");
    }
    Outcome::Completed { size, source_size }
}

/// Post-terminal cleanup. Inputs and intermediates never outlive a job;
/// the published output stays only for completed, non-deleted jobs.
///
/// A job deleted while it was still executing stayed in the registry so it
/// kept counting against the active-job limits; this is where it is
/// finally dropped - the worker has exited, so the resources really are
/// free. (`delete_job` already made it invisible to its owner.)
fn cleanup(state: &AppState, job: &Job) {
    let deleted = job.is_deleted();
    let keep_output = job.state() == crate::jobs::JobState::Completed && !deleted;
    if keep_output {
        state.remove_tree_or_defer(&job.paths.input_dir);
        state.remove_tree_or_defer(&job.paths.work_dir);
    } else {
        state.remove_tree_or_defer(&job.paths.root);
    }
    if deleted {
        state.registry.remove_finished_job(job);
    }
}
