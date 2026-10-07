//! Structural validation of an untrusted PDF or Office file, before any
//! engine is allowed near it.
//!
//! This is the document-side counterpart of `image::probe_file`: cheap, no
//! rendering, no external tool, and it answers one question - *is this file
//! really the format its name claims?* The upload path needs that answer
//! because a container sniff (`format::Container`) cannot distinguish DOCX
//! from XLSX from ODT: all three are ZIP archives.
//!
//! What each probe actually establishes:
//!
//! - **Office**: the ZIP central directory is readable and names the part
//!   that defines the format - `word/document.xml`, `xl/workbook.xml` or
//!   `ppt/presentation.xml` for OOXML; for OpenDocument, the `mimetype`
//!   member's contents. This is the same evidence LibreOffice itself uses,
//!   so a file that passes is one LibreOffice will recognize as that type.
//! - **PDF**: the `%PDF-` header and a trailer marker. This is a
//!   WELL-FORMEDNESS floor, not a guarantee the document is undamaged: a
//!   PDF with a valid skeleton and a corrupt body is only caught by the
//!   engine, which fails that job. Page count and encryption are
//!   deliberately NOT determined here - that needs a real PDF parser, and
//!   running one on untrusted input is the engine's job, not the upload
//!   path's.
//!
//! Anything that fails gets a stable error code; the host maps it to a
//! user-facing message and never surfaces the detail.

use crate::format::{OfficeFormat, SourceFormat};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How far back from the end of a PDF the trailer marker is looked for.
/// `%%EOF` is the last token of a conforming file; some writers leave a
/// little padding after it.
const PDF_TRAILER_WINDOW: usize = 2048;

/// Cap on the `mimetype` member read from an OpenDocument file. The longest
/// value is 47 bytes; anything longer is not one of them.
const ODF_MIMETYPE_MAX: usize = 128;

/// Entries examined in a ZIP's central directory. Far above any real Office
/// file, and a bound on the work an archive with absurdly many members can
/// cause.
const MAX_ZIP_ENTRIES: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentErrorKind {
    /// The bytes are not the container the claim requires at all.
    NotADocument,
    /// A readable document, but not the format the name claimed.
    TypeMismatch,
    /// The container itself is damaged or truncated.
    Corrupt,
}

#[derive(Debug, Clone)]
pub struct DocumentError {
    kind: DocumentErrorKind,
    detail: Option<String>,
}

impl DocumentError {
    pub fn new(kind: DocumentErrorKind) -> DocumentError {
        DocumentError { kind, detail: None }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> DocumentError {
        self.detail = Some(detail.into());
        self
    }

    pub fn kind(&self) -> DocumentErrorKind {
        self.kind
    }

    /// Stable code for the host's error mapping. Never user-facing text.
    pub fn code(&self) -> &'static str {
        match self.kind {
            DocumentErrorKind::NotADocument => "UNSUPPORTED_FILE_TYPE",
            DocumentErrorKind::TypeMismatch => "FILE_TYPE_MISMATCH",
            DocumentErrorKind::Corrupt => "FILE_CORRUPT",
        }
    }

    /// Server-log detail only.
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl std::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code())?;
        if let Some(detail) = &self.detail {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

/// What a document probe established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentProbe {
    /// The format the CONTENT turned out to be - not the claim.
    pub format: SourceFormat,
}

/// Verifies that `path` really holds `claimed`, which must be a PDF or an
/// Office format (images go through `image::probe_file`).
pub fn probe_file(path: &Path, claimed: SourceFormat) -> Result<DocumentProbe, DocumentError> {
    match claimed {
        SourceFormat::Pdf => probe_pdf(path),
        SourceFormat::Office(office) => probe_office(path, office),
        SourceFormat::Image(_) => Err(DocumentError::new(DocumentErrorKind::NotADocument)
            .with_detail("an image was passed to the document probe")),
    }
}

fn io_failed(e: std::io::Error) -> DocumentError {
    DocumentError::new(DocumentErrorKind::Corrupt).with_detail(e.to_string())
}

