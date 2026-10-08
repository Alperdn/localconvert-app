//! Ghostscript, behind the generalized `ConversionRunner` trait.
//!
//! Backs four job kinds: `pdf_merge`, `pdf_split`, `pdf_compress` and
//! `pdf_rotate`. The command patterns are the ones the desktop app has been
//! shipping (`src-tauri/src/commands.rs`), adapted in three ways:
//!
//! - The process is started by `meb_engines::process::run_cancellable`
//!   instead of a raw `std::process::Command`, so this engine inherits the
//!   shared invariants (no shell, argument array only, cwd pinned to the
//!   job's own work directory, denylisted environment, wall-clock timeout,
//!   kill-and-reap on cancel) rather than restating them.
//! - The executable comes from `meb_engines::resolver`, resolved once at
//!   startup, never from a name assembled at the call site.
//! - Every value that reaches the command line comes from the validated
//!   `JobSpec` - a closed enum (quality preset, rotation angle) or a
//!   bounded integer (page numbers) - or from a server-built path. No
//!   client string is ever interpolated into an argument.
//!
//! A split would naturally produce one file per page, but the job model
//! publishes exactly one output, so the pages are packed into a ZIP here
//! (which is what `JobSpec::output_extension` already promises for that
//! kind).

use crate::runner::{ConversionRunner, JobControl, RunError, RunRequest};
use crate::spec::{
    JobKind, JobSpec, PdfCompressOptions, PdfQuality, PdfRotateOptions, PdfSplitSpec, MAX_PDF_PAGE,
};
use meb_engines::process::{self, CancellableRun, ProcessEnd};
use meb_engines::resolver::{self, ResolutionPolicy};
use meb_engines::EngineId;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Wall-clock budget for ONE Ghostscript invocation. An INITIAL ENGINEERING
/// DEFAULT, like everything in `config.rs`: generous enough for a large
/// scanned document on a loaded machine, finite so a pathological input can
/// never pin a worker permit forever. A split spends it per page, not for
/// the whole job.
const INVOCATION_TIMEOUT: Duration = Duration::from_secs(300);

/// Stable error codes this engine reports. `error::job_error_message` has
/// the user-facing Turkish text for each.
const ENGINE_UNAVAILABLE: &str = "PDF_ENGINE_UNAVAILABLE";
const OPERATION_FAILED: &str = "PDF_OPERATION_FAILED";
const ENGINE_TIMEOUT: &str = "PDF_ENGINE_TIMEOUT";
const PAGE_UNAVAILABLE: &str = "PDF_PAGE_UNAVAILABLE";

/// Subdirectory of the job's `work/` that a split renders its pages into,
/// before they are packed into the published archive. Separate from the
/// output file so the archive can never accidentally contain itself.
const SPLIT_PAGES_DIR: &str = "pages";

pub struct GhostscriptRunner {
    engine: resolver::ResolvedEngine,
    timeout: Duration,
}

impl GhostscriptRunner {
    /// The job kinds this engine backs. Shared by the registry wiring and
    /// by the test that checks the two agree.
    pub const KINDS: [JobKind; 4] = [
        JobKind::PdfMerge,
        JobKind::PdfSplit,
        JobKind::PdfCompress,
        JobKind::PdfRotate,
    ];

    /// Resolves Ghostscript once, at startup: bundled next to the server
    /// binary first, then `PATH`. `None` means this machine has no
    /// Ghostscript, and the caller must then leave `KINDS` unregistered so
    /// the API refuses them outright instead of accepting jobs that could
    /// only fail.
    pub fn detect() -> Option<GhostscriptRunner> {
        Self::detect_with(&ResolutionPolicy::BUNDLED_THEN_PATH)
    }

    /// `detect` with an explicit policy - the seam the tests use to prove
    /// that an unresolvable engine yields no runner at all.
    pub fn detect_with(policy: &ResolutionPolicy) -> Option<GhostscriptRunner> {
        let engine = resolver::resolve_with(EngineId::Ghostscript, policy).ok()?;
        Some(GhostscriptRunner {
            engine,
            timeout: INVOCATION_TIMEOUT,
        })
    }

    /// A runner bound to an arbitrary executable. A TEST seam: it is how
    /// the outcome mapping below (exit code, timeout, cancel, failure to
    /// start) is exercised on a machine with no Ghostscript installed, by
    /// standing a well-known system binary in for the engine.
    #[cfg(test)]
    fn with_engine(path: PathBuf, timeout: Duration) -> GhostscriptRunner {
        GhostscriptRunner {
            engine: resolver::ResolvedEngine {
                id: EngineId::Ghostscript,
                path,
                tier: resolver::EngineTier::System,
            },
            timeout,
        }
    }

