//! The uploaded-audio pipeline (Phase C):
//!
//! ```text
//! validate source -> limits -> ffprobe -> ffmpeg normalize (16k mono PCM16 WAV)
//!   -> exact-duration + silence checks -> SpeechEngine -> Transcript
//! ```
//!
//! Pure Rust with no Tauri types so it is unit/integration-testable: progress
//! goes out through a callback, cancellation comes in through an `AtomicBool`.
//! Every failure path returns a structured `SpeechError`; the job directory
//! is a `SpeechJobDir`, so all temporary audio is removed on every exit path.

use super::job::SpeechJobDir;
use super::limits::{free_disk_bytes, SpeechLimits};
use crate::engines::ascii_link;
use crate::engines::audio_prep::{self, ensure_local_input_path};
use crate::engines::audio_wav::{analyze_energy, read_wav_info};
use crate::engines::speech::{SpeechEngine, TranscribeRequest, Transcript};
use crate::engines::speech_error::{SpeechError, SpeechErrorCode};
use crate::security::path_validation;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const NORMALIZED_WAV: &str = "norm.wav";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// "Ses hazırlanıyor"
    Preparing,
    /// "Dikte ediliyor"
    Transcribing,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputInfo {
    /// For display only - never used to build a path.
    pub file_name: String,
    pub size_bytes: u64,
    pub duration_secs: Option<f64>,
    pub extension: String,
}

fn cancelled(cancel: &AtomicBool) -> Result<(), SpeechError> {
    if cancel.load(Ordering::SeqCst) {
        Err(SpeechError::new(SpeechErrorCode::SpeechCancelled))
    } else {
        Ok(())
    }
}

/// Validates a frontend-supplied path down to a canonical local file with an
/// allowlisted extension and an acceptable size. No process is started.
pub fn validate_source(raw: &str, limits: &SpeechLimits) -> Result<(PathBuf, &'static str, u64), SpeechError> {
    let trimmed = raw.trim();
    if trimmed.contains("://") || trimmed.starts_with("\\\\") || trimmed.starts_with("//") {
        return Err(SpeechError::with_message(
            SpeechErrorCode::SpeechInputUnsupported,
            "Yalnızca bu bilgisayardaki yerel dosyalar kullanılabilir.",
        ));
    }
    // Extension allowlist first: a non-audio file is refused without being opened.
    let ext = audio_prep::accepted_extension(Path::new(trimmed))
        .ok_or_else(|| SpeechError::new(SpeechErrorCode::SpeechInputUnsupported))?;
    let canonical = path_validation::validate_input_file(trimmed).map_err(|e| {
        let code = if Path::new(trimmed).exists() {
            SpeechErrorCode::SpeechInputUnsupported
        } else {
            SpeechErrorCode::SpeechInputNotFound
        };
        SpeechError::new(code).with_detail(e)
    })?;
    ensure_local_input_path(&canonical)?;
    if canonical.to_string_lossy().starts_with("UNC\\") {
        return Err(SpeechError::with_message(
            SpeechErrorCode::SpeechInputUnsupported,
            "Yalnızca bu bilgisayardaki yerel dosyalar kullanılabilir.",
        ));
    }
    let size = std::fs::metadata(&canonical)
        .map_err(|e| SpeechError::new(SpeechErrorCode::SpeechInputNotFound).with_detail(e.to_string()))?
        .len();
    limits.check_input_size(size)?;
    Ok((canonical, ext, size))
}

/// Everything the UI shows before "Başlat": name, size, duration (if the
/// container states one). Uses the same validation + ffprobe as the real job.
pub fn inspect_input(raw: &str, limits: &SpeechLimits) -> Result<InputInfo, SpeechError> {
    let (path, ext, size) = validate_source(raw, limits)?;
    let dir = SpeechJobDir::create().map_err(|e| {
        SpeechError::new(SpeechErrorCode::SpeechPreprocessFailed).with_detail(e.to_string())
    })?;
    let never = AtomicBool::new(false);
    let probe = audio_prep::probe(&audio_prep::spec_abs(&path), dir.path(), &never)?;
    if let Some(d) = probe.duration_secs {
        limits.check_duration(d)?;
    }
    Ok(InputInfo {
        file_name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        size_bytes: size,
        duration_secs: probe.duration_secs,
        extension: ext.to_string(),
    })
}