fn probe_pdf(path: &Path) -> Result<DocumentProbe, DocumentError> {
    let mut file = std::fs::File::open(path).map_err(io_failed)?;
    let length = file.metadata().map_err(io_failed)?.len();

    let mut header = [0u8; 8];
    let read = read_at_most(&mut file, &mut header).map_err(io_failed)?;
    if !header[..read].starts_with(b"%PDF-") {
        return Err(DocumentError::new(DocumentErrorKind::TypeMismatch)
            .with_detail("missing %PDF- header"));
    }

    // The trailer marker proves the writer finished the file, which is the
    // one cheap check that catches a truncated upload the byte counter
    // could not (a client that declared exactly what it sent).
    let window = PDF_TRAILER_WINDOW.min(length as usize);
    let mut tail = vec![0u8; window];
    file.seek(SeekFrom::End(-(window as i64))).map_err(io_failed)?;
    let read = read_at_most(&mut file, &mut tail).map_err(io_failed)?;
    if !contains(&tail[..read], b"%%EOF") {
        return Err(DocumentError::new(DocumentErrorKind::Corrupt)
            .with_detail("no %%EOF trailer: truncated or incomplete PDF"));
    }
    Ok(DocumentProbe {
        format: SourceFormat::Pdf,
    })
}

fn probe_office(path: &Path, claimed: OfficeFormat) -> Result<DocumentProbe, DocumentError> {
    let file = std::fs::File::open(path).map_err(io_failed)?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| {
        // A ZIP whose central directory will not parse is not usable by any
        // Office engine either. (An encrypted OOXML file is an OLE wrapper,
        // not a ZIP, so it never reaches this point.)
        DocumentError::new(DocumentErrorKind::Corrupt).with_detail(e.to_string())
    })?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(DocumentError::new(DocumentErrorKind::Corrupt)
            .with_detail(format!("{} entries is not a document", archive.len())));
    }

    let found = if claimed.is_opendocument() {
        opendocument_format(&mut archive)?
    } else {
        ooxml_format(&archive)?
    };
    if found != claimed {
        return Err(DocumentError::new(DocumentErrorKind::TypeMismatch).with_detail(format!(
            "content is {}, name claimed {}",
            found.canonical_extension(),
            claimed.canonical_extension()
        )));
    }
    Ok(DocumentProbe {
        format: SourceFormat::Office(found),
    })
}

/// The OOXML part that defines which application owns the document. Exactly
/// one of the three may be present; a ZIP with none of them is not OOXML.
fn ooxml_format<R: Read + Seek>(
    archive: &zip::ZipArchive<R>,
) -> Result<OfficeFormat, DocumentError> {
    let mut found: Option<OfficeFormat> = None;
    for name in archive.file_names() {
        let format = match name {
            "word/document.xml" => OfficeFormat::Docx,
            "xl/workbook.xml" => OfficeFormat::Xlsx,
            "ppt/presentation.xml" => OfficeFormat::Pptx,
            _ => continue,
        };
        if found.is_some_and(|already| already != format) {
            return Err(DocumentError::new(DocumentErrorKind::Corrupt)
                .with_detail("archive claims to be more than one Office format"));
        }
        found = Some(format);
    }
    found.ok_or_else(|| {
        DocumentError::new(DocumentErrorKind::TypeMismatch)
            .with_detail("ZIP holds no Office document part")
    })
}

/// OpenDocument files declare their type in a `mimetype` member, which the
/// format requires to be first and stored uncompressed.
fn opendocument_format<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<OfficeFormat, DocumentError> {
    let entry = archive.by_name("mimetype").map_err(|_| {
        DocumentError::new(DocumentErrorKind::TypeMismatch)
            .with_detail("ZIP has no mimetype member")
    })?;
    let mut declared = Vec::new();
    entry
        .take(ODF_MIMETYPE_MAX as u64)
        .read_to_end(&mut declared)
        .map_err(io_failed)?;
    let declared = String::from_utf8_lossy(&declared);
    let declared = declared.trim();
    OfficeFormat::ALL
        .into_iter()
        .filter(|f| f.is_opendocument())
        .find(|f| f.mime_type() == declared)
        .ok_or_else(|| {
            DocumentError::new(DocumentErrorKind::TypeMismatch)
                .with_detail("mimetype is not an OpenDocument type")
        })
}

