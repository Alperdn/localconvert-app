//! LibreOffice, behind the generalized `ConversionRunner` trait.
//!
//! Backs two job kinds, and the distinction between them is the point of
//! this module's shape:
//!
//! - `office_convert` is **rendering**. LibreOffice already knows how to
//!   lay out a DOCX/XLSX/PPTX/ODT/ODS/ODP, so exporting it to PDF, or to
//!   the other format of its own document class, is a faithful operation.
//! - `pdf_to_office` is **reconstruction**: inferring paragraphs and table
//!   cells back out of a format that only records fixed-position drawing
//!   operations. The only implementation available is LibreOffice's own
//!   `writer_pdf_import` filter, which is a real feature with real limits -
//!   decent on a text PDF, close to useless on a scanned one. It is
//!   therefore marked experimental all the way out to the download name
//!   (`JobKind::is_experimental`, `JobSpec::output_name`), and it is a
//!   separate kind rather than a direction of the first one.
//!
//! The desktop app draws exactly this boundary in
//! `src-tauri/src/reconstruction.rs`; this is the same policy, enforced in
//! the one place the web server can reach LibreOffice from.
//!
//! The invocation is the one `src-tauri/src/engines/office.rs` has been
//! shipping, adapted onto `meb_engines::process` (no shell, argument array,
//! cwd pinned to the job's work directory, denylisted environment,
//! wall-clock timeout, kill-and-reap on cancel) instead of a raw
//! `std::process::Command`.
//!
//! Known limitation, carried over from the desktop: the timeout kills the
//! direct child. For a headless `--convert-to` run `soffice` is itself the
//! long-running process, so that covers this invocation - but it is not a
//! process-tree sandbox. See `docs/OFFICE_ENGINE.md`.

use crate::runner::{ConversionRunner, JobControl, RunError, RunRequest};
use crate::spec::{JobKind, JobSpec, OfficeConvertSpec, PdfToOfficeSpec};
use meb_core::format::SourceFormat;
use meb_engines::process::{self, CancellableRun, ProcessEnd};
use meb_engines::resolver::{self, ResolutionPolicy};
use meb_engines::EngineId;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Wall-clock budget for ONE LibreOffice invocation.
///
/// The same 600s the desktop app uses, for the same measured reason: a
/// large real-world document costs the fresh-profile startup (see
/// `profile_arg`) plus the conversion itself, which ranged from ~280s with
/// warm OS/font/AV caches up to ~1300s on a genuinely cold first run. A
/// shorter budget fails such a document intermittently depending on cache
/// state, which is worse than failing it predictably. See
/// `src-tauri/src/engines/office_manifest.rs` and
/// `docs/OFFICE_ENGINE.md`.
const INVOCATION_TIMEOUT: Duration = Duration::from_secs(600);

/// Stable error codes this engine reports. `error::job_error_message` has
/// the user-facing Turkish text for each.
const ENGINE_UNAVAILABLE: &str = "OFFICE_ENGINE_UNAVAILABLE";
const CONVERSION_FAILED: &str = "OFFICE_CONVERSION_FAILED";
const ENGINE_TIMEOUT: &str = "OFFICE_ENGINE_TIMEOUT";
/// LibreOffice exits 0 after refusing a document often enough that this is
/// a distinct, expected outcome rather than an internal error.
const NO_OUTPUT: &str = "OFFICE_NO_OUTPUT";

/// Subdirectory of the job's `work/` that holds this invocation's private
/// LibreOffice profile. See `profile_arg`.
const PROFILE_DIR: &str = "loffice_profile";

pub struct LibreOfficeRunner {
    engine: resolver::ResolvedEngine,
    timeout: Duration,
}

impl LibreOfficeRunner {
    /// The job kinds this engine backs. Shared by the registry wiring and
    /// by the test that checks the two agree.
    pub const KINDS: [JobKind; 2] = [JobKind::OfficeConvert, JobKind::PdfToOffice];

    /// Resolves LibreOffice once, at startup: bundled next to the server
    /// binary first, then `PATH`. `None` means this machine has none, and
    /// the caller must then leave `KINDS` unregistered so the API refuses
    /// them outright instead of accepting jobs that could only fail.
    pub fn detect() -> Option<LibreOfficeRunner> {
        Self::detect_with(&ResolutionPolicy::BUNDLED_THEN_PATH)
    }

