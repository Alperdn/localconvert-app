//! Job endpoints. Every handler that takes a job id goes through
//! `Registry::find_job` (owner-scoped, uniform 404) - there is no other way
//! to obtain a job from a client-supplied id.

use crate::error::ApiError;
use crate::ids::JobId;
use crate::jobs::{Job, JobSnapshot, JobState};
use crate::names::{content_disposition, stem_of};
use crate::session::Owner;
use crate::spec::{CreateJobRequest, JobKind, JobSpec};
use crate::storage::is_regular_file_within;
use crate::AppState;
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, CONTENT_TYPE,
};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::Stream;
use std::convert::Infallible;
use tokio_util::io::ReaderStream;

const MAX_JOB_REQUEST_BYTES: usize = 16 * 1024;

/// Creates one job: name a kind, name an input, describe the work.
///
/// Order of decisions - each one narrows what the rest may assume:
/// 1. the body is a strictly-parsed `CreateJobRequest` (unknown fields are
///    rejected, so no id or path can be smuggled in);
/// 2. `kind` must be one this API publishes (`JobKind::parse`);
/// 3. an engine must be registered for that kind, or the kind is not
///    reachable at all (`UNSUPPORTED_CONVERSION`) - this is where a kind
///    whose engine is missing or unimplemented is refused, before any work,
///    workspace or job record exists;
/// 4. every input must be this owner's file, and there must be the number
///    the kind takes;
/// 5. the request becomes a validated `JobSpec` - the last point at which
///    client input is interpreted. Everything downstream reads the spec.
pub async fn create_job(
    State(state): State<AppState>,
    owner: Owner,
    body: Bytes,
) -> Result<(StatusCode, Json<JobSnapshot>), ApiError> {
    if body.len() > MAX_JOB_REQUEST_BYTES {
        return Err(ApiError::invalid_request());
    }
    // Parsed by hand (not the `Json` extractor) so every rejection uses the
    // structured error envelope. Unknown fields are rejected.
    let request: CreateJobRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::invalid_request())?;
    let kind = JobKind::parse(&request.kind).ok_or_else(ApiError::unsupported_conversion)?;
    let runner = state
        .runners
        .for_kind(kind)
        .ok_or_else(ApiError::unsupported_conversion)?;

    let mut files = Vec::new();
    for raw_id in request.input_ids(kind)? {
        files.push(state.registry.find_file(&owner, raw_id)?);
    }
    let sources: Vec<_> = files.iter().map(|f| f.format).collect();
    let spec = JobSpec::from_request(kind, &request, &sources)?;

    // The download name is built from the FIRST input, which for a
    // single-input job is simply "the file".
    let display_stem = stem_of(&files[0].display_name).to_string();
    let job_id = JobId::generate();
    let paths = state.storage.job_paths(&owner, job_id);
    let job = Job::new(
        job_id,
        owner.clone(),
        files.iter().map(|f| f.id).collect(),
        display_stem,
        spec,
        paths.clone(),
    );
    state.registry.insert_job_within_limits(
        job.clone(),
        state.config.max_active_jobs_per_session,
        state.config.max_active_jobs_global,
    )?;

    // Stage the inputs into the job's own workspace now, so the job no
    // longer depends on the uploads (which may expire independently).
    // Names are positional and server-built, so a display name never
    // becomes a path component.
    let staging: Vec<(std::path::PathBuf, std::path::PathBuf)> = files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let blob = state.storage.file_blob(&owner, file.id);
            let input = paths.input_dir.join(format!(
                "source-{index:02}.{}",
                file.format.canonical_extension()
            ));
            (blob, input)
        })
        .collect();
    let inputs: Vec<std::path::PathBuf> = staging.iter().map(|(_, i)| i.clone()).collect();
    let staged = {
        let (paths, staging) = (paths.clone(), staging.clone());
        tokio::task::spawn_blocking(move || stage_workspace(&paths, &staging)).await
    };
    if !matches!(staged, Ok(Ok(()))) {
        tracing::error!(job_id = %job_id, "workspace staging failed");
        job.finish(crate::jobs::Outcome::Failed {
            code: "WORKSPACE_ERROR",
        });
        state.remove_tree_or_defer(&paths.root);
        return Ok((StatusCode::ACCEPTED, Json(job.snapshot())));
    }

    tracing::info!(
        job_id = %job_id,
        inputs = files.len(),
        kind = kind.wire(),
        output_format = job.spec.output_extension(),
        "job queued"
    );
    tokio::spawn(crate::worker::run_job(
        state.clone(),
        job.clone(),
        runner,
        inputs,
    ));
    Ok((StatusCode::ACCEPTED, Json(job.snapshot())))
}

