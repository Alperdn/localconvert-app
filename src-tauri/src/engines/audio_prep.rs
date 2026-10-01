//! Speech audio preprocessing on top of the bundled minimal FFmpeg
//! (`EngineId::AudioFfmpeg` / `AudioFfprobe`).
//!
//! FFmpeg's ONLY job here: validate (ffprobe) and normalize (ffmpeg) untrusted
//! uploaded audio to 16 kHz / mono / PCM s16le WAV in the job directory.
//! It knows nothing about transcription.
//!
//! Hardening for untrusted input (see also `scripts/ffmpeg/build-ffmpeg.sh`,
//! which compiles in only the `file` protocol and a handful of audio decoders):
//! - structured argv only, no shell (`process::build_command_ex`);
//! - `-protocol_whitelist file` and a `file:` prefix on the input, so even a
//!   future build with more protocols could not open a URL from this path;
//! - input path is a validated absolute local path built by the backend;
//!   UNC/remote paths and URLs are rejected before any process starts;
//! - `-map 0:a:0 -vn -sn -dn`: only the first audio stream is decoded;
//! - `-t <limit+1s>`: output size is bounded even if a container lies about
//!   its duration;
//! - `-nostdin`, and cwd = the job directory.

use super::bundle;
use super::engine_id::EngineId;
use super::ffmpeg_manifest::{self, FfmpegCheck};
use super::process::{self, CancellableRun, ProcessEnd};
use super::resolver::{self, ResolvedEngine};
use super::speech_error::{SpeechError, SpeechErrorCode};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// Decode speed is typically >50x real time; this only bounds a hung process.
pub const NORMALIZE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Extensions the pipeline accepts. An extension is listed here ONLY because a
/// real fixture of that type passes end to end (see `speech::tests`).
pub const ACCEPTED_EXTENSIONS: [&str; 6] = ["wav", "mp3", "m4a", "aac", "flac", "ogg"];

pub fn accepted_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    ACCEPTED_EXTENSIONS.iter().copied().find(|e| *e == ext)
}

#[derive(Debug, Clone)]
pub struct ProbeInfo {
    /// `None` when the container does not state a duration (e.g. raw ADTS AAC);
    /// the exact duration is then measured from the normalized WAV.
    pub duration_secs: Option<f64>,
}

#[derive(Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}
#[derive(Deserialize)]
struct ProbeStream {
    #[serde(default)]
    codec_type: String,
    duration: Option<String>,
}
#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

fn parse_probe(json: &str) -> Result<ProbeInfo, SpeechError> {
    let p: ProbeJson = serde_json::from_str(json)
        .map_err(|e| SpeechError::new(SpeechErrorCode::SpeechPreprocessFailed).with_detail(e.to_string()))?;
    let audio: Vec<&ProbeStream> = p.streams.iter().filter(|s| s.codec_type == "audio").collect();
    if audio.is_empty() {
        return Err(SpeechError::with_message(
            SpeechErrorCode::SpeechInputUnsupported,
            "Bu dosyada ses akışı bulunamadı.",
        ));
    }
    let dur = p
        .format
        .and_then(|f| f.duration)
        .or_else(|| audio[0].duration.clone())
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d >= 0.0);
    Ok(ProbeInfo { duration_secs: dur })
}