    /// `detect` with an explicit policy - the seam the tests use to prove
    /// that an unresolvable engine yields no runner at all.
    pub fn detect_with(policy: &ResolutionPolicy) -> Option<LibreOfficeRunner> {
        let engine = resolver::resolve_with(EngineId::Office, policy).ok()?;
        Some(LibreOfficeRunner {
            engine,
            timeout: INVOCATION_TIMEOUT,
        })
    }

    /// A runner bound to an arbitrary executable. A TEST seam: it is how
    /// the outcome mapping is exercised on a machine with no LibreOffice
    /// installed, by standing a well-known system binary in for the engine.
    #[cfg(test)]
    fn with_engine(path: PathBuf, timeout: Duration) -> LibreOfficeRunner {
        LibreOfficeRunner {
            engine: resolver::ResolvedEngine {
                id: EngineId::Office,
                path,
                tier: resolver::EngineTier::System,
            },
            timeout,
        }
    }

    /// Runs one LibreOffice invocation to completion inside `work_dir`.
    ///
    /// LibreOffice's stderr never reaches the client - it goes into the
    /// server log as the failure's `detail`.
    fn invoke(
        &self,
        args: &[String],
        work_dir: &Path,
        control: &JobControl<'_>,
    ) -> Result<(), RunError> {
        if control.is_cancelled() {
            return Err(RunError::Cancelled);
        }
        let result = process::run_cancellable(
            &self.engine,
            args,
            work_dir,
            CancellableRun {
                timeout: Some(self.timeout),
                cancel: control.cancel_flag(),
                low_priority: false,
                on_stderr_line: None,
            },
        );
        match result.end {
            ProcessEnd::Success => Ok(()),
            ProcessEnd::Cancelled => Err(RunError::Cancelled),
            ProcessEnd::TimedOut => Err(RunError::Failed {
                code: ENGINE_TIMEOUT,
                detail: "libreoffice exceeded its invocation timeout and was killed".to_string(),
            }),
            ProcessEnd::SpawnFailed => Err(RunError::Failed {
                code: ENGINE_UNAVAILABLE,
                detail: format!("libreoffice could not be started: {}", result.stderr_tail),
            }),
            ProcessEnd::Failed(status) => Err(RunError::Failed {
                code: CONVERSION_FAILED,
                detail: format!(
                    "libreoffice exited with {}: {}",
                    status
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "no status".to_string()),
                    result.stderr_tail
                ),
            }),
        }
    }

    /// The shared conversion: one `soffice --headless --convert-to` run,
    /// then the rename LibreOffice's output naming forces on us.
    ///
    /// `filter` is the export filter; `import_pdf` adds
    /// `--infilter=writer_pdf_import`, which is what makes a PDF *input*
    /// load into Writer instead of being rejected.
    fn convert(
        &self,
        request: &RunRequest<'_>,
        control: &JobControl<'_>,
        filter: &str,
        import_pdf: bool,
    ) -> Result<(), RunError> {
        let work_dir = work_dir(request)?;
        let input = request.input();

        // Argument order follows the desktop app's proven invocation.
        let mut args = vec!["--headless".to_string()];
        if import_pdf {
            args.push("--infilter=writer_pdf_import".to_string());
        }
        args.push(profile_arg(&work_dir.join(PROFILE_DIR)));
        args.push("--convert-to".to_string());
        args.push(filter.to_string());
        args.push("--outdir".to_string());
        args.push(work_dir.to_string_lossy().to_string());
        args.push(input.to_string_lossy().to_string());

        self.invoke(&args, work_dir, control)?;
        finalize(request)
    }

    fn office_convert(
        &self,
        spec: &OfficeConvertSpec,
        request: &RunRequest<'_>,
        control: &JobControl<'_>,
    ) -> Result<(), RunError> {
        self.convert(request, control, export_filter(spec.target), false)
    }

    fn reconstruct(
        &self,
        spec: &PdfToOfficeSpec,
        request: &RunRequest<'_>,
        control: &JobControl<'_>,
    ) -> Result<(), RunError> {
        self.convert(
            request,
            control,
            spec.target.libreoffice_filter(),
            // The whole reason this kind exists: the PDF import filter.
            true,
        )
    }
}

