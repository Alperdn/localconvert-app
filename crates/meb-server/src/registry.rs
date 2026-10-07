//! In-memory registry of uploaded files and jobs, with ONE centralized,
//! owner-scoped way to reach a record from a client-supplied id.
//!
//! `find_file` / `find_job` are the only functions that turn a client id
//! into a record. They parse strictly (`ids`), check the owner, and return
//! the same `ApiError::not_found()` for unknown, malformed and foreign ids.
//! A job found this way is wrapped in `OwnedJob`, whose constructor is
//! private to this module - so a handler cannot hold a job it did not get
//! through the owner check. The registry deliberately has no "get by id"
//! without an owner.
//!
//! No database: there is no persistent history by design, and a restart
//! purges all storage (see `storage`).

use crate::error::ApiError;
use crate::ids::{FileId, JobId};
use crate::jobs::{Job, JobState};
use crate::session::Owner;
use meb_core::format::SourceFormat;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub struct FileRecord {
    pub id: FileId,
    owner: Owner,
    pub display_name: String,
    /// The format the CONTENT proved to be at upload, never the claim its
    /// name made.
    pub format: SourceFormat,
    pub size: u64,
    /// Pixel dimensions, for an image. `None` for a PDF or an Office
    /// document, which have no single meaningful one.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub created_at_ms: u64,
    created: Instant,
}

#[derive(Debug, Serialize)]
pub struct FileView {
    pub file_id: String,
    pub display_name: String,
    pub size: u64,
    pub detected_format: &'static str,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub created_at_ms: u64,
}

impl FileRecord {
    pub(crate) fn new(
        id: FileId,
        owner: Owner,
        display_name: String,
        format: SourceFormat,
        size: u64,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Self {
        FileRecord {
            id,
            owner,
            display_name,
            format,
            size,
            width,
            height,
            created_at_ms: crate::jobs::now_ms(),
            created: Instant::now(),
        }
    }

    pub fn view(&self) -> FileView {
        FileView {
            file_id: self.id.to_string(),
            display_name: self.display_name.clone(),
            size: self.size,
            detected_format: self.format.canonical_extension(),
            width: self.width,
            height: self.height,
            created_at_ms: self.created_at_ms,
        }
    }

    pub(crate) fn owner(&self) -> &Owner {
        &self.owner
    }
}

/// A job proven to belong to the requesting owner.
pub struct OwnedJob(Arc<Job>);

impl Deref for OwnedJob {
    type Target = Job;
    fn deref(&self) -> &Job {
        &self.0
    }
}

/// Quota an admitted upload holds while it streams, before it becomes a
/// `FileRecord`. Counted exactly like a stored file, so concurrent uploads
/// of one session can never add up past the session's limits.
struct Reserved {
    owner: Owner,
    bytes: u64,
}

#[derive(Default)]
struct Inner {
    files: HashMap<FileId, Arc<FileRecord>>,
    jobs: HashMap<JobId, Arc<Job>>,
    reservations: HashMap<u64, Reserved>,
    next_reservation: u64,
}

impl Inner {
    /// (bytes held, file count) for one owner: stored uploads, the outputs
    /// of its jobs, and uploads currently streaming.
    fn usage_of(&self, owner: &Owner) -> (u64, usize) {
        let (bytes, count) = self
            .files
            .values()
            .filter(|f| f.owner == *owner)
            .fold((0u64, 0usize), |(b, c), f| {
                (b.saturating_add(f.size), c + 1)
            });
        let outputs: u64 = self
            .jobs
            .values()
            .filter(|j| j.owner() == owner)
            .filter_map(|j| j.snapshot().result.map(|r| r.size))
            .sum();
        let (pending_bytes, pending_count) = self
            .reservations
            .values()
            .filter(|r| r.owner == *owner)
            .fold((0u64, 0usize), |(b, c), r| {
                (b.saturating_add(r.bytes), c + 1)
            });
        (
            bytes.saturating_add(outputs).saturating_add(pending_bytes),
            count + pending_count,
        )
    }
}

/// An admitted upload's quota, held until it is committed by `insert_file`
/// or released by dropping this guard (any failure path, including a
/// timeout or a dropped connection).
pub struct UploadReservation {
    inner: Arc<Mutex<Inner>>,
    id: u64,
    held: bool,
}

impl Drop for UploadReservation {
    fn drop(&mut self) {
        if self.held {
            if let Ok(mut inner) = self.inner.lock() {
                inner.reservations.remove(&self.id);
            }
        }
    }
}

#[derive(Default)]
pub struct Registry {
    inner: Arc<Mutex<Inner>>,
}

impl Registry {
    fn lock(&self) -> Result<MutexGuard<'_, Inner>, ApiError> {
        self.inner
            .lock()
            .map_err(|_| ApiError::internal("registry", "poisoned lock"))
    }

    pub fn find_file(&self, owner: &Owner, raw_id: &str) -> Result<Arc<FileRecord>, ApiError> {
        let id = FileId::parse(raw_id).ok_or_else(ApiError::not_found)?;
        let inner = self.lock()?;
        match inner.files.get(&id) {
            Some(rec) if rec.owner == *owner => Ok(rec.clone()),
            _ => Err(ApiError::not_found()),
        }
    }

