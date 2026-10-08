//! What a job is asked to do - the kind-agnostic seam between the HTTP API
//! and the engines.
//!
//! - `JobKind` is the CLOSED set of operations this API exposes. A client
//!   string becomes a `JobKind` only through `JobKind::parse`; there is no
//!   other way, so a request cannot name an operation the server does not
//!   publish.
//! - `JobSpec` is the validated, typed description of ONE such operation:
//!   one variant per kind, built once (`JobSpec::from_request`) and then
//!   trusted by the job model, the worker and the engine. Everything the
//!   kind-agnostic layers need from it - the output extension, the MIME
//!   type, the download name, and what a valid output looks like - is a
//!   method here, which is why `jobs.rs` and `worker.rs` contain no
//!   image-specific (or, later, PDF/Office-specific) code.
//!
//! Adding a kind is therefore: a `JobKind` variant, a spec variant with its
//! own options type, the arms of the methods below, and a runner registered
//! for it (`runner::RunnerRegistry`). Nothing else changes.
//!
//! Option types are `deny_unknown_fields` without exception: a client must
//! not be able to smuggle engine parameters (paths, codecs, filters, GPU
//! flags) past this boundary.

use crate::error::ApiError;
use meb_core::format::{Container, DocumentClass, OfficeFormat, SourceFormat};
use meb_core::image::{NativeImageFormat, MAX_DECODED_PIXELS, MAX_DIMENSION};
use serde::Deserialize;
use std::path::Path;

/// Leading bytes read when a kind identifies its output by signature.
const OUTPUT_HEAD_BYTES: usize = 64;

/// How many inputs a kind takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// Exactly one input (every kind today).
    One,
    /// One or more, in the order the request named them, up to `MAX_INPUTS`
    /// - PDF merge is the first of these.
    OneOrMore,
}

/// Cap on a multi-input job. Bounds both the work one request can ask for
/// and the size of the workspace it stages.
pub const MAX_INPUTS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// Raster image conversion/resize/recompression by the in-process
    /// native pipeline. Covers image optimization: same format in and out,
    /// with a quality (and optionally a size) that shrinks the file.
    ImageConvert,
    /// Several PDFs into one, in the order given.
    PdfMerge,
    /// Selected pages out of one PDF, delivered as a ZIP of single pages.
    PdfSplit,
    /// One PDF re-written at a lower quality preset.
    PdfCompress,
    /// One PDF with every page rotated by a right angle.
    PdfRotate,
    /// One PDF with a text watermark drawn over every page.
    PdfWatermark,
    /// One PDF with a recognized text layer added (the scan itself is
    /// unchanged; the result is searchable).
    PdfOcr,
    /// One Office document converted to PDF or to another Office format.
    OfficeConvert,
    /// One PDF reconstructed back into an editable text document.
    ///
    /// Deliberately a kind of its own rather than a direction of
    /// `OfficeConvert`, because it is a different problem: Office -> PDF is
    /// rendering (the engine already knows the layout and prints it), while
    /// PDF -> Office is reconstruction (inferring paragraphs and tables out
    /// of a format that only records fixed-position drawing operations).
    /// There is no general solution to it, so this kind is EXPERIMENTAL -
    /// see `JobKind::is_experimental`. The desktop app draws the same
    /// boundary in `src-tauri/src/reconstruction.rs`.
    PdfToOffice,
    /// One PDF re-written with a password required to open it.
    PdfProtect,
    /// One PDF re-written with its password protection removed.
    PdfUnlock,
    /// One PDF re-written without any document metadata.
    PdfMetadataStrip,
    /// One PDF with a page number drawn on every page.
    PdfPageNumbers,
    /// One PDF with the named pages removed.
    PdfDeletePages,
    /// One PDF with its pages re-arranged into a given order.
    PdfReorderPages,
    /// The text already present in one PDF, extracted to a `.txt` file.
    ///
    /// Extraction, not recognition: it reads the text a PDF already
    /// contains and finds nothing in a scan (that is `PdfOcr`).
    PdfExtractText,
}

impl JobKind {
    pub const ALL: [JobKind; 16] = [
        JobKind::ImageConvert,
        JobKind::PdfMerge,
        JobKind::PdfSplit,
        JobKind::PdfCompress,
        JobKind::PdfRotate,
        JobKind::PdfWatermark,
        JobKind::PdfOcr,
        JobKind::OfficeConvert,
        JobKind::PdfToOffice,
        JobKind::PdfProtect,
        JobKind::PdfUnlock,
        JobKind::PdfMetadataStrip,
        JobKind::PdfPageNumbers,
        JobKind::PdfDeletePages,
        JobKind::PdfReorderPages,
        JobKind::PdfExtractText,
    ];

    /// The value a client sends as `kind`, and the one echoed in snapshots.
    pub fn wire(self) -> &'static str {
        match self {
            // Historical name of the image-conversion kind; kept so the
            // existing frontend keeps working.
            JobKind::ImageConvert => "convert",
            JobKind::PdfMerge => "pdf_merge",
            JobKind::PdfSplit => "pdf_split",
            JobKind::PdfCompress => "pdf_compress",
            JobKind::PdfRotate => "pdf_rotate",
            JobKind::PdfWatermark => "pdf_watermark",
            JobKind::PdfOcr => "pdf_ocr",
            JobKind::OfficeConvert => "office_convert",
            JobKind::PdfToOffice => "pdf_to_office",
            JobKind::PdfProtect => "pdf_protect",
            JobKind::PdfUnlock => "pdf_unlock",
            JobKind::PdfMetadataStrip => "pdf_metadata_strip",
            JobKind::PdfPageNumbers => "pdf_page_numbers",
            JobKind::PdfDeletePages => "pdf_delete_pages",
            JobKind::PdfReorderPages => "pdf_reorder_pages",
            JobKind::PdfExtractText => "pdf_extract_text",
        }
    }

    /// Whether this operation cannot be relied on to preserve the
    /// document, however technically successful the engine call is.
    ///
    /// This is published by `GET /capabilities` and marked on the download
    /// name (see `output_name`), so a user is told before and after, rather
    /// than discovering it by comparing the result to the original.
    pub fn is_experimental(self) -> bool {
        match self {
            JobKind::PdfToOffice => true,
            JobKind::ImageConvert
            | JobKind::PdfMerge
            | JobKind::PdfSplit
            | JobKind::PdfCompress
            | JobKind::PdfRotate
            | JobKind::PdfWatermark
            | JobKind::PdfOcr
            | JobKind::OfficeConvert
            | JobKind::PdfProtect
            | JobKind::PdfUnlock
            | JobKind::PdfMetadataStrip
            | JobKind::PdfPageNumbers
            | JobKind::PdfDeletePages
            | JobKind::PdfReorderPages
            | JobKind::PdfExtractText => false,
        }
    }

    pub fn parse(raw: &str) -> Option<JobKind> {
        JobKind::ALL.into_iter().find(|k| k.wire() == raw)
    }

    pub fn arity(self) -> Arity {
        match self {
            JobKind::PdfMerge => Arity::OneOrMore,
            JobKind::ImageConvert
            | JobKind::PdfSplit
            | JobKind::PdfCompress
            | JobKind::PdfRotate
            | JobKind::PdfWatermark
            | JobKind::PdfOcr
            | JobKind::OfficeConvert
            | JobKind::PdfToOffice
            | JobKind::PdfProtect
            | JobKind::PdfUnlock
            | JobKind::PdfMetadataStrip
            | JobKind::PdfPageNumbers
            | JobKind::PdfDeletePages
            | JobKind::PdfReorderPages
            | JobKind::PdfExtractText => Arity::One,
        }
    }

    /// Whether this kind can be asked to work on `count` inputs.
    pub fn accepts_input_count(self, count: usize) -> bool {
        match self.arity() {
            Arity::One => count == 1,
            Arity::OneOrMore => (1..=MAX_INPUTS).contains(&count),
        }
    }
}

/// Options accepted for `kind: "convert"`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageConvertOptions {
    pub quality: Option<u8>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ImageConvertSpec {
    pub source_format: NativeImageFormat,
    pub target: NativeImageFormat,
    pub options: ImageConvertOptions,
}

/// Highest page number any PDF operation will address. Bounds both the work
/// a request can ask for and the number of files a split can produce.
pub const MAX_PDF_PAGE: u32 = 5_000;

/// Quality presets for `pdf_compress`. A closed set, so no free-form value
/// reaches the engine's command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfQuality {
    /// Smallest, screen resolution.
    Screen,
    /// The default: readable on screen, much smaller than print quality.
    #[default]
    Ebook,
    Printer,
    Prepress,
}