/// Reads until the buffer is full or the file ends. `Read::read` may return
/// fewer bytes than asked for without being at EOF.
fn read_at_most<R: Read>(reader: &mut R, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meb_document_probe_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    /// A minimal ZIP holding the given (name, contents) members. `stored`
    /// members are written uncompressed, as OpenDocument requires of
    /// `mimetype`.
    fn zip_with(path: &Path, members: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, contents) in members {
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
    }

    fn cleanup(path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn a_well_formed_pdf_passes_and_a_truncated_one_does_not() {
        let path = temp_path("doc.pdf");
        write(&path, b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\ntrailer\n%%EOF\n");
        assert_eq!(
            probe_file(&path, SourceFormat::Pdf).unwrap().format,
            SourceFormat::Pdf
        );

        // Header present, trailer missing: the upload stopped early.
        write(&path, b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n");
        let err = probe_file(&path, SourceFormat::Pdf).err().unwrap();
        assert_eq!(err.kind(), DocumentErrorKind::Corrupt);
        assert_eq!(err.code(), "FILE_CORRUPT");
        cleanup(&path);
    }

    #[test]
    fn a_file_renamed_to_pdf_is_rejected() {
        let path = temp_path("fake.pdf");
        write(&path, b"MZ\x90\x00this is an executable\n%%EOF");
        let err = probe_file(&path, SourceFormat::Pdf).err().unwrap();
        assert_eq!(err.kind(), DocumentErrorKind::TypeMismatch);
        cleanup(&path);
    }

    #[test]
    fn ooxml_type_comes_from_its_parts_not_its_name() {
        let path = temp_path("sheet.xlsx");
        zip_with(
            &path,
            &[
                ("[Content_Types].xml", b"<Types/>"),
                ("xl/workbook.xml", b"<workbook/>"),
            ],
        );
        assert_eq!(
            probe_file(&path, SourceFormat::Office(OfficeFormat::Xlsx))
                .unwrap()
                .format,
            SourceFormat::Office(OfficeFormat::Xlsx)
        );

        // The same bytes claimed as a DOCX: the parts say otherwise.
        let err = probe_file(&path, SourceFormat::Office(OfficeFormat::Docx))
            .err()
            .unwrap();
        assert_eq!(err.kind(), DocumentErrorKind::TypeMismatch);
        assert_eq!(err.code(), "FILE_TYPE_MISMATCH");
        cleanup(&path);
    }

    #[test]
    fn a_plain_zip_is_not_an_office_document() {
        let path = temp_path("stuff.docx");
        zip_with(&path, &[("notes.txt", b"just a zip")]);
        let err = probe_file(&path, SourceFormat::Office(OfficeFormat::Docx))
            .err()
            .unwrap();
        assert_eq!(err.kind(), DocumentErrorKind::TypeMismatch);
        cleanup(&path);
    }

    #[test]
    fn opendocument_type_comes_from_its_mimetype_member() {
        let path = temp_path("text.odt");
        zip_with(
            &path,
            &[
                ("mimetype", OfficeFormat::Odt.mime_type().as_bytes()),
                ("content.xml", b"<document/>"),
            ],
        );
        assert_eq!(
            probe_file(&path, SourceFormat::Office(OfficeFormat::Odt))
                .unwrap()
                .format,
            SourceFormat::Office(OfficeFormat::Odt)
        );
        // Claimed as a spreadsheet; the mimetype member says text.
        assert_eq!(
            probe_file(&path, SourceFormat::Office(OfficeFormat::Ods))
                .err()
                .unwrap()
                .kind(),
            DocumentErrorKind::TypeMismatch
        );
        cleanup(&path);
    }

    #[test]
    fn a_damaged_container_is_corrupt_not_a_mismatch() {
        let path = temp_path("broken.docx");
        write(&path, b"PK\x03\x04 and then nothing that parses");
        let err = probe_file(&path, SourceFormat::Office(OfficeFormat::Docx))
            .err()
            .unwrap();
        assert_eq!(err.kind(), DocumentErrorKind::Corrupt);
        cleanup(&path);
    }

    #[test]
    fn an_image_never_reaches_the_document_probe() {
        let path = temp_path("photo.png");
        write(&path, b"\x89PNG\r\n\x1a\n");
        let err = probe_file(
            &path,
            SourceFormat::Image(crate::image::NativeImageFormat::Png),
        )
        .err()
        .unwrap();
        assert_eq!(err.kind(), DocumentErrorKind::NotADocument);
        cleanup(&path);
    }
}