impl ConversionRunner for LibreOfficeRunner {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        match request.spec {
            JobSpec::OfficeConvert(spec) => self.office_convert(spec, request, control),
            JobSpec::PdfToOffice(spec) => self.reconstruct(spec, request, control),
            JobSpec::ImageConvert(_)
            | JobSpec::ImageOptimize(_)
            | JobSpec::PdfMerge(_)
            | JobSpec::PdfSplit(_)
            | JobSpec::PdfCompress(_)
            | JobSpec::PdfRotate(_)
            | JobSpec::PdfWatermark(_)
            | JobSpec::PdfOcr(_)
            | JobSpec::PdfProtect(_)
            | JobSpec::PdfUnlock(_)
            | JobSpec::PdfMetadataStrip
            | JobSpec::PdfPageNumbers(_)
            | JobSpec::PdfDeletePages(_)
            | JobSpec::PdfReorderPages(_)
            | JobSpec::PdfExtractText
            => Err(RunError::wrong_kind("LibreOfficeRunner")),
        }
    }
}

/// The job's own work directory - the only place this engine writes, the
/// cwd the invocation gets, and the parent of its private profile.
fn work_dir<'a>(request: &RunRequest<'a>) -> Result<&'a Path, RunError> {
    request.output.parent().ok_or(RunError::Failed {
        code: CONVERSION_FAILED,
        detail: "the job output path has no parent directory".to_string(),
    })
}

/// `-env:UserInstallation=` pointing at a profile directory of this job's
/// own, inside its workspace.
///
/// Not an optimization: without it every LibreOffice process shares the
/// default profile and races on its lock file, so two conversions running
/// at the same time (which on a server is the normal case, not the
/// exceptional one) make one of them fail with an opaque "another instance
/// is already running". It is a documented headless-mode pitfall, and the
/// desktop app carries the same flag for the same reason.
fn profile_arg(profile_dir: &Path) -> String {
    format!("-env:UserInstallation={}", to_file_uri(profile_dir))
}

/// A filesystem path in the `file://` URI form `-env:UserInstallation=`
/// requires: forward slashes and a scheme, on Windows too.
fn to_file_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// The LibreOffice export filter that writes `target`.
fn export_filter(target: SourceFormat) -> &'static str {
    match target {
        SourceFormat::Pdf => "pdf",
        SourceFormat::Office(f) => f.libreoffice_filter(),
        // `office_conversion_targets` never yields an image, so this is
        // unreachable for a validated spec; a filter name that cannot
        // convert anything is still the safe thing to return.
        SourceFormat::Image(_) => "pdf",
    }
}

/// Turns "the engine exited 0" into "the job produced its promised output".
///
/// Two things have to happen, and neither is optional:
///
/// - LibreOffice declining a document while still exiting 0 is a common,
///   expected outcome, so the absence of a written file is a job failure
///   with its own code - not a success, and not an internal error.
/// - LibreOffice names its output after the input, so the result has to be
///   moved onto the path the worker will validate and publish.
fn finalize(request: &RunRequest<'_>) -> Result<(), RunError> {
    let produced = produced_path(request)?;
    if !produced.is_file() {
        return Err(RunError::Failed {
            code: NO_OUTPUT,
            detail: "libreoffice reported success but wrote no output file".to_string(),
        });
    }
    if produced == request.output {
        return Ok(());
    }
    std::fs::rename(&produced, request.output).map_err(|e| RunError::Failed {
        code: CONVERSION_FAILED,
        detail: format!("could not move the converted document into place: {e}"),
    })
}