/// A right-angle rotation. Only these four values are accepted, and they
/// collapse to the three distinct results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum PdfRotation {
    #[serde(rename = "90")]
    Clockwise90,
    #[serde(rename = "180")]
    Half,
    #[serde(rename = "270")]
    Clockwise270,
}

impl PdfRotation {
    pub fn degrees(self) -> u16 {
        match self {
            PdfRotation::Clockwise90 => 90,
            PdfRotation::Half => 180,
            PdfRotation::Clockwise270 => 270,
        }
    }
}

/// Languages the OCR engine may be asked for. A closed set: the value
/// becomes a Tesseract `-l` argument and a traineddata file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrLanguage {
    Turkish,
    English,
    /// Both, and the DEFAULT: a Turkish school's scans are mostly Turkish
    /// with English fragments (course names, software, references), and
    /// Tesseract reads that better when told both languages than when told
    /// either one. It costs recognition time, which an OCR job is spending
    /// anyway.
    #[default]
    TurkishAndEnglish,
}

impl OcrLanguage {
    /// The engine's language code.
    pub fn code(self) -> &'static str {
        match self {
            OcrLanguage::Turkish => "tur",
            OcrLanguage::English => "eng",
            OcrLanguage::TurkishAndEnglish => "tur+eng",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PdfMergeSpec {
    /// How many PDFs are being merged, in request order.
    pub input_count: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfSplitOptions {
    /// 1-based page numbers to extract. Absent means every page.
    pub pages: Option<Vec<u32>>,
}

#[derive(Debug, Clone)]
pub struct PdfSplitSpec {
    /// Requested pages, de-duplicated and ascending. `None` = all pages.
    pub pages: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfCompressOptions {
    #[serde(default)]
    pub quality: PdfQuality,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfRotateOptions {
    pub rotation: PdfRotation,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfWatermarkOptions {
    pub text: String,
    /// 0.05-1.0. Absent means the default.
    pub opacity: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct PdfWatermarkSpec {
    /// Sanitized watermark text: no control characters, bounded length.
    pub text: String,
    pub opacity: f32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfOcrOptions {
    #[serde(default)]
    pub language: OcrLanguage,
}

#[derive(Debug, Clone)]
pub struct OfficeConvertSpec {
    pub source_format: OfficeFormat,
    /// What to produce: PDF, or an Office format of the same document
    /// class. Never an image, never the source format itself - all checked
    /// when the spec is built (`office_conversion_targets`).
    pub target: SourceFormat,
}

#[derive(Debug, Clone)]
pub struct PdfToOfficeSpec {
    /// The editable format to reconstruct into. Only a text-class format:
    /// the engine opens the PDF in Writer, and nothing in this project
    /// reconstructs a spreadsheet or a slide deck from one.
    pub target: OfficeFormat,
}

/// Where a page number is drawn. A closed set: it chooses between fixed
/// coordinate formulas, so no client value becomes a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageNumberPosition {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    #[default]
    BottomCenter,
    BottomRight,
}

/// Longest password accepted. The PDF standard security handler hashes at
/// most 127 bytes of a password anyway (and the pre-2.0 revisions only 32),
/// so a longer one would be silently truncated by any reader - a bound here
/// is honest where accepting it would not be.
const MAX_PASSWORD_CHARS: usize = 64;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfProtectOptions {
    /// Required to OPEN the result.
    pub password: String,
    /// Optional, and required to CHANGE the result (remove the protection,
    /// re-encrypt it). Absent means the same password does both, which is
    /// what a single-password request means.
    #[serde(default)]
    pub owner_password: Option<String>,
}

/// Debug is implemented by hand for every password-carrying type below: the
/// spec is held in the job record for the job's whole life, and a derived
/// `Debug` would put the password into any log line that ever formats a
/// job. There is no code path that needs to see it.
#[derive(Clone)]
pub struct PdfProtectSpec {
    pub user_password: String,
    pub owner_password: String,
}

impl std::fmt::Debug for PdfProtectSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PdfProtectSpec { passwords: <redacted> }")
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfUnlockOptions {
    /// The password that opens the document. Absent is valid: a PDF can be
    /// encrypted with an empty user password and only a set of
    /// restrictions, and that one opens without a password at all.
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Clone)]
pub struct PdfUnlockSpec {
    pub password: Option<String>,
}

impl std::fmt::Debug for PdfUnlockSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PdfUnlockSpec { password: <redacted> }")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfPageNumbersOptions {
    #[serde(default)]
    pub position: PageNumberPosition,
    /// The number printed on the FIRST page. Absent means 1.
    pub start_at: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct PdfPageNumbersSpec {
    pub position: PageNumberPosition,
    pub start_at: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfDeletePagesOptions {
    /// 1-based page numbers to remove. Required: a delete with no pages
    /// named would be a copy presented as an edit.
    pub pages: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct PdfDeletePagesSpec {
    /// De-duplicated and ascending.
    pub pages: Vec<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfReorderPagesOptions {
    /// The new order, as 1-based numbers of the ORIGINAL pages.
    pub order: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct PdfReorderPagesSpec {
    /// A permutation of `1..=order.len()`: every original page appears
    /// exactly once. A list that dropped or repeated a page would be a
    /// delete or a duplicate, which are different operations (one of them
    /// is `pdf_delete_pages`), so it is refused rather than reinterpreted.
    pub order: Vec<u32>,
}

/// Everything an Office document of this format may be converted to: PDF,
/// or another Office format of the SAME document class (`docx <-> odt`, but
/// never `docx -> ods`). Converting a format to itself is excluded - the
/// result would be a copy presented as a conversion.
///
/// The ONE definition of the Office conversion matrix: `JobSpec::
/// from_request` validates against it and `GET /capabilities` publishes it,
/// so what the API accepts and what it advertises cannot drift apart.
pub fn office_conversion_targets(source: OfficeFormat) -> Vec<SourceFormat> {
    let mut targets = vec![SourceFormat::Pdf];
    targets.extend(
        OfficeFormat::ALL
            .into_iter()
            .filter(|f| *f != source && f.document_class() == source.document_class())
            .map(SourceFormat::Office),
    );
    targets
}

/// The formats a PDF may be reconstructed into. Text-class only, and
/// EXPERIMENTAL in both of them.
pub fn pdf_reconstruction_targets() -> Vec<OfficeFormat> {
    OfficeFormat::ALL
        .into_iter()
        .filter(|f| f.document_class() == DocumentClass::Text)
        .collect()
}

/// Longest watermark text accepted. Long enough for a school's name and a
/// date; short enough that it cannot be used to pad a command line.
const MAX_WATERMARK_CHARS: usize = 120;
const DEFAULT_WATERMARK_OPACITY: f32 = 0.3;

/// One validated unit of work. Built only by `from_request`.
#[derive(Debug, Clone)]
pub enum JobSpec {
    ImageConvert(ImageConvertSpec),
    PdfMerge(PdfMergeSpec),
    PdfSplit(PdfSplitSpec),
    PdfCompress(PdfCompressOptions),
    PdfRotate(PdfRotateOptions),
    PdfWatermark(PdfWatermarkSpec),
    PdfOcr(PdfOcrOptions),
    OfficeConvert(OfficeConvertSpec),
    PdfToOffice(PdfToOfficeSpec),
    PdfProtect(PdfProtectSpec),
    PdfUnlock(PdfUnlockSpec),
    PdfMetadataStrip,
    PdfPageNumbers(PdfPageNumbersSpec),
    PdfDeletePages(PdfDeletePagesSpec),
    PdfReorderPages(PdfReorderPagesSpec),
    PdfExtractText,
}

/// A create-job request as it arrives. Parsed strictly: an unknown
/// top-level field is a rejected request, not an ignored one. `options` is
/// kept opaque here and parsed into the named kind's own options type, so
/// each kind keeps its own strict schema.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateJobRequest {
    pub kind: String,
    /// A single input. Mutually exclusive with `file_ids`.
    #[serde(default)]
    pub file_id: Option<String>,
    /// Several inputs, in the order they should be processed (PDF merge).
    /// Mutually exclusive with `file_id`.
    #[serde(default)]
    pub file_ids: Option<Vec<String>>,
    #[serde(default)]
    pub output_format: Option<String>,
    #[serde(default)]
    pub options: Option<serde_json::Value>,
}

impl CreateJobRequest {
    /// The requested inputs, in order. Exactly one of the two forms must be
    /// present, and the count must suit the kind - a request naming three
    /// files for a single-input kind is refused here, not quietly truncated.
    ///
    /// The ids are still just strings at this point; each one becomes a file
    /// only through the owner-scoped `Registry::find_file`.
    pub fn input_ids(&self, kind: JobKind) -> Result<Vec<&str>, ApiError> {
        let ids: Vec<&str> = match (&self.file_id, &self.file_ids) {
            (Some(one), None) => vec![one.as_str()],
            (None, Some(many)) => many.iter().map(String::as_str).collect(),
            // Neither, or both: the request does not say what to work on.
            _ => return Err(ApiError::invalid_request()),
        };
        if !kind.accepts_input_count(ids.len()) {
            return Err(ApiError::invalid_request());
        }
        Ok(ids)
    }
}

impl JobSpec {
    /// Validates `request` against the kind it names, for inputs whose own
    /// formats the upload stage already PROVED (never their claimed ones).
    ///
    /// `sources` is in request order and has already been checked against
    /// the kind's arity. A kind applied to a source of the wrong category -
    /// `convert` on a PDF, say - is an unsupported conversion, not a
    /// malformed request.
    pub fn from_request(
        kind: JobKind,
        request: &CreateJobRequest,
        sources: &[SourceFormat],
    ) -> Result<JobSpec, ApiError> {
        if !kind.accepts_input_count(sources.len()) {
            return Err(ApiError::invalid_request());
        }
        match kind {
            JobKind::ImageConvert => {
                let source_format = sources
                    .first()
                    .and_then(|s| s.image())
                    .ok_or_else(ApiError::unsupported_conversion)?;
                let requested = request
                    .output_format
                    .as_deref()
                    .ok_or_else(ApiError::invalid_request)?;
                let target = NativeImageFormat::from_extension(requested)
                    .ok_or_else(ApiError::unsupported_conversion)?;
                let options: ImageConvertOptions = parse_options(request.options.as_ref())?;
                validate_image_options(&options)?;
                Ok(JobSpec::ImageConvert(ImageConvertSpec {
                    source_format,
                    target,
                    options,
                }))
            }
            JobKind::PdfMerge => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                reject_options(request)?;
                Ok(JobSpec::PdfMerge(PdfMergeSpec {
                    input_count: sources.len(),
                }))
            }
            JobKind::PdfSplit => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                let options: PdfSplitOptions = parse_options(request.options.as_ref())?;
                Ok(JobSpec::PdfSplit(PdfSplitSpec {
                    pages: normalize_pages(options.pages)?,
                }))
            }
            JobKind::PdfCompress => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                Ok(JobSpec::PdfCompress(parse_options(
                    request.options.as_ref(),
                )?))
            }
            JobKind::PdfRotate => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                // No default: a rotation job must say which way to turn.
                let options: PdfRotateOptions = require_options(request.options.as_ref())?;
                Ok(JobSpec::PdfRotate(options))
            }
            JobKind::PdfWatermark => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                let options: PdfWatermarkOptions = require_options(request.options.as_ref())?;
                Ok(JobSpec::PdfWatermark(PdfWatermarkSpec {
                    text: sanitize_watermark(&options.text)?,
                    opacity: validate_opacity(options.opacity)?,
                }))
            }
            JobKind::PdfOcr => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                Ok(JobSpec::PdfOcr(parse_options(request.options.as_ref())?))
            }
            JobKind::OfficeConvert => {
                let source_format = sources
                    .first()
                    .and_then(|s| s.office())
                    .ok_or_else(ApiError::unsupported_conversion)?;
                let requested = request
                    .output_format
                    .as_deref()
                    .ok_or_else(ApiError::invalid_request)?;
                let target = SourceFormat::from_extension(requested)
                    .ok_or_else(ApiError::unsupported_conversion)?;
                // The matrix is the single definition in
                // `office_conversion_targets`: it already excludes images,
                // the source format itself (which would be a copy
                // presented as a conversion) and every cross-class pair
                // (a text document cannot become a spreadsheet).
                if !office_conversion_targets(source_format).contains(&target) {
                    return Err(ApiError::unsupported_conversion());
                }
                reject_options(request)?;
                Ok(JobSpec::OfficeConvert(OfficeConvertSpec {
                    source_format,
                    target,
                }))
            }
            JobKind::PdfToOffice => {
                require_all_pdf(sources)?;
                let requested = request
                    .output_format
                    .as_deref()
                    .ok_or_else(ApiError::invalid_request)?;
                let target = SourceFormat::from_extension(requested)
                    .and_then(|t| t.office())
                    .filter(|t| pdf_reconstruction_targets().contains(t))
                    .ok_or_else(ApiError::unsupported_conversion)?;
                reject_options(request)?;
                Ok(JobSpec::PdfToOffice(PdfToOfficeSpec { target }))
            }
            JobKind::PdfProtect => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                // No default: a protect job with no password would produce
                // an unprotected file under a name that says otherwise.
                let options: PdfProtectOptions = require_options(request.options.as_ref())?;
                let user_password = validate_password(&options.password)?;
                let owner_password = match &options.owner_password {
                    Some(raw) => validate_password(raw)?,
                    None => user_password.clone(),
                };
                Ok(JobSpec::PdfProtect(PdfProtectSpec {
                    user_password,
                    owner_password,
                }))
            }
            JobKind::PdfUnlock => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                let options: PdfUnlockOptions = parse_options(request.options.as_ref())?;
                let password = match &options.password {
                    Some(raw) => Some(validate_password(raw)?),
                    None => None,
                };
                Ok(JobSpec::PdfUnlock(PdfUnlockSpec { password }))
            }
            JobKind::PdfMetadataStrip => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                reject_options(request)?;
                Ok(JobSpec::PdfMetadataStrip)
            }
            JobKind::PdfPageNumbers => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                let options: PdfPageNumbersOptions = parse_options(request.options.as_ref())?;
                let start_at = options.start_at.unwrap_or(1);
                // Bounded by the same page cap every PDF operation uses, so
                // the drawn number cannot be arbitrarily wide either.
                if start_at == 0 || start_at > MAX_PDF_PAGE {
                    return Err(ApiError::invalid_request());
                }
                Ok(JobSpec::PdfPageNumbers(PdfPageNumbersSpec {
                    position: options.position,
                    start_at,
                }))
            }
            JobKind::PdfDeletePages => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                let options: PdfDeletePagesOptions = require_options(request.options.as_ref())?;
                let pages = normalize_pages(Some(options.pages))?
                    .ok_or_else(ApiError::invalid_request)?;
                Ok(JobSpec::PdfDeletePages(PdfDeletePagesSpec { pages }))
            }
            JobKind::PdfReorderPages => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                let options: PdfReorderPagesOptions = require_options(request.options.as_ref())?;
                Ok(JobSpec::PdfReorderPages(PdfReorderPagesSpec {
                    order: validate_page_order(options.order)?,
                }))
            }
            JobKind::PdfExtractText => {
                require_all_pdf(sources)?;
                reject_output_format(request)?;
                reject_options(request)?;
                Ok(JobSpec::PdfExtractText)
            }
        }
    }

    pub fn kind(&self) -> JobKind {
        match self {
            JobSpec::ImageConvert(_) => JobKind::ImageConvert,
            JobSpec::PdfMerge(_) => JobKind::PdfMerge,
            JobSpec::PdfSplit(_) => JobKind::PdfSplit,
            JobSpec::PdfCompress(_) => JobKind::PdfCompress,
            JobSpec::PdfRotate(_) => JobKind::PdfRotate,
            JobSpec::PdfWatermark(_) => JobKind::PdfWatermark,
            JobSpec::PdfOcr(_) => JobKind::PdfOcr,
            JobSpec::OfficeConvert(_) => JobKind::OfficeConvert,
            JobSpec::PdfToOffice(_) => JobKind::PdfToOffice,
            JobSpec::PdfProtect(_) => JobKind::PdfProtect,
            JobSpec::PdfUnlock(_) => JobKind::PdfUnlock,
            JobSpec::PdfMetadataStrip => JobKind::PdfMetadataStrip,
            JobSpec::PdfPageNumbers(_) => JobKind::PdfPageNumbers,
            JobSpec::PdfDeletePages(_) => JobKind::PdfDeletePages,
            JobSpec::PdfReorderPages(_) => JobKind::PdfReorderPages,
            JobSpec::PdfExtractText => JobKind::PdfExtractText,
        }
    }

    /// Extension of the job's single output file. Server-chosen: it comes
    /// from this spec, never from a client string.
    ///
    /// A kind that naturally produces several files (PDF split) publishes
    /// one archive instead, so this stays single-valued.
    pub fn output_extension(&self) -> &'static str {
        match self {
            JobSpec::ImageConvert(s) => s.target.canonical_extension(),
            JobSpec::PdfSplit(_) => "zip",
            JobSpec::PdfExtractText => "txt",
            JobSpec::PdfMerge(_)
            | JobSpec::PdfCompress(_)
            | JobSpec::PdfRotate(_)
            | JobSpec::PdfWatermark(_)
            | JobSpec::PdfOcr(_)
            | JobSpec::PdfProtect(_)
            | JobSpec::PdfUnlock(_)
            | JobSpec::PdfMetadataStrip
            | JobSpec::PdfPageNumbers(_)
            | JobSpec::PdfDeletePages(_)
            | JobSpec::PdfReorderPages(_) => "pdf",
            JobSpec::OfficeConvert(s) => s.target.canonical_extension(),
            JobSpec::PdfToOffice(s) => s.target.canonical_extension(),
        }
    }

    pub fn output_mime(&self) -> &'static str {
        match self {
            JobSpec::ImageConvert(s) => s.target.mime_type(),
            JobSpec::PdfSplit(_) => "application/zip",
            JobSpec::PdfExtractText => "text/plain; charset=utf-8",
            JobSpec::PdfMerge(_)
            | JobSpec::PdfCompress(_)
            | JobSpec::PdfRotate(_)
            | JobSpec::PdfWatermark(_)
            | JobSpec::PdfOcr(_)
            | JobSpec::PdfProtect(_)
            | JobSpec::PdfUnlock(_)
            | JobSpec::PdfMetadataStrip
            | JobSpec::PdfPageNumbers(_)
            | JobSpec::PdfDeletePages(_)
            | JobSpec::PdfReorderPages(_) => "application/pdf",
            JobSpec::OfficeConvert(s) => s.target.mime_type(),
            JobSpec::PdfToOffice(s) => s.target.mime_type(),
        }
    }

    /// Download name offered for a completed job, from the (sanitized)
    /// display stem of the input.
    pub fn output_name(&self, display_stem: &str) -> String {
        let extension = self.output_extension();
        // A suffix whenever the result would otherwise be indistinguishable
        // from the input it came from, and for every operation that changed
        // the document rather than its format.
        let suffix = match self {
            JobSpec::ImageConvert(s) if s.source_format == s.target => Some("donusturuldu"),
            JobSpec::ImageConvert(_) => None,
            JobSpec::PdfMerge(_) => Some("birlestirildi"),
            JobSpec::PdfSplit(_) => Some("sayfalar"),
            JobSpec::PdfCompress(_) => Some("sikistirildi"),
            JobSpec::PdfRotate(_) => Some("donduruldu"),
            JobSpec::PdfWatermark(_) => Some("filigranli"),
            JobSpec::PdfOcr(_) => Some("aranabilir"),
            JobSpec::OfficeConvert(_) => None,
            // Not a naming convenience: the result of a reconstruction is
            // marked experimental in the name the user downloads, because
            // that is the artifact they will keep and possibly hand on.
            JobSpec::PdfToOffice(_) => Some("deneysel"),
            JobSpec::PdfProtect(_) => Some("korumali"),
            JobSpec::PdfUnlock(_) => Some("korumasiz"),
            JobSpec::PdfMetadataStrip => Some("ustverisiz"),
            JobSpec::PdfPageNumbers(_) => Some("numarali"),
            JobSpec::PdfDeletePages(_) => Some("sayfa_silindi"),
            JobSpec::PdfReorderPages(_) => Some("yeniden_siralandi"),
            JobSpec::PdfExtractText => Some("metin"),
        };
        match suffix {
            Some(suffix) => format!("{display_stem}_{suffix}.{extension}"),
            None => format!("{display_stem}.{extension}"),
        }
    }

    /// Whether a produced file really is what this job promised. An engine
    /// "success" whose output fails this is never published (see
    /// `worker::publish_output`); `Err` carries the reason, for the log.
    ///
    /// Each kind checks to the depth that is cheap and meaningful for it: a
    /// signature for a raster image the engine just wrote, and the full
    /// container check for a document, where the signature alone (every
    /// Office file is a ZIP) would prove almost nothing.
    pub fn validate_output(&self, path: &Path) -> Result<(), &'static str> {
        match self {
            JobSpec::ImageConvert(s) => {
                let head = head_of(path, OUTPUT_HEAD_BYTES);
                if meb_core::image::sniff_format(&head) == Some(s.target) {
                    Ok(())
                } else {
                    Err("content is not the requested image format")
                }
            }
            JobSpec::PdfMerge(_)
            | JobSpec::PdfCompress(_)
            | JobSpec::PdfRotate(_)
            | JobSpec::PdfWatermark(_)
            | JobSpec::PdfOcr(_)
            // A protected output is encrypted, which the PDF probe is
            // unaffected by: it reads the header and the trailer, both of
            // which stay in the clear in an encrypted PDF.
            | JobSpec::PdfProtect(_)
            | JobSpec::PdfUnlock(_)
            | JobSpec::PdfMetadataStrip
            | JobSpec::PdfPageNumbers(_)
            | JobSpec::PdfDeletePages(_)
            | JobSpec::PdfReorderPages(_) => probe_as(path, SourceFormat::Pdf),
            // The one text output. There is no format to probe, so what is
            // checked is what the promise actually was: real, decodable
            // text. An empty file would be a "successful" extraction that
            // gave the user nothing.
            JobSpec::PdfExtractText => match std::fs::read(path) {
                Ok(bytes) if bytes.is_empty() => Err("the extracted text file is empty"),
                Ok(bytes) if std::str::from_utf8(&bytes).is_err() => {
                    Err("the extracted text is not valid UTF-8")
                }
                Ok(_) => Ok(()),
                Err(_) => Err("the extracted text file could not be read back"),
            },
            JobSpec::PdfSplit(_) => {
                let head = head_of(path, OUTPUT_HEAD_BYTES);
                if matches!(Container::sniff(&head), Some(Container::Zip)) {
                    Ok(())
                } else {
                    Err("split output is not a ZIP archive")
                }
            }
            // An Office output is probed in full: every Office file is a
            // ZIP, so its signature alone would prove almost nothing.
            JobSpec::OfficeConvert(s) => probe_as(path, s.target),
            // A reconstruction may be a poor rendering of the original, but
            // it still has to be a real document of the requested type.
            JobSpec::PdfToOffice(s) => probe_as(path, SourceFormat::Office(s.target)),
        }
    }

    /// The image-conversion work, for a runner that only handles that kind.
    /// A runner handed any other kind is a registry wiring mistake and
    /// reports `RunError::wrong_kind` rather than guessing.
    pub fn image_convert(&self) -> Option<&ImageConvertSpec> {
        match self {
            JobSpec::ImageConvert(s) => Some(s),
            _ => None,
        }
    }
}