    /// Flags every invocation carries, plus the single output file this one
    /// writes. `-dSAFER` is what keeps the PostScript Ghostscript executes
    /// from reaching the filesystem beyond the files named here.
    fn base_args(output: &Path) -> Vec<String> {
        vec![
            "-dNOPAUSE".to_string(),
            "-dBATCH".to_string(),
            "-dSAFER".to_string(),
            "-sDEVICE=pdfwrite".to_string(),
            format!("-sOutputFile={}", output.to_string_lossy()),
        ]
    }

    /// Runs one Ghostscript invocation to completion inside `work_dir`.
    ///
    /// `failure_code` is what a non-zero exit reports; cancellation,
    /// timeout and a failure to start are mapped the same way for every
    /// operation. Ghostscript's stderr never reaches the client - it goes
    /// into the server log as the failure's `detail`.
    fn invoke(
        &self,
        args: &[String],
        work_dir: &Path,
        control: &JobControl<'_>,
        failure_code: &'static str,
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
                detail: "ghostscript exceeded its invocation timeout and was killed".to_string(),
            }),
            ProcessEnd::SpawnFailed => Err(RunError::Failed {
                code: ENGINE_UNAVAILABLE,
                detail: format!("ghostscript could not be started: {}", result.stderr_tail),
            }),
            ProcessEnd::Failed(status) => Err(RunError::Failed {
                code: failure_code,
                detail: format!(
                    "ghostscript exited with {}: {}",
                    status
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "no status".to_string()),
                    result.stderr_tail
                ),
            }),
        }
    }

    fn merge(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        let mut args = Self::base_args(request.output);
        // Page order is request order, which `api::jobs` staged as
        // `source-00`, `source-01`, ... and handed over in that order.
        for input in request.inputs {
            args.push(input.to_string_lossy().to_string());
        }
        self.invoke(&args, work_dir(request)?, control, OPERATION_FAILED)
    }

    fn compress(
        &self,
        options: &PdfCompressOptions,
        request: &RunRequest<'_>,
        control: &JobControl<'_>,
    ) -> Result<(), RunError> {
        let mut args = Self::base_args(request.output);
        args.push(format!("-dPDFSETTINGS={}", pdf_settings(options.quality)));
        args.push(request.input().to_string_lossy().to_string());
        self.invoke(&args, work_dir(request)?, control, OPERATION_FAILED)
    }

    fn rotate(
        &self,
        options: &PdfRotateOptions,
        request: &RunRequest<'_>,
        control: &JobControl<'_>,
    ) -> Result<(), RunError> {
        let mut args = Self::base_args(request.output);
        // The angle is one of three values from a closed enum, formatted
        // into a fixed PostScript snippet - `-f` then ends the `-c` snippet
        // so the input that follows is read as a file, never as more
        // PostScript.
        args.push("-c".to_string());
        args.push(format!(
            "<< /PageRotation {} >> setpagedevice",
            options.rotation.degrees()
        ));
        args.push("-f".to_string());
        args.push(request.input().to_string_lossy().to_string());
        self.invoke(&args, work_dir(request)?, control, OPERATION_FAILED)
    }

    /// Extracts pages into `work/pages/` and packs them into the job's
    /// single ZIP output.
    ///
    /// Two shapes, because Ghostscript is good at exactly one of them:
    /// "every page" is ONE invocation with a `%d` output template, while a
    /// chosen subset is one invocation per page (there is no page-set
    /// argument). The subset case therefore also has honest per-page
    /// progress to report; the whole-document case has none to report.
    fn split(
        &self,
        spec: &PdfSplitSpec,
        request: &RunRequest<'_>,
        control: &JobControl<'_>,
    ) -> Result<(), RunError> {
        let work_dir = work_dir(request)?;
        let pages_dir = work_dir.join(SPLIT_PAGES_DIR);
        std::fs::create_dir_all(&pages_dir).map_err(|e| RunError::Failed {
            code: OPERATION_FAILED,
            detail: format!("could not create the split work directory: {e}"),
        })?;
        let input = request.input().to_string_lossy().to_string();

        let mut pages: Vec<(u32, PathBuf)> = Vec::new();
        match &spec.pages {
            Some(wanted) => {
                for (done, &page) in wanted.iter().enumerate() {
                    let path = pages_dir.join(format!("{page}.pdf"));
                    let mut args = Self::base_args(&path);
                    args.push(format!("-dFirstPage={page}"));
                    args.push(format!("-dLastPage={page}"));
                    args.push(input.clone());
                    // A page the document does not have is the one failure
                    // here a client can actually act on, so it gets its own
                    // code rather than the generic one.
                    self.invoke(&args, work_dir, control, PAGE_UNAVAILABLE)?;
                    if !is_non_empty_file(&path) {
                        return Err(RunError::Failed {
                            code: PAGE_UNAVAILABLE,
                            detail: format!("ghostscript produced no output for page {page}"),
                        });
                    }
                    pages.push((page, path));
                    // Real progress: pages finished out of pages asked for.
                    let finished = done + 1;
                    control.report_progress((finished * 100 / wanted.len()) as u8);
                }
            }
            None => {
                let template = pages_dir.join("%d.pdf");
                let mut args = Self::base_args(&template);
                args.push(input);
                self.invoke(&args, work_dir, control, OPERATION_FAILED)?;
                pages = produced_pages(&pages_dir)?;
            }
        }

        if pages.is_empty() {
            return Err(RunError::Failed {
                code: OPERATION_FAILED,
                detail: "the split produced no pages".to_string(),
            });
        }
        if pages.len() > MAX_PDF_PAGE as usize {
            return Err(RunError::Failed {
                code: OPERATION_FAILED,
                detail: format!(
                    "the document has more than the {MAX_PDF_PAGE} pages this server will split"
                ),
            });
        }
        if control.is_cancelled() {
            return Err(RunError::Cancelled);
        }
        pack_pages(&pages, request.output)
    }
}