/// Runs the whole file job. `on_state` is called on every state change and on
/// real engine progress (`Some(pct)`); it is never called with invented progress.
pub fn run_file_job(
    raw_path: &str,
    language: &str,
    limits: &SpeechLimits,
    engine: &dyn SpeechEngine,
    cancel: &AtomicBool,
    on_state: &(dyn Fn(JobState, Option<u8>) + Sync),
) -> Result<Transcript, SpeechError> {
    run_file_job_in(&super::job::speech_temp_root(), raw_path, language, limits, engine, cancel, on_state)
}

/// Same as `run_file_job` with an explicit job-directory root (tests use non-ASCII roots).
pub fn run_file_job_in(
    job_root: &Path,
    raw_path: &str,
    language: &str,
    limits: &SpeechLimits,
    engine: &dyn SpeechEngine,
    cancel: &AtomicBool,
    on_state: &(dyn Fn(JobState, Option<u8>) + Sync),
) -> Result<Transcript, SpeechError> {
    on_state(JobState::Preparing, None);

    if !engine.info().languages.iter().any(|l| l == language) {
        return Err(SpeechError::new(SpeechErrorCode::SpeechLanguageUnsupported));
    }
    let (input, ext, size) = validate_source(raw_path, limits)?;
    cancelled(cancel)?;

    let job_dir = SpeechJobDir::create_in(job_root)
        .map_err(|e| SpeechError::new(SpeechErrorCode::SpeechPreprocessFailed).with_detail(e.to_string()))?;
    let free = free_disk_bytes(job_dir.path());
    // Duration unknown yet: worst-case disk requirement first, tightened after probing.
    limits.check_disk(size, None, free)?;

    // Children never see the user's path: the source is hard-linked (or copied) into the
    // UUID job dir under a generated ASCII name. The original path is display metadata only.
    let input_name = format!("input.{ext}");
    ascii_link::stage_file(&input, job_dir.path(), &input_name, cancel)?;
    cancelled(cancel)?;
    let input_spec = audio_prep::spec_rel(&input_name);

    let probe = audio_prep::probe(&input_spec, job_dir.path(), cancel)?;
    cancelled(cancel)?;
    if let Some(d) = probe.duration_secs {
        limits.check_duration(d)?;
        limits.check_disk(size, Some(d), free)?;
    }

    let wav = audio_prep::normalize(&input_spec, job_dir.path(), NORMALIZED_WAV, limits.max_audio_secs, cancel)?;
    cancelled(cancel)?;

    let info = read_wav_info(&wav)
        .filter(|i| i.is_speech_normalized())
        .ok_or_else(|| SpeechError::new(SpeechErrorCode::SpeechPreprocessFailed).with_detail("normalized wav invalid"))?;
    // Authoritative duration: measured from the decoded samples, independent
    // of whatever the container claimed.
    limits.check_duration(info.duration_secs())?;
    if info.duration_secs() < 0.1 {
        return Err(SpeechError::new(SpeechErrorCode::SpeechNoSpeechDetected));
    }
    // Pure silence never reaches Whisper (it hallucinates text on silence).
    let energy = analyze_energy(&wav, &info)
        .map_err(|e| SpeechError::new(SpeechErrorCode::SpeechPreprocessFailed).with_detail(e.to_string()))?;
    if energy.looks_silent() {
        return Err(SpeechError::new(SpeechErrorCode::SpeechNoSpeechDetected));
    }
    cancelled(cancel)?;

    on_state(JobState::Transcribing, None);
    let progress = |p: Option<u8>| on_state(JobState::Transcribing, p);
    let req = TranscribeRequest { job_dir: job_dir.path(), wav_name: NORMALIZED_WAV, language };
    let transcript = engine.transcribe(&req, cancel, &progress)?;
    cancelled(cancel)?;
    Ok(transcript)
    // `job_dir` drops here (and on every early return above): normalized WAV,
    // JSON output and any intermediates are deleted.
}
