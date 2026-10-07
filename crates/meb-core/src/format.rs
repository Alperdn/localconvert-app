//! What a file IS - the shared vocabulary both hosts use to talk about an
//! input before any engine touches it.
//!
//! `NativeImageFormat` (in `crate::image`) describes only what the raster
//! pipeline handles. `SourceFormat` is the wider set an input may belong to:
//! an image, a PDF, or an Office document. It exists so the layers above
//! (upload admission, job routing, capability reporting) can speak about a
//! DOCX or a PDF without pretending it is an image.
//!
//! Identity is never taken from a client-declared MIME type. A name's
//! extension is a CLAIM (`from_extension`); the claim is accepted only once
//! the bytes agree with it - cheaply, from the leading bytes
//! (`Container::sniff`), and then properly by `crate::document::probe_file`
//! or `crate::image::probe_file`.

use crate::image::NativeImageFormat;

/// The engine family an input belongs to. One category, one pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatCategory {
    Image,
    Pdf,
    Office,
}

/// Office formats this project converts: the OOXML trio and their
/// OpenDocument equivalents. Legacy binary `.doc`/`.xls`/`.ppt` are
/// deliberately absent - they are a different container (OLE) and are not
/// admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OfficeFormat {
    Docx,
    Xlsx,
    Pptx,
    Odt,
    Ods,
    Odp,
}

impl OfficeFormat {
    pub const ALL: [OfficeFormat; 6] = [
        OfficeFormat::Docx,
        OfficeFormat::Xlsx,
        OfficeFormat::Pptx,
        OfficeFormat::Odt,
        OfficeFormat::Ods,
        OfficeFormat::Odp,
    ];

    pub fn canonical_extension(self) -> &'static str {
        match self {
            OfficeFormat::Docx => "docx",
            OfficeFormat::Xlsx => "xlsx",
            OfficeFormat::Pptx => "pptx",
            OfficeFormat::Odt => "odt",
            OfficeFormat::Ods => "ods",
            OfficeFormat::Odp => "odp",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            OfficeFormat::Docx => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            }
            OfficeFormat::Xlsx => {
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            }
            OfficeFormat::Pptx => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }
            OfficeFormat::Odt => "application/vnd.oasis.opendocument.text",
            OfficeFormat::Ods => "application/vnd.oasis.opendocument.spreadsheet",
            OfficeFormat::Odp => "application/vnd.oasis.opendocument.presentation",
        }
    }

    /// The LibreOffice export filter that writes this format.
    pub fn libreoffice_filter(self) -> &'static str {
        // For these six the filter name happens to equal the extension;
        // spelled out so a future format with a different filter (e.g.
        // "writer8") has an obvious place to go.
        self.canonical_extension()
    }

    /// Whether this is an OpenDocument (not OOXML) format. The two families
    /// are told apart differently inside their shared ZIP container.
    pub fn is_opendocument(self) -> bool {
        matches!(
            self,
            OfficeFormat::Odt | OfficeFormat::Ods | OfficeFormat::Odp
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceFormat {
    Image(NativeImageFormat),
    Pdf,
    Office(OfficeFormat),
}

impl SourceFormat {
    /// Interprets a file name's extension as a CLAIM about the content. The
    /// claim still has to survive a probe before anything trusts it.
    pub fn from_extension(ext: &str) -> Option<SourceFormat> {
        let lower = ext.to_lowercase();
        if let Some(image) = NativeImageFormat::from_extension(&lower) {
            return Some(SourceFormat::Image(image));
        }
        match lower.as_str() {
            "pdf" => Some(SourceFormat::Pdf),
            "docx" => Some(SourceFormat::Office(OfficeFormat::Docx)),
            "xlsx" => Some(SourceFormat::Office(OfficeFormat::Xlsx)),
            "pptx" => Some(SourceFormat::Office(OfficeFormat::Pptx)),
            "odt" => Some(SourceFormat::Office(OfficeFormat::Odt)),
            "ods" => Some(SourceFormat::Office(OfficeFormat::Ods)),
            "odp" => Some(SourceFormat::Office(OfficeFormat::Odp)),
            _ => None,
        }
    }

    pub fn category(self) -> FormatCategory {
        match self {
            SourceFormat::Image(_) => FormatCategory::Image,
            SourceFormat::Pdf => FormatCategory::Pdf,
            SourceFormat::Office(_) => FormatCategory::Office,
        }
    }

    /// The one extension server-side file names use for this format.
    pub fn canonical_extension(self) -> &'static str {
        match self {
            SourceFormat::Image(f) => f.canonical_extension(),
            SourceFormat::Pdf => "pdf",
            SourceFormat::Office(f) => f.canonical_extension(),
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            SourceFormat::Image(f) => f.mime_type(),
            SourceFormat::Pdf => "application/pdf",
            SourceFormat::Office(f) => f.mime_type(),
        }
    }

    pub fn image(self) -> Option<NativeImageFormat> {
        match self {
            SourceFormat::Image(f) => Some(f),
            _ => None,
        }
    }

    pub fn office(self) -> Option<OfficeFormat> {
        match self {
            SourceFormat::Office(f) => Some(f),
            _ => None,
        }
    }
}

/// The outer wrapper a file's first bytes reveal.
///
/// This is all that can be known from a short prefix: every OOXML and
/// OpenDocument file is a ZIP, so the container narrows an upload to a
/// family, and the exact format is settled by
/// `crate::document::probe_file` once the whole file is on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// Magic bytes of a raster image the native pipeline recognizes.
    Image(NativeImageFormat),
    Pdf,
    Zip,
}

