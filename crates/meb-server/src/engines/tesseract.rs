//! Tesseract, behind the generalized `ConversionRunner` trait.
//!
//! Backs one job kind, `pdf_ocr`: a scanned PDF in, the same PDF with a
//! searchable text layer out. The pipeline is the one the desktop app has
//! been shipping (`src-tauri/src/commands.rs`), with the same three steps:
//!
//! 1. Ghostscript renders each page to a 300 dpi PNG.
//! 2. Tesseract recognizes each PNG and writes it back out as a one-page
//!    PDF whose invisible text layer sits over the original image.
//! 3. Ghostscript writes those pages into the single output PDF.
//!
//! So this runner needs TWO engines, and is availability-gated on BOTH:
//! `detect` yields nothing unless Ghostscript and Tesseract both resolve,
//! which keeps `pdf_ocr` off the API entirely on a machine that has only
//! one of them (rather than accepting jobs that fail halfway through).
//! Steps 1 and 3 are Ghostscript's own `pub(crate)` helpers, so every
//! Ghostscript argument vector in this server is still built in one file.
//!
//! Page by page, not in one pass, for two reasons: there is real progress
//! to report (pages recognized out of pages rendered), and the per-page
//! timeout is a bound on one page's recognition rather than on a whole
//! document - OCR is the slowest thing this server does, and a single
//! budget for a 200-page scan would have to be so large as to be no bound
//! at all.
//!
//! The recognition language comes from a closed enum (`spec::OcrLanguage`),
//! because it becomes both a command-line argument and a `traineddata` file
//! name. No client string reaches either.

use crate::runner::{ConversionRunner, JobControl, RunError, RunRequest};
use crate::spec::{JobKind, JobSpec, OcrLanguage, PdfOcrOptions, MAX_PDF_PAGE};
use meb_engines::process::{self, CancellableRun, ProcessEnd};
use meb_engines::resolver::{self, ResolutionPolicy};
use meb_engines::EngineId;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Wall-clock budget for recognizing ONE page. An initial engineering
/// default: a dense A4 scan at 300 dpi takes a few seconds per page on a
/// modern core and well under a minute on a slow one, so this is generous
/// by a wide margin while still bounding a pathological page.
const PAGE_TIMEOUT: Duration = Duration::from_secs(120);

/// Stable error codes this engine reports. `error::job_error_message` has
/// the user-facing Turkish text for each.
const ENGINE_UNAVAILABLE: &str = "OCR_ENGINE_UNAVAILABLE";
const OCR_FAILED: &str = "OCR_FAILED";
const ENGINE_TIMEOUT: &str = "OCR_ENGINE_TIMEOUT";
const LANGUAGE_UNAVAILABLE: &str = "OCR_LANGUAGE_UNAVAILABLE";
const NO_PAGES: &str = "OCR_NO_PAGES";

/// Subdirectory of the job's `work/` that the rendered page images and the
/// per-page recognized PDFs live in. Separate from the output file, so the
/// final document can never be assembled out of itself.
const PAGES_DIR: &str = "ocr";

/// Suffix of a recognized page, appended to the page number. Tesseract is
/// given a base name and adds `.pdf` itself.
const RECOGNIZED_SUFFIX: &str = "-ocr";

/// The Tesseract "config" that makes it emit a searchable PDF rather than
/// a text file. It is a file name Tesseract looks up in its own tessdata
/// directory, not a free-form option.
const PDF_CONFIG: &str = "pdf";

/// Where Tesseract's language data lives, relative to the executable. Both
/// a bundled engine and the usual system installs put it here.
const TESSDATA_DIR: &str = "tessdata";

pub struct TesseractRunner {
    /// Steps 1 and 3. Held rather than re-resolved so an OCR job cannot
    /// start with a Ghostscript that has gone missing since startup.
    ghostscript: crate::engines::GhostscriptRunner,
    engine: resolver::ResolvedEngine,
    /// The language-data directory, when one is next to the executable.
    /// Passed explicitly so recognition does not depend on `TESSDATA_PREFIX`
    /// being set in the server's environment - which the process runner's
    /// env handling would not pass on anyway.
    tessdata: Option<PathBuf>,
    timeout: Duration,
}

impl TesseractRunner {
    /// The job kinds this engine backs.
    pub const KINDS: [JobKind; 1] = [JobKind::PdfOcr];

    /// Resolves BOTH engines once, at startup. `None` if either is missing:
    /// an OCR job needs the renderer as much as the recognizer, so half an
    /// OCR pipeline is the same as none.
    pub fn detect() -> Option<TesseractRunner> {
        Self::detect_with(&ResolutionPolicy::BUNDLED_THEN_PATH)
    }

