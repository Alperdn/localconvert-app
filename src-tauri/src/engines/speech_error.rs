//! Structured speech errors.
//!
//! `code` is stable (the frontend branches on it). `message` is natural Turkish
//! built only from fixed strings and numbers - never a path, executable name or
//! raw process output. Anything technical goes in `technical_detail`, which is
//! excluded from `Display` and from the serialized `SpeechErrorDto` (same
//! contract as `engines::error::EngineError`).

use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SpeechErrorCode {
    SpeechEngineMissing,
    SpeechModelMissing,
    /// Manifest/hash validation failed.
    SpeechEngineInvalid,
    /// Engine cannot run on this CPU / architecture.
    SpeechCpuUnsupported,
    /// A required OS runtime (VC++) is absent.
    SpeechRuntimeMissing,
    SpeechInputUnsupported,
    SpeechInputNotFound,
    SpeechPreprocessFailed,
    SpeechTranscriptionFailed,
    SpeechCancelled,
    SpeechResourceLimit,
    SpeechNoSpeechDetected,
    SpeechLanguageUnsupported,
    SpeechBusy,
    /// Raised by the microphone flow (Phase D); defined now so the code,
    /// its Turkish text and its tests exist before the UI does.
    #[allow(dead_code)]
    MicrophonePermissionDenied,
}

impl SpeechErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpeechEngineMissing => "SPEECH_ENGINE_MISSING",
            Self::SpeechModelMissing => "SPEECH_MODEL_MISSING",
            Self::SpeechEngineInvalid => "SPEECH_ENGINE_INVALID",
            Self::SpeechCpuUnsupported => "SPEECH_CPU_UNSUPPORTED",
            Self::SpeechRuntimeMissing => "SPEECH_RUNTIME_MISSING",
            Self::SpeechInputUnsupported => "SPEECH_INPUT_UNSUPPORTED",
            Self::SpeechInputNotFound => "SPEECH_INPUT_NOT_FOUND",
            Self::SpeechPreprocessFailed => "SPEECH_PREPROCESS_FAILED",
            Self::SpeechTranscriptionFailed => "SPEECH_TRANSCRIPTION_FAILED",
            Self::SpeechCancelled => "SPEECH_CANCELLED",
            Self::SpeechResourceLimit => "SPEECH_RESOURCE_LIMIT",
            Self::SpeechNoSpeechDetected => "SPEECH_NO_SPEECH_DETECTED",
            Self::SpeechLanguageUnsupported => "SPEECH_LANGUAGE_UNSUPPORTED",
            Self::SpeechBusy => "SPEECH_BUSY",
            Self::MicrophonePermissionDenied => "MICROPHONE_PERMISSION_DENIED",
        }
    }

    #[cfg(test)]
    pub const ALL: [SpeechErrorCode; 15] = [
        Self::SpeechEngineMissing,
        Self::SpeechModelMissing,
        Self::SpeechEngineInvalid,
        Self::SpeechCpuUnsupported,
        Self::SpeechRuntimeMissing,
        Self::SpeechInputUnsupported,
        Self::SpeechInputNotFound,
        Self::SpeechPreprocessFailed,
        Self::SpeechTranscriptionFailed,
        Self::SpeechCancelled,
        Self::SpeechResourceLimit,
        Self::SpeechNoSpeechDetected,
        Self::SpeechLanguageUnsupported,
        Self::SpeechBusy,
        Self::MicrophonePermissionDenied,
    ];

    /// Default Turkish text for the code. Constructors below may substitute a
    /// more specific fixed sentence (e.g. which limit was exceeded).
    pub fn default_message_tr(self) -> &'static str {
        match self {
            Self::SpeechEngineMissing => "Konuşma tanıma bileşeni bu kurulumda bulunamadı. Lütfen uygulamayı yeniden yükleyin veya BT yöneticinizle iletişime geçin.",
            Self::SpeechModelMissing => "Konuşma tanıma modeli bu kurulumda bulunamadı. Lütfen uygulamayı yeniden yükleyin veya BT yöneticinizle iletişime geçin.",
            Self::SpeechEngineInvalid => "Konuşma tanıma bileşenleri doğrulanamadı; kurulum bozulmuş olabilir. Lütfen uygulamayı yeniden yükleyin.",
            Self::SpeechCpuUnsupported => "Bu bilgisayarın işlemcisi konuşma tanıma bileşeni tarafından henüz desteklenmiyor.",
            Self::SpeechRuntimeMissing => "Konuşma tanıma için gereken çalışma zamanı dosyaları eksik. Lütfen uygulamayı yeniden yükleyin veya BT yöneticinizle iletişime geçin.",
            Self::SpeechInputUnsupported => "Bu ses dosyası desteklenmiyor. Desteklenen biçimler: WAV, MP3, M4A, AAC, FLAC, OGG.",
            Self::SpeechInputNotFound => "Ses dosyasına erişilemiyor. Dosya taşınmış veya silinmiş olabilir.",
            Self::SpeechPreprocessFailed => "Ses dosyası hazırlanamadı. Dosya bozuk olabilir.",
            Self::SpeechTranscriptionFailed => "Dikte işlemi tamamlanamadı. Lütfen yeniden deneyin.",
            Self::SpeechCancelled => "Dikte işlemi iptal edildi.",
            Self::SpeechResourceLimit => "Bu ses kaydı, izin verilen sınırları aşıyor.",
            Self::SpeechNoSpeechDetected => "Konuşma algılanamadı.",
            Self::SpeechLanguageUnsupported => "Seçilen dil bu kurulumdaki konuşma tanıma modeli tarafından desteklenmiyor.",
            Self::SpeechBusy => "Başka bir dikte işlemi sürüyor. Lütfen bitmesini bekleyin.",
            Self::MicrophonePermissionDenied => "Mikrofon erişimine izin verilmedi. Windows gizlilik ayarlarından bu uygulama için mikrofon iznini açın.",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpeechError {
    pub code: SpeechErrorCode,
    message: String,
    technical_detail: Option<String>,
}

impl SpeechError {
    pub fn new(code: SpeechErrorCode) -> Self {
        Self { code, message: code.default_message_tr().to_string(), technical_detail: None }
    }
    pub fn with_message(code: SpeechErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), technical_detail: None }
    }
    /// In-memory only. Never displayed, serialized or logged persistently.
    pub fn with_detail(mut self, d: impl Into<String>) -> Self {
        self.technical_detail = Some(d.into());
        self
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    #[allow(dead_code)]
    pub fn technical_detail(&self) -> Option<&str> {
        self.technical_detail.as_deref()
    }
    pub fn to_dto(&self) -> SpeechErrorDto {
        SpeechErrorDto { code: self.code.as_str().to_string(), message: self.message.clone() }
    }
}