/// Outcome of resolving + verifying the bundled audio FFmpeg.
pub fn resolve_verified(id: EngineId) -> Result<ResolvedEngine, SpeechError> {
    debug_assert!(matches!(id, EngineId::AudioFfmpeg | EngineId::AudioFfprobe));
    let dir = resolver::bundled_engine_dir(id).ok_or_else(|| SpeechError::new(SpeechErrorCode::SpeechEngineMissing))?;
    ffmpeg_manifest::check_dir(&dir).map_err(|c| match c {
        FfmpegCheck::Missing => SpeechError::with_message(
            SpeechErrorCode::SpeechEngineMissing,
            "Ses hazırlama bileşeni bu kurulumda bulunamadı. Lütfen uygulamayı yeniden yükleyin veya BT yöneticinizle iletişime geçin.",
        ),
        FfmpegCheck::ArchitectureUnsupported => SpeechError::new(SpeechErrorCode::SpeechCpuUnsupported),
        FfmpegCheck::Corrupt | FfmpegCheck::VersionMismatch => SpeechError::with_message(
            SpeechErrorCode::SpeechEngineInvalid,
            "Ses hazırlama bileşeni doğrulanamadı; kurulum bozulmuş olabilir. Lütfen uygulamayı yeniden yükleyin.",
        ),
    })?;
    resolver::resolve(id).map_err(|e| {
        SpeechError::new(SpeechErrorCode::SpeechEngineMissing).with_detail(format!("{:?}", e))
    })
}

/// A local, absolute, non-UNC, non-URL path. Anything else is refused before
/// a process is ever started.
pub fn ensure_local_input_path(p: &Path) -> Result<(), SpeechError> {
    let s = p.to_string_lossy();
    let bad = !p.is_absolute()
        || s.contains('\0')
        || s.contains("://")
        || s.starts_with("\\\\")
        || s.starts_with("//");
    if bad {
        return Err(SpeechError::with_message(
            SpeechErrorCode::SpeechInputUnsupported,
            "Yalnızca bu bilgisayardaki yerel dosyalar kullanılabilir.",
        ));
    }
    Ok(())
}

/// `file:` spec for an absolute local path (used only to *inspect* a picked file;
/// ffprobe/ffmpeg open Unicode paths natively).
pub fn spec_abs(p: &Path) -> String {
    format!("file:{}", p.to_string_lossy())
}

/// `file:` spec for a generated ASCII name relative to the job dir (the child's cwd).
pub fn spec_rel(name: &str) -> String {
    debug_assert!(name.is_ascii() && !name.contains(['/', '\\']));
    format!("file:{name}")
}

fn end_to_error(end: &ProcessEnd, stderr: &str, fail_code: SpeechErrorCode) -> SpeechError {
    match end {
        ProcessEnd::Cancelled => SpeechError::new(SpeechErrorCode::SpeechCancelled),
        ProcessEnd::TimedOut => SpeechError::new(fail_code).with_detail("timed out"),
        ProcessEnd::SpawnFailed => SpeechError::new(SpeechErrorCode::SpeechEngineInvalid).with_detail(stderr.to_string()),
        ProcessEnd::Failed(code) => SpeechError::new(fail_code).with_detail(format!("exit {:?}: {}", code, stderr)),
        ProcessEnd::Success => SpeechError::new(fail_code),
    }
}