    /// A deleted job is already gone as far as any client is concerned - it
    /// stays in the map only until its worker exits (so it keeps counting
    /// against the active-job limits), and gets the same 404 as an id that
    /// never existed.
    pub fn find_job(&self, owner: &Owner, raw_id: &str) -> Result<OwnedJob, ApiError> {
        let id = JobId::parse(raw_id).ok_or_else(ApiError::not_found)?;
        let inner = self.lock()?;
        match inner.jobs.get(&id) {
            Some(job) if job.owner() == owner && !job.is_deleted() => Ok(OwnedJob(job.clone())),
            _ => Err(ApiError::not_found()),
        }
    }

    /// Admits one upload: checks the session's file count and byte quota
    /// and reserves this upload's share IN THE SAME LOCKED SECTION, so two
    /// concurrent uploads cannot both pass a check that only one of them
    /// fits in. The returned guard releases the reservation unless
    /// `insert_file` commits it.
    pub(crate) fn reserve_upload(
        &self,
        owner: &Owner,
        bytes: u64,
        max_files: usize,
        quota_bytes: u64,
    ) -> Result<UploadReservation, ApiError> {
        let mut inner = self.lock()?;
        let (held, count) = inner.usage_of(owner);
        if count >= max_files || held.saturating_add(bytes) > quota_bytes {
            return Err(ApiError::session_quota_exceeded());
        }
        inner.next_reservation += 1;
        let id = inner.next_reservation;
        inner.reservations.insert(
            id,
            Reserved {
                owner: owner.clone(),
                bytes,
            },
        );
        Ok(UploadReservation {
            inner: self.inner.clone(),
            id,
            held: true,
        })
    }

    /// Commits a reserved upload: the reservation is swapped for the stored
    /// record under one lock, so the session's usage never dips in between.
    pub(crate) fn insert_file(
        &self,
        rec: FileRecord,
        mut reservation: UploadReservation,
    ) -> Result<Arc<FileRecord>, ApiError> {
        let rec = Arc::new(rec);
        {
            let mut inner = self.lock()?;
            inner.reservations.remove(&reservation.id);
            inner.files.insert(rec.id, rec.clone());
        }
        // Already removed above; stop the guard from touching the lock.
        reservation.held = false;
        Ok(rec)
    }

    /// Inserts a job after re-checking the active-job limits under the same
    /// lock (so concurrent requests cannot both squeeze past them).
    pub(crate) fn insert_job_within_limits(
        &self,
        job: Arc<Job>,
        per_owner_limit: usize,
        global_limit: usize,
    ) -> Result<(), ApiError> {
        let mut inner = self.lock()?;
        let active = |j: &&Arc<Job>| !j.state().is_terminal();
        if inner.jobs.values().filter(active).count() >= global_limit {
            return Err(ApiError::server_busy());
        }
        if inner
            .jobs
            .values()
            .filter(active)
            .filter(|j| j.owner() == job.owner())
            .count()
            >= per_owner_limit
        {
            return Err(ApiError::too_many_jobs());
        }
        inner.jobs.insert(job.id, job);
        Ok(())
    }

    /// Removes the job record (the caller already owns it via `OwnedJob`).
    pub(crate) fn remove_job(&self, job: &OwnedJob) {
        self.remove_job_id(job.id);
    }

    /// Removes a job its worker has finished with. Not reachable from a
    /// client id: the worker only ever has the job it was spawned for.
    pub(crate) fn remove_finished_job(&self, job: &Job) {
        self.remove_job_id(job.id);
    }

    fn remove_job_id(&self, id: JobId) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.jobs.remove(&id);
        }
    }

    /// Janitor: removes and returns files older than `upload_ttl` and
    /// terminal jobs finished more than `job_ttl` ago.
    pub(crate) fn take_expired(
        &self,
        upload_ttl: Duration,
        job_ttl: Duration,
    ) -> (Vec<Arc<FileRecord>>, Vec<Arc<Job>>) {
        let Ok(mut inner) = self.inner.lock() else {
            return (Vec::new(), Vec::new());
        };
        let now = Instant::now();
        let expired_files: Vec<FileId> = inner
            .files
            .values()
            .filter(|f| now.duration_since(f.created) >= upload_ttl)
            .map(|f| f.id)
            .collect();
        let expired_jobs: Vec<JobId> = inner
            .jobs
            .values()
            .filter(|j| {
                j.finished_at()
                    .is_some_and(|t| now.duration_since(t) >= job_ttl)
                    && j.state().is_terminal()
            })
            .map(|j| j.id)
            .collect();
        let files = expired_files
            .iter()
            .filter_map(|id| inner.files.remove(id))
            .collect();
        let jobs = expired_jobs
            .iter()
            .filter_map(|id| inner.jobs.remove(id))
            .collect();
        (files, jobs)
    }

    /// Owners that still hold any file or job (so their session is kept).
    pub(crate) fn owners_with_resources(&self) -> HashSet<Owner> {
        let Ok(inner) = self.inner.lock() else {
            return HashSet::new();
        };
        inner
            .files
            .values()
            .map(|f| f.owner.clone())
            .chain(inner.jobs.values().map(|j| j.owner().clone()))
            .collect()
    }

    pub(crate) fn cancel_all(&self) {
        let jobs: Vec<Arc<Job>> = match self.inner.lock() {
            Ok(inner) => inner.jobs.values().cloned().collect(),
            Err(_) => return,
        };
        for job in jobs {
            if !matches!(
                job.state(),
                JobState::Completed | JobState::Failed | JobState::Cancelled
            ) {
                job.request_cancel();
            }
        }
    }
}
