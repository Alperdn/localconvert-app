//! The engine boundary.
//!
//! The job worker never knows how a conversion is performed: it hands a
//! `ConversionRunner` an input path and an output path - both inside the
//! job's server-owned workspace - plus a `JobControl` (cancel flag +
//! progress sink), and validates whatever comes back.
//!
//! - In-process engines (the native image pipeline today) implement this
//!   trait directly by calling `meb_core`.
//! - External engines (LibreOffice, FFmpeg/ffprobe, whisper.cpp) will
//!   implement it on top of the unified process runner described in
//!   docs/WEB_ARCHITECTURE_PROPOSAL.md §E (allowlisted executable, argument
//!   array, controlled env/cwd, timeout, process-tree kill). They must NOT
//!   use the legacy `src-tauri/src/converter.rs` launcher.
//!
//! The web layer contains no image-processing code: `NativeImageRunner` is
//! a thin adapter over `meb_core::image::convert_file_with_hooks`, which is
//! the same pipeline (and the same limits) the desktop app uses.

use crate::jobs::ConvertOptions;
use meb_core::image::{
    self, NativeConvertOptions, NativeImageFormat, PipelineHooks, PipelineStage,
};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct RunRequest<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub target: NativeImageFormat,
    pub options: &'a ConvertOptions,
}

/// What an engine may observe/report while running.
pub struct JobControl<'a> {
    pub(crate) cancel: &'a AtomicBool,
    pub(crate) progress: &'a (dyn Fn(u8) + Sync),
}

impl JobControl<'_> {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Real progress only (0-100). Never call with an invented value.
    pub fn report_progress(&self, pct: u8) {
        (self.progress)(pct)
    }
}

#[derive(Debug)]
pub enum RunError {
    Cancelled,
    /// `code` is a stable error code shown to the user (translated);
    /// `detail` is for server logs only.
    Failed {
        code: &'static str,
        detail: String,
    },
}

pub trait ConversionRunner: Send + Sync + 'static {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError>;
}

/// Adapter over the shared native image engine.
pub struct NativeImageRunner;

impl ConversionRunner for NativeImageRunner {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        let options = NativeConvertOptions {
            width: request.options.width,
            height: request.options.height,
            quality: request.options.quality,
            png_compression_level: None,
        };
        let is_cancelled = || control.is_cancelled();
        // Progress is reported at the pipeline's real stage boundaries.
        let on_stage = |stage: PipelineStage| {
            control.report_progress(match stage {
                PipelineStage::Decoded => 25,
                PipelineStage::Transformed => 50,
                PipelineStage::Encoded => 75,
                PipelineStage::Written => 100,
            })
        };
        let hooks = PipelineHooks {
            is_cancelled: &is_cancelled,
            on_stage: &on_stage,
        };
        image::convert_file_with_hooks(
            request.input,
            request.output,
            request.target,
            &options,
            &hooks,
        )
        .map_err(|e| {
            if e.kind() == image::ImageNativeErrorKind::Cancelled {
                RunError::Cancelled
            } else {
                RunError::Failed {
                    code: e.code(),
                    detail: format!("{e:?}"),
                }
            }
        })
    }
}