/// Confirms a produced document really is `expected`, using the same probe
/// the upload path uses on untrusted input.
fn probe_as(path: &Path, expected: SourceFormat) -> Result<(), &'static str> {
    match meb_core::document::probe_file(path, expected) {
        Ok(probe) if probe.format == expected => Ok(()),
        Ok(_) => Err("output is a document of the wrong type"),
        Err(_) => Err("output is not a readable document of the requested type"),
    }
}

/// Every input of a PDF operation must really be a PDF.
fn require_all_pdf(sources: &[SourceFormat]) -> Result<(), ApiError> {
    if sources.iter().all(|s| *s == SourceFormat::Pdf) {
        Ok(())
    } else {
        Err(ApiError::unsupported_conversion())
    }
}

/// For a kind whose output format is fixed, naming one is a contradiction,
/// not a hint to be ignored.
fn reject_output_format(request: &CreateJobRequest) -> Result<(), ApiError> {
    if request.output_format.is_some() {
        return Err(ApiError::invalid_request());
    }
    Ok(())
}

/// For a kind that takes no options, sending some is a rejected request -
/// the same rule `deny_unknown_fields` applies within a kind.
fn reject_options(request: &CreateJobRequest) -> Result<(), ApiError> {
    match &request.options {
        None | Some(serde_json::Value::Null) => Ok(()),
        Some(_) => Err(ApiError::invalid_request()),
    }
}

