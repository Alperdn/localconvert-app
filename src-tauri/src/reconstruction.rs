//! PDF -> Office is a fundamentally different problem from Office -> PDF,
//! and this module exists to keep that boundary explicit in code instead
//! of letting both directions silently share one code path.
//!
//! Office -> PDF is *rendering*: LibreOffice already knows how to lay out
//! a DOCX/XLSX/PPTX and print it to PDF faithfully. See `engines::office`.
//!
//! PDF -> Office is *reconstruction*: inferring editable structure
//! (paragraphs, table cells, slide shapes) back out of a format that only
//! records fixed-position drawing operations. There is no general
//! solution to this, and the only implementation available in this
//! codebase today is LibreOffice's own PDF importer
//! (`--infilter=writer_pdf_import`), which is a real, working feature -
//! not a stub - but with real, explainable limits: it does reasonably
//! well on a text-based PDF and does not meaningfully recover structure
//! from a scanned/rasterized one.
//!
//! `PdfToXlsx` and `PdfToPptx` have no such importer to lean on - nothing
//! in this codebase (or a plausible LibreOffice call) reconstructs a
//! spreadsheet or a slide deck from a PDF - so they fail with a clear
//! `FEATURE_NOT_IMPLEMENTED` instead of quietly falling through to
//! whatever LibreOffice happens to do with an unrecognized target format
//! (which, before this module existed, was a silent, mislabeled TXT
//! export - see the `_ => ("txt", "txt")` fallback this replaces).

use crate::engines::{error::EngineError, office};
use std::path::Path;

/// How much to trust a PDF -> Office conversion path, independent of
/// whether the underlying call "succeeds" in the technical sense. Not
/// consumed by any UI yet (Phase 2 work) - documents the classification
/// this step's audit assigned to each direction (see module docs above)
/// as a compiled-checked fact rather than only a comment.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconstructionQuality {
    /// Backed by a real conversion path with known, explainable limitations.
    Partial,
    /// No working implementation exists.
    NotImplemented,
}

pub struct PdfToDocx;
pub struct PdfToXlsx;
pub struct PdfToPptx;

impl PdfToDocx {
    #[allow(dead_code)]
    pub const QUALITY: ReconstructionQuality = ReconstructionQuality::Partial;

    /// Backed today by LibreOffice's `writer_pdf_import` filter. `filter`
    /// is the concrete LibreOffice export filter for the requested output
    /// extension (docx/doc/odt/rtf/html/txt/epub).
    pub fn convert(input: &str, output: &str, filter: &str, job_dir: &Path) -> Result<String, EngineError> {
        office::convert(input, output, filter, true, job_dir)
    }
}

impl PdfToXlsx {
    #[allow(dead_code)]
    pub const QUALITY: ReconstructionQuality = ReconstructionQuality::NotImplemented;

    pub fn convert(_input: &str, _output: &str, _job_dir: &Path) -> Result<String, EngineError> {
        Err(EngineError::feature_not_implemented(None, "PDF to Excel conversion"))
    }
}

impl PdfToPptx {
    #[allow(dead_code)]
    pub const QUALITY: ReconstructionQuality = ReconstructionQuality::NotImplemented;

    pub fn convert(_input: &str, _output: &str, _job_dir: &Path) -> Result<String, EngineError> {
        Err(EngineError::feature_not_implemented(None, "PDF to PowerPoint conversion"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_to_xlsx_never_attempts_a_conversion() {
        let err = PdfToXlsx::convert("in.pdf", "out.xlsx", Path::new(".")).unwrap_err();
        assert_eq!(err.code(), "FEATURE_NOT_IMPLEMENTED");
    }

    #[test]
    fn pdf_to_pptx_never_attempts_a_conversion() {
        let err = PdfToPptx::convert("in.pdf", "out.pptx", Path::new(".")).unwrap_err();
        assert_eq!(err.code(), "FEATURE_NOT_IMPLEMENTED");
    }
}
