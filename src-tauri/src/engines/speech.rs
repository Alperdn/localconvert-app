//! `SpeechEngine`: the abstraction the rest of the app transcribes through, and
//! its whisper.cpp implementation.
//!
//! Nothing outside this file knows the engine is a `whisper-cli` sidecar, which
//! flags it takes or what its JSON looks like; the model (id, languages,
//! hash) comes from the manifest. Replacing the engine or model means a new
//! implementation of this trait / a new manifest - not touching the job
//! pipeline or the UI.
//!
//! Local-only guarantees: the executable and model are resolved ONLY from the
//! bundled engine directory and re-hashed against `manifest.json`
//! (`speech_manifest::check_dir`); argv is a structured array; the frontend
//! supplies neither path; there is no network client anywhere in this crate.

use super::engine_id::EngineId;
use super::process::{self, CancellableRun, ProcessEnd};
use super::resolver::{self, ResolvedEngine};
use super::speech_error::{SpeechError, SpeechErrorCode};
use super::speech_manifest::{self, ManifestCheck, SpeechManifest};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    /// Language actually used/detected (e.g. "tr").
    pub language: String,
    pub segments: Vec<Segment>,
    /// Segment-level timestamps are approximate (Whisper decodes in ~30 s
    /// windows), never frame-accurate.
    pub timestamps_available: bool,
}