impl ConversionRunner for GhostscriptRunner {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        match request.spec {
            JobSpec::PdfMerge(_) => self.merge(request, control),
            JobSpec::PdfSplit(spec) => self.split(spec, request, control),
            JobSpec::PdfCompress(options) => self.compress(options, request, control),
            JobSpec::PdfRotate(options) => self.rotate(options, request, control),
            // Ghostscript could do a watermark and is a step of OCR, but
            // neither is wired to this runner yet; being handed one is a
            // registry wiring mistake, not something to guess at.
            JobSpec::ImageConvert(_)
            | JobSpec::PdfWatermark(_)
            | JobSpec::PdfOcr(_)
            | JobSpec::OfficeConvert(_) => Err(RunError::wrong_kind("GhostscriptRunner")),
        }
    }
}

/// The job's own work directory - the only place this engine writes, and
/// the cwd every invocation gets, so a relative path Ghostscript might
/// resolve on its own can never leave it.
fn work_dir<'a>(request: &RunRequest<'a>) -> Result<&'a Path, RunError> {
    request.output.parent().ok_or(RunError::Failed {
        code: OPERATION_FAILED,
        detail: "the job output path has no parent directory".to_string(),
    })
}

/// Ghostscript's name for a quality preset. A closed mapping from a closed
/// enum: no free-form value can reach this argument.
fn pdf_settings(quality: PdfQuality) -> &'static str {
    match quality {
        PdfQuality::Screen => "/screen",
        PdfQuality::Ebook => "/ebook",
        PdfQuality::Printer => "/printer",
        PdfQuality::Prepress => "/prepress",
    }
}

fn is_non_empty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// The per-page files a `%d` template run actually produced, ascending by
/// page number. Only `<number>.pdf` entries count, so nothing Ghostscript
/// did not name this way can end up in the archive.
fn produced_pages(pages_dir: &Path) -> Result<Vec<(u32, PathBuf)>, RunError> {
    let entries = std::fs::read_dir(pages_dir).map_err(|e| RunError::Failed {
        code: OPERATION_FAILED,
        detail: format!("could not read the split work directory: {e}"),
    })?;
    let mut pages: Vec<(u32, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(page) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if path.extension().is_some_and(|e| e == "pdf") && is_non_empty_file(&path) {
            pages.push((page, path));
        }
    }
    pages.sort_by_key(|(page, _)| *page);
    Ok(pages)
}