impl fmt::Display for SpeechError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

/// What crosses the IPC boundary: code + Turkish message. No detail.
#[derive(Debug, Clone, Serialize)]
pub struct SpeechErrorDto {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_stable_screaming_snake_id_and_turkish_text() {
        let mut ids = std::collections::HashSet::new();
        for c in SpeechErrorCode::ALL {
            let id = c.as_str();
            assert!(ids.insert(id), "duplicate id {id}");
            assert!(id.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_'), "{id}");
            let m = c.default_message_tr();
            assert!(m.len() > 8);
            // Natural-Turkish sanity: ends like a sentence, contains no engine internals.
            assert!(m.ends_with('.'), "{id}: {m}");
            let l = m.to_lowercase();
            for banned in ["whisper", "ffmpeg", "ffprobe", ".exe", "stderr", "\\", "exception", "error:"] {
                assert!(!l.contains(banned), "{id} leaks '{banned}': {m}");
            }
        }
    }

    #[test]
    fn required_codes_from_the_spec_exist_with_exact_names() {
        let names: Vec<&str> = SpeechErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        for want in [
            "SPEECH_ENGINE_MISSING",
            "SPEECH_MODEL_MISSING",
            "SPEECH_INPUT_UNSUPPORTED",
            "SPEECH_PREPROCESS_FAILED",
            "SPEECH_TRANSCRIPTION_FAILED",
            "SPEECH_CANCELLED",
            "SPEECH_RESOURCE_LIMIT",
            "MICROPHONE_PERMISSION_DENIED",
            "SPEECH_NO_SPEECH_DETECTED",
        ] {
            assert!(names.contains(&want), "missing {want}");
        }
    }

    #[test]
    fn technical_detail_never_reaches_display_or_dto() {
        let e = SpeechError::new(SpeechErrorCode::SpeechTranscriptionFailed)
            .with_detail("C:\\secret\\path\\whisper-cli.exe: assertion failed");
        assert!(!e.to_string().contains("secret"));
        let json = serde_json::to_string(&e.to_dto()).unwrap();
        assert!(!json.contains("secret") && !json.contains("whisper"));
        assert!(json.contains("SPEECH_TRANSCRIPTION_FAILED"));
        assert!(e.technical_detail().unwrap().contains("secret"));
    }
}