/// Options a kind cannot default: absent is a malformed request.
fn require_options<T: serde::de::DeserializeOwned>(
    raw: Option<&serde_json::Value>,
) -> Result<T, ApiError> {
    let value = raw
        .filter(|v| !v.is_null())
        .ok_or_else(ApiError::invalid_request)?;
    serde_json::from_value(value.clone()).map_err(|_| ApiError::invalid_request())
}

/// De-duplicates and sorts requested page numbers, rejecting page 0 (PDF
/// pages are 1-based) and anything past `MAX_PDF_PAGE`. An empty list asks
/// for nothing and is refused rather than silently meaning "all".
fn normalize_pages(pages: Option<Vec<u32>>) -> Result<Option<Vec<u32>>, ApiError> {
    let Some(mut pages) = pages else {
        return Ok(None);
    };
    if pages.is_empty() || pages.len() > MAX_PDF_PAGE as usize {
        return Err(ApiError::invalid_request());
    }
    if pages.iter().any(|p| *p == 0 || *p > MAX_PDF_PAGE) {
        return Err(ApiError::invalid_request());
    }
    pages.sort_unstable();
    pages.dedup();
    Ok(Some(pages))
}

/// A password is secret display data: it is never a command line argument
/// (these operations run in-process), so what matters is that it is a
/// usable password at all - non-empty once trimmed of nothing, free of
/// control characters, and short enough that the PDF security handler will
/// actually hash all of it.
///
/// Whitespace is NOT trimmed: it is significant in a password, and silently
/// changing one would lock the user out of their own file.
fn validate_password(raw: &str) -> Result<String, ApiError> {
    let length = raw.chars().count();
    if length == 0 || length > MAX_PASSWORD_CHARS || raw.chars().any(char::is_control) {
        return Err(ApiError::invalid_request());
    }
    Ok(raw.to_string())
}