/// Creates the job's directories and links (or copies) each stored blob to
/// its staged input path.
fn stage_workspace(
    paths: &crate::storage::JobPaths,
    staging: &[(std::path::PathBuf, std::path::PathBuf)],
) -> std::io::Result<()> {
    std::fs::create_dir_all(&paths.input_dir)?;
    std::fs::create_dir_all(&paths.work_dir)?;
    std::fs::create_dir_all(&paths.out_dir)?;
    for (blob, input) in staging {
        if std::fs::hard_link(blob, input).is_err() {
            std::fs::copy(blob, input)?;
        }
    }
    Ok(())
}

pub async fn get_job(
    State(state): State<AppState>,
    owner: Owner,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<JobSnapshot>, ApiError> {
    let job = state.registry.find_job(&owner, &id)?;
    Ok(Json(job.snapshot()))
}

pub async fn cancel_job(
    State(state): State<AppState>,
    owner: Owner,
    UrlPath(id): UrlPath<String>,
) -> Result<(StatusCode, Json<JobSnapshot>), ApiError> {
    let job = state.registry.find_job(&owner, &id)?;
    let resulting = job.request_cancel();
    let status = if resulting.is_terminal() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((status, Json(job.snapshot())))
}

/// Cancels if needed and makes the job invisible to its owner immediately
/// (later requests: 404).
///
/// Teardown depends on whether a worker is still executing it. A terminal
/// job has no worker left, so its record and workspace go now. A job that
/// is still queued or running keeps its registry entry - and therefore
/// keeps counting against the active-job limits, so deleting cannot be
/// used to start more work than the limits allow - until its worker exits
/// and removes both (see `worker::cleanup`).
pub async fn delete_job(
    State(state): State<AppState>,
    owner: Owner,
    UrlPath(id): UrlPath<String>,
) -> Result<StatusCode, ApiError> {
    let job = state.registry.find_job(&owner, &id)?;
    // Marked deleted BEFORE cancelling: the worker reads this flag when it
    // wakes, so it cannot decide to keep an output for a deleted job.
    job.mark_deleted();
    job.request_cancel();
    if job.state().is_terminal() {
        state.registry.remove_job(&job);
        state.remove_tree_or_defer(&job.paths.root);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// SSE: the first event is always the CURRENT snapshot (so a reconnecting
/// client never depends on having seen earlier transient events), then one
/// event per change; the stream ends after the terminal snapshot.
pub async fn job_events(
    State(state): State<AppState>,
    owner: Owner,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, ApiError> {
    let job = state.registry.find_job(&owner, &id)?;
    let receiver = job.subscribe();
    drop(job);
    let mut sse = Sse::new(snapshot_stream(receiver))
        .keep_alive(KeepAlive::default())
        .into_response();
    sse.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    sse.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    Ok(sse)
}

fn snapshot_stream(
    receiver: tokio::sync::watch::Receiver<JobSnapshot>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    futures_util::stream::unfold(
        (receiver, true, false),
        |(mut rx, first, done)| async move {
            if done {
                return None;
            }
            if !first && rx.changed().await.is_err() {
                return None; // job gone (deleted) - end the stream
            }
            let snapshot = rx.borrow_and_update().clone();
            let terminal = snapshot.state.is_terminal();
            let event = Event::default()
                .event("job")
                .id(snapshot.seq.to_string())
                .json_data(&snapshot)
                .unwrap_or_else(|_| Event::default().event("error"));
            Some((Ok(event), (rx, false, terminal)))
        },
    )
}

pub async fn download(
    State(state): State<AppState>,
    owner: Owner,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, ApiError> {
    let job = state.registry.find_job(&owner, &id)?;
    let snapshot = job.snapshot();
    if snapshot.state != JobState::Completed {
        return Err(ApiError::not_ready());
    }
    let result = snapshot
        .result
        .ok_or_else(|| ApiError::internal("download", "completed job without result"))?;
    let path = job.output_path();
    if !is_regular_file_within(&job.paths.out_dir, &path) {
        // Expired/deleted between the state check and here.
        return Err(ApiError::not_found());
    }
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found())?;
    let size = file
        .metadata()
        .await
        .map(|m| m.len())
        .map_err(|e| ApiError::internal("download", e))?;

    let mut response = Body::from_stream(ReaderStream::new(file)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static(job.spec.output_mime()),
    );
    headers.insert(CONTENT_LENGTH, HeaderValue::from(size));
    headers.insert(
        CONTENT_DISPOSITION,
        content_disposition(&result.output_name),
    );
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    tracing::info!(job_id = %job.id, size, "download served");
    Ok(response)
}