    /// `detect` with an explicit policy - the seam the tests use to prove
    /// that an unresolvable engine yields no runner at all.
    pub fn detect_with(policy: &ResolutionPolicy) -> Option<TesseractRunner> {
        let ghostscript = crate::engines::GhostscriptRunner::detect_with(policy)?;
        let engine = resolver::resolve_with(EngineId::Tesseract, policy).ok()?;
        let tessdata = tessdata_dir(&engine.path);
        Some(TesseractRunner {
            ghostscript,
            engine,
            tessdata,
            timeout: PAGE_TIMEOUT,
        })
    }

    /// A runner bound to arbitrary executables. A TEST seam: it is how the
    /// pipeline's outcome mapping is exercised on a machine that has
    /// neither engine installed, by standing a well-known system binary in
    /// for them.
    #[cfg(test)]
    fn with_engines(
        ghostscript: crate::engines::GhostscriptRunner,
        tesseract: PathBuf,
        timeout: Duration,
    ) -> TesseractRunner {
        TesseractRunner {
            ghostscript,
            engine: resolver::ResolvedEngine {
                id: EngineId::Tesseract,
                path: tesseract,
                tier: resolver::EngineTier::System,
            },
            tessdata: None,
            timeout,
        }
    }

    fn ocr(
        &self,
        options: &PdfOcrOptions,
        request: &RunRequest<'_>,
        control: &JobControl<'_>,
    ) -> Result<(), RunError> {
        let work_dir = work_dir(request)?;
        let pages_dir = work_dir.join(PAGES_DIR);

        // Step 1: the scan becomes images, because Tesseract reads images
        // and not PDFs.
        let images = self.ghostscript.render_pages_to_png(
            request.input(),
            &pages_dir,
            work_dir,
            control,
            NO_PAGES,
        )?;
        if images.is_empty() {
            return Err(RunError::Failed {
                code: NO_PAGES,
                detail: "no page of the document could be rendered for recognition".to_string(),
            });
        }
        if images.len() > MAX_PDF_PAGE as usize {
            return Err(RunError::Failed {
                code: OCR_FAILED,
                detail: format!(
                    "the document has more than the {MAX_PDF_PAGE} pages this server will OCR"
                ),
            });
        }

        // Step 2: one recognition per page, which is also where the only
        // honest progress of this job comes from.
        let total = images.len();
        let mut recognized: Vec<PathBuf> = Vec::with_capacity(total);
        for (done, (page, image)) in images.iter().enumerate() {
            let base = pages_dir.join(format!("{page}{RECOGNIZED_SUFFIX}"));
            self.recognize(image, &base, options.language, work_dir, control)?;
            let produced = base.with_extension("pdf");
            if !is_non_empty_file(&produced) {
                return Err(RunError::Failed {
                    code: OCR_FAILED,
                    detail: format!("recognition of page {page} produced no output"),
                });
            }
            recognized.push(produced);
            // Pages recognized out of pages rendered. The assembly that
            // follows is the remaining tenth.
            control.report_progress(((done + 1) * 90 / total) as u8);
        }

        // Step 3: the pages become one document again. A single page is
        // already that document, so it is moved rather than re-written -
        // one less pass over the file, and nothing to re-compress.
        if let [only] = recognized.as_slice() {
            move_onto(only, request.output)?;
        } else {
            self.ghostscript.combine_pdfs(
                &recognized,
                request.output,
                work_dir,
                control,
                OCR_FAILED,
            )?;
        }
        control.report_progress(100);
        Ok(())
    }

    /// Recognizes one page image, writing `<base>.pdf`.
    fn recognize(
        &self,
        image: &Path,
        base: &Path,
        language: OcrLanguage,
        work_dir: &Path,
        control: &JobControl<'_>,
    ) -> Result<(), RunError> {
        if control.is_cancelled() {
            return Err(RunError::Cancelled);
        }
        let args = recognize_args(image, base, language, self.tessdata.as_deref());
        let result = process::run_cancellable(
            &self.engine,
            &args,
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
                detail: "tesseract exceeded its per-page timeout and was killed".to_string(),
            }),
            ProcessEnd::SpawnFailed => Err(RunError::Failed {
                code: ENGINE_UNAVAILABLE,
                detail: format!("tesseract could not be started: {}", result.stderr_tail),
            }),
            ProcessEnd::Failed(status) => Err(RunError::Failed {
                code: failure_code(&result.stderr_tail),
                detail: format!(
                    "tesseract exited with {}: {}",
                    status
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "no status".to_string()),
                    result.stderr_tail
                ),
            }),
        }
    }
}

