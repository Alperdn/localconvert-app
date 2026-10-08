//! The engine boundary.
//!
//! The job worker never knows how a conversion is performed: it hands a
//! `ConversionRunner` the job's input paths and the output path - all inside
//! the job's server-owned workspace - plus the job's `JobSpec` (what to do)
//! and a `JobControl` (cancel flag + progress sink), and validates whatever
//! comes back.
//!
//! One runner per `JobKind`, wired in a `RunnerRegistry`:
//!
//! - In-process engines (the native image pipeline today) implement the
//!   trait directly by calling `meb_core`.
//! - External engines (Ghostscript for PDF operations, LibreOffice for
//!   Office conversion, Tesseract for OCR, FFmpeg, whisper.cpp) will
//!   implement it on top of the unified process runner described in
//!   docs/WEB_ARCHITECTURE_PROPOSAL.md §E (allowlisted executable, argument
//!   array, controlled env/cwd, timeout, process-tree kill). They must NOT
//!   use the legacy `src-tauri/src/converter.rs` launcher.
//!
//! A kind with no runner registered is not reachable: the create-job handler
//! rejects it with `UNSUPPORTED_CONVERSION` before a job exists. That is how
//! a kind whose engine is missing (or not implemented yet) stays off the API,
//! rather than failing every job it accepts.
//!
//! The web layer contains no image-processing code: `NativeImageRunner` is
//! a thin adapter over `meb_core::image::convert_file_with_hooks`, which is
//! the same pipeline (and the same limits) the desktop app uses.

use crate::spec::{JobKind, JobSpec};
use meb_core::image::{self, NativeConvertOptions, PipelineHooks, PipelineStage};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct RunRequest<'a> {
    /// The job's inputs, staged in its own `in/` directory, in the order the
    /// request named them. Single-input kinds get exactly one; multi-input
    /// kinds (PDF merge) get the whole list.
    pub inputs: &'a [PathBuf],
    /// Where the engine must write its single output. Nothing else in the
    /// workspace is published.
    pub output: &'a Path,
    /// What to do - already validated; a runner may trust it.
    pub spec: &'a JobSpec,
}

impl RunRequest<'_> {
    /// The first input. Every kind has at least one, so this is infallible
    /// for a request the worker built.
    pub fn input(&self) -> &Path {
        self.inputs
            .first()
            .map(PathBuf::as_path)
            // Unreachable for a worker-built request; a runner must not have
            // to handle it, and a bogus path fails the engine honestly.
            .unwrap_or_else(|| Path::new(""))
    }
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

    /// The job's cancel flag itself, for a runner that hands cancellation
    /// to something which polls it on its own thread - specifically
    /// `meb_engines::process::run_cancellable`, which uses it to kill and
    /// reap the child. In-process engines use `is_cancelled` instead.
    pub(crate) fn cancel_flag(&self) -> &AtomicBool {
        self.cancel
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

impl RunError {
    /// For a runner handed a spec of a kind it does not implement - a wiring
    /// mistake in the registry, not a client error.
    pub fn wrong_kind(runner: &str) -> RunError {
        RunError::Failed {
            code: "INTERNAL_ERROR",
            detail: format!("{runner} was given a job kind it does not handle"),
        }
    }
}

pub trait ConversionRunner: Send + Sync + 'static {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError>;
}

/// Which runner executes which `JobKind`.
///
/// Built once at startup. Lookup is the only way the worker obtains a
/// runner, so the set of kinds this server will actually execute is exactly
/// the set of keys here.
#[derive(Clone, Default)]
pub struct RunnerRegistry {
    by_kind: HashMap<JobKind, Arc<dyn ConversionRunner>>,
}

impl RunnerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The engines this server ships with. Kinds backed by an external tool
    /// are added here as each one is migrated onto the process runner.
    ///
    /// An external engine is probed ONCE, here, and registered only if it
    /// actually resolved on this machine. That is what keeps the promise
    /// above: a kind whose tool is missing is absent from the registry, so
    /// the create-job handler refuses it with `UNSUPPORTED_CONVERSION`
    /// instead of accepting jobs that could only ever fail.
    pub fn production() -> Self {
        let registry = Self::new().with(JobKind::ImageConvert, Arc::new(NativeImageRunner));
        let registry = register_external(
            registry,
            "ghostscript",
            &crate::engines::GhostscriptRunner::KINDS,
            crate::engines::GhostscriptRunner::detect()
                .map(|r| Arc::new(r) as Arc<dyn ConversionRunner>),
        );
        register_external(
            registry,
            "libreoffice",
            &crate::engines::LibreOfficeRunner::KINDS,
            crate::engines::LibreOfficeRunner::detect()
                .map(|r| Arc::new(r) as Arc<dyn ConversionRunner>),
        )
    }

    pub fn with(mut self, kind: JobKind, runner: Arc<dyn ConversionRunner>) -> Self {
        self.by_kind.insert(kind, runner);
        self
    }

    /// One runner for every kind. A TEST seam (runners that block, panic or
    /// fail, to exercise the job lifecycle) - production wiring names the
    /// kind each engine actually handles.
    pub fn uniform(runner: Arc<dyn ConversionRunner>) -> Self {
        JobKind::ALL
            .into_iter()
            .fold(Self::new(), |registry, kind| {
                registry.with(kind, runner.clone())
            })
    }

    pub fn for_kind(&self, kind: JobKind) -> Option<Arc<dyn ConversionRunner>> {
        self.by_kind.get(&kind).cloned()
    }

    pub fn supports(&self, kind: JobKind) -> bool {
        self.by_kind.contains_key(&kind)
    }
}