/// A page order must be a permutation of `1..=n`: exactly the pages the
/// document's first `n` pages are, each once. Anything else means some
/// other operation (a delete, a duplication, an extraction) and is refused
/// rather than guessed at. Whether `n` really is the document's page count
/// is the engine's check, since only it has the document.
fn validate_page_order(order: Vec<u32>) -> Result<Vec<u32>, ApiError> {
    if order.is_empty() || order.len() > MAX_PDF_PAGE as usize {
        return Err(ApiError::invalid_request());
    }
    let mut sorted = order.clone();
    sorted.sort_unstable();
    if !sorted.iter().copied().eq(1..=order.len() as u32) {
        return Err(ApiError::invalid_request());
    }
    Ok(order)
}

/// Watermark text is drawn into a document, so it is display data: control
/// characters are dropped and the length is bounded. It is never a command
/// line argument on its own (see the engine's `Arg` handling).
fn sanitize_watermark(raw: &str) -> Result<String, ApiError> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_WATERMARK_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Err(ApiError::invalid_request());
    }
    Ok(trimmed.to_string())
}

fn validate_opacity(opacity: Option<f32>) -> Result<f32, ApiError> {
    match opacity {
        None => Ok(DEFAULT_WATERMARK_OPACITY),
        // Rejects NaN and infinities too, which no comparison with a range
        // would accept.
        Some(o) if o.is_finite() && (0.05..=1.0).contains(&o) => Ok(o),
        Some(_) => Err(ApiError::invalid_request()),
    }
}

/// Reads at most `limit` leading bytes. A short or unreadable file yields
/// fewer (or none), which every caller treats as "does not match".
fn head_of(path: &Path, limit: usize) -> Vec<u8> {
    use std::io::Read;
    let mut head = vec![0u8; limit];
    let read = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    head.truncate(read);
    head
}

/// Deserializes a kind's options from the opaque `options` object. Absent
/// options mean "the defaults"; anything that is not that kind's own schema
/// (including an unknown field) is an invalid request.
fn parse_options<T>(raw: Option<&serde_json::Value>) -> Result<T, ApiError>
where
    T: Default + serde::de::DeserializeOwned,
{
    match raw {
        None | Some(serde_json::Value::Null) => Ok(T::default()),
        Some(value) => {
            serde_json::from_value(value.clone()).map_err(|_| ApiError::invalid_request())
        }
    }
}