impl ConversionRunner for TesseractRunner {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        match request.spec {
            JobSpec::PdfOcr(options) => self.ocr(options, request, control),
            JobSpec::ImageConvert(_)
            | JobSpec::PdfMerge(_)
            | JobSpec::PdfSplit(_)
            | JobSpec::PdfCompress(_)
            | JobSpec::PdfRotate(_)
            | JobSpec::PdfWatermark(_)
            | JobSpec::OfficeConvert(_)
            | JobSpec::PdfToOffice(_)
            | JobSpec::PdfProtect(_)
            | JobSpec::PdfUnlock(_)
            | JobSpec::PdfMetadataStrip
            | JobSpec::PdfPageNumbers(_)
            | JobSpec::PdfDeletePages(_)
            | JobSpec::PdfReorderPages(_)
            | JobSpec::PdfExtractText => Err(RunError::wrong_kind("TesseractRunner")),
        }
    }
}

/// The argument vector for one page.
///
/// Tesseract's own order: the image, then the OUTPUT BASE name (it appends
/// the extension itself), then options, then the output "config" last.
/// Everything here is either a server-built path or a value from the closed
/// language enum - a free function so the vector can be checked by a test
/// without starting anything.
fn recognize_args(
    image: &Path,
    base: &Path,
    language: OcrLanguage,
    tessdata: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        image.to_string_lossy().to_string(),
        base.to_string_lossy().to_string(),
    ];
    if let Some(dir) = tessdata {
        args.push("--tessdata-dir".to_string());
        args.push(dir.to_string_lossy().to_string());
    }
    args.push("-l".to_string());
    args.push(language.code().to_string());
    args.push(PDF_CONFIG.to_string());
    args
}

/// Which failure a non-zero exit was.
///
/// Missing language data is the one OCR failure a user can act on (install
/// the `traineddata`, or ask for a language the server has), and Tesseract
/// does not have an exit code for it - so it is recognized from the message
/// it prints. A best-effort refinement: anything unrecognized stays the
/// generic failure, so a changed message costs a better error code and
/// never a wrong one.
fn failure_code(stderr: &str) -> &'static str {
    const LANGUAGE_MARKERS: [&str; 3] = [
        "Failed loading language",
        "Error opening data file",
        "Could not initialize tesseract",
    ];
    if LANGUAGE_MARKERS.iter().any(|m| stderr.contains(m)) {
        LANGUAGE_UNAVAILABLE
    } else {
        OCR_FAILED
    }
}

/// The language-data directory next to the executable, if there is one.
fn tessdata_dir(executable: &Path) -> Option<PathBuf> {
    let candidate = executable.parent()?.join(TESSDATA_DIR);
    candidate.is_dir().then_some(candidate)
}

/// The job's own work directory - the only place this engine writes, and
/// the cwd every invocation gets.
fn work_dir<'a>(request: &RunRequest<'a>) -> Result<&'a Path, RunError> {
    request.output.parent().ok_or(RunError::Failed {
        code: OCR_FAILED,
        detail: "the job output path has no parent directory".to_string(),
    })
}

