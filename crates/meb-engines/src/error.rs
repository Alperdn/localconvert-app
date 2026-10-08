//! Structured engine/dependency errors.
//!
//! `code()` is stable and safe to match/branch on (e.g.
//! `"OFFICE_ENGINE_NOT_AVAILABLE"`). `user_message()` is safe to show
//! directly in the UI - it is built only from fixed strings and engine
//! display names, never a filesystem path or executable name.
//!
//! Anything with a real path or raw stderr in it belongs in
//! `technical_detail`, which this app never writes to disk or a log file
//! (there is no logging crate/log file in this codebase) - it exists only
//! to be inspected during development via `{:?}`, and is deliberately
//! excluded from `Display`/`to_string()`, which is what ends up in
//! `ConversionResult.error` and other frontend-visible strings.

use super::engine_id::EngineId;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineErrorKind {
    NotAvailable,
    Corrupted,
    VersionUnsupported,
    FeatureNotImplemented,
    InputInvalid,
    OutputInvalid,
    ProcessFailed,
    Timeout,
}

impl EngineErrorKind {
    fn suffix(self) -> &'static str {
        match self {
            EngineErrorKind::NotAvailable => "ENGINE_NOT_AVAILABLE",
            EngineErrorKind::Corrupted => "ENGINE_CORRUPTED",
            EngineErrorKind::VersionUnsupported => "ENGINE_VERSION_UNSUPPORTED",
            EngineErrorKind::FeatureNotImplemented => "FEATURE_NOT_IMPLEMENTED",
            EngineErrorKind::InputInvalid => "INPUT_INVALID",
            EngineErrorKind::OutputInvalid => "OUTPUT_INVALID",
            EngineErrorKind::ProcessFailed => "PROCESS_FAILED",
            EngineErrorKind::Timeout => "TIMEOUT",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EngineError {
    kind: EngineErrorKind,
    engine: Option<EngineId>,
    user_message: String,
    technical_detail: Option<String>,
}

impl EngineError {
    pub fn new(kind: EngineErrorKind, engine: Option<EngineId>, user_message: impl Into<String>) -> Self {
        Self {
            kind,
            engine,
            user_message: user_message.into(),
            technical_detail: None,
        }
    }

    /// Attaches an in-memory-only technical detail (raw stderr, a path,
    /// an OS error). Never persisted, never included in `Display`.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.technical_detail = Some(detail.into());
        self
    }

    pub fn not_available(engine: EngineId) -> Self {
        Self::new(
            EngineErrorKind::NotAvailable,
            Some(engine),
            format!("{} engine is not available on this installation.", engine.display_name()),
        )
    }

    pub fn feature_not_implemented(engine: Option<EngineId>, feature: &str) -> Self {
        Self::new(
            EngineErrorKind::FeatureNotImplemented,
            engine,
            format!("{} is not implemented yet.", feature),
        )
    }

    pub fn process_failed(engine: EngineId, user_message: impl Into<String>) -> Self {
        Self::new(EngineErrorKind::ProcessFailed, Some(engine), user_message)
    }

    pub fn timeout(engine: EngineId) -> Self {
        Self::new(
            EngineErrorKind::Timeout,
            Some(engine),
            format!("{} timed out and was stopped.", engine.display_name()),
        )
    }

    pub fn input_invalid(message: impl Into<String>) -> Self {
        Self::new(EngineErrorKind::InputInvalid, None, message)
    }

    pub fn output_invalid(message: impl Into<String>) -> Self {
        Self::new(EngineErrorKind::OutputInvalid, None, message)
    }

    #[allow(dead_code)]
    pub fn corrupted(engine: EngineId) -> Self {
        Self::new(
            EngineErrorKind::Corrupted,
            Some(engine),
            format!("{} engine installation appears to be corrupted.", engine.display_name()),
        )
    }

    #[allow(dead_code)]
    pub fn version_unsupported(engine: EngineId) -> Self {
        Self::new(
            EngineErrorKind::VersionUnsupported,
            Some(engine),
            format!("{} engine version is not supported.", engine.display_name()),
        )
    }

    pub fn kind(&self) -> EngineErrorKind {
        self.kind
    }

    pub fn engine(&self) -> Option<EngineId> {
        self.engine
    }

    pub fn code(&self) -> String {
        match self.engine {
            Some(engine) => format!("{}_{}", engine.code_prefix(), self.kind.suffix()),
            None => self.kind.suffix().to_string(),
        }
    }

    pub fn user_message(&self) -> &str {
        &self.user_message
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `technical_detail` is intentionally excluded here: this Display
        // impl is what `From<EngineError> for String` uses, and that
        // string is what callers put straight into `ConversionResult`/
        // `Result<T, String>` values the frontend sees.
        write!(f, "{}: {}", self.code(), self.user_message)
    }
}

impl From<EngineError> for String {
    fn from(e: EngineError) -> String {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_not_available_has_expected_code_and_safe_message() {
        let err = EngineError::not_available(EngineId::Office);
        assert_eq!(err.code(), "OFFICE_ENGINE_NOT_AVAILABLE");
        assert_eq!(
            err.user_message(),
            "Office conversion engine is not available on this installation."
        );
        let rendered = err.to_string();
        assert!(!rendered.contains(':') || rendered.starts_with("OFFICE_ENGINE_NOT_AVAILABLE:"));
        assert!(!rendered.to_lowercase().contains(".exe"));
        assert!(!rendered.contains('\\'));
    }

    #[test]
    fn technical_detail_never_appears_in_display() {
        let err = EngineError::process_failed(EngineId::Ffmpeg, "Video conversion failed.")
            .with_detail("C:\\Users\\alice\\secret\\input.mp4: stack trace ...");
        let rendered = err.to_string();
        assert!(!rendered.contains("alice"));
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn office_timeout_has_expected_code() {
        let err = EngineError::timeout(EngineId::Office);
        assert_eq!(err.code(), "OFFICE_TIMEOUT");
    }

    #[test]
    fn pdf_to_xlsx_reports_feature_not_implemented() {
        let err = EngineError::feature_not_implemented(None, "PDF to Excel conversion");
        assert_eq!(err.code(), "FEATURE_NOT_IMPLEMENTED");
    }
}
