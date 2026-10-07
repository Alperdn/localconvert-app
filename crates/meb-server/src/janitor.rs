//! Periodic expiry: unused uploads, terminal jobs (and their outputs) past
//! their TTL, idle sessions without resources, and deferred deletions.
//! Running jobs are never touched here - only terminal ones expire.

use crate::AppState;
use std::time::Duration;

pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    let interval: Duration = state.config.janitor_interval;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // first tick fires immediately
        loop {
            ticker.tick().await;
            let s = state.clone();
            let _ = tokio::task::spawn_blocking(move || sweep(&s)).await;
        }
    })
}

pub fn sweep(state: &AppState) {
    let (files, jobs) = state
        .registry
        .take_expired(state.config.upload_ttl, state.config.job_ttl);
    for file in &files {
        state.remove_tree_or_defer(&state.storage.file_dir(file.owner(), file.id));
    }
    for job in &jobs {
        state.remove_tree_or_defer(&job.paths.root);
    }

    let pending: Vec<_> = match state.pending_deletions.lock() {
        Ok(mut p) => std::mem::take(&mut *p),
        Err(_) => Vec::new(),
    };
    for path in pending {
        state.remove_tree_or_defer(&path);
    }

    let in_use = state.registry.owners_with_resources();
    let forgotten = state
        .sessions
        .prune_idle(state.config.session_idle_ttl, |o| in_use.contains(o));
    for owner in &forgotten {
        state.limiter.forget_idle(owner.key());
        state.remove_tree_or_defer(&state.storage.session_dir(owner));
    }

    if !files.is_empty() || !jobs.is_empty() || !forgotten.is_empty() {
        tracing::info!(
            expired_files = files.len(),
            expired_jobs = jobs.len(),
            pruned_sessions = forgotten.len(),
            "janitor sweep"
        );
    }
}