fn is_non_empty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Puts the single recognized page where the job promised its output.
/// A rename within the job's own workspace, with a copy as the fallback.
fn move_onto(produced: &Path, output: &Path) -> Result<(), RunError> {
    if std::fs::rename(produced, output).is_ok() {
        return Ok(());
    }
    std::fs::copy(produced, output).map_err(|e| RunError::Failed {
        code: OCR_FAILED,
        detail: format!("the recognized page could not be moved to the output: {e}"),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::GhostscriptRunner;
    use crate::spec::{CreateJobRequest, JobSpec};
    use meb_core::format::SourceFormat;
    use std::sync::atomic::AtomicBool;

    fn ocr_spec(options: serde_json::Value) -> JobSpec {
        let request = CreateJobRequest {
            kind: JobKind::PdfOcr.wire().to_string(),
            file_id: Some("f".to_string()),
            file_ids: None,
            output_format: None,
            options: if options.is_null() { None } else { Some(options) },
        };
        JobSpec::from_request(JobKind::PdfOcr, &request, &[SourceFormat::Pdf]).unwrap()
    }

    fn run(runner: &TesseractRunner, spec: &JobSpec, dir: &Path) -> Result<(), RunError> {
        let inputs = vec![dir.join("source-00.pdf")];
        std::fs::write(&inputs[0], b"%PDF-1.7\ntrailer\n%%EOF\n").unwrap();
        let output = dir.join("result.pdf");
        let request = RunRequest {
            inputs: &inputs,
            output: &output,
            spec,
        };
        let cancel = AtomicBool::new(false);
        let progress = |_: u8| {};
        let control = JobControl {
            cancel: &cancel,
            progress: &progress,
        };
        runner.run(&request, &control)
    }

    fn work_space() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meb_ocr_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_unresolvable_engine_yields_no_runner_at_all() {
        // The empty policy finds nothing anywhere, which is what a machine
        // without the tools looks like. No runner means `pdf_ocr` is absent
        // from the registry and refused by the API, rather than accepted
        // and then failed.
        assert!(TesseractRunner::detect_with(&ResolutionPolicy::EMPTY).is_none());
    }

    #[test]
    fn ocr_needs_both_engines_so_either_one_missing_is_no_runner() {
        // Both tiers of the real policy are consulted for BOTH ids, so this
        // asserts the conjunction rather than a fact about this machine:
        // the runner exists exactly when both engines resolve.
        let policy = ResolutionPolicy::BUNDLED_THEN_PATH;
        let ghostscript = GhostscriptRunner::detect_with(&policy).is_some();
        let tesseract = resolver::resolve_with(EngineId::Tesseract, &policy).is_ok();
        assert_eq!(
            TesseractRunner::detect_with(&policy).is_some(),
            ghostscript && tesseract,
            "ghostscript={ghostscript}, tesseract={tesseract}"
        );
    }

    #[test]
    fn the_argument_vector_names_the_language_and_asks_for_a_pdf() {
        let args = recognize_args(
            Path::new("/job/work/ocr/1.png"),
            Path::new("/job/work/ocr/1-ocr"),
            OcrLanguage::TurkishAndEnglish,
            None,
        );
        // The image, then the OUTPUT BASE - not the output file: Tesseract
        // appends the extension itself, and giving it `1-ocr.pdf` would
        // produce `1-ocr.pdf.pdf`.
        assert_eq!(args[0], "/job/work/ocr/1.png");
        assert_eq!(args[1], "/job/work/ocr/1-ocr");
        assert!(!args[1].ends_with(".pdf"));
        // The language, from the closed enum, and the config that makes the
        // output a searchable PDF - which Tesseract requires LAST.
        let language = args.iter().position(|a| a == "-l").unwrap();
        assert_eq!(args[language + 1], "tur+eng");
        assert_eq!(args.last().unwrap(), "pdf");

        // The language data directory is passed only when there is one.
        assert!(!args.iter().any(|a| a == "--tessdata-dir"));
        let with_data = recognize_args(
            Path::new("1.png"),
            Path::new("1-ocr"),
            OcrLanguage::Turkish,
            Some(Path::new("/engines/tesseract/tessdata")),
        );
        let at = with_data
            .iter()
            .position(|a| a == "--tessdata-dir")
            .expect("the data directory was not passed");
        assert_eq!(with_data[at + 1], "/engines/tesseract/tessdata");
        // Still last, whatever else was added.
        assert_eq!(with_data.last().unwrap(), "pdf");
    }

    #[test]
    fn the_default_language_is_turkish_and_english() {
        // A Turkish school's documents are mostly Turkish with English
        // fragments, and Tesseract handles that better when told both than
        // when told either.
        assert_eq!(OcrLanguage::default().code(), "tur+eng");
        let JobSpec::PdfOcr(options) = ocr_spec(serde_json::Value::Null) else {
            panic!("expected an OCR spec");
        };
        assert_eq!(options.language.code(), "tur+eng");
    }

    #[test]
    fn no_argument_is_ever_a_shell_or_a_command_string() {
        // The process runner takes an argument array and no shell, and this
        // keeps the engine honest about it: every argument is one value.
        for language in [
            OcrLanguage::Turkish,
            OcrLanguage::English,
            OcrLanguage::TurkishAndEnglish,
        ] {
            let args = recognize_args(
                Path::new("1.png"),
                Path::new("1-ocr"),
                language,
                Some(Path::new("/engines/tessdata")),
            );
            for arg in &args {
                assert!(
                    !arg.contains("&&")
                        && !arg.contains('|')
                        && !arg.contains(';')
                        && !arg.contains('\n'),
                    "{arg} looks like a command line"
                );
            }
            // And the language is one of exactly three known values.
            assert!(args.contains(&language.code().to_string()));
        }
    }

    #[test]
    fn missing_language_data_is_told_apart_from_a_failed_page() {
        assert_eq!(
            failure_code("Error opening data file /usr/share/tessdata/tur.traineddata"),
            LANGUAGE_UNAVAILABLE
        );
        assert_eq!(
            failure_code("Failed loading language 'tur'"),
            LANGUAGE_UNAVAILABLE
        );
        // Anything unrecognized stays the generic failure: a changed
        // message must cost a better code, never a wrong one.
        assert_eq!(failure_code("Image too large to process"), OCR_FAILED);
        assert_eq!(failure_code(""), OCR_FAILED);
    }

    #[test]
    fn the_runner_handles_exactly_the_one_document_kind() {
        assert_eq!(TesseractRunner::KINDS, [JobKind::PdfOcr]);
        let dir = work_space();
        let runner = TesseractRunner::with_engines(
            GhostscriptRunner::with_engine(PathBuf::from("ping"), Duration::from_secs(5)),
            PathBuf::from("ping"),
            Duration::from_secs(5),
        );
        // A kind this runner does not back is a registry wiring mistake.
        let foreign = {
            let request = CreateJobRequest {
                kind: JobKind::PdfCompress.wire().to_string(),
                file_id: Some("f".to_string()),
                file_ids: None,
                output_format: None,
                options: None,
            };
            JobSpec::from_request(JobKind::PdfCompress, &request, &[SourceFormat::Pdf]).unwrap()
        };
        assert!(matches!(
            run(&runner, &foreign, &dir),
            Err(RunError::Failed { code: "INTERNAL_ERROR", .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ping` stands in for both engines: it exits 0 on Windows without
    /// writing anything, which is exactly the "the renderer claimed success
    /// but produced no page" case - the one outcome that must not be
    /// reported as a successful OCR.
    #[cfg(windows)]
    #[test]
    fn a_render_that_produces_no_page_is_not_a_success() {
        let dir = work_space();
        let runner = TesseractRunner::with_engines(
            GhostscriptRunner::with_engine(PathBuf::from("ping"), Duration::from_secs(20)),
            PathBuf::from("ping"),
            Duration::from_secs(20),
        );
        let spec = ocr_spec(serde_json::Value::Null);
        match run(&runner, &spec, &dir) {
            Err(RunError::Failed { code, .. }) => assert_eq!(code, NO_PAGES),
            other => panic!("expected a no-pages failure, got {other:?}"),
        }
        assert!(
            !dir.join("result.pdf").exists(),
            "a failed OCR still published an output"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_engine_that_cannot_be_started_is_reported_as_unavailable() {
        let dir = work_space();
        let runner = TesseractRunner::with_engines(
            GhostscriptRunner::with_engine(
                PathBuf::from("meb-no-such-engine"),
                Duration::from_secs(5),
            ),
            PathBuf::from("meb-no-such-engine"),
            Duration::from_secs(5),
        );
        let spec = ocr_spec(serde_json::Value::Null);
        match run(&runner, &spec, &dir) {
            // The render step is first, so it is the one that reports - with
            // its own code, since what failed is the page rendering.
            Err(RunError::Failed { code, .. }) => {
                assert_eq!(code, "PDF_ENGINE_UNAVAILABLE")
            }
            other => panic!("expected an unavailable engine, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_job_stops_before_recognizing_anything() {
        let dir = work_space();
        let runner = TesseractRunner::with_engines(
            GhostscriptRunner::with_engine(PathBuf::from("ping"), Duration::from_secs(5)),
            PathBuf::from("ping"),
            Duration::from_secs(5),
        );
        let spec = ocr_spec(serde_json::Value::Null);
        let inputs = vec![dir.join("source-00.pdf")];
        std::fs::write(&inputs[0], b"%PDF-1.7\ntrailer\n%%EOF\n").unwrap();
        let output = dir.join("result.pdf");
        let request = RunRequest {
            inputs: &inputs,
            output: &output,
            spec: &spec,
        };
        let cancel = AtomicBool::new(true);
        let progress = |_: u8| {};
        let control = JobControl {
            cancel: &cancel,
            progress: &progress,
        };
        assert!(matches!(
            runner.run(&request, &control),
            Err(RunError::Cancelled)
        ));
        assert!(!output.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_code_this_engine_reports_has_a_user_facing_message() {
        for code in [
            ENGINE_UNAVAILABLE,
            OCR_FAILED,
            ENGINE_TIMEOUT,
            LANGUAGE_UNAVAILABLE,
            NO_PAGES,
        ] {
            assert_ne!(
                crate::error::job_error_message(code),
                crate::error::job_error_message("A_CODE_THAT_DOES_NOT_EXIST"),
                "{code} falls through to the generic message"
            );
        }
    }
}