/// Where LibreOffice will have written the result.
///
/// It names its output after the INPUT's stem, in `--outdir`, with the
/// target's extension - it has no flag for "write exactly this path". The
/// job's own output is `work/result.<ext>`, so a rename always follows. The
/// extension is taken from the output path the job promised rather than
/// from the filter string, so the two can never disagree about what file to
/// look for.
fn produced_path(request: &RunRequest<'_>) -> Result<PathBuf, RunError> {
    let failed = |detail: &str| RunError::Failed {
        code: CONVERSION_FAILED,
        detail: detail.to_string(),
    };
    let dir = work_dir(request)?;
    let stem = request
        .input()
        .file_stem()
        .ok_or_else(|| failed("the staged input has no file name"))?;
    let extension = request
        .output
        .extension()
        .ok_or_else(|| failed("the job output path has no extension"))?;
    let mut name = stem.to_os_string();
    name.push(".");
    name.push(extension);
    Ok(dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{office_conversion_targets, pdf_reconstruction_targets};
    use meb_core::format::OfficeFormat;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    /// The argument vector `convert` builds, without spawning anything -
    /// LibreOffice is not installed on most dev machines, and these are
    /// assertions about the command line, not about LibreOffice.
    fn args_for(request: &RunRequest<'_>, filter: &str, import_pdf: bool) -> Vec<String> {
        let work_dir = request.output.parent().unwrap();
        let mut args = vec!["--headless".to_string()];
        if import_pdf {
            args.push("--infilter=writer_pdf_import".to_string());
        }
        args.push(profile_arg(&work_dir.join(PROFILE_DIR)));
        args.push("--convert-to".to_string());
        args.push(filter.to_string());
        args.push("--outdir".to_string());
        args.push(work_dir.to_string_lossy().to_string());
        args.push(request.input().to_string_lossy().to_string());
        args
    }

    fn request<'a>(
        inputs: &'a [PathBuf],
        output: &'a Path,
        spec: &'a JobSpec,
    ) -> RunRequest<'a> {
        RunRequest {
            inputs,
            output,
            spec,
        }
    }

    fn office_spec(source: OfficeFormat, target: SourceFormat) -> JobSpec {
        JobSpec::OfficeConvert(OfficeConvertSpec {
            source_format: source,
            target,
        })
    }

    #[test]
    fn an_unresolvable_engine_yields_no_runner_at_all() {
        // The contract the registry depends on: no LibreOffice means no
        // runner, which means `office_convert` and `pdf_to_office` stay off
        // the API rather than accepting jobs that must fail.
        assert!(LibreOfficeRunner::detect_with(&ResolutionPolicy::EMPTY).is_none());
    }

    #[test]
    fn every_invocation_is_headless_and_has_its_own_profile_inside_the_job() {
        let inputs = vec![PathBuf::from("/data/jobs/j1/in/source-00.docx")];
        let output = PathBuf::from("/data/jobs/j1/work/result.pdf");
        let spec = office_spec(OfficeFormat::Docx, SourceFormat::Pdf);
        let request = request(&inputs, &output, &spec);
        let args = args_for(&request, "pdf", false);

        assert_eq!(args[0], "--headless");
        let profile = args
            .iter()
            .find(|a| a.starts_with("-env:UserInstallation="))
            .expect("no per-job profile was requested");
        // Inside the job's own work directory, so two concurrent jobs can
        // never share a profile (or its lock file).
        assert!(
            profile.contains("/data/jobs/j1/work/loffice_profile"),
            "{profile}"
        );
        assert!(profile.starts_with("-env:UserInstallation=file:///"));
        // The profile flag must precede --convert-to, as in the invocation
        // the desktop app has proven.
        let profile_at = args.iter().position(|a| a == profile).unwrap();
        let convert_at = args.iter().position(|a| a == "--convert-to").unwrap();
        assert!(profile_at < convert_at);
        // The input is last, and is the only bare (non-flag) argument
        // besides the --outdir and --convert-to values.
        assert_eq!(args.last().unwrap(), "/data/jobs/j1/in/source-00.docx");
    }

    #[test]
    fn a_windows_profile_path_becomes_a_file_uri_with_forward_slashes() {
        let uri = to_file_uri(Path::new(r"C:\data\jobs\j1\work\loffice_profile"));
        assert_eq!(uri, "file:///C:/data/jobs/j1/work/loffice_profile");
        assert!(!uri.contains('\\'));
        // A POSIX absolute path keeps its single leading slash.
        assert_eq!(
            to_file_uri(Path::new("/srv/jobs/j1/work/loffice_profile")),
            "file:///srv/jobs/j1/work/loffice_profile"
        );
    }

    #[test]
    fn only_the_reconstruction_direction_asks_for_the_pdf_import_filter() {
        let inputs = vec![PathBuf::from("in/source-00.docx")];
        let output = PathBuf::from("work/result.pdf");
        let spec = office_spec(OfficeFormat::Docx, SourceFormat::Pdf);
        let rendering = args_for(&request(&inputs, &output, &spec), "pdf", false);
        assert!(!rendering.iter().any(|a| a.contains("writer_pdf_import")));

        let inputs = vec![PathBuf::from("in/source-00.pdf")];
        let output = PathBuf::from("work/result.docx");
        let spec = JobSpec::PdfToOffice(PdfToOfficeSpec {
            target: OfficeFormat::Docx,
        });
        let reconstruction = args_for(&request(&inputs, &output, &spec), "docx", true);
        assert_eq!(reconstruction[1], "--infilter=writer_pdf_import");
    }

    #[test]
    fn the_filter_and_the_promised_output_extension_always_agree() {
        // `produced_path` looks for `<input stem>.<output extension>` while
        // `--convert-to` is given the filter name. If a filter's name ever
        // stopped matching the extension it writes, the rename would look
        // for a file that is not there - so the two must be checked to
        // agree for every pair the API accepts.
        for source in OfficeFormat::ALL {
            for target in office_conversion_targets(source) {
                assert_eq!(
                    export_filter(target),
                    target.canonical_extension(),
                    "{source:?} -> {target:?}"
                );
            }
        }
        for target in pdf_reconstruction_targets() {
            assert_eq!(target.libreoffice_filter(), target.canonical_extension());
        }
    }

    #[test]
    fn the_rename_looks_for_the_name_libreoffice_actually_writes() {
        // LibreOffice names its output after the INPUT stem in --outdir,
        // never after the path the job wants.
        let inputs = vec![PathBuf::from("/j/in/source-00.docx")];
        let output = PathBuf::from("/j/work/result.pdf");
        let spec = office_spec(OfficeFormat::Docx, SourceFormat::Pdf);
        let produced = produced_path(&request(&inputs, &output, &spec)).unwrap();
        assert_eq!(produced, PathBuf::from("/j/work/source-00.pdf"));
        // It lands in the job's work directory, next to - and never on top
        // of - the output the worker will validate and publish.
        assert_eq!(produced.parent(), output.parent());
        assert_ne!(produced, output);
    }

    #[test]
    fn a_reconstruction_output_is_named_experimental_for_the_user() {
        let spec = JobSpec::PdfToOffice(PdfToOfficeSpec {
            target: OfficeFormat::Docx,
        });
        assert_eq!(spec.output_name("rapor"), "rapor_deneysel.docx");
        assert!(spec.kind().is_experimental());
        // The faithful direction carries no such mark.
        let rendering = office_spec(OfficeFormat::Docx, SourceFormat::Pdf);
        assert_eq!(rendering.output_name("rapor"), "rapor.pdf");
        assert!(!rendering.kind().is_experimental());
    }

    #[test]
    fn no_argument_is_ever_a_shell_or_a_command_string() {
        let inputs = vec![PathBuf::from("/j/in/source-00.docx")];
        let output = PathBuf::from("/j/work/result.odt");
        let spec = office_spec(OfficeFormat::Docx, SourceFormat::Office(OfficeFormat::Odt));
        for arg in args_for(&request(&inputs, &output, &spec), "odt", false) {
            for forbidden in ["&&", "||", ";", "|", "`", "$(", "cmd.exe", "powershell"] {
                assert!(
                    !arg.contains(forbidden),
                    "{forbidden:?} reached an argument: {arg:?}"
                );
            }
        }
    }

    #[test]
    fn the_runner_handles_exactly_the_two_document_kinds() {
        for kind in JobKind::ALL {
            let handled = LibreOfficeRunner::KINDS.contains(&kind);
            assert_eq!(
                handled,
                matches!(kind, JobKind::OfficeConvert | JobKind::PdfToOffice),
                "{} is handled inconsistently",
                kind.wire()
            );
        }
    }

    // ------------------------------------------- outcome mapping ----------
    //
    // As in `ghostscript.rs`: `ping` stands in for the engine so the parts a
    // command-line unit test cannot reach - the timeout firing, the cancel
    // flag killing and reaping the child, a non-zero exit becoming a job
    // failure, a missing executable being reported as an unavailable engine
    // - are driven against a real child process.

    struct Control {
        cancel: AtomicBool,
        progress: Mutex<Vec<u8>>,
    }

    impl Control {
        fn new(cancelled: bool) -> Control {
            Control {
                cancel: AtomicBool::new(cancelled),
                progress: Mutex::new(Vec::new()),
            }
        }
    }

    fn with_control<T>(control: &Control, body: impl FnOnce(&JobControl<'_>) -> T) -> T {
        let sink = |pct: u8| control.progress.lock().unwrap().push(pct);
        body(&JobControl {
            cancel: &control.cancel,
            progress: &sink,
        })
    }

    #[test]
    #[cfg(windows)]
    fn a_successful_invocation_is_a_successful_job() {
        let runner = LibreOfficeRunner::with_engine(PathBuf::from("ping"), Duration::from_secs(30));
        let control = Control::new(false);
        let args = ["127.0.0.1".to_string(), "-n".to_string(), "1".to_string()];
        let result = with_control(&control, |c| {
            runner.invoke(&args, &std::env::temp_dir(), c)
        });
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    #[cfg(windows)]
    fn a_non_zero_exit_is_a_conversion_failure_and_never_leaks_stderr_to_the_client() {
        let runner = LibreOfficeRunner::with_engine(PathBuf::from("ping"), Duration::from_secs(30));
        let control = Control::new(false);
        let args = [
            "-n".to_string(),
            "1".to_string(),
            "meb-donustur.invalid".to_string(),
        ];
        let result = with_control(&control, |c| {
            runner.invoke(&args, &std::env::temp_dir(), c)
        });
        match result {
            Err(RunError::Failed { code, detail }) => {
                assert_eq!(code, CONVERSION_FAILED);
                // The code is what the client sees; the diagnostics stay in
                // the log-only detail.
                assert!(detail.contains("exited with"), "{detail}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    #[cfg(windows)]
    fn a_hung_invocation_is_killed_and_reported_as_a_timeout() {
        let runner =
            LibreOfficeRunner::with_engine(PathBuf::from("ping"), Duration::from_millis(300));
        let control = Control::new(false);
        let args = ["127.0.0.1".to_string(), "-n".to_string(), "30".to_string()];
        let start = std::time::Instant::now();
        let result = with_control(&control, |c| {
            runner.invoke(&args, &std::env::temp_dir(), c)
        });
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the timeout branch did not fire promptly"
        );
        match result {
            Err(RunError::Failed { code, .. }) => assert_eq!(code, ENGINE_TIMEOUT),
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_job_never_starts_a_process() {
        // Deliberately an executable that does not exist: if the cancel
        // check did not come first, this would report SpawnFailed instead.
        let runner = LibreOfficeRunner::with_engine(
            PathBuf::from("no-such-engine-executable"),
            Duration::from_secs(30),
        );
        let control = Control::new(true);
        let result = with_control(&control, |c| runner.invoke(&[], &std::env::temp_dir(), c));
        assert!(matches!(result, Err(RunError::Cancelled)), "{result:?}");
    }

    #[test]
    fn an_engine_that_cannot_be_started_is_reported_as_unavailable_not_as_a_bad_document() {
        let runner = LibreOfficeRunner::with_engine(
            PathBuf::from("no-such-engine-executable"),
            Duration::from_secs(30),
        );
        let control = Control::new(false);
        let result = with_control(&control, |c| runner.invoke(&[], &std::env::temp_dir(), c));
        match result {
            Err(RunError::Failed { code, .. }) => assert_eq!(code, ENGINE_UNAVAILABLE),
            other => panic!("expected an unavailable engine, got {other:?}"),
        }
    }

    /// A job workspace on disk: `work/` with a staged input beside it.
    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meb_lo_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_engine_that_exits_zero_without_writing_anything_is_not_a_success() {
        // The failure mode this step exists for: LibreOffice declining a
        // document and still exiting 0. The job must fail with its own
        // code, not succeed and not report an internal error.
        let dir = workspace();
        let inputs = vec![dir.join("source-00.docx")];
        let output = dir.join("result.pdf");
        let spec = office_spec(OfficeFormat::Docx, SourceFormat::Pdf);
        match finalize(&request(&inputs, &output, &spec)) {
            Err(RunError::Failed { code, .. }) => assert_eq!(code, NO_OUTPUT),
            other => panic!("expected a missing-output failure, got {other:?}"),
        }
        assert!(!output.exists(), "nothing may be left at the output path");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_produced_file_is_renamed_onto_the_path_the_job_promised() {
        let dir = workspace();
        let inputs = vec![dir.join("source-00.docx")];
        let output = dir.join("result.pdf");
        let spec = office_spec(OfficeFormat::Docx, SourceFormat::Pdf);
        // Stand in for what LibreOffice writes: the input stem with the
        // target extension, in the work directory.
        std::fs::write(dir.join("source-00.pdf"), b"%PDF-1.7\ntrailer\n%%EOF\n").unwrap();

        assert!(finalize(&request(&inputs, &output, &spec)).is_ok());
        assert!(output.is_file(), "the output was not moved into place");
        assert_eq!(
            std::fs::read(&output).unwrap(),
            b"%PDF-1.7\ntrailer\n%%EOF\n",
            "the moved file is not the one the engine wrote"
        );
        assert!(!dir.join("source-00.pdf").exists(), "a stray copy was left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_code_this_engine_reports_has_a_user_facing_message() {
        for code in [ENGINE_UNAVAILABLE, CONVERSION_FAILED, ENGINE_TIMEOUT, NO_OUTPUT] {
            let message = crate::error::job_error_message(code);
            assert_ne!(
                message,
                crate::error::job_error_message("A_CODE_THAT_DOES_NOT_EXIST"),
                "{code} falls through to the generic message"
            );
        }
    }
}