/// Bytes `Container::sniff` needs: the longest signature it checks is 8.
pub const CONTAINER_SNIFF_BYTES: usize = 16;

impl Container {
    /// Identifies the container from a file's leading bytes. `None` means
    /// "not something this project opens" (or too few bytes to tell).
    pub fn sniff(head: &[u8]) -> Option<Container> {
        if head.starts_with(b"%PDF-") {
            return Some(Container::Pdf);
        }
        // Local file header of a non-empty ZIP. An empty archive
        // (`PK\x05\x06`) carries no document, so it is not admitted.
        if head.starts_with(b"PK\x03\x04") {
            return Some(Container::Zip);
        }
        crate::image::sniff_format(head).map(Container::Image)
    }

    /// Whether a file in this container could be `claimed`. True does NOT
    /// mean the file IS that format - for a ZIP it cannot, since DOCX, XLSX
    /// and ODT share one container - only that it is not already excluded.
    pub fn could_hold(self, claimed: SourceFormat) -> bool {
        match (self, claimed) {
            (Container::Image(sniffed), SourceFormat::Image(c)) => sniffed == c,
            (Container::Pdf, SourceFormat::Pdf) => true,
            (Container::Zip, SourceFormat::Office(_)) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_categories_and_back() {
        for (ext, category) in [
            ("jpg", FormatCategory::Image),
            ("JPEG", FormatCategory::Image),
            ("png", FormatCategory::Image),
            ("pdf", FormatCategory::Pdf),
            ("PDF", FormatCategory::Pdf),
            ("docx", FormatCategory::Office),
            ("odp", FormatCategory::Office),
        ] {
            let format = SourceFormat::from_extension(ext)
                .unwrap_or_else(|| panic!("{ext} should be a known source format"));
            assert_eq!(format.category(), category, "{ext}");
        }
        // Every Office format round-trips through its canonical extension.
        for office in OfficeFormat::ALL {
            assert_eq!(
                SourceFormat::from_extension(office.canonical_extension()),
                Some(SourceFormat::Office(office))
            );
        }
    }

    #[test]
    fn formats_this_project_does_not_open_are_not_claimable() {
        for ext in ["doc", "xls", "ppt", "exe", "zip", "7z", "heic", "svg", ""] {
            assert_eq!(
                SourceFormat::from_extension(ext),
                None,
                "{ext} must not be claimable yet"
            );
        }
    }

    #[test]
    fn containers_come_from_magic_bytes_only() {
        assert_eq!(Container::sniff(b"%PDF-1.7\n"), Some(Container::Pdf));
        assert_eq!(Container::sniff(b"PK\x03\x04rest"), Some(Container::Zip));
        assert_eq!(
            Container::sniff(b"\x89PNG\r\n\x1a\n"),
            Some(Container::Image(NativeImageFormat::Png))
        );
        // An empty ZIP, an OLE (legacy .doc) header and plain text are not
        // containers this project admits.
        assert_eq!(Container::sniff(b"PK\x05\x06"), None);
        assert_eq!(Container::sniff(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"), None);
        assert_eq!(Container::sniff(b"hello"), None);
        assert_eq!(Container::sniff(b""), None);
    }

    #[test]
    fn a_container_excludes_claims_from_other_families() {
        let zip = Container::sniff(b"PK\x03\x04").unwrap();
        assert!(zip.could_hold(SourceFormat::Office(OfficeFormat::Docx)));
        // One ZIP could be any Office format: the container cannot tell
        // them apart, so neither is excluded here.
        assert!(zip.could_hold(SourceFormat::Office(OfficeFormat::Ods)));
        assert!(!zip.could_hold(SourceFormat::Pdf));
        assert!(!zip.could_hold(SourceFormat::Image(NativeImageFormat::Png)));

        let pdf = Container::sniff(b"%PDF-1.4").unwrap();
        assert!(pdf.could_hold(SourceFormat::Pdf));
        assert!(!pdf.could_hold(SourceFormat::Office(OfficeFormat::Docx)));

        // An image's container is exact, so a renamed PNG is excluded.
        let png = Container::sniff(b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(png.could_hold(SourceFormat::Image(NativeImageFormat::Png)));
        assert!(!png.could_hold(SourceFormat::Image(NativeImageFormat::Jpeg)));
    }
}