impl Transcript {
    pub fn plain_text(&self) -> String {
        self.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeechEngineInfo {
    pub engine_name: String,
    pub engine_version: String,
    pub model_id: String,
    pub languages: Vec<String>,
    /// The engine can report segment timestamps.
    pub supports_timestamps: bool,
    /// The engine reports real progress (never fabricated).
    pub supports_progress: bool,
}

pub struct TranscribeRequest<'a> {
    /// Working directory; all other paths are names relative to it.
    pub job_dir: &'a Path,
    /// Normalized 16 kHz mono WAV, relative to `job_dir`.
    pub wav_name: &'a str,
    /// `"tr"` or `"auto"`; must be one of `SpeechEngine::info().languages`.
    pub language: &'a str,
}

pub trait SpeechEngine: Send + Sync {
    fn info(&self) -> SpeechEngineInfo;
    /// `on_progress(Some(pct))` only for real engine-reported progress.
    fn transcribe(
        &self,
        req: &TranscribeRequest<'_>,
        cancel: &AtomicBool,
        on_progress: &(dyn Fn(Option<u8>) + Sync),
    ) -> Result<Transcript, SpeechError>;
}

// ----------------------------------------------------------------------------
// Status (capability model input)
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SpeechEngineStatus {
    Available,
    EngineMissing,
    ModelMissing,
    /// Manifest/hash validation failed.
    EngineInvalid,
    /// Binary present and valid but cannot run on this CPU.
    CpuUnsupported,
    /// A required OS runtime DLL is missing.
    RuntimeMissing,
    /// Speech engine fine, but the audio preprocessing bundle is not.
    AudioPrepUnavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeechEngineReport {
    pub status: SpeechEngineStatus,
    /// Turkish, safe to show as-is (never a path/exe name).
    pub message: String,
    pub engine: Option<String>,
    pub model: Option<String>,
    pub languages: Vec<String>,
    pub supports_timestamps: bool,
    /// Turkish, safe to show. `Some` when this CPU lacks the fast instruction
    /// sets (measured: 3-8x slower without AVX2) - never a blocker, only a heads-up.
    pub performance_note: Option<String>,
}

/// AVX2 machines run the small model ~4x faster than real time; AVX-only about
/// real time; no AVX about 2x slower than real time (measured on the bundled build).
pub(crate) fn performance_note_for(avx2: bool) -> Option<String> {
    if avx2 {
        None
    } else {
        Some("Bu bilgisayarın işlemcisi dikte için en hızlı komut kümelerini desteklemiyor; işlem normalden yavaş olabilir.".to_string())
    }
}

fn cpu_has_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

fn check_to_error(c: ManifestCheck) -> SpeechError {
    match c {
        ManifestCheck::EngineMissing => SpeechError::new(SpeechErrorCode::SpeechEngineMissing),
        ManifestCheck::ModelMissing => SpeechError::new(SpeechErrorCode::SpeechModelMissing),
        ManifestCheck::RuntimeMissing => SpeechError::new(SpeechErrorCode::SpeechRuntimeMissing),
        ManifestCheck::ArchitectureUnsupported => SpeechError::new(SpeechErrorCode::SpeechCpuUnsupported),
        ManifestCheck::Corrupt => SpeechError::new(SpeechErrorCode::SpeechEngineInvalid),
    }
}

fn error_to_status(code: SpeechErrorCode) -> SpeechEngineStatus {
    match code {
        SpeechErrorCode::SpeechEngineMissing => SpeechEngineStatus::EngineMissing,
        SpeechErrorCode::SpeechModelMissing => SpeechEngineStatus::ModelMissing,
        SpeechErrorCode::SpeechCpuUnsupported => SpeechEngineStatus::CpuUnsupported,
        SpeechErrorCode::SpeechRuntimeMissing => SpeechEngineStatus::RuntimeMissing,
        _ => SpeechEngineStatus::EngineInvalid,
    }
}

/// Full backend self-check, not merely "the exe exists": manifest, hashes of
/// every binary and the model, architecture, CPU/runtime "does it actually
/// start", and the audio-preprocessing bundle.
pub fn report() -> SpeechEngineReport {
    report_from_load(WhisperCppEngine::load())
}

pub(crate) fn report_from_load(loaded: Result<WhisperCppEngine, SpeechError>) -> SpeechEngineReport {
    match loaded {
        Err(e) => SpeechEngineReport {
            status: error_to_status(e.code),
            message: e.message().to_string(),
            engine: None,
            model: None,
            languages: vec![],
            supports_timestamps: false,
            performance_note: None,
        },
        Ok(engine) => {
            let info = engine.info();
            if let Err(e) = super::audio_prep::status() {
                return SpeechEngineReport {
                    status: SpeechEngineStatus::AudioPrepUnavailable,
                    message: e.message().to_string(),
                    engine: Some(format!("{} {}", info.engine_name, info.engine_version)),
                    model: Some(info.model_id),
                    languages: info.languages,
                    supports_timestamps: info.supports_timestamps,
                    performance_note: performance_note_for(cpu_has_avx2()),
                };
            }
            SpeechEngineReport {
                status: SpeechEngineStatus::Available,
                message: "Konuşma tanıma hazır.".to_string(),
                engine: Some(format!("{} {}", info.engine_name, info.engine_version)),
                model: Some(info.model_id),
                languages: info.languages,
                supports_timestamps: info.supports_timestamps,
                performance_note: performance_note_for(cpu_has_avx2()),
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Runnable check (CPU / runtime)
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Runnable {
    Yes,
    CpuUnsupported,
    RuntimeMissing,
    Broken,
}

const STATUS_DLL_NOT_FOUND: i32 = 0xC000_0135_u32 as i32;
const STATUS_ILLEGAL_INSTRUCTION: i32 = 0xC000_001D_u32 as i32;

/// Classifies the outcome of `whisper-cli --version`. Pure so it is testable
/// without the real engine.
pub(crate) fn classify_version_run(end: &ProcessEnd, output: &str) -> Runnable {
    match end {
        ProcessEnd::Success => {
            // ggml selects a CPU backend variant at load time; if none of the
            // shipped variants matches this CPU, none is loaded and the engine
            // cannot compute anything.
            if output.contains("loaded CPU backend") {
                Runnable::Yes
            } else {
                Runnable::CpuUnsupported
            }
        }
        ProcessEnd::Failed(Some(c)) if *c == STATUS_DLL_NOT_FOUND => Runnable::RuntimeMissing,
        ProcessEnd::Failed(Some(c)) if *c == STATUS_ILLEGAL_INSTRUCTION => Runnable::CpuUnsupported,
        ProcessEnd::SpawnFailed => Runnable::RuntimeMissing,
        _ => Runnable::Broken,
    }
}

fn probe_runnable(exe: &ResolvedEngine) -> Runnable {
    let never = AtomicBool::new(false);
    let res = process::run_cancellable(
        exe,
        &["--version".to_string()],
        &std::env::temp_dir(),
        CancellableRun { timeout: Some(std::time::Duration::from_secs(30)), cancel: &never, low_priority: false, on_stderr_line: None },
    );
    classify_version_run(&res.end, &format!("{}\n{}", res.stdout, res.stderr_tail))
}

/// Cached per process: (binary hash-stamp is verified separately on every load).
fn runnable_cached(exe: &ResolvedEngine) -> Runnable {
    static CACHE: OnceLock<Runnable> = OnceLock::new();
    *CACHE.get_or_init(|| probe_runnable(exe))
}

// ----------------------------------------------------------------------------
// whisper.cpp implementation
// ----------------------------------------------------------------------------

pub struct WhisperCppEngine {
    exe: ResolvedEngine,
    manifest: SpeechManifest,
    model_path: PathBuf,
}

impl WhisperCppEngine {
    /// Loads and fully validates the bundled engine. Every call re-checks the
    /// manifest and the file hashes (cached by size+mtime, so this is cheap
    /// after the first call in a process).
    pub fn load() -> Result<Self, SpeechError> {
        let dir = resolver::bundled_engine_dir(EngineId::Speech)
            .ok_or_else(|| SpeechError::new(SpeechErrorCode::SpeechEngineMissing))?;
        let manifest = speech_manifest::check_dir(&dir).map_err(check_to_error)?;
        Self::from_verified(dir, manifest)
    }

    /// Test-only: validate and load an engine directory other than the bundled
    /// one (e.g. a copy with the model removed). Production code never takes a
    /// directory from anywhere but `resolver::bundled_engine_dir`.
    #[cfg(test)]
    pub(crate) fn load_from_dir(dir: &Path) -> Result<Self, SpeechError> {
        let manifest = speech_manifest::check_dir(dir).map_err(check_to_error)?;
        let exe = ResolvedEngine {
            id: EngineId::Speech,
            path: dir.join(&manifest.executable_relative_path),
            tier: resolver::EngineTier::Bundled,
        };
        Self::finish_load(dir.to_path_buf(), manifest, exe)
    }

    fn from_verified(dir: PathBuf, manifest: SpeechManifest) -> Result<Self, SpeechError> {
        let exe = resolver::resolve(EngineId::Speech)
            .map_err(|e| SpeechError::new(SpeechErrorCode::SpeechEngineMissing).with_detail(format!("{:?}", e)))?;
        Self::finish_load(dir, manifest, exe)
    }

    fn finish_load(dir: PathBuf, manifest: SpeechManifest, exe: ResolvedEngine) -> Result<Self, SpeechError> {
        match runnable_cached(&exe) {
            Runnable::Yes => {}
            Runnable::CpuUnsupported => return Err(SpeechError::new(SpeechErrorCode::SpeechCpuUnsupported)),
            Runnable::RuntimeMissing => return Err(SpeechError::new(SpeechErrorCode::SpeechRuntimeMissing)),
            Runnable::Broken => return Err(SpeechError::new(SpeechErrorCode::SpeechEngineInvalid)),
        }
        let full_model = speech_manifest::model_path(&dir, &manifest)
            .ok_or_else(|| SpeechError::new(SpeechErrorCode::SpeechEngineInvalid))?;
        // The model's real location may contain any characters (Turkish, Cyrillic, ...);
        // whisper-cli only ever receives an ASCII name for it (see `ascii_link`).
        Ok(Self { exe, manifest, model_path: full_model })
    }

    /// `model_arg` is what whisper-cli is told to open: an ASCII path relative to the job
    /// dir (its cwd), or the absolute model path when that is already pure ASCII.
    pub(crate) fn build_args(&self, req: &TranscribeRequest<'_>, model_arg: &str, out_prefix: &str) -> Vec<String> {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let threads = threads.saturating_sub(1).clamp(1, 8);
        let mut a: Vec<String> = vec![
            "-m".into(),
            model_arg.to_string(),
            "-f".into(),
            req.wav_name.to_string(),
            "-l".into(),
            req.language.to_string(),
            "-t".into(),
            threads.to_string(),
        ];
        a.extend(
            [
                "-oj",         // JSON with millisecond segment offsets
                "-np",         // no informational prints on stdout
                "-pp",         // real progress on stderr
                "-ng",         // never use a GPU backend (none is bundled)
                "-sns",        // suppress non-speech tokens ([Muzik], ...)
                "-nth", "0.6", // no-speech threshold
                "-of",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        a.push(out_prefix.to_string());
        a
    }
}

#[derive(Deserialize)]
struct WhisperJson {
    #[serde(default)]
    result: WhisperResult,
    #[serde(default)]
    transcription: Vec<WhisperSegment>,
}
#[derive(Deserialize, Default)]
struct WhisperResult {
    #[serde(default)]
    language: String,
}
#[derive(Deserialize)]
struct WhisperSegment {
    #[serde(default)]
    offsets: WhisperOffsets,
    #[serde(default)]
    text: String,
}
#[derive(Deserialize, Default)]
struct WhisperOffsets {
    #[serde(default)]
    from: i64,
    #[serde(default)]
    to: i64,
}

/// "whisper_print_progress_callback: progress =  10%" -> Some(10)
pub(crate) fn parse_progress(line: &str) -> Option<u8> {
    let idx = line.find("progress =")?;
    let rest = line[idx + "progress =".len()..].trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let v: u32 = digits.parse().ok()?;
    if rest[digits.len()..].starts_with('%') && v <= 100 {
        Some(v as u8)
    } else {
        None
    }
}

fn is_noise_tag(t: &str) -> bool {
    let t = t.trim();
    if t.is_empty() {
        return true;
    }
    let lower = t.to_lowercase();
    if lower.chars().all(|c| matches!(c, '♪' | '♫' | '*' | '-' | '.' | ' ' | '_')) {
        return true;
    }
    let bracketed = (t.starts_with('[') && t.ends_with(']'))
        || (t.starts_with('(') && t.ends_with(')'))
        || (t.starts_with('*') && t.ends_with('*') && t.len() > 2);
    bracketed
        && (lower.contains("müzik")
            || lower.contains("music")
            || lower.contains("blank")
            || lower.contains("silence")
            || lower.contains("sessiz")
            || lower.contains("noise")
            || lower.contains("gürültü")
            || lower.contains("alkış")
            || lower.contains("applause")
            || lower.contains("laughter"))
}

/// Lowercased, letters/digits only, single-spaced, combining marks removed -
/// so `Altyazı M.K.` and `ALTYAZI  M.K` compare equal.
fn fold(s: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if ('\u{0300}'..='\u{036f}').contains(&c) {
            continue;
        }
        if c.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        } else {
            pending_space = true;
        }
    }
    out
}

/// Subtitle-credit strings Whisper emits on non-speech (its training data was
/// full of them). Nobody dictates these, so they are dropped wherever they occur.
const CREDIT_HALLUCINATIONS_EXACT: &[&str] = &["altyazı m k", "altyazi m k", "altyazı mk", "altyazı", "amara org"];
const CREDIT_HALLUCINATION_PREFIXES: &[&str] = &["altyazı m k ", "subtitles by ", "subtitle by ", "amara org "];

/// Sign-offs Whisper also invents on silence/music, but which a real speaker can
/// genuinely say - so they are dropped ONLY when they are the entire transcript.
const SIGNOFF_HALLUCINATIONS: &[&str] = &[
    "izlediğiniz için teşekkürler",
    "izlediğiniz için teşekkür ederim",
    "izlediğiniz için teşekkür ederiz",
    "abone olun",
    "kanala abone olun",
    "videoyu beğenmeyi unutmayın",
    "bir sonraki videoda görüşmek üzere",
    "thanks for watching",
    "thank you for watching",
    "teşekkürler",
    "teşekkür ederim",
];

fn is_credit_hallucination(text: &str) -> bool {
    let f = fold(text);
    CREDIT_HALLUCINATIONS_EXACT.iter().any(|h| f == fold(h))
        || CREDIT_HALLUCINATION_PREFIXES.iter().any(|p| {
            let p = fold(p);
            f == p || f.starts_with(&format!("{p} "))
        })
}

fn is_signoff_hallucination(text: &str) -> bool {
    let f = fold(text);
    SIGNOFF_HALLUCINATIONS.iter().any(|h| f == fold(h))
}

/// Post-processing that removes what Whisper is known to invent on
/// non-speech: bracketed noise tags, empty segments, subtitle-credit strings
/// ("Altyazı M.K."), runaway repetition loops (>= 3 identical consecutive
/// segments collapse to one), and a transcript that is nothing but a sign-off.
pub(crate) fn clean_segments(raw: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut run = 0usize;
    for mut s in raw {
        s.text = s.text.trim().to_string();
        if is_noise_tag(&s.text) || is_credit_hallucination(&s.text) {
            continue;
        }
        if let Some(prev) = out.last() {
            if prev.text == s.text {
                run += 1;
                if run >= 2 {
                    continue; // third+ identical repeat: drop
                }
            } else {
                run = 0;
            }
        }
        out.push(s);
    }
    // Two identical segments in a row are plausible speech; three-plus were
    // trimmed above to two - collapse a remaining exact pair only when the
    // whole transcript is that one repeated phrase (a loop, not speech).
    if out.len() >= 2 && out.iter().all(|s| s.text == out[0].text) {
        out.truncate(1);
    }
    // A whole transcript that is only a stock sign-off is Whisper filling silence.
    if !out.is_empty() && out.iter().all(|s| is_signoff_hallucination(&s.text)) {
        out.clear();
    }
    out
}

fn timestamps_look_valid(segs: &[Segment]) -> bool {
    !segs.is_empty()
        && segs.iter().all(|s| s.end_ms >= s.start_ms)
        && segs.windows(2).all(|w| w[1].start_ms >= w[0].start_ms)
        && segs.iter().any(|s| s.end_ms > 0)
}

pub(crate) fn parse_whisper_json(bytes: &[u8]) -> Result<Transcript, SpeechError> {
    let j: WhisperJson = serde_json::from_slice(bytes)
        .map_err(|e| SpeechError::new(SpeechErrorCode::SpeechTranscriptionFailed).with_detail(e.to_string()))?;
    let raw: Vec<Segment> = j
        .transcription
        .into_iter()
        .map(|s| Segment {
            start_ms: s.offsets.from.max(0) as u64,
            end_ms: s.offsets.to.max(0) as u64,
            text: s.text,
        })
        .collect();
    let segments = clean_segments(raw);
    if segments.is_empty() {
        return Err(SpeechError::new(SpeechErrorCode::SpeechNoSpeechDetected));
    }
    let ts = timestamps_look_valid(&segments);
    Ok(Transcript { language: j.result.language, segments, timestamps_available: ts })
}

impl SpeechEngine for WhisperCppEngine {
    fn info(&self) -> SpeechEngineInfo {
        SpeechEngineInfo {
            engine_name: self.manifest.engine_name.clone(),
            engine_version: self.manifest.engine_version.clone(),
            model_id: self.manifest.model.id.clone(),
            languages: self.manifest.model.languages.clone(),
            supports_timestamps: true,
            supports_progress: true,
        }
    }

    fn transcribe(
        &self,
        req: &TranscribeRequest<'_>,
        cancel: &AtomicBool,
        on_progress: &(dyn Fn(Option<u8>) + Sync),
    ) -> Result<Transcript, SpeechError> {
        if !self.manifest.supports_language(req.language) {
            return Err(SpeechError::new(SpeechErrorCode::SpeechLanguageUnsupported));
        }
        debug_assert!(!req.wav_name.contains(['/', '\\']));
        const OUT_PREFIX: &str = "transcript";
        let model_arg = super::ascii_link::expose_model(&self.model_path, req.job_dir, cancel)?;
        let args = self.build_args(req, &model_arg, OUT_PREFIX);
        let json_path = req.job_dir.join(format!("{OUT_PREFIX}.json"));
        let _ = std::fs::remove_file(&json_path);

        let progress_cb = move |line: &str| {
            if let Some(p) = parse_progress(line) {
                on_progress(Some(p));
            }
        };
        let res = process::run_cancellable(
            &self.exe,
            &args,
            req.job_dir,
            CancellableRun {
                timeout: None, // bounded by the audio-duration limit and user cancel
                cancel,
                low_priority: true,
                on_stderr_line: Some(&progress_cb),
            },
        );
        match res.end {
            ProcessEnd::Cancelled => Err(SpeechError::new(SpeechErrorCode::SpeechCancelled)),
            ProcessEnd::Success => {
                let bytes = std::fs::read(&json_path).map_err(|e| {
                    SpeechError::new(SpeechErrorCode::SpeechTranscriptionFailed).with_detail(e.to_string())
                })?;
                parse_whisper_json(&bytes)
            }
            other => Err(SpeechError::new(SpeechErrorCode::SpeechTranscriptionFailed)
                .with_detail(format!("{:?}: {}", other, res.stderr_tail))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(a: u64, b: u64, t: &str) -> Segment {
        Segment { start_ms: a, end_ms: b, text: t.into() }
    }

    #[test]
    fn progress_lines_are_parsed_and_garbage_is_not() {
        assert_eq!(parse_progress("whisper_print_progress_callback: progress =  10%"), Some(10));
        assert_eq!(parse_progress("whisper_print_progress_callback: progress = 100%"), Some(100));
        assert_eq!(parse_progress("progress = 101%"), None);
        assert_eq!(parse_progress("something else"), None);
        assert_eq!(parse_progress("progress = abc%"), None);
    }

    #[test]
    fn noise_tags_and_empty_segments_are_dropped() {
        let raw = vec![
            seg(0, 1, " [Müzik]"),
            seg(1, 2, "(music)"),
            seg(2, 3, "[BLANK_AUDIO]"),
            seg(3, 4, "♪"),
            seg(4, 5, "  "),
            seg(5, 6, "Merhaba dünya."),
            seg(6, 7, "[Alkışlar]"),
        ];
        let out = clean_segments(raw);
        assert_eq!(out, vec![seg(5, 6, "Merhaba dünya.")]);
    }

    #[test]
    fn subtitle_credit_hallucinations_are_dropped_in_any_spelling() {
        for credit in ["Altyazı M.K.", "altyazı m.k", " ALTYAZI  M.K. ", "Altyazı", "Subtitles by the Amara.org community", "Amara.org"] {
            assert!(clean_segments(vec![seg(0, 1, credit)]).is_empty(), "{credit}");
            // ... and also when mixed into real speech
            let out = clean_segments(vec![seg(0, 1, "Merhaba arkadaşlar."), seg(1, 2, credit)]);
            assert_eq!(out.len(), 1, "{credit}");
        }
    }

    #[test]
    fn a_lone_signoff_is_dropped_but_the_same_words_inside_real_speech_are_kept() {
        assert!(clean_segments(vec![seg(0, 1, "İzlediğiniz için teşekkürler.")]).is_empty());
        assert!(clean_segments(vec![seg(0, 1, "Abone olun.")]).is_empty());
        let real = vec![seg(0, 9, "Sunumumuz burada bitiyor."), seg(9, 12, "İzlediğiniz için teşekkürler.")];
        assert_eq!(clean_segments(real).len(), 2, "a genuine closing line must survive");
    }

    #[test]
    fn speech_that_merely_mentions_subtitles_is_kept() {
        for t in ["Altyazı ekleme özelliğini açın.", "Altyazılar otomatik oluşturulur.", "Bu videoda altyazı nasıl eklenir anlatacağım."] {
            assert_eq!(clean_segments(vec![seg(0, 1, t)]).len(), 1, "{t}");
        }
    }

    #[test]
    fn legit_bracket_like_speech_is_kept() {
        let out = clean_segments(vec![seg(0, 1, "(yani şöyle demek istiyorum)")]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn repetition_loops_are_collapsed() {
        let raw: Vec<Segment> = (0..6).map(|i| seg(i, i + 1, "Bu bir deneme cümlesidir.")).collect();
        let out = clean_segments(raw);
        assert_eq!(out.len(), 1);
        // a runaway loop of a stock sign-off is pure hallucination: nothing survives
        let loop_signoff: Vec<Segment> = (0..6).map(|i| seg(i, i + 1, "Teşekkür ederim.")).collect();
        assert!(clean_segments(loop_signoff).is_empty());
        // a repeated phrase amid other speech is kept (up to 2 in a row)
        let mixed = vec![seg(0, 1, "Evet."), seg(1, 2, "Evet."), seg(2, 3, "Hayır.")];
        assert_eq!(clean_segments(mixed).len(), 3);
    }

    #[test]
    fn only_noise_means_no_speech_detected() {
        let json = r#"{"result":{"language":"tr"},"transcription":[{"offsets":{"from":0,"to":1000},"text":" [Müzik]"}]}"#.as_bytes();
        assert_eq!(parse_whisper_json(json).unwrap_err().code, SpeechErrorCode::SpeechNoSpeechDetected);
        let empty = br#"{"transcription":[]}"#;
        assert_eq!(parse_whisper_json(empty).unwrap_err().code, SpeechErrorCode::SpeechNoSpeechDetected);
    }

    #[test]
    fn json_with_offsets_yields_timestamps_and_broken_json_fails_cleanly() {
        let json = r#"{"result":{"language":"tr"},"transcription":[
            {"offsets":{"from":0,"to":3520},"text":" Merhaba."},
            {"offsets":{"from":3520,"to":7000},"text":" Nasılsınız?"}]}"#.as_bytes();
        let t = parse_whisper_json(json).unwrap();
        assert!(t.timestamps_available);
        assert_eq!(t.language, "tr");
        assert_eq!(t.segments[1].start_ms, 3520);
        assert_eq!(t.plain_text(), "Merhaba. Nasılsınız?");
        assert_eq!(parse_whisper_json(b"{ nope").unwrap_err().code, SpeechErrorCode::SpeechTranscriptionFailed);
    }

    #[test]
    fn timestamps_are_not_fabricated_when_offsets_are_absent_or_unordered() {
        let no_offsets = br#"{"transcription":[{"text":" Merhaba."}]}"#;
        assert!(!parse_whisper_json(no_offsets).unwrap().timestamps_available);
        let unordered = br#"{"transcription":[
            {"offsets":{"from":5000,"to":6000},"text":" a."},{"offsets":{"from":0,"to":1000},"text":" b."}]}"#;
        assert!(!parse_whisper_json(unordered).unwrap().timestamps_available);
    }

    #[test]
    fn version_run_classification_distinguishes_cpu_runtime_and_ok() {
        assert_eq!(classify_version_run(&ProcessEnd::Success, "load_backend: loaded CPU backend from x"), Runnable::Yes);
        assert_eq!(classify_version_run(&ProcessEnd::Success, "whisper.cpp version: 1.9.2"), Runnable::CpuUnsupported);
        assert_eq!(classify_version_run(&ProcessEnd::Failed(Some(STATUS_ILLEGAL_INSTRUCTION)), ""), Runnable::CpuUnsupported);
        assert_eq!(classify_version_run(&ProcessEnd::Failed(Some(STATUS_DLL_NOT_FOUND)), ""), Runnable::RuntimeMissing);
        assert_eq!(classify_version_run(&ProcessEnd::Failed(Some(1)), ""), Runnable::Broken);
        assert_eq!(classify_version_run(&ProcessEnd::SpawnFailed, ""), Runnable::RuntimeMissing);
    }

    #[test]
    fn performance_note_appears_only_without_avx2_and_is_turkish() {
        assert!(performance_note_for(true).is_none());
        let n = performance_note_for(false).unwrap();
        assert!(n.contains("yavaş") && n.ends_with('.'));
        assert!(!n.contains('\\') && !n.to_lowercase().contains("avx"));
    }

    #[test]
    fn error_to_status_mapping_covers_the_spec_states() {
        assert_eq!(error_to_status(SpeechErrorCode::SpeechEngineMissing), SpeechEngineStatus::EngineMissing);
        assert_eq!(error_to_status(SpeechErrorCode::SpeechModelMissing), SpeechEngineStatus::ModelMissing);
        assert_eq!(error_to_status(SpeechErrorCode::SpeechCpuUnsupported), SpeechEngineStatus::CpuUnsupported);
        assert_eq!(error_to_status(SpeechErrorCode::SpeechEngineInvalid), SpeechEngineStatus::EngineInvalid);
    }
}

#[cfg(test)]
mod credit_prefix_tests {
    use super::*;

    #[test]
    fn credit_prefix_matching_is_word_bounded() {
        assert!(is_credit_hallucination("Altyazı M.K. Altyazı M.K."));
        assert!(!is_credit_hallucination("Altyazı M.Kutlu"));
        assert!(is_credit_hallucination("Subtitles by the Amara.org community"));
        assert!(!is_credit_hallucination("Subtitlesbyx"));
    }
}