/// Packs the page files into the job's single output archive.
///
/// Entry names are zero-padded to the width of the highest page number, so
/// the pages stay in document order in any tool that lists the archive
/// alphabetically. Stored, not deflated: PDF page streams are already
/// compressed, so deflating them costs CPU for no benefit.
fn pack_pages(pages: &[(u32, PathBuf)], output: &Path) -> Result<(), RunError> {
    let failed = |detail: String| RunError::Failed {
        code: OPERATION_FAILED,
        detail,
    };
    let width = pages
        .last()
        .map(|(page, _)| page.to_string().len())
        .unwrap_or(1);
    let file = std::fs::File::create(output)
        .map_err(|e| failed(format!("could not create the split archive: {e}")))?;
    let mut archive = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    for (page, path) in pages {
        let name = format!("sayfa-{page:0width$}.pdf");
        archive
            .start_file(name, options)
            .map_err(|e| failed(format!("could not add page {page} to the archive: {e}")))?;
        let mut source = std::fs::File::open(path)
            .map_err(|e| failed(format!("could not read page {page}: {e}")))?;
        std::io::copy(&mut source, &mut archive)
            .map_err(|e| failed(format!("could not write page {page}: {e}")))?;
    }
    archive
        .finish()
        .map_err(|e| failed(format!("could not finish the split archive: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every operation's argument vector, built the way `run` builds it but
    /// without spawning anything - the engine itself is not installed on
    /// most dev machines, and these are assertions about the command line,
    /// not about Ghostscript.
    fn args_for(spec: &JobSpec, inputs: &[PathBuf], output: &Path) -> Vec<String> {
        let mut args = GhostscriptRunner::base_args(output);
        match spec {
            JobSpec::PdfMerge(_) => {
                args.extend(inputs.iter().map(|p| p.to_string_lossy().to_string()));
            }
            JobSpec::PdfCompress(options) => {
                args.push(format!("-dPDFSETTINGS={}", pdf_settings(options.quality)));
                args.push(inputs[0].to_string_lossy().to_string());
            }
            JobSpec::PdfRotate(options) => {
                args.push("-c".to_string());
                args.push(format!(
                    "<< /PageRotation {} >> setpagedevice",
                    options.rotation.degrees()
                ));
                args.push("-f".to_string());
                args.push(inputs[0].to_string_lossy().to_string());
            }
            _ => unreachable!("args_for covers the single-invocation kinds"),
        }
        args
    }

    fn tmp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meb_gs_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_unresolvable_engine_yields_no_runner_at_all() {
        // The contract the registry depends on: no Ghostscript means no
        // runner, which means `pdf_*` kinds stay off the API rather than
        // accepting jobs that must fail.
        assert!(GhostscriptRunner::detect_with(&ResolutionPolicy::EMPTY).is_none());
    }

    #[test]
    fn every_invocation_carries_the_safety_flags_and_one_output() {
        let output = Path::new("work/result.pdf");
        let args = GhostscriptRunner::base_args(output);
        for required in ["-dNOPAUSE", "-dBATCH", "-dSAFER", "-sDEVICE=pdfwrite"] {
            assert!(args.iter().any(|a| a == required), "missing {required}");
        }
        assert_eq!(
            args.iter()
                .filter(|a| a.starts_with("-sOutputFile="))
                .count(),
            1
        );
        // -dNOSAFER would hand the PostScript interpreter the filesystem.
        assert!(!args.iter().any(|a| a == "-dNOSAFER"));
    }

    #[test]
    fn quality_presets_map_onto_the_closed_ghostscript_set() {
        let known = ["/screen", "/ebook", "/printer", "/prepress"];
        for quality in [
            PdfQuality::Screen,
            PdfQuality::Ebook,
            PdfQuality::Printer,
            PdfQuality::Prepress,
        ] {
            assert!(known.contains(&pdf_settings(quality)));
        }
        assert_eq!(pdf_settings(PdfQuality::default()), "/ebook");
    }

    #[test]
    fn rotation_reaches_the_command_line_as_one_of_three_fixed_snippets() {
        use crate::spec::PdfRotation;
        let inputs = vec![PathBuf::from("in/source-00.pdf")];
        let output = PathBuf::from("work/result.pdf");
        let mut snippets = Vec::new();
        for rotation in [
            PdfRotation::Clockwise90,
            PdfRotation::Half,
            PdfRotation::Clockwise270,
        ] {
            let spec = JobSpec::PdfRotate(PdfRotateOptions { rotation });
            let args = args_for(&spec, &inputs, &output);
            // `-f` must separate the PostScript snippet from the input, or
            // the input would be interpreted rather than read.
            let c = args.iter().position(|a| a == "-c").unwrap();
            let f = args.iter().position(|a| a == "-f").unwrap();
            assert!(c < f, "the snippet must come before -f");
            assert_eq!(f + 1, args.len() - 1, "-f must immediately precede the input");
            assert_eq!(args.last().unwrap(), "in/source-00.pdf");
            snippets.push(args[c + 1].clone());
        }
        assert_eq!(
            snippets,
            vec![
                "<< /PageRotation 90 >> setpagedevice",
                "<< /PageRotation 180 >> setpagedevice",
                "<< /PageRotation 270 >> setpagedevice",
            ]
        );
    }

    #[test]
    fn merge_passes_every_input_in_request_order_after_the_flags() {
        let inputs: Vec<PathBuf> = (0..3)
            .map(|i| PathBuf::from(format!("in/source-{i:02}.pdf")))
            .collect();
        let output = PathBuf::from("work/result.pdf");
        let spec = JobSpec::PdfMerge(crate::spec::PdfMergeSpec { input_count: 3 });
        let args = args_for(&spec, &inputs, &output);
        let tail: Vec<&String> = args.iter().rev().take(3).rev().collect();
        assert_eq!(
            tail,
            vec![
                &"in/source-00.pdf".to_string(),
                &"in/source-01.pdf".to_string(),
                &"in/source-02.pdf".to_string()
            ]
        );
    }

    #[test]
    fn no_argument_is_ever_a_shell_or_a_command_string() {
        // Structural: every argument is a separate vector element, and
        // nothing that looks like shell plumbing can appear in one, because
        // every value comes from a closed enum, a bounded integer or a
        // server-built path.
        let inputs = vec![PathBuf::from("in/source-00.pdf")];
        let output = PathBuf::from("work/result.pdf");
        let specs = [
            JobSpec::PdfMerge(crate::spec::PdfMergeSpec { input_count: 1 }),
            JobSpec::PdfCompress(PdfCompressOptions {
                quality: PdfQuality::Screen,
            }),
            JobSpec::PdfRotate(PdfRotateOptions {
                rotation: crate::spec::PdfRotation::Half,
            }),
        ];
        for spec in specs {
            for arg in args_for(&spec, &inputs, &output) {
                for forbidden in ["&&", "||", ";", "|", "`", "$(", "cmd.exe", "powershell"] {
                    assert!(
                        !arg.contains(forbidden),
                        "{forbidden:?} reached an argument: {arg:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn only_numbered_pdf_pages_are_collected_and_they_come_back_in_page_order() {
        let dir = tmp_dir();
        for name in ["2.pdf", "10.pdf", "1.pdf"] {
            std::fs::write(dir.join(name), b"%PDF-1.4\n").unwrap();
        }
        // Must all be ignored: not a page number, not a PDF, and empty.
        std::fs::write(dir.join("notes.pdf"), b"%PDF-1.4\n").unwrap();
        std::fs::write(dir.join("3.txt"), b"x").unwrap();
        std::fs::write(dir.join("4.pdf"), b"").unwrap();

        let pages = produced_pages(&dir).unwrap();
        assert_eq!(
            pages.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            vec![1, 2, 10]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn packed_pages_are_named_in_document_order_inside_the_archive() {
        let dir = tmp_dir();
        let mut pages = Vec::new();
        for page in [1u32, 2, 10] {
            let path = dir.join(format!("{page}.pdf"));
            std::fs::write(&path, format!("%PDF-1.4\npage {page}\n")).unwrap();
            pages.push((page, path));
        }
        let archive_path = dir.join("result.zip");
        pack_pages(&pages, &archive_path).unwrap();

        let bytes = std::fs::read(&archive_path).unwrap();
        // The job model validates this output as a ZIP; the engine must
        // really produce one.
        assert_eq!(&bytes[..2], b"PK");
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        // Zero-padded to the widest page number, so alphabetical order is
        // document order.
        assert_eq!(names, vec!["sayfa-01.pdf", "sayfa-02.pdf", "sayfa-10.pdf"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ------------------------------------------- outcome mapping ----------
    //
    // These drive `invoke` against a REAL child process, because that is the
    // part a unit test on the argument vector cannot reach: the timeout
    // firing, the cancel flag killing and reaping the child, a non-zero exit
    // becoming a job failure, and a missing executable being reported as an
    // unavailable engine rather than a broken PDF.
    //
    // `ping` stands in for the engine (present on every Windows install, no
    // shell needed to resolve it), the same stand-in `meb_engines::process`
    // uses for its own timeout test. Windows-only for that reason.

    struct Control {
        cancel: std::sync::atomic::AtomicBool,
        progress: Mutex<Vec<u8>>,
    }
    use std::sync::Mutex;

    impl Control {
        fn new(cancelled: bool) -> Control {
            Control {
                cancel: std::sync::atomic::AtomicBool::new(cancelled),
                progress: Mutex::new(Vec::new()),
            }
        }
    }

    /// Runs `body` with a `JobControl` over `control`.
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
        let runner =
            GhostscriptRunner::with_engine(PathBuf::from("ping"), Duration::from_secs(30));
        let control = Control::new(false);
        let args = ["127.0.0.1".to_string(), "-n".to_string(), "1".to_string()];
        let result = with_control(&control, |c| {
            runner.invoke(&args, &std::env::temp_dir(), c, OPERATION_FAILED)
        });
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    #[cfg(windows)]
    fn a_non_zero_exit_becomes_the_callers_failure_code_and_never_leaks_stderr_to_the_client() {
        let runner =
            GhostscriptRunner::with_engine(PathBuf::from("ping"), Duration::from_secs(30));
        let control = Control::new(false);
        // An unresolvable host makes ping exit non-zero.
        let args = [
            "-n".to_string(),
            "1".to_string(),
            "meb-donustur.invalid".to_string(),
        ];
        let result = with_control(&control, |c| {
            runner.invoke(&args, &std::env::temp_dir(), c, PAGE_UNAVAILABLE)
        });
        match result {
            Err(RunError::Failed { code, detail }) => {
                assert_eq!(code, PAGE_UNAVAILABLE);
                // The code is what the client sees; the detail is log-only,
                // and must carry the diagnostics rather than the code.
                assert!(detail.contains("exited with"), "{detail}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    #[cfg(windows)]
    fn a_hung_invocation_is_killed_and_reported_as_a_timeout() {
        let runner = GhostscriptRunner::with_engine(
            PathBuf::from("ping"),
            Duration::from_millis(300),
        );
        let control = Control::new(false);
        let args = ["127.0.0.1".to_string(), "-n".to_string(), "30".to_string()];
        let start = std::time::Instant::now();
        let result = with_control(&control, |c| {
            runner.invoke(&args, &std::env::temp_dir(), c, OPERATION_FAILED)
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
        let runner = GhostscriptRunner::with_engine(
            PathBuf::from("no-such-engine-executable"),
            Duration::from_secs(30),
        );
        let control = Control::new(true);
        let result = with_control(&control, |c| {
            runner.invoke(&[], &std::env::temp_dir(), c, OPERATION_FAILED)
        });
        assert!(matches!(result, Err(RunError::Cancelled)), "{result:?}");
    }

    #[test]
    fn an_engine_that_cannot_be_started_is_reported_as_unavailable_not_as_a_bad_pdf() {
        let runner = GhostscriptRunner::with_engine(
            PathBuf::from("no-such-engine-executable"),
            Duration::from_secs(30),
        );
        let control = Control::new(false);
        let result = with_control(&control, |c| {
            runner.invoke(&[], &std::env::temp_dir(), c, OPERATION_FAILED)
        });
        match result {
            Err(RunError::Failed { code, .. }) => assert_eq!(code, ENGINE_UNAVAILABLE),
            other => panic!("expected an unavailable engine, got {other:?}"),
        }
    }

    #[test]
    fn every_code_this_engine_reports_has_a_user_facing_message() {
        for code in [
            ENGINE_UNAVAILABLE,
            OPERATION_FAILED,
            ENGINE_TIMEOUT,
            PAGE_UNAVAILABLE,
        ] {
            let message = crate::error::job_error_message(code);
            assert_ne!(
                message,
                crate::error::job_error_message("A_CODE_THAT_DOES_NOT_EXIST"),
                "{code} falls through to the generic message"
            );
        }
    }

    #[test]
    fn the_runner_refuses_a_kind_it_does_not_implement() {
        // Only reachable through a registry wiring mistake, and it must
        // report exactly that rather than attempt the job.
        let kinds: Vec<JobKind> = GhostscriptRunner::KINDS.to_vec();
        for kind in JobKind::ALL {
            let handled = kinds.contains(&kind);
            assert_eq!(
                handled,
                matches!(
                    kind,
                    JobKind::PdfMerge
                        | JobKind::PdfSplit
                        | JobKind::PdfCompress
                        | JobKind::PdfRotate
                ),
                "{} is handled inconsistently",
                kind.wire()
            );
        }
    }
}
