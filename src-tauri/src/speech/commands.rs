//! Tauri commands for Ses Dikte. Deliberately thin: the frontend can start a
//! job on a file it picked, cancel the running job, and read status/limits.
//! It can NOT supply an executable, a model path, an engine name or a URL -
//! `path` is validated by `pipeline::validate_source` (local file, allowlisted
//! extension, size limit) and everything else is fixed in the backend.

use super::job::JobRegistry;
use super::limits::{SpeechLimits, DEFAULT_LIMITS};
use super::pipeline::{self, InputInfo, JobState};
use crate::engines::speech::{self, SpeechEngineReport, Transcript, WhisperCppEngine};
use crate::engines::speech_error::{SpeechError, SpeechErrorCode, SpeechErrorDto};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{AppHandle, Emitter, State};

pub const UPDATE_EVENT: &str = "speech-job-update";

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptDto {
    pub language: String,
    pub text: String,
    pub segments: Vec<speech::Segment>,
    /// Segment-level, approximate. `false` => the UI must hide the option.
    pub timestamps_available: bool,
}

impl From<Transcript> for TranscriptDto {
    fn from(t: Transcript) -> Self {
        Self { language: t.language.clone(), text: t.plain_text(), timestamps_available: t.timestamps_available, segments: t.segments }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobUpdate {
    pub job_id: String,
    pub state: JobState,
    pub elapsed_secs: u64,
    /// Real engine-reported progress only; `None` = indeterminate.
    pub progress_pct: Option<u8>,
    pub transcript: Option<TranscriptDto>,
    pub error: Option<SpeechErrorDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LimitsDto {
    pub max_input_mb: u64,
    pub max_audio_hours: u64,
    pub max_recording_hours: u64,
}

#[tauri::command]
pub async fn speech_get_status() -> SpeechEngineReport {
    // Hashing the model on first use is CPU/disk work: keep it off the async runtime.
    tauri::async_runtime::spawn_blocking(speech::report)
        .await
        .unwrap_or_else(|_| speech::report())
}

#[tauri::command]
pub async fn speech_get_limits() -> LimitsDto {
    let l = DEFAULT_LIMITS;
    LimitsDto {
        max_input_mb: l.max_input_bytes / (1024 * 1024),
        max_audio_hours: l.max_audio_secs / 3600,
        max_recording_hours: l.max_recording_secs / 3600,
    }
}

#[tauri::command]
pub async fn speech_inspect_file(path: String) -> Result<InputInfo, SpeechErrorDto> {
    tauri::async_runtime::spawn_blocking(move || pipeline::inspect_input(&path, &DEFAULT_LIMITS))
        .await
        .map_err(|_| SpeechError::new(SpeechErrorCode::SpeechPreprocessFailed).to_dto())?
        .map_err(|e| e.to_dto())
}

#[tauri::command]
pub fn speech_cancel_job(registry: State<'_, Arc<JobRegistry>>, job_id: String) -> bool {
    registry.cancel(&job_id)
}

#[tauri::command]
pub async fn speech_start_file_job(
    app: AppHandle,
    registry: State<'_, Arc<JobRegistry>>,
    path: String,
    language: String,
) -> Result<String, SpeechErrorDto> {
    let slot = registry.inner().try_begin().map_err(|e| e.to_dto())?;
    let job_id = slot.id.clone();

    // Fail fast (and visibly) when the engine is missing/corrupt/unsupported,
    // instead of starting a job that can only fail. No cloud fallback exists.
    let engine = tauri::async_runtime::spawn_blocking(WhisperCppEngine::load)
        .await
        .map_err(|_| SpeechError::new(SpeechErrorCode::SpeechTranscriptionFailed).to_dto())?
        .map_err(|e| e.to_dto())?;

    let id_for_thread = job_id.clone();
    std::thread::spawn(move || {
        run_job_thread(app, slot, id_for_thread, engine, path, language, DEFAULT_LIMITS);
    });
    Ok(job_id)
}

fn run_job_thread(
    app: AppHandle,
    slot: super::job::JobSlot,
    job_id: String,
    engine: WhisperCppEngine,
    path: String,
    language: String,
    limits: SpeechLimits,
) {
    let started = Instant::now();
    let current: Mutex<(JobState, Option<u8>)> = Mutex::new((JobState::Preparing, None));
    let done = AtomicBool::new(false);

    let emit = |state: JobState, pct: Option<u8>, transcript: Option<TranscriptDto>, error: Option<SpeechErrorDto>| {
        let _ = app.emit(
            UPDATE_EVENT,
            JobUpdate { job_id: job_id.clone(), state, elapsed_secs: started.elapsed().as_secs(), progress_pct: pct, transcript, error },
        );
    };

    let result = std::thread::scope(|scope| {
        // 1 Hz ticker so the UI can show "Geçen süre" even when the engine
        // reports no progress.
        scope.spawn(|| {
            while !done.load(Ordering::SeqCst) {
                let (s, p) = *current.lock().unwrap_or_else(|e| e.into_inner());
                emit(s, p, None, None);
                for _ in 0..10 {
                    if done.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });
        let on_state = |s: JobState, p: Option<u8>| {
            *current.lock().unwrap_or_else(|e| e.into_inner()) = (s, p);
            emit(s, p, None, None);
        };
        let r = pipeline::run_file_job(&path, &language, &limits, &engine, &slot.cancel, &on_state);
        done.store(true, Ordering::SeqCst);
        r
    });

    // Free the single job slot BEFORE announcing the outcome: the UI may click
    // "Yeniden dene" the instant it sees a terminal state, and that start must
    // not be refused as SPEECH_BUSY. (The job's temp dir is already gone - it is
    // owned by `run_file_job` - so nothing is left behind either way.)
    drop(slot);

    match result {
        Ok(t) => emit(JobState::Completed, Some(100), Some(t.into()), None),
        Err(e) if e.code == SpeechErrorCode::SpeechCancelled => emit(JobState::Cancelled, None, None, Some(e.to_dto())),
        Err(e) => emit(JobState::Failed, None, None, Some(e.to_dto())),
    }
}