fn validate_image_options(options: &ImageConvertOptions) -> Result<(), ApiError> {
    if let Some(q) = options.quality {
        if !(1..=100).contains(&q) {
            return Err(ApiError::invalid_request());
        }
    }
    match (options.width, options.height) {
        (None, None) => Ok(()),
        (Some(w), Some(h)) => {
            // Same bounds the engine applies to inputs, applied to the
            // requested output so a resize cannot allocate past them.
            if w == 0
                || h == 0
                || w > MAX_DIMENSION
                || h > MAX_DIMENSION
                || (w as u64 * h as u64) > MAX_DECODED_PIXELS
            {
                Err(ApiError::invalid_dimensions())
            } else {
                Ok(())
            }
        }
        _ => Err(ApiError::invalid_dimensions()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        kind: &str,
        output_format: Option<&str>,
        options: serde_json::Value,
    ) -> CreateJobRequest {
        CreateJobRequest {
            kind: kind.to_string(),
            file_id: Some("f".to_string()),
            file_ids: None,
            output_format: output_format.map(str::to_string),
            options: if options.is_null() { None } else { Some(options) },
        }
    }

    fn image_spec(target: &str, source: NativeImageFormat) -> Result<JobSpec, ApiError> {
        JobSpec::from_request(
            JobKind::ImageConvert,
            &request("convert", Some(target), serde_json::Value::Null),
            &[SourceFormat::Image(source)],
        )
    }

    #[test]
    fn only_published_kinds_parse() {
        assert_eq!(JobKind::parse("convert"), Some(JobKind::ImageConvert));
        assert_eq!(JobKind::parse("pdf_merge"), Some(JobKind::PdfMerge));
        // Case matters, and a plausible-looking name is not a kind.
        assert_eq!(JobKind::parse("CONVERT"), None);
        assert_eq!(JobKind::parse("pdf_encrypt"), None);
        assert_eq!(JobKind::parse("exec"), None);
        assert_eq!(JobKind::parse(""), None);
        // Every kind round-trips through its wire name.
        for kind in JobKind::ALL {
            assert_eq!(JobKind::parse(kind.wire()), Some(kind));
        }
    }

    #[test]
    fn unknown_option_fields_are_rejected_not_ignored() {
        let bad = JobSpec::from_request(
            JobKind::ImageConvert,
            &request(
                "convert",
                Some("png"),
                serde_json::json!({ "output_path": "C:\\x.png" }),
            ),
            &[SourceFormat::Image(NativeImageFormat::Jpeg)],
        );
        assert_eq!(bad.err().unwrap().code, "INVALID_REQUEST");
    }

    #[test]
    fn an_unsupported_target_and_a_missing_one_are_distinguished() {
        assert_eq!(
            image_spec("exe", NativeImageFormat::Jpeg)
                .err()
                .unwrap()
                .code,
            "UNSUPPORTED_CONVERSION"
        );
        let missing = JobSpec::from_request(
            JobKind::ImageConvert,
            &request("convert", None, serde_json::Value::Null),
            &[SourceFormat::Image(NativeImageFormat::Jpeg)],
        );
        assert_eq!(missing.err().unwrap().code, "INVALID_REQUEST");
    }

    #[test]
    fn a_kind_refuses_a_source_of_the_wrong_category() {
        // `convert` is the raster-image kind; a PDF is not convertible by
        // it, and that is an unsupported conversion rather than a malformed
        // request.
        for source in [
            SourceFormat::Pdf,
            SourceFormat::Office(meb_core::format::OfficeFormat::Docx),
        ] {
            let refused = JobSpec::from_request(
                JobKind::ImageConvert,
                &request("convert", Some("png"), serde_json::Value::Null),
                &[source],
            );
            assert_eq!(
                refused.err().unwrap().code,
                "UNSUPPORTED_CONVERSION",
                "{source:?}"
            );
        }
    }

    #[test]
    fn output_naming_and_sniffing_come_from_the_spec() {
        let converted = image_spec("png", NativeImageFormat::Jpeg).unwrap();
        assert_eq!(converted.output_extension(), "png");
        assert_eq!(converted.output_mime(), "image/png");
        assert_eq!(converted.output_name("foto"), "foto.png");

        // Same format in and out: the name must not look like the original.
        let same = image_spec("png", NativeImageFormat::Png).unwrap();
        assert_eq!(same.output_name("foto"), "foto_donusturuldu.png");

        // Output validation reads the produced file, so it rejects an
        // engine that wrote the wrong format, an empty file, or nothing.
        let dir = std::env::temp_dir().join(format!(
            "meb_spec_output_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let produced = dir.join("result.png");

        std::fs::write(&produced, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(converted.validate_output(&produced).is_ok());

        std::fs::write(&produced, b"%PDF-1.7").unwrap();
        assert!(converted.validate_output(&produced).is_err());

        std::fs::write(&produced, b"").unwrap();
        assert!(converted.validate_output(&produced).is_err());

        assert!(converted.validate_output(&dir.join("absent.png")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn spec_of(
        kind: JobKind,
        sources: &[SourceFormat],
        output_format: Option<&str>,
        options: serde_json::Value,
    ) -> Result<JobSpec, ApiError> {
        let mut request = request(kind.wire(), output_format, options);
        if sources.len() != 1 {
            request.file_id = None;
            request.file_ids = Some(sources.iter().map(|_| "f".to_string()).collect());
        }
        JobSpec::from_request(kind, &request, sources)
    }

    const PDF: SourceFormat = SourceFormat::Pdf;

    #[test]
    fn pdf_operations_accept_only_pdf_inputs() {
        let not_pdf = [
            SourceFormat::Image(NativeImageFormat::Png),
            SourceFormat::Office(OfficeFormat::Docx),
        ];
        for kind in [
            JobKind::PdfMerge,
            JobKind::PdfSplit,
            JobKind::PdfCompress,
            JobKind::PdfOcr,
        ] {
            for source in not_pdf {
                assert_eq!(
                    spec_of(kind, &[source], None, serde_json::Value::Null)
                        .err()
                        .unwrap()
                        .code,
                    "UNSUPPORTED_CONVERSION",
                    "{} accepted {source:?}",
                    kind.wire()
                );
            }
        }
        // One non-PDF among several refuses the whole merge.
        assert!(spec_of(
            JobKind::PdfMerge,
            &[PDF, SourceFormat::Image(NativeImageFormat::Png)],
            None,
            serde_json::Value::Null,
        )
        .is_err());
    }

    #[test]
    fn merge_takes_many_inputs_and_the_other_pdf_kinds_take_one() {
        assert!(JobKind::PdfMerge.accepts_input_count(2));
        assert!(JobKind::PdfMerge.accepts_input_count(MAX_INPUTS));
        assert!(!JobKind::PdfMerge.accepts_input_count(0));
        assert!(!JobKind::PdfMerge.accepts_input_count(MAX_INPUTS + 1));
        assert!(!JobKind::PdfSplit.accepts_input_count(2));

        let merged = spec_of(
            JobKind::PdfMerge,
            &[PDF, PDF, PDF],
            None,
            serde_json::Value::Null,
        )
        .unwrap();
        assert_eq!(merged.output_extension(), "pdf");
        assert_eq!(merged.output_mime(), "application/pdf");
        assert_eq!(merged.output_name("rapor"), "rapor_birlestirildi.pdf");
    }

    #[test]
    fn a_fixed_output_format_cannot_be_overridden_and_unused_options_are_refused() {
        // These kinds decide their own output; naming one contradicts the
        // request rather than being ignored.
        assert_eq!(
            spec_of(JobKind::PdfMerge, &[PDF], Some("docx"), serde_json::Value::Null)
                .err()
                .unwrap()
                .code,
            "INVALID_REQUEST"
        );
        // A kind with no options rejects any.
        assert_eq!(
            spec_of(
                JobKind::PdfMerge,
                &[PDF],
                None,
                serde_json::json!({ "quality": "screen" })
            )
            .err()
            .unwrap()
            .code,
            "INVALID_REQUEST"
        );
    }

    #[test]
    fn split_pages_are_normalized_and_bounded() {
        let pages = |value: serde_json::Value| {
            spec_of(JobKind::PdfSplit, &[PDF], None, value).map(|spec| match spec {
                JobSpec::PdfSplit(s) => s.pages,
                other => panic!("expected a split spec, got {other:?}"),
            })
        };
        // Out of order and duplicated: sorted and de-duplicated.
        assert_eq!(
            pages(serde_json::json!({ "pages": [5, 1, 5, 2] })).unwrap(),
            Some(vec![1, 2, 5])
        );
        // No `pages` means every page.
        assert_eq!(pages(serde_json::Value::Null).unwrap(), None);
        // Page 0 does not exist (PDF pages are 1-based), an empty list asks
        // for nothing, and a page past the cap is refused.
        assert!(pages(serde_json::json!({ "pages": [0] })).is_err());
        assert!(pages(serde_json::json!({ "pages": [] })).is_err());
        assert!(pages(serde_json::json!({ "pages": [MAX_PDF_PAGE + 1] })).is_err());

        let spec = spec_of(JobKind::PdfSplit, &[PDF], None, serde_json::Value::Null).unwrap();
        // Several pages come back as one archive.
        assert_eq!(spec.output_extension(), "zip");
        assert_eq!(spec.output_mime(), "application/zip");
        assert_eq!(spec.output_name("rapor"), "rapor_sayfalar.zip");
    }

    #[test]
    fn rotation_and_quality_accept_only_their_closed_sets() {
        let rotate = |value: serde_json::Value| spec_of(JobKind::PdfRotate, &[PDF], None, value);
        assert!(rotate(serde_json::json!({ "rotation": "90" })).is_ok());
        assert!(rotate(serde_json::json!({ "rotation": "270" })).is_ok());
        // Not a right angle, a free-form number, or missing entirely.
        assert!(rotate(serde_json::json!({ "rotation": "45" })).is_err());
        assert!(rotate(serde_json::json!({ "rotation": 90 })).is_err());
        assert_eq!(
            rotate(serde_json::Value::Null).err().unwrap().code,
            "INVALID_REQUEST"
        );

        let compress = |value: serde_json::Value| spec_of(JobKind::PdfCompress, &[PDF], None, value);
        assert!(compress(serde_json::json!({ "quality": "screen" })).is_ok());
        // Absent options default; an unknown preset does not.
        assert!(compress(serde_json::Value::Null).is_ok());
        assert!(compress(serde_json::json!({ "quality": "lossless" })).is_err());
        assert!(compress(serde_json::json!({ "quality": "-dEVIL" })).is_err());

        let ocr = |value: serde_json::Value| spec_of(JobKind::PdfOcr, &[PDF], None, value);
        assert!(ocr(serde_json::json!({ "language": "turkish" })).is_ok());
        assert!(ocr(serde_json::json!({ "language": "turkish_and_english" })).is_ok());
        assert!(ocr(serde_json::json!({ "language": "klingon" })).is_err());
        assert_eq!(OcrLanguage::default().code(), "tur+eng");
    }

    #[test]
    fn watermark_text_is_sanitized_and_opacity_is_bounded() {
        let watermark = |value: serde_json::Value| {
            spec_of(JobKind::PdfWatermark, &[PDF], None, value).map(|spec| match spec {
                JobSpec::PdfWatermark(s) => s,
                other => panic!("expected a watermark spec, got {other:?}"),
            })
        };
        let spec = watermark(serde_json::json!({ "text": "  Gizli\r\n  " })).unwrap();
        assert_eq!(spec.text, "Gizli");
        assert_eq!(spec.opacity, 0.3, "default opacity");

        // Text that is only whitespace or control characters says nothing.
        assert!(watermark(serde_json::json!({ "text": "\r\n\t" })).is_err());
        assert!(watermark(serde_json::json!({ "text": "" })).is_err());
        // Over-long text is truncated, not rejected.
        let long = "x".repeat(500);
        assert_eq!(
            watermark(serde_json::json!({ "text": long })).unwrap().text.chars().count(),
            120
        );

        for bad in [0.0, -1.0, 1.5, f32::NAN, f32::INFINITY] {
            let value = serde_json::json!({ "text": "Gizli", "opacity": bad });
            // NaN and infinity are not representable in JSON, so those
            // bodies are malformed; either way the request is refused.
            assert!(
                watermark(value).is_err() || !bad.is_finite(),
                "opacity {bad} was accepted"
            );
        }
        assert!(watermark(serde_json::json!({ "text": "Gizli", "opacity": 0.5 })).is_ok());
    }

    #[test]
    fn office_conversion_targets_documents_only() {
        let docx = [SourceFormat::Office(OfficeFormat::Docx)];
        let convert =
            |target: &str| spec_of(JobKind::OfficeConvert, &docx, Some(target), serde_json::Value::Null);

        let to_pdf = convert("pdf").unwrap();
        assert_eq!(to_pdf.output_extension(), "pdf");
        assert_eq!(to_pdf.output_name("mektup"), "mektup.pdf");
        assert_eq!(convert("odt").unwrap().output_extension(), "odt");

        // An image is not an Office conversion target, and converting a
        // DOCX to a DOCX would just be a copy.
        assert_eq!(
            convert("png").err().unwrap().code,
            "UNSUPPORTED_CONVERSION"
        );
        assert_eq!(
            convert("docx").err().unwrap().code,
            "UNSUPPORTED_CONVERSION"
        );
        assert_eq!(convert("exe").err().unwrap().code, "UNSUPPORTED_CONVERSION");
        // The target is required.
        assert_eq!(
            spec_of(JobKind::OfficeConvert, &docx, None, serde_json::Value::Null)
                .err()
                .unwrap()
                .code,
            "INVALID_REQUEST"
        );
        // And a PDF is not an Office input - that direction is a kind of
        // its own (`pdf_to_office`), not a variation of this one.
        assert!(spec_of(
            JobKind::OfficeConvert,
            &[PDF],
            Some("docx"),
            serde_json::Value::Null
        )
        .is_err());
    }

    #[test]
    fn an_office_conversion_never_crosses_a_document_class() {
        // A text document cannot become a spreadsheet: asking LibreOffice
        // for it does not produce a worse result, it produces a meaningless
        // one, so the API must refuse the pair rather than run it.
        let cross_class = [
            (OfficeFormat::Docx, "xlsx"),
            (OfficeFormat::Docx, "ods"),
            (OfficeFormat::Xlsx, "docx"),
            (OfficeFormat::Xlsx, "pptx"),
            (OfficeFormat::Pptx, "odt"),
            (OfficeFormat::Odp, "ods"),
        ];
        for (source, target) in cross_class {
            let result = spec_of(
                JobKind::OfficeConvert,
                &[SourceFormat::Office(source)],
                Some(target),
                serde_json::Value::Null,
            );
            assert_eq!(
                result.err().map(|e| e.code),
                Some("UNSUPPORTED_CONVERSION"),
                "{source:?} -> {target} must be refused"
            );
        }

        // Within a class, both directions work, and so does PDF - which is
        // exactly what the published matrix says.
        for source in OfficeFormat::ALL {
            let targets = office_conversion_targets(source);
            assert!(targets.contains(&SourceFormat::Pdf), "{source:?}");
            assert!(
                !targets.contains(&SourceFormat::Office(source)),
                "{source:?} must not convert to itself"
            );
            for target in &targets {
                let result = spec_of(
                    JobKind::OfficeConvert,
                    &[SourceFormat::Office(source)],
                    Some(target.canonical_extension()),
                    serde_json::Value::Null,
                );
                assert!(
                    result.is_ok(),
                    "{source:?} -> {target:?} is advertised but refused"
                );
            }
            // Every target is PDF or a format of the same class.
            for target in targets {
                match target {
                    SourceFormat::Pdf => {}
                    SourceFormat::Office(f) => {
                        assert_eq!(f.document_class(), source.document_class())
                    }
                    SourceFormat::Image(_) => panic!("an image is not a document target"),
                }
            }
        }
    }

    #[test]
    fn pdf_reconstruction_accepts_only_text_formats_and_labels_itself_experimental() {
        let reconstruct = |target: &str| {
            spec_of(
                JobKind::PdfToOffice,
                &[PDF],
                Some(target),
                serde_json::Value::Null,
            )
        };

        // The two the engine's PDF importer can actually target.
        for target in pdf_reconstruction_targets() {
            let spec = reconstruct(target.canonical_extension()).unwrap();
            assert_eq!(spec.output_extension(), target.canonical_extension());
            assert_eq!(spec.output_mime(), target.mime_type());
            // The user is told in the one place they cannot miss.
            assert_eq!(
                spec.output_name("rapor"),
                format!("rapor_deneysel.{}", target.canonical_extension())
            );
        }
        assert!(JobKind::PdfToOffice.is_experimental());

        // Nothing reconstructs a spreadsheet or a slide deck from a PDF, so
        // those are refused rather than silently exported as something else.
        for target in ["xlsx", "ods", "pptx", "odp", "pdf", "png", "exe"] {
            assert_eq!(
                reconstruct(target).err().map(|e| e.code),
                Some("UNSUPPORTED_CONVERSION"),
                "{target} must not be a reconstruction target"
            );
        }
        // The target is required, and the input must be a PDF.
        assert_eq!(
            spec_of(JobKind::PdfToOffice, &[PDF], None, serde_json::Value::Null)
                .err()
                .unwrap()
                .code,
            "INVALID_REQUEST"
        );
        assert!(spec_of(
            JobKind::PdfToOffice,
            &[SourceFormat::Office(OfficeFormat::Docx)],
            Some("docx"),
            serde_json::Value::Null
        )
        .is_err());
    }

    #[test]
    fn only_reconstruction_is_experimental() {
        // A label that drifted onto a faithful operation would be as
        // misleading as one missing from an unreliable one.
        for kind in JobKind::ALL {
            assert_eq!(
                kind.is_experimental(),
                kind == JobKind::PdfToOffice,
                "{}",
                kind.wire()
            );
        }
    }

    #[test]
    fn every_kind_names_its_output_distinctly() {
        // No two kinds may produce the same download name from one stem:
        // a user converting and then compressing the same file must not end
        // up with two different documents under one name.
        let specs = [
            spec_of(JobKind::PdfMerge, &[PDF, PDF], None, serde_json::Value::Null).unwrap(),
            spec_of(JobKind::PdfSplit, &[PDF], None, serde_json::Value::Null).unwrap(),
            spec_of(JobKind::PdfCompress, &[PDF], None, serde_json::Value::Null).unwrap(),
            spec_of(
                JobKind::PdfRotate,
                &[PDF],
                None,
                serde_json::json!({ "rotation": "90" }),
            )
            .unwrap(),
            spec_of(
                JobKind::PdfWatermark,
                &[PDF],
                None,
                serde_json::json!({ "text": "Gizli" }),
            )
            .unwrap(),
            spec_of(JobKind::PdfOcr, &[PDF], None, serde_json::Value::Null).unwrap(),
            spec_of(
                JobKind::PdfToOffice,
                &[PDF],
                Some("docx"),
                serde_json::Value::Null,
            )
            .unwrap(),
            spec_of(
                JobKind::PdfProtect,
                &[PDF],
                None,
                serde_json::json!({ "password": "parola" }),
            )
            .unwrap(),
            spec_of(JobKind::PdfUnlock, &[PDF], None, serde_json::Value::Null).unwrap(),
            spec_of(
                JobKind::PdfMetadataStrip,
                &[PDF],
                None,
                serde_json::Value::Null,
            )
            .unwrap(),
            spec_of(
                JobKind::PdfPageNumbers,
                &[PDF],
                None,
                serde_json::Value::Null,
            )
            .unwrap(),
            spec_of(
                JobKind::PdfDeletePages,
                &[PDF],
                None,
                serde_json::json!({ "pages": [2] }),
            )
            .unwrap(),
            spec_of(
                JobKind::PdfReorderPages,
                &[PDF],
                None,
                serde_json::json!({ "order": [2, 1] }),
            )
            .unwrap(),
            spec_of(
                JobKind::PdfExtractText,
                &[PDF],
                None,
                serde_json::Value::Null,
            )
            .unwrap(),
        ];
        let names: Vec<String> = specs.iter().map(|s| s.output_name("rapor")).collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "colliding output names: {names:?}");
    }

    #[test]
    fn a_pdf_output_must_really_be_a_pdf() {
        let spec = spec_of(JobKind::PdfMerge, &[PDF, PDF], None, serde_json::Value::Null).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "meb_spec_pdf_out_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let produced = dir.join("result.pdf");

        std::fs::write(&produced, b"%PDF-1.7\ntrailer\n%%EOF\n").unwrap();
        assert!(spec.validate_output(&produced).is_ok());

        // An engine that exited 0 but wrote something else, or a truncated
        // PDF, is never published.
        std::fs::write(&produced, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(spec.validate_output(&produced).is_err());
        std::fs::write(&produced, b"%PDF-1.7\nno trailer here").unwrap();
        assert!(spec.validate_output(&produced).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_kind_refuses_the_wrong_number_of_inputs() {
        // `convert` works on one image; naming several is a malformed
        // request rather than a silent "first one wins".
        assert!(JobKind::ImageConvert.accepts_input_count(1));
        assert!(!JobKind::ImageConvert.accepts_input_count(0));
        assert!(!JobKind::ImageConvert.accepts_input_count(2));

        let two = CreateJobRequest {
            kind: "convert".to_string(),
            file_id: None,
            file_ids: Some(vec!["a".to_string(), "b".to_string()]),
            output_format: Some("png".to_string()),
            options: None,
        };
        assert_eq!(
            two.input_ids(JobKind::ImageConvert).err().unwrap().code,
            "INVALID_REQUEST"
        );

        // Neither form, and both forms, are equally unusable.
        let neither = CreateJobRequest {
            kind: "convert".to_string(),
            file_id: None,
            file_ids: None,
            output_format: Some("png".to_string()),
            options: None,
        };
        assert!(neither.input_ids(JobKind::ImageConvert).is_err());
        let both = CreateJobRequest {
            kind: "convert".to_string(),
            file_id: Some("a".to_string()),
            file_ids: Some(vec!["b".to_string()]),
            output_format: Some("png".to_string()),
            options: None,
        };
        assert!(both.input_ids(JobKind::ImageConvert).is_err());

        let one = CreateJobRequest {
            kind: "convert".to_string(),
            file_id: Some("a".to_string()),
            file_ids: None,
            output_format: Some("png".to_string()),
            options: None,
        };
        assert_eq!(one.input_ids(JobKind::ImageConvert).unwrap(), ["a"]);
    }

    /// The seven kinds that need no external engine. Their options are the
    /// only thing between a client and an in-process document rewrite, so
    /// each one's rules are checked here rather than in the engine.
    #[test]
    fn the_in_process_pdf_operations_validate_their_own_options() {
        let spec = |kind: JobKind, options: serde_json::Value| spec_of(kind, &[PDF], None, options);

        // A protect job must say what the password is; an empty one, a
        // control character, and an over-long one are all refused.
        assert!(spec(JobKind::PdfProtect, serde_json::json!({ "password": "açık kapı" })).is_ok());
        assert!(spec(
            JobKind::PdfProtect,
            serde_json::json!({ "password": "a", "owner_password": "b" })
        )
        .is_ok());
        for bad in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({ "password": "" }),
            serde_json::json!({ "password": "pa\u{7}rola" }),
            serde_json::json!({ "password": "x".repeat(MAX_PASSWORD_CHARS + 1) }),
            // Not this kind's schema.
            serde_json::json!({ "pass": "parola" }),
        ] {
            assert_eq!(
                spec(JobKind::PdfProtect, bad.clone()).err().map(|e| e.code),
                Some("INVALID_REQUEST"),
                "accepted {bad}"
            );
        }
        // Whitespace is significant in a password and is not trimmed away.
        let padded = spec(JobKind::PdfProtect, serde_json::json!({ "password": " p " })).unwrap();
        match padded {
            JobSpec::PdfProtect(s) => {
                assert_eq!(s.user_password, " p ");
                // One password given means it is both passwords.
                assert_eq!(s.owner_password, " p ");
            }
            other => panic!("expected a protect spec, got {other:?}"),
        }

        // Unlocking a PDF that only carries restrictions needs no password,
        // so absent options are valid here.
        assert!(spec(JobKind::PdfUnlock, serde_json::Value::Null).is_ok());
        assert!(spec(JobKind::PdfUnlock, serde_json::json!({ "password": "p" })).is_ok());
        assert!(spec(JobKind::PdfUnlock, serde_json::json!({ "password": "" })).is_err());

        // Two kinds take nothing at all.
        for kind in [JobKind::PdfMetadataStrip, JobKind::PdfExtractText] {
            assert!(spec(kind, serde_json::Value::Null).is_ok());
            assert_eq!(
                spec(kind, serde_json::json!({ "pages": [1] }))
                    .err()
                    .map(|e| e.code),
                Some("INVALID_REQUEST"),
                "{} accepted options",
                kind.wire()
            );
        }

        // Page numbers: a closed position set and a bounded start.
        assert!(spec(JobKind::PdfPageNumbers, serde_json::Value::Null).is_ok());
        assert!(spec(
            JobKind::PdfPageNumbers,
            serde_json::json!({ "position": "top_right", "start_at": 12 })
        )
        .is_ok());
        for bad in [
            serde_json::json!({ "position": "middle" }),
            serde_json::json!({ "position": "bottom center" }),
            serde_json::json!({ "start_at": 0 }),
            serde_json::json!({ "start_at": MAX_PDF_PAGE + 1 }),
        ] {
            assert!(
                spec(JobKind::PdfPageNumbers, bad.clone()).is_err(),
                "accepted {bad}"
            );
        }
        let default = spec(JobKind::PdfPageNumbers, serde_json::Value::Null).unwrap();
        match default {
            JobSpec::PdfPageNumbers(s) => {
                assert_eq!(s.start_at, 1);
                assert_eq!(s.position, PageNumberPosition::BottomCenter);
            }
            other => panic!("expected a page-numbers spec, got {other:?}"),
        }

        // Deleting pages: the same normalization every page list gets, and
        // a list is required - a delete with no pages would be a copy.
        let deleted = spec(
            JobKind::PdfDeletePages,
            serde_json::json!({ "pages": [3, 1, 3] }),
        )
        .unwrap();
        match deleted {
            JobSpec::PdfDeletePages(s) => assert_eq!(s.pages, vec![1, 3]),
            other => panic!("expected a delete spec, got {other:?}"),
        }
        for bad in [
            serde_json::Value::Null,
            serde_json::json!({ "pages": [] }),
            serde_json::json!({ "pages": [0] }),
            serde_json::json!({ "pages": [MAX_PDF_PAGE + 1] }),
        ] {
            assert!(
                spec(JobKind::PdfDeletePages, bad.clone()).is_err(),
                "accepted {bad}"
            );
        }

        // Reordering: the order must be a permutation of 1..=n. A list that
        // dropped or repeated a page is a different operation, not a
        // reorder, so it is refused rather than reinterpreted.
        assert!(spec(JobKind::PdfReorderPages, serde_json::json!({ "order": [1] })).is_ok());
        let reordered =
            spec(JobKind::PdfReorderPages, serde_json::json!({ "order": [3, 1, 2] })).unwrap();
        match reordered {
            // Kept in the requested order, NOT sorted: the order is the
            // instruction.
            JobSpec::PdfReorderPages(s) => assert_eq!(s.order, vec![3, 1, 2]),
            other => panic!("expected a reorder spec, got {other:?}"),
        }
        for bad in [
            serde_json::Value::Null,
            serde_json::json!({ "order": [] }),
            // Repeats a page.
            serde_json::json!({ "order": [1, 1, 2] }),
            // Drops page 2.
            serde_json::json!({ "order": [1, 3] }),
            // 1-based, like every other page number in this API.
            serde_json::json!({ "order": [0, 1] }),
        ] {
            assert!(
                spec(JobKind::PdfReorderPages, bad.clone()).is_err(),
                "accepted {bad}"
            );
        }

        // And all seven work on PDFs only.
        for kind in [
            JobKind::PdfProtect,
            JobKind::PdfUnlock,
            JobKind::PdfMetadataStrip,
            JobKind::PdfPageNumbers,
            JobKind::PdfDeletePages,
            JobKind::PdfReorderPages,
            JobKind::PdfExtractText,
        ] {
            let options = match kind {
                JobKind::PdfProtect => serde_json::json!({ "password": "p" }),
                JobKind::PdfDeletePages => serde_json::json!({ "pages": [1] }),
                JobKind::PdfReorderPages => serde_json::json!({ "order": [1] }),
                _ => serde_json::Value::Null,
            };
            let refused = spec_of(
                kind,
                &[SourceFormat::Office(OfficeFormat::Docx)],
                None,
                options,
            );
            assert_eq!(
                refused.err().map(|e| e.code),
                Some("UNSUPPORTED_CONVERSION"),
                "{} accepted a DOCX",
                kind.wire()
            );
        }
    }

    #[test]
    fn extracted_text_is_the_one_job_that_does_not_produce_a_document() {
        let spec = spec_of(
            JobKind::PdfExtractText,
            &[PDF],
            None,
            serde_json::Value::Null,
        )
        .unwrap();
        assert_eq!(spec.output_extension(), "txt");
        assert_eq!(spec.output_mime(), "text/plain; charset=utf-8");
        assert_eq!(spec.output_name("rapor"), "rapor_metin.txt");

        let dir = std::env::temp_dir().join(format!(
            "meb_spec_text_out_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let produced = dir.join("result.txt");

        std::fs::write(&produced, "Öğrenci listesi\n").unwrap();
        assert!(spec.validate_output(&produced).is_ok());
        // An extraction that produced nothing, or bytes that are not text,
        // is never published as a success.
        std::fs::write(&produced, b"").unwrap();
        assert!(spec.validate_output(&produced).is_err());
        std::fs::write(&produced, [0xff, 0xfe, 0x00]).unwrap();
        assert!(spec.validate_output(&produced).is_err());
        assert!(spec.validate_output(&dir.join("absent.txt")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_dimension_and_quality_bounds_are_enforced() {
        let with = |options: serde_json::Value| {
            JobSpec::from_request(
                JobKind::ImageConvert,
                &request("convert", Some("png"), options),
                &[SourceFormat::Image(NativeImageFormat::Jpeg)],
            )
        };
        assert!(with(serde_json::json!({ "quality": 80 })).is_ok());
        assert!(with(serde_json::json!({ "quality": 0 })).is_err());
        assert!(with(serde_json::json!({ "quality": 101 })).is_err());
        assert!(with(serde_json::json!({ "width": 10, "height": 10 })).is_ok());
        // A half-specified resize is ambiguous, not a default.
        assert_eq!(
            with(serde_json::json!({ "width": 10 }))
                .err()
                .unwrap()
                .code,
            "INVALID_DIMENSIONS"
        );
        assert!(with(serde_json::json!({ "width": 0, "height": 10 })).is_err());
        assert!(with(serde_json::json!({ "width": 999_999, "height": 999_999 })).is_err());
    }
}