/// Registers `runner` for every kind it backs, or none of them if the
/// engine did not resolve on this machine.
///
/// The all-or-nothing part is the point: a half-wired engine would make
/// `GET /capabilities` advertise an operation that `POST /jobs` accepts and
/// then always fails. Either the tool is there and every kind it backs is
/// executable, or the kinds are absent from the API entirely.
fn register_external(
    registry: RunnerRegistry,
    engine: &'static str,
    kinds: &[JobKind],
    runner: Option<Arc<dyn ConversionRunner>>,
) -> RunnerRegistry {
    let kind_names: Vec<&str> = kinds.iter().copied().map(JobKind::wire).collect();
    match runner {
        Some(runner) => {
            tracing::info!(engine, kinds = ?kind_names, "external engine available");
            kinds.iter().fold(registry, |registry, kind| {
                registry.with(*kind, runner.clone())
            })
        }
        None => {
            tracing::warn!(
                engine,
                kinds = ?kind_names,
                "external engine not found: these job kinds stay off this server's API"
            );
            registry
        }
    }
}

/// Adapter over the shared native image engine.
pub struct NativeImageRunner;

impl ConversionRunner for NativeImageRunner {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        let Some(spec) = request.spec.image_convert() else {
            return Err(RunError::wrong_kind("NativeImageRunner"));
        };
        let options = NativeConvertOptions {
            width: spec.options.width,
            height: spec.options.height,
            quality: spec.options.quality,
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
            request.input(),
            request.output,
            spec.target,
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

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    impl ConversionRunner for Noop {
        fn run(&self, _: &RunRequest<'_>, _: &JobControl<'_>) -> Result<(), RunError> {
            Ok(())
        }
    }

    #[test]
    fn an_unregistered_kind_has_no_runner() {
        let empty = RunnerRegistry::new();
        for kind in JobKind::ALL {
            assert!(!empty.supports(kind));
            assert!(empty.for_kind(kind).is_none());
        }
    }

    /// Kinds for which NO engine is implemented yet. They are part of the
    /// API's vocabulary - a client gets a clear `UNSUPPORTED_CONVERSION`
    /// rather than a parse error - but nothing pretends to execute them.
    ///
    /// This is about code, not about this machine: a kind backed by an
    /// implemented external engine is not listed, even though whether it is
    /// *wired* depends on that tool being installed. Moving a kind off this
    /// list means implementing its runner and registering it in
    /// `production()` in the same change.
    const AWAITING_AN_ENGINE: [JobKind; 2] = [JobKind::PdfWatermark, JobKind::PdfOcr];

    /// The external engines `production()` wires, each with whether it
    /// resolved here. Mirrors that function, so a new engine added there
    /// and not here fails `every_external_engine_is_covered_by_the_table`.
    fn external_engines() -> Vec<(&'static [JobKind], bool)> {
        vec![
            (
                &crate::engines::GhostscriptRunner::KINDS,
                crate::engines::GhostscriptRunner::detect().is_some(),
            ),
            (
                &crate::engines::LibreOfficeRunner::KINDS,
                crate::engines::LibreOfficeRunner::detect().is_some(),
            ),
        ]
    }

    #[test]
    fn every_external_engine_is_covered_by_the_table() {
        // Every kind that is neither in-process nor awaiting an engine must
        // belong to exactly one external engine above.
        for kind in JobKind::ALL {
            if kind == JobKind::ImageConvert || AWAITING_AN_ENGINE.contains(&kind) {
                continue;
            }
            let owners = external_engines()
                .iter()
                .filter(|(kinds, _)| kinds.contains(&kind))
                .count();
            assert_eq!(
                owners,
                1,
                "{} is backed by {owners} external engines, expected exactly 1",
                kind.wire()
            );
        }
    }

    #[test]
    fn production_wires_exactly_the_kinds_it_can_execute() {
        let registry = RunnerRegistry::production();
        // An external engine is only wired when it really resolved here, so
        // what the registry must agree with is that fact - not a hardcoded
        // expectation about the dev/CI machine.
        let engines = external_engines();
        for kind in JobKind::ALL {
            let expected = if AWAITING_AN_ENGINE.contains(&kind) {
                false
            } else if let Some((_, available)) =
                engines.iter().find(|(kinds, _)| kinds.contains(&kind))
            {
                *available
            } else {
                true
            };
            assert_eq!(
                registry.supports(kind),
                expected,
                "kind {} is {} a runner, which is not what production() promises",
                kind.wire(),
                if registry.supports(kind) {
                    "wired to"
                } else {
                    "missing"
                }
            );
        }
        // The native image pipeline is in-process, so it is always wired.
        assert!(registry.supports(JobKind::ImageConvert));
        // A kind with no engine at all is never wired, whatever is installed.
        for kind in AWAITING_AN_ENGINE {
            assert!(!registry.supports(kind));
        }
    }

    #[test]
    fn uniform_covers_every_kind() {
        let registry = RunnerRegistry::uniform(Arc::new(Noop));
        assert!(JobKind::ALL.into_iter().all(|k| registry.supports(k)));
    }
}