/// Validates `input` with the bundled ffprobe.
pub fn probe(input_spec: &str, job_dir: &Path, cancel: &AtomicBool) -> Result<ProbeInfo, SpeechError> {
    let ffprobe = resolve_verified(EngineId::AudioFfprobe)?;
    let args: Vec<String> = [
        "-v", "error", "-hide_banner", "-protocol_whitelist", "file", "-print_format", "json",
        "-show_format", "-show_streams", "-i",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(std::iter::once(input_spec.to_string()))
    .collect();
    let res = process::run_cancellable(
        &ffprobe,
        &args,
        job_dir,
        CancellableRun { timeout: Some(PROBE_TIMEOUT), cancel, low_priority: false, on_stderr_line: None },
    );
    match res.end {
        ProcessEnd::Success => parse_probe(&res.stdout),
        // ffprobe could not make sense of the file: corrupt or not audio.
        other => Err(end_to_error(&other, &res.stderr_tail, SpeechErrorCode::SpeechPreprocessFailed)),
    }
}

/// Decodes the first audio stream of `input` to `<job_dir>/<out_name>` as
/// 16 kHz mono PCM s16le WAV, at most `max_secs + 1` seconds long.
pub fn normalize(
    input_spec: &str,
    job_dir: &Path,
    out_name: &str,
    max_secs: u64,
    cancel: &AtomicBool,
) -> Result<PathBuf, SpeechError> {
    debug_assert!(!out_name.contains(['/', '\\']));
    let ffmpeg = resolve_verified(EngineId::AudioFfmpeg)?;
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-v", "error", "-protocol_whitelist", "file", "-i"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push(input_spec.to_string());
    for a in [
        "-map", "0:a:0", "-vn", "-sn", "-dn", "-map_metadata", "-1", "-ac", "1", "-ar", "16000", "-c:a",
        "pcm_s16le", "-t",
    ] {
        args.push(a.to_string());
    }
    args.push((max_secs + 1).to_string());
    args.push("-y".into());
    args.push(out_name.to_string());

    let res = process::run_cancellable(
        &ffmpeg,
        &args,
        job_dir,
        CancellableRun { timeout: Some(NORMALIZE_TIMEOUT), cancel, low_priority: false, on_stderr_line: None },
    );
    let out = job_dir.join(out_name);
    match res.end {
        ProcessEnd::Success if out.is_file() => Ok(out),
        other => {
            let _ = std::fs::remove_file(&out);
            Err(end_to_error(&other, &res.stderr_tail, SpeechErrorCode::SpeechPreprocessFailed))
        }
    }
}

/// Cheap "is the audio prep bundle usable" check for the capability model.
pub fn status() -> Result<(), SpeechError> {
    resolve_verified(EngineId::AudioFfmpeg)?;
    resolve_verified(EngineId::AudioFfprobe)?;
    Ok(())
}

#[allow(dead_code)]
pub fn sha256_of(p: &Path) -> Option<String> {
    bundle::sha256_file(p).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_allowlist_is_exactly_the_six_formats() {
        for ok in ["a.wav", "a.MP3", "x.m4a", "x.aac", "x.FLAC", "x.ogg"] {
            assert!(accepted_extension(Path::new(ok)).is_some(), "{ok}");
        }
        for bad in ["a.mp4", "a.wma", "a.opus", "a.txt", "a", "a.exe", "a.mp3.exe", "a.webm"] {
            assert!(accepted_extension(Path::new(bad)).is_none(), "{bad}");
        }
    }

    #[test]
    fn remote_and_relative_and_unc_inputs_are_refused_before_any_process() {
        for bad in [
            "http://example.com/a.mp3",
            "https://example.com/a.mp3",
            "ftp://x/a.mp3",
            "rtmp://x/a",
            "\\\\server\\share\\a.mp3",
            "//server/share/a.mp3",
            "relative\\a.mp3",
            "a.mp3",
        ] {
            let e = ensure_local_input_path(Path::new(bad)).unwrap_err();
            assert_eq!(e.code, SpeechErrorCode::SpeechInputUnsupported, "{bad}");
        }
        assert!(ensure_local_input_path(Path::new("C:\\Users\\x\\a.mp3")).is_ok() || !cfg!(windows));
    }

    #[test]
    fn probe_json_parsing_handles_audio_video_only_and_missing_duration() {
        let ok = r#"{"streams":[{"codec_type":"audio","duration":"3.5"}],"format":{"duration":"3.6"}}"#;
        let p = parse_probe(ok).unwrap();
        assert_eq!(p.duration_secs, Some(3.6));
        let no_dur = r#"{"streams":[{"codec_type":"audio"}],"format":{}}"#;
        assert_eq!(parse_probe(no_dur).unwrap().duration_secs, None);
        let video_only = r#"{"streams":[{"codec_type":"video"}],"format":{"duration":"1"}}"#;
        assert_eq!(parse_probe(video_only).unwrap_err().code, SpeechErrorCode::SpeechInputUnsupported);
        assert_eq!(parse_probe("not json").unwrap_err().code, SpeechErrorCode::SpeechPreprocessFailed);
        let nan = r#"{"streams":[{"codec_type":"audio"}],"format":{"duration":"N/A"}}"#;
        assert_eq!(parse_probe(nan).unwrap().duration_secs, None);
    }
}
