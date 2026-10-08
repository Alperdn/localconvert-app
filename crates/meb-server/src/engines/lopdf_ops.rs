//! The PDF operations that need no engine at all.
//!
//! Seven job kinds - protect, unlock, metadata strip, page numbers, delete
//! pages, reorder pages, extract text - done in-process with `lopdf`, the
//! same pure-Rust library the desktop app uses for its PDF text editing.
//!
//! Because nothing is spawned, this runner is UNCONDITIONALLY registered
//! (`runner::RunnerRegistry::production`): there is no tool to resolve, no
//! machine on which these kinds are unavailable, and therefore nothing to
//! availability-gate. That is the whole reason they are separated from the
//! Ghostscript-backed operations, which look identical from the API but are
//! only there when Ghostscript is.
//!
//! The invariants the external engines get from `meb_engines::process` have
//! in-process equivalents here, and they are not weaker:
//!
//! - No process, no shell, no argument vector: a client value cannot reach
//!   a command line because there is no command line. The validated
//!   `JobSpec` is read directly (a closed enum for a page-number position,
//!   bounded integers for page numbers, a length-bounded password).
//! - Every write goes to the single output path the worker chose inside the
//!   job's own workspace. The input is only ever read.
//! - A password lives in the spec and is never logged: the password-bearing
//!   specs have a redacting `Debug` (see `spec.rs`), and no `detail` string
//!   built here contains one.
//! - Work is bounded by the document, not by a timeout: every operation is
//!   a single load/transform/save over a file whose size the upload limit
//!   already capped. Cancellation is checked at the real boundaries
//!   (before the load, before the save, and per page where there is a page
//!   loop), since a `lopdf` call cannot be interrupted from outside.
//!
//! One rule applies to all of them: an input that is still ENCRYPTED when
//! it has been loaded cannot be operated on - its objects are ciphertext -
//! so every kind except `pdf_unlock` refuses it with
//! `PDF_PASSWORD_REQUIRED` instead of producing a damaged document.

use crate::runner::{ConversionRunner, JobControl, RunError, RunRequest};
use crate::spec::{
    JobKind, JobSpec, PageNumberPosition, PdfDeletePagesSpec, PdfPageNumbersSpec, PdfProtectSpec,
    PdfReorderPagesSpec, PdfUnlockSpec,
};
use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
use lopdf::{Dictionary, Document, EncryptionState, EncryptionVersion, Object, ObjectId, Stream};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Stable error codes this runner reports. `error::job_error_message` has
/// the user-facing Turkish text for each.
const READ_FAILED: &str = "PDF_READ_FAILED";
const WRITE_FAILED: &str = "PDF_WRITE_FAILED";
const PASSWORD_REQUIRED: &str = "PDF_PASSWORD_REQUIRED";
const WRONG_PASSWORD: &str = "PDF_WRONG_PASSWORD";
const ALREADY_PROTECTED: &str = "PDF_ALREADY_PROTECTED";
const ENCRYPTION_UNSUPPORTED: &str = "PDF_ENCRYPTION_UNSUPPORTED";
const PAGE_UNAVAILABLE: &str = "PDF_PAGE_UNAVAILABLE";
const NO_PAGES_LEFT: &str = "PDF_NO_PAGES_LEFT";
const NO_TEXT_FOUND: &str = "PDF_NO_TEXT_FOUND";
const OPERATION_FAILED: &str = "PDF_OPERATION_FAILED";

/// Length of an AES-256 file encryption key, in bytes.
const AES_256_KEY_BYTES: usize = 32;

/// Point size of a drawn page number, and the margin it is inset by. Both
/// are typographic defaults rather than options: a page number is a fixed
/// piece of furniture, and every value that could vary is one more thing a
/// client could ask for that this server would have to validate.
const PAGE_NUMBER_SIZE: f32 = 10.0;
const PAGE_NUMBER_MARGIN: f32 = 28.0;

/// Width of one digit of Helvetica, in em. From the font's own metrics (all
/// ten digits are 556/1000 em wide), used to place a centred or
/// right-aligned number without measuring anything.
const HELVETICA_DIGIT_EM: f32 = 0.556;

/// US Letter, the fallback when a page declares no media box anywhere in
/// its inheritance chain. Such a PDF is already malformed; placing the
/// number by a plausible page size beats refusing the document.
const DEFAULT_PAGE_WIDTH: f32 = 612.0;
const DEFAULT_PAGE_HEIGHT: f32 = 792.0;

/// The resource name the page-number font is bound to. Distinctive on
/// purpose: it is added to the resource dictionary the page already uses,
/// so it must not collide with a font the document itself defines.
const FONT_RESOURCE_NAME: &[u8] = b"MEBPageNo";

/// How far up a page tree this runner will walk looking for an inherited
/// attribute. A bound, not a limit anyone reaches: it stops a document with
/// a /Parent cycle from spinning forever.
const MAX_TREE_DEPTH: usize = 32;

pub struct LopdfRunner;

impl LopdfRunner {
    /// The job kinds this runner backs. Shared by the registry wiring and
    /// by the test that checks the two agree.
    pub const KINDS: [JobKind; 7] = [
        JobKind::PdfProtect,
        JobKind::PdfUnlock,
        JobKind::PdfMetadataStrip,
        JobKind::PdfPageNumbers,
        JobKind::PdfDeletePages,
        JobKind::PdfReorderPages,
        JobKind::PdfExtractText,
    ];
}

impl ConversionRunner for LopdfRunner {
    fn run(&self, request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
        match request.spec {
            JobSpec::PdfProtect(spec) => protect(spec, request, control),
            JobSpec::PdfUnlock(spec) => unlock(spec, request, control),
            JobSpec::PdfMetadataStrip => strip_metadata(request, control),
            JobSpec::PdfPageNumbers(spec) => page_numbers(spec, request, control),
            JobSpec::PdfDeletePages(spec) => delete_pages(spec, request, control),
            JobSpec::PdfReorderPages(spec) => reorder_pages(spec, request, control),
            JobSpec::PdfExtractText => extract_text(request, control),
            JobSpec::ImageConvert(_)
            | JobSpec::PdfMerge(_)
            | JobSpec::PdfSplit(_)
            | JobSpec::PdfCompress(_)
            | JobSpec::PdfRotate(_)
            | JobSpec::PdfWatermark(_)
            | JobSpec::PdfOcr(_)
            | JobSpec::OfficeConvert(_)
            | JobSpec::PdfToOffice(_) => Err(RunError::wrong_kind("LopdfRunner")),
        }
    }
}

// --- the operations -----------------------------------------------------

/// Re-writes the document encrypted with AES-256, so that opening it needs
/// `user_password`.
///
/// The permission bits stay fully permissive: what the user asked for is a
/// password on the file, and silently also forbidding printing or copying
/// would be a restriction they never requested. The owner password is what
/// guards removing the protection again.
fn protect(
    spec: &PdfProtectSpec,
    request: &RunRequest<'_>,
    control: &JobControl<'_>,
) -> Result<(), RunError> {
    let mut doc = load(request, control)?;
    if doc.is_encrypted() {
        // Still encrypted after loading means lopdf could not open it with
        // the empty password: it is already protected by one we do not
        // have, so there is nothing to re-encrypt.
        return Err(RunError::Failed {
            code: ALREADY_PROTECTED,
            detail: "the input is already password protected".to_string(),
        });
    }

    let mut key = [0u8; AES_256_KEY_BYTES];
    getrandom::fill(&mut key).map_err(|e| RunError::Failed {
        code: OPERATION_FAILED,
        detail: format!("the system random source is unavailable: {e}"),
    })?;

    let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
    let version = EncryptionVersion::V5 {
        // The document metadata is encrypted with everything else: leaving
        // it in the clear would publish the title and author of a file the
        // user just asked to lock.
        encrypt_metadata: true,
        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
        file_encryption_key: &key,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: &spec.owner_password,
        user_password: &spec.user_password,
        permissions: lopdf::Permissions::all(),
    };
    let state = EncryptionState::try_from(version).map_err(|e| RunError::Failed {
        code: OPERATION_FAILED,
        detail: format!("the encryption parameters were rejected: {e}"),
    })?;
    doc.encrypt(&state).map_err(|e| RunError::Failed {
        code: OPERATION_FAILED,
        detail: format!("encrypting the document failed: {e}"),
    })?;
    save(&mut doc, request, control)
}

/// Re-writes the document with its protection removed.
///
/// Three honest outcomes, because a "protected" PDF is really two different
/// things:
///
/// - Encrypted with a user password: the password is needed, and a wrong
///   one is reported as such rather than as a corrupt file.
/// - Encrypted with only an owner password (it opens with no password, but
///   declares restrictions): `lopdf` already decrypted it while loading,
///   so re-writing it is exactly the requested result and no password is
///   needed.
/// - Not encrypted at all: the output is a faithful copy. Nothing was
///   removed because nothing was there, which is the outcome the user
///   wanted either way.
fn unlock(
    spec: &PdfUnlockSpec,
    request: &RunRequest<'_>,
    control: &JobControl<'_>,
) -> Result<(), RunError> {
    let mut doc = load(request, control)?;
    if doc.is_encrypted() {
        let Some(password) = spec.password.as_deref() else {
            return Err(RunError::Failed {
                code: PASSWORD_REQUIRED,
                detail: "the input needs a password and the request gave none".to_string(),
            });
        };
        // `decrypt` both authenticates and rewrites every object in the
        // clear, and removes the /Encrypt entry from the trailer, so the
        // saved document carries no protection at all.
        doc.decrypt(password).map_err(decryption_error)?;
    }
    save(&mut doc, request, control)
}

/// Re-writes the document without any metadata: no document information
/// dictionary (title, author, producer, dates) and no XMP metadata stream.
fn strip_metadata(request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
    let mut doc = load_decrypted(request, control)?;

    // The /Info dictionary, and the object holding it.
    if let Some(info) = doc.trailer.remove(b"Info") {
        if let Ok(id) = info.as_reference() {
            doc.delete_object(id);
        }
    }
    // The XMP packet, which duplicates most of /Info and is what a
    // metadata-stripping tool that only removed /Info would leave behind.
    let xmp = doc
        .catalog_mut()
        .ok()
        .and_then(|catalog| catalog.remove(b"Metadata"));
    if let Some(id) = xmp.and_then(|m| m.as_reference().ok()) {
        doc.delete_object(id);
    }
    // Per-page private application data: not document metadata in the
    // /Info sense, but it is producer-written provenance and the user asked
    // for the document to stop carrying any.
    for page_id in doc.get_pages().into_values() {
        if let Ok(page) = doc.get_dictionary_mut(page_id) {
            page.remove(b"PieceInfo");
        }
    }
    if let Ok(catalog) = doc.catalog_mut() {
        catalog.remove(b"PieceInfo");
    }

    // Everything that referred only to the removed objects goes with them,
    // so no stripped value survives as an unreferenced object in the file.
    doc.prune_objects();
    save(&mut doc, request, control)
}

/// Draws a page number on every page.
fn page_numbers(
    spec: &PdfPageNumbersSpec,
    request: &RunRequest<'_>,
    control: &JobControl<'_>,
) -> Result<(), RunError> {
    let mut doc = load_decrypted(request, control)?;
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    if pages.is_empty() {
        return Err(RunError::Failed {
            code: READ_FAILED,
            detail: "the document has no pages".to_string(),
        });
    }

    let total = pages.len();
    for (index, page_id) in pages.into_iter().enumerate() {
        if control.is_cancelled() {
            return Err(RunError::Cancelled);
        }
        // `start_at` is bounded by the spec, and `index` by the document,
        // so this cannot overflow into a nonsense number.
        let number = spec.start_at as u64 + index as u64;
        stamp_page(&mut doc, page_id, &number.to_string(), spec.position).map_err(|e| {
            RunError::Failed {
                code: OPERATION_FAILED,
                detail: format!("page {} could not be numbered: {e}", index + 1),
            }
        })?;
        // Real progress: pages stamped out of pages in the document. The
        // save that follows is the remaining tenth.
        control.report_progress(((index + 1) * 90 / total) as u8);
    }
    save(&mut doc, request, control)
}

/// Removes the named pages, keeping the rest in their original order.
fn delete_pages(
    spec: &PdfDeletePagesSpec,
    request: &RunRequest<'_>,
    control: &JobControl<'_>,
) -> Result<(), RunError> {
    let mut doc = load_decrypted(request, control)?;
    let count = doc.get_pages().len();

    // A page the document does not have is the one failure here a client
    // can act on, so it is reported as such instead of being ignored -
    // which would delete a different set of pages than was asked for.
    if let Some(missing) = spec.pages.iter().find(|p| **p as usize > count) {
        return Err(RunError::Failed {
            code: PAGE_UNAVAILABLE,
            detail: format!("page {missing} of a {count}-page document was requested"),
        });
    }
    if spec.pages.len() >= count {
        return Err(RunError::Failed {
            code: NO_PAGES_LEFT,
            detail: format!("deleting {} of {count} pages leaves none", spec.pages.len()),
        });
    }

    doc.delete_pages(&spec.pages);
    doc.prune_objects();
    save(&mut doc, request, control)
}

/// Re-arranges the pages into the requested order.
///
/// The page tree is rebuilt flat - the root /Pages node lists every page
/// directly - because the requested order is a flat sequence, and any
/// intermediate node of the original tree would otherwise have to be
/// re-partitioned to match it. A flat page tree is ordinary, valid PDF;
/// what it costs is the (purely structural) original grouping.
fn reorder_pages(
    spec: &PdfReorderPagesSpec,
    request: &RunRequest<'_>,
    control: &JobControl<'_>,
) -> Result<(), RunError> {
    let mut doc = load_decrypted(request, control)?;
    let pages = doc.get_pages();

    // The spec guarantees the order is a permutation of 1..=order.len();
    // only the document can say whether that is also its page count.
    if spec.order.len() != pages.len() {
        return Err(RunError::Failed {
            code: PAGE_UNAVAILABLE,
            detail: format!(
                "an order of {} pages was given for a {}-page document",
                spec.order.len(),
                pages.len()
            ),
        });
    }

    let root = doc
        .catalog()
        .and_then(|catalog| catalog.get(b"Pages"))
        .and_then(Object::as_reference)
        .map_err(|e| RunError::Failed {
            code: READ_FAILED,
            detail: format!("the document has no page tree: {e}"),
        })?;

    let mut kids = Vec::with_capacity(spec.order.len());
    for wanted in &spec.order {
        let page_id = *pages.get(wanted).ok_or_else(|| RunError::Failed {
            code: PAGE_UNAVAILABLE,
            detail: format!("page {wanted} is not in the document"),
        })?;
        kids.push(Object::Reference(page_id));
        // Every page now hangs off the root node, so inherited attributes
        // keep resolving (to the root's, which is where they must be).
        if let Ok(page) = doc.get_dictionary_mut(page_id) {
            page.set("Parent", Object::Reference(root));
        }
    }

    let count = kids.len() as i64;
    let tree = doc.get_dictionary_mut(root).map_err(|e| RunError::Failed {
        code: READ_FAILED,
        detail: format!("the page tree is unreadable: {e}"),
    })?;
    tree.set("Kids", Object::Array(kids));
    tree.set("Count", count);

    // The intermediate nodes of the old tree are now unreferenced.
    doc.prune_objects();
    save(&mut doc, request, control)
}

/// Writes out the text the PDF already contains, as UTF-8.
fn extract_text(request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<(), RunError> {
    let doc = load_decrypted(request, control)?;
    let page_numbers: Vec<u32> = doc.get_pages().into_keys().collect();
    if page_numbers.is_empty() {
        return Err(RunError::Failed {
            code: READ_FAILED,
            detail: "the document has no pages".to_string(),
        });
    }

    let text = doc.extract_text(&page_numbers).map_err(|e| RunError::Failed {
        code: READ_FAILED,
        detail: format!("the document's text could not be decoded: {e}"),
    })?;
    // A PDF of scanned images contains no text at all. That is not a
    // failure of this operation, it is the answer - but an empty file would
    // not say so, and the user has a different tool for it (`pdf_ocr`), so
    // it gets its own code rather than a zero-byte "success".
    if text.trim().is_empty() {
        return Err(RunError::Failed {
            code: NO_TEXT_FOUND,
            detail: "the document contains no extractable text".to_string(),
        });
    }
    if control.is_cancelled() {
        return Err(RunError::Cancelled);
    }
    std::fs::write(request.output, text.as_bytes()).map_err(|e| RunError::Failed {
        code: WRITE_FAILED,
        detail: format!("the extracted text could not be written: {e}"),
    })?;
    control.report_progress(100);
    Ok(())
}

// --- shared steps -------------------------------------------------------

/// Loads the job's input. Reports a corrupt or unreadable PDF as such; an
/// encrypted one loads successfully here (its objects stay ciphertext), so
/// that `pdf_unlock` can be the one kind that handles it.
fn load(request: &RunRequest<'_>, control: &JobControl<'_>) -> Result<Document, RunError> {
    if control.is_cancelled() {
        return Err(RunError::Cancelled);
    }
    let doc = Document::load(request.input()).map_err(|e| match e {
        lopdf::Error::Decryption(_) => RunError::Failed {
            code: PASSWORD_REQUIRED,
            detail: "the document could not be opened without a password".to_string(),
        },
        other => RunError::Failed {
            code: READ_FAILED,
            detail: format!("the document could not be read: {other}"),
        },
    })?;
    if control.is_cancelled() {
        return Err(RunError::Cancelled);
    }
    Ok(doc)
}

/// `load` for the six kinds that have to read the document's objects. An
/// input that is still encrypted is refused rather than operated on: its
/// page tree, content streams and strings are ciphertext, so the result
/// would be a damaged document reported as a success.
fn load_decrypted(
    request: &RunRequest<'_>,
    control: &JobControl<'_>,
) -> Result<Document, RunError> {
    let doc = load(request, control)?;
    if doc.is_encrypted() {
        return Err(RunError::Failed {
            code: PASSWORD_REQUIRED,
            detail: "the input is password protected; remove the protection first".to_string(),
        });
    }
    Ok(doc)
}

/// Writes the document to the single output path the job promised.
fn save(
    doc: &mut Document,
    request: &RunRequest<'_>,
    control: &JobControl<'_>,
) -> Result<(), RunError> {
    if control.is_cancelled() {
        return Err(RunError::Cancelled);
    }
    doc.save(request.output).map_err(|e| RunError::Failed {
        code: WRITE_FAILED,
        detail: format!("the result could not be written: {e}"),
    })?;
    control.report_progress(100);
    Ok(())
}

/// Maps a decryption failure onto a code the user can act on. The password
/// itself is never part of the detail.
fn decryption_error(error: lopdf::Error) -> RunError {
    use lopdf::encryption::DecryptionError;
    match error {
        lopdf::Error::Decryption(DecryptionError::IncorrectPassword) => RunError::Failed {
            code: WRONG_PASSWORD,
            detail: "the supplied password does not open this document".to_string(),
        },
        lopdf::Error::Decryption(
            DecryptionError::UnsupportedEncryption | DecryptionError::UnsupportedVersion,
        ) => RunError::Failed {
            code: ENCRYPTION_UNSUPPORTED,
            detail: "the document uses an encryption scheme this server cannot read".to_string(),
        },
        other => RunError::Failed {
            code: READ_FAILED,
            detail: format!("the document could not be decrypted: {other}"),
        },
    }
}

// --- drawing a page number ----------------------------------------------

/// Appends a page number to one page's content, and makes sure the page can
/// resolve the font it uses.
fn stamp_page(
    doc: &mut Document,
    page_id: ObjectId,
    number: &str,
    position: PageNumberPosition,
) -> Result<(), lopdf::Error> {
    let (width, height) = page_size(doc, page_id);
    let (x, y) = place(position, width, height, number.len());

    // `number` is decimal digits built from integers here, so it needs no
    // PDF string escaping - and the assertion is kept honest by a test
    // (`a_drawn_page_number_is_only_ever_digits`).
    debug_assert!(number.bytes().all(|b| b.is_ascii_digit()));
    let ops = format!(
        "Q\nq\nBT\n/{font} {size} Tf\n0 g\n{x:.2} {y:.2} Td\n({number}) Tj\nET\nQ\n",
        font = String::from_utf8_lossy(FONT_RESOURCE_NAME),
        size = PAGE_NUMBER_SIZE,
    );

    // The overlay is wrapped around the page's existing content rather than
    // simply appended: a content stream that left the graphics state
    // pushed would otherwise shift or recolour the number. The leading `q`
    // stream and the `Q` that opens the overlay make the number's own state
    // independent of whatever the page did.
    let prologue = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let overlay = doc.add_object(Stream::new(Dictionary::new(), ops.into_bytes()));

    let page = doc.get_dictionary_mut(page_id)?;
    let contents = match page.get(b"Contents") {
        Ok(Object::Reference(id)) => vec![Object::Reference(*id)],
        Ok(Object::Array(existing)) => existing.clone(),
        // A page with no content at all is legal (it is blank); the number
        // becomes its only content.
        _ => Vec::new(),
    };
    let mut rebuilt = Vec::with_capacity(contents.len() + 2);
    rebuilt.push(Object::Reference(prologue));
    rebuilt.extend(contents);
    rebuilt.push(Object::Reference(overlay));
    page.set("Contents", Object::Array(rebuilt));

    attach_helvetica(doc, page_id)
}

/// Where the number's baseline goes, in PDF user space (origin at the
/// bottom-left of the page).
fn place(
    position: PageNumberPosition,
    width: f32,
    height: f32,
    digits: usize,
) -> (f32, f32) {
    let text_width = HELVETICA_DIGIT_EM * PAGE_NUMBER_SIZE * digits as f32;
    let left = PAGE_NUMBER_MARGIN;
    let centre = (width - text_width) / 2.0;
    let right = width - PAGE_NUMBER_MARGIN - text_width;
    let bottom = PAGE_NUMBER_MARGIN;
    // The top row is inset by the margin plus the line's own height, so the
    // glyphs sit inside the margin rather than straddling it.
    let top = height - PAGE_NUMBER_MARGIN - PAGE_NUMBER_SIZE;
    match position {
        PageNumberPosition::TopLeft => (left, top),
        PageNumberPosition::TopCenter => (centre, top),
        PageNumberPosition::TopRight => (right, top),
        PageNumberPosition::BottomLeft => (left, bottom),
        PageNumberPosition::BottomCenter => (centre, bottom),
        PageNumberPosition::BottomRight => (right, bottom),
    }
}

/// A page's media box, in points. Inherited from the page tree when the
/// page does not carry one itself, which is both legal and common.
fn page_size(doc: &Document, page_id: ObjectId) -> (f32, f32) {
    let found = inherited(doc, page_id, b"MediaBox").and_then(|object| {
        let values = object.as_array().ok()?;
        if values.len() != 4 {
            return None;
        }
        let numbers: Vec<f32> = values.iter().filter_map(|v| v.as_float().ok()).collect();
        if numbers.len() != 4 {
            return None;
        }
        // The box is two opposite corners, in either order.
        let width = (numbers[2] - numbers[0]).abs();
        let height = (numbers[3] - numbers[1]).abs();
        (width > 1.0 && height > 1.0).then_some((width, height))
    });
    found.unwrap_or((DEFAULT_PAGE_WIDTH, DEFAULT_PAGE_HEIGHT))
}

/// Resolves an inheritable page attribute: the page's own value, or the
/// nearest ancestor's.
fn inherited<'a>(doc: &'a Document, page_id: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut node = page_id;
    for _ in 0..MAX_TREE_DEPTH {
        let dict = doc.get_dictionary(node).ok()?;
        if let Ok(value) = dict.get_deref(key, doc) {
            return Some(value);
        }
        node = dict.get(b"Parent").and_then(Object::as_reference).ok()?;
    }
    None
}

/// Makes `FONT_RESOURCE_NAME` resolve to Helvetica for this page.
///
/// The font is added to the resource dictionary the page ALREADY uses -
/// its own, a shared one it references, or the one it inherits from an
/// ancestor - rather than by setting a fresh /Resources on the page. The
/// difference matters: a page-level resource dictionary shadows the
/// inherited one completely, so creating one would silently strip the fonts
/// and images the page's existing content depends on.
fn attach_helvetica(doc: &mut Document, page_id: ObjectId) -> Result<(), lopdf::Error> {
    let font = doc.add_object(Dictionary::from_iter(vec![
        (b"Type".to_vec(), Object::Name(b"Font".to_vec())),
        (b"Subtype".to_vec(), Object::Name(b"Type1".to_vec())),
        // One of the 14 standard fonts: present in every PDF reader, so
        // nothing has to be embedded.
        (b"BaseFont".to_vec(), Object::Name(b"Helvetica".to_vec())),
        (b"Encoding".to_vec(), Object::Name(b"WinAnsiEncoding".to_vec())),
    ]));

    match resources_slot(doc, page_id) {
        // The /Font sub-dictionary is a separate object: add the entry
        // there, which is where this page's fonts already live.
        Some(ResourcesSlot::Fonts(id)) => {
            doc.get_dictionary_mut(id)?.set(FONT_RESOURCE_NAME, Object::Reference(font));
        }
        Some(ResourcesSlot::Shared(id)) => {
            let resources = doc.get_dictionary_mut(id)?;
            set_font(resources, font);
        }
        Some(ResourcesSlot::Inline(node)) => {
            let resources = doc
                .get_dictionary_mut(node)?
                .get_mut(b"Resources")?
                .as_dict_mut()?;
            set_font(resources, font);
        }
        // No resource dictionary anywhere: there is nothing to shadow, so
        // the page gets one of its own.
        None => {
            let mut resources = Dictionary::new();
            set_font(&mut resources, font);
            doc.get_dictionary_mut(page_id)?
                .set("Resources", Object::Dictionary(resources));
        }
    }
    Ok(())
}

/// Where a page's resource dictionary lives.
enum ResourcesSlot {
    /// The /Font sub-dictionary is its own object, with this id.
    Fonts(ObjectId),
    /// The resource dictionary is its own object, with this id.
    Shared(ObjectId),
    /// The resource dictionary is inline in this object (the page itself or
    /// an ancestor page-tree node).
    Inline(ObjectId),
}

fn resources_slot(doc: &Document, page_id: ObjectId) -> Option<ResourcesSlot> {
    let mut node = page_id;
    for _ in 0..MAX_TREE_DEPTH {
        let dict = doc.get_dictionary(node).ok()?;
        match dict.get(b"Resources") {
            Ok(Object::Reference(id)) => {
                let id = *id;
                // Prefer the /Font dictionary itself when it is a separate
                // object, so a resource dictionary shared between pages is
                // not rewritten at all.
                return Some(match font_object(doc, doc.get_dictionary(id).ok()?) {
                    Some(fonts) => ResourcesSlot::Fonts(fonts),
                    None => ResourcesSlot::Shared(id),
                });
            }
            Ok(Object::Dictionary(resources)) => {
                return Some(match font_object(doc, resources) {
                    Some(fonts) => ResourcesSlot::Fonts(fonts),
                    None => ResourcesSlot::Inline(node),
                });
            }
            _ => {}
        }
        node = dict.get(b"Parent").and_then(Object::as_reference).ok()?;
    }
    None
}

/// The id of a resource dictionary's /Font object, when it is one.
fn font_object(doc: &Document, resources: &Dictionary) -> Option<ObjectId> {
    let id = resources.get(b"Font").and_then(Object::as_reference).ok()?;
    // Only if it really is a dictionary we can add an entry to.
    doc.get_dictionary(id).ok().map(|_| id)
}

/// Adds the page-number font to a resource dictionary's inline /Font
/// dictionary, creating that dictionary if the resources have none.
fn set_font(resources: &mut Dictionary, font: ObjectId) {
    let mut fonts = match resources.get(b"Font") {
        Ok(Object::Dictionary(existing)) => existing.clone(),
        _ => Dictionary::new(),
    };
    fonts.set(FONT_RESOURCE_NAME, Object::Reference(font));
    resources.set("Font", Object::Dictionary(fonts));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{CreateJobRequest, JobSpec, MAX_PDF_PAGE};
    use meb_core::format::SourceFormat;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    /// A job workspace that cleans itself up.
    struct Workspace {
        dir: PathBuf,
    }

    impl Workspace {
        fn new() -> Workspace {
            let dir = std::env::temp_dir()
                .join(format!("meb_lopdf_{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&dir).unwrap();
            Workspace { dir }
        }

        fn input(&self, bytes: &[u8]) -> PathBuf {
            let path = self.dir.join("source-00.pdf");
            std::fs::write(&path, bytes).unwrap();
            path
        }

        fn output(&self, extension: &str) -> PathBuf {
            self.dir.join(format!("result.{extension}"))
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A real, multi-page PDF, built by lopdf itself so the fixture cannot
    /// drift from what the library accepts.
    fn fixture_pdf(pages: usize, text: Option<&str>) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(Dictionary::from_iter(vec![
            (b"Type".to_vec(), Object::Name(b"Font".to_vec())),
            (b"Subtype".to_vec(), Object::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), Object::Name(b"Helvetica".to_vec())),
        ]));
        let resources = doc.add_object(Dictionary::from_iter(vec![(
            b"Font".to_vec(),
            Object::Dictionary(Dictionary::from_iter(vec![(
                b"F1".to_vec(),
                Object::Reference(font),
            )])),
        )]));

        let mut kids = Vec::new();
        for page in 1..=pages {
            let body = match text {
                Some(text) => format!("BT\n/F1 24 Tf\n72 700 Td\n({text} {page}) Tj\nET\n"),
                None => String::new(),
            };
            let content = doc.add_object(Stream::new(Dictionary::new(), body.into_bytes()));
            let page_id = doc.add_object(Dictionary::from_iter(vec![
                (b"Type".to_vec(), Object::Name(b"Page".to_vec())),
                (b"Parent".to_vec(), Object::Reference(pages_id)),
                (b"Contents".to_vec(), Object::Reference(content)),
            ]));
            kids.push(Object::Reference(page_id));
        }

        doc.objects.insert(
            pages_id,
            Object::Dictionary(Dictionary::from_iter(vec![
                (b"Type".to_vec(), Object::Name(b"Pages".to_vec())),
                (b"Count".to_vec(), Object::Integer(pages as i64)),
                (b"Kids".to_vec(), Object::Array(kids)),
                // Inherited by every page, which is what makes this
                // fixture exercise the inheritance walk.
                (b"Resources".to_vec(), Object::Reference(resources)),
                (
                    b"MediaBox".to_vec(),
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Integer(612),
                        Object::Integer(792),
                    ]),
                ),
            ])),
        );
        let catalog = doc.add_object(Dictionary::from_iter(vec![
            (b"Type".to_vec(), Object::Name(b"Catalog".to_vec())),
            (b"Pages".to_vec(), Object::Reference(pages_id)),
        ]));
        doc.trailer.set("Root", Object::Reference(catalog));
        let info = doc.add_object(Dictionary::from_iter(vec![
            (
                b"Title".to_vec(),
                Object::string_literal("Gizli Ogrenci Listesi"),
            ),
            (b"Author".to_vec(), Object::string_literal("Okul")),
        ]));
        doc.trailer.set("Info", Object::Reference(info));

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    /// Runs one job to completion against a real file, and returns what the
    /// runner reported plus the output path.
    fn run_job(
        spec: &JobSpec,
        input: &Path,
        output: &Path,
    ) -> (Result<(), RunError>, Vec<u8>) {
        let inputs = vec![input.to_path_buf()];
        let request = RunRequest {
            inputs: &inputs,
            output,
            spec,
        };
        let cancel = AtomicBool::new(false);
        let progress = |_: u8| {};
        let control = JobControl {
            cancel: &cancel,
            progress: &progress,
        };
        let outcome = LopdfRunner.run(&request, &control);
        let produced = std::fs::read(output).unwrap_or_default();
        (outcome, produced)
    }

    fn spec_for(kind: JobKind, options: serde_json::Value) -> JobSpec {
        let request = CreateJobRequest {
            kind: kind.wire().to_string(),
            file_id: Some("f".to_string()),
            file_ids: None,
            output_format: None,
            options: if options.is_null() { None } else { Some(options) },
        };
        JobSpec::from_request(kind, &request, &[SourceFormat::Pdf])
            .unwrap_or_else(|e| panic!("{} spec was refused: {}", kind.wire(), e.code))
    }

    fn code_of(outcome: Result<(), RunError>) -> &'static str {
        match outcome {
            Ok(()) => "OK",
            Err(RunError::Cancelled) => "CANCELLED",
            Err(RunError::Failed { code, .. }) => code,
        }
    }

    #[test]
    fn this_runner_needs_no_engine_so_it_handles_exactly_its_seven_kinds() {
        // The point of the whole module: no `detect`, nothing to resolve,
        // nothing that could make these kinds unavailable on a machine.
        assert_eq!(LopdfRunner::KINDS.len(), 7);
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(2, Some("Sayfa")));
        let output = workspace.output("pdf");
        // A kind this runner does not back is a wiring mistake, never a
        // guess at what was meant.
        let foreign = spec_for(JobKind::PdfCompress, serde_json::Value::Null);
        assert_eq!(
            code_of(run_job(&foreign, &input, &output).0),
            "INTERNAL_ERROR"
        );
    }

    #[test]
    fn a_protected_document_needs_its_password_to_be_opened_again() {
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(2, Some("Sayfa")));
        let output = workspace.output("pdf");

        let spec = spec_for(
            JobKind::PdfProtect,
            serde_json::json!({ "password": "açık-kapı-42" }),
        );
        let (outcome, produced) = run_job(&spec, &input, &output);
        assert_eq!(code_of(outcome), "OK");
        assert!(produced.starts_with(b"%PDF-"), "the output is not a PDF");
        // The spec's own output check must accept it: an encrypted PDF is
        // still a PDF.
        assert!(spec.validate_output(&output).is_ok());

        // Opening it without the password leaves it encrypted; with the
        // password it opens and the original text is back.
        let locked = Document::load(&output).unwrap();
        assert!(locked.is_encrypted(), "the output is not protected");
        let mut unlocking = Document::load(&output).unwrap();
        assert!(
            unlocking.decrypt("yanlış-şifre").is_err(),
            "a wrong password opened the document"
        );
        let mut opened = Document::load(&output).unwrap();
        opened.decrypt("açık-kapı-42").unwrap();
        let text = opened.extract_text(&[1]).unwrap();
        assert!(text.contains("Sayfa"), "the content did not survive: {text:?}");
    }

    #[test]
    fn unlocking_round_trips_a_protected_document_and_reports_a_wrong_password() {
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(1, Some("Sayfa")));
        let locked = workspace.output("pdf");
        let protect = spec_for(
            JobKind::PdfProtect,
            serde_json::json!({ "password": "parola" }),
        );
        assert_eq!(code_of(run_job(&protect, &input, &locked).0), "OK");

        let unlocked = workspace.dir.join("unlocked.pdf");
        // The wrong password is reported as such, not as a corrupt file.
        let wrong = spec_for(
            JobKind::PdfUnlock,
            serde_json::json!({ "password": "başka" }),
        );
        assert_eq!(
            code_of(run_job(&wrong, &locked, &unlocked).0),
            "PDF_WRONG_PASSWORD"
        );
        // No password at all, for a document that needs one.
        let none = spec_for(JobKind::PdfUnlock, serde_json::Value::Null);
        assert_eq!(
            code_of(run_job(&none, &locked, &unlocked).0),
            "PDF_PASSWORD_REQUIRED"
        );
        // And the right one produces a document that opens with none.
        let right = spec_for(
            JobKind::PdfUnlock,
            serde_json::json!({ "password": "parola" }),
        );
        assert_eq!(code_of(run_job(&right, &locked, &unlocked).0), "OK");
        let opened = Document::load(&unlocked).unwrap();
        assert!(!opened.is_encrypted(), "the protection was not removed");
        assert!(opened.extract_text(&[1]).unwrap().contains("Sayfa"));
    }

    #[test]
    fn every_other_operation_refuses_a_still_encrypted_input() {
        // Operating on ciphertext would produce a damaged document and
        // report it as a success, which is the one outcome that must not
        // be possible here.
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(2, Some("Sayfa")));
        let locked = workspace.output("pdf");
        let protect = spec_for(
            JobKind::PdfProtect,
            serde_json::json!({ "password": "parola" }),
        );
        assert_eq!(code_of(run_job(&protect, &input, &locked).0), "OK");

        let out = workspace.dir.join("out.bin");
        let refused = [
            spec_for(JobKind::PdfMetadataStrip, serde_json::Value::Null),
            spec_for(JobKind::PdfPageNumbers, serde_json::Value::Null),
            spec_for(JobKind::PdfDeletePages, serde_json::json!({ "pages": [1] })),
            spec_for(
                JobKind::PdfReorderPages,
                serde_json::json!({ "order": [2, 1] }),
            ),
            spec_for(JobKind::PdfExtractText, serde_json::Value::Null),
        ];
        for spec in refused {
            assert_eq!(
                code_of(run_job(&spec, &locked, &out).0),
                "PDF_PASSWORD_REQUIRED",
                "{:?} accepted an encrypted input",
                spec.kind().wire()
            );
        }
        // Protecting an already-protected document is its own answer.
        assert_eq!(
            code_of(run_job(&protect, &locked, &out).0),
            "PDF_ALREADY_PROTECTED"
        );
    }

    #[test]
    fn stripping_metadata_leaves_the_pages_and_removes_the_values() {
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(2, Some("Sayfa")));
        let output = workspace.output("pdf");
        let spec = spec_for(JobKind::PdfMetadataStrip, serde_json::Value::Null);
        let (outcome, produced) = run_job(&spec, &input, &output);
        assert_eq!(code_of(outcome), "OK");

        let stripped = Document::load(&output).unwrap();
        assert!(
            !stripped.trailer.has(b"Info"),
            "the document information dictionary survived"
        );
        assert!(!stripped.catalog().unwrap().has(b"Metadata"));
        // Not just unlinked: the value itself is gone from the file.
        assert!(
            !contains(&produced, b"Gizli Ogrenci Listesi"),
            "the stripped title is still in the bytes of the file"
        );
        // And the document is otherwise intact.
        assert_eq!(stripped.get_pages().len(), 2);
        assert!(spec.validate_output(&output).is_ok());
    }

    #[test]
    fn page_numbers_are_drawn_on_every_page_without_losing_the_content() {
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(3, Some("Sayfa")));
        let output = workspace.output("pdf");
        let spec = spec_for(
            JobKind::PdfPageNumbers,
            serde_json::json!({ "position": "bottom_right", "start_at": 7 }),
        );
        assert_eq!(code_of(run_job(&spec, &input, &output).0), "OK");

        let numbered = Document::load(&output).unwrap();
        assert_eq!(numbered.get_pages().len(), 3);
        for (index, (page_number, page_id)) in numbered.get_pages().into_iter().enumerate() {
            let content = numbered.get_page_content(page_id).unwrap();
            let content = String::from_utf8_lossy(&content);
            // `start_at` is honoured, so the drawn numbers run 7, 8, 9.
            let expected = 7 + index;
            assert!(
                content.contains(&format!("({expected}) Tj")),
                "page {page_number} does not carry the number {expected}: {content}"
            );
            // The page's own text is still there, and the font the number
            // uses resolves.
            assert!(content.contains("(Sayfa"), "page {page_number} lost its text");
            let fonts = numbered.get_page_fonts(page_id).unwrap();
            assert!(
                fonts.contains_key(FONT_RESOURCE_NAME),
                "page {page_number} cannot resolve the page-number font"
            );
            // The inherited font the page already used is still reachable,
            // which is what would break if a fresh /Resources had been set
            // on the page.
            assert!(
                fonts.contains_key(b"F1".as_slice()),
                "page {page_number} lost the font its own content uses"
            );
        }
        assert!(spec.validate_output(&output).is_ok());
    }

    #[test]
    fn a_drawn_page_number_is_only_ever_digits() {
        // The number is written into a PDF string literal unescaped, which
        // is safe exactly as long as it is digits. It comes from
        // `start_at + index`, both integers, and `start_at` is bounded by
        // the spec - so this checks the bound, not a sanitizer.
        for start_at in [1, 2, MAX_PDF_PAGE] {
            let spec = spec_for(
                JobKind::PdfPageNumbers,
                serde_json::json!({ "start_at": start_at }),
            );
            let JobSpec::PdfPageNumbers(spec) = spec else {
                panic!("expected a page-numbers spec");
            };
            let rendered = (spec.start_at as u64 + 1).to_string();
            assert!(rendered.bytes().all(|b| b.is_ascii_digit()), "{rendered}");
        }
    }

    #[test]
    fn page_number_positions_all_land_inside_the_page() {
        // A position is a closed enum precisely so the coordinates can be
        // checked once, here, for every value it can have.
        let (width, height) = (612.0, 792.0);
        for position in [
            PageNumberPosition::TopLeft,
            PageNumberPosition::TopCenter,
            PageNumberPosition::TopRight,
            PageNumberPosition::BottomLeft,
            PageNumberPosition::BottomCenter,
            PageNumberPosition::BottomRight,
        ] {
            let (x, y) = place(position, width, height, 3);
            assert!(x > 0.0 && x < width, "{position:?} x = {x}");
            assert!(y > 0.0 && y < height, "{position:?} y = {y}");
            // And inside the margin, not straddling the page edge.
            assert!(
                y >= PAGE_NUMBER_MARGIN - 0.01 && y + PAGE_NUMBER_SIZE <= height,
                "{position:?} y = {y}"
            );
        }
    }

    #[test]
    fn deleting_pages_keeps_the_rest_in_order_and_refuses_the_impossible() {
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(4, Some("Sayfa")));
        let output = workspace.output("pdf");

        let spec = spec_for(JobKind::PdfDeletePages, serde_json::json!({ "pages": [2, 3] }));
        assert_eq!(code_of(run_job(&spec, &input, &output).0), "OK");
        let kept = Document::load(&output).unwrap();
        assert_eq!(kept.get_pages().len(), 2);
        // Pages 1 and 4 remain, in that order.
        let first = String::from_utf8_lossy(
            &kept.get_page_content(kept.get_pages()[&1]).unwrap(),
        )
        .to_string();
        let second = String::from_utf8_lossy(
            &kept.get_page_content(kept.get_pages()[&2]).unwrap(),
        )
        .to_string();
        assert!(first.contains("(Sayfa 1)"), "{first}");
        assert!(second.contains("(Sayfa 4)"), "{second}");

        // A page the document does not have, rather than a silently
        // different deletion.
        let missing = spec_for(JobKind::PdfDeletePages, serde_json::json!({ "pages": [9] }));
        assert_eq!(
            code_of(run_job(&missing, &input, &output).0),
            "PDF_PAGE_UNAVAILABLE"
        );
        // And a request that would leave no document at all.
        let everything = spec_for(
            JobKind::PdfDeletePages,
            serde_json::json!({ "pages": [1, 2, 3, 4] }),
        );
        assert_eq!(
            code_of(run_job(&everything, &input, &output).0),
            "PDF_NO_PAGES_LEFT"
        );
    }

    #[test]
    fn reordering_puts_the_pages_in_the_requested_order() {
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(3, Some("Sayfa")));
        let output = workspace.output("pdf");
        let spec = spec_for(
            JobKind::PdfReorderPages,
            serde_json::json!({ "order": [3, 1, 2] }),
        );
        assert_eq!(code_of(run_job(&spec, &input, &output).0), "OK");

        let reordered = Document::load(&output).unwrap();
        let pages = reordered.get_pages();
        assert_eq!(pages.len(), 3);
        for (slot, original) in [(1u32, 3), (2, 1), (3, 2)] {
            let content = reordered.get_page_content(pages[&slot]).unwrap();
            let content = String::from_utf8_lossy(&content);
            assert!(
                content.contains(&format!("(Sayfa {original})")),
                "slot {slot} holds {content}"
            );
        }
        assert!(spec.validate_output(&output).is_ok());
    }

    #[test]
    fn an_order_that_does_not_match_the_document_is_refused() {
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(3, Some("Sayfa")));
        let output = workspace.output("pdf");
        // A valid permutation, but of the wrong length: the API cannot know
        // the page count, so the engine is where that is caught.
        let spec = spec_for(JobKind::PdfReorderPages, serde_json::json!({ "order": [2, 1] }));
        assert_eq!(
            code_of(run_job(&spec, &input, &output).0),
            "PDF_PAGE_UNAVAILABLE"
        );
    }

    #[test]
    fn extracted_text_is_the_documents_own_text_and_an_image_only_pdf_says_so() {
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(2, Some("Ogrenci")));
        let output = workspace.output("txt");
        let spec = spec_for(JobKind::PdfExtractText, serde_json::Value::Null);
        let (outcome, produced) = run_job(&spec, &input, &output);
        assert_eq!(code_of(outcome), "OK");

        let text = String::from_utf8(produced).expect("the output is not UTF-8");
        assert!(text.contains("Ogrenci 1"), "{text:?}");
        assert!(text.contains("Ogrenci 2"), "{text:?}");
        assert!(spec.validate_output(&output).is_ok());

        // A PDF with no text at all is the answer "there is none", with its
        // own code - not a zero-byte file reported as a success.
        let scanned = workspace.dir.join("scan.pdf");
        std::fs::write(&scanned, fixture_pdf(1, None)).unwrap();
        let empty = workspace.dir.join("empty.txt");
        assert_eq!(
            code_of(run_job(&spec, &scanned, &empty).0),
            "PDF_NO_TEXT_FOUND"
        );
        assert!(!empty.exists(), "a failed extraction still wrote a file");
    }

    #[test]
    fn an_unreadable_input_is_reported_as_unreadable() {
        let workspace = Workspace::new();
        let input = workspace.input(b"%PDF-1.7\nthis is not a PDF body\n%%EOF\n");
        let output = workspace.output("pdf");
        let spec = spec_for(JobKind::PdfMetadataStrip, serde_json::Value::Null);
        assert_eq!(
            code_of(run_job(&spec, &input, &output).0),
            "PDF_READ_FAILED"
        );
    }

    #[test]
    fn a_cancelled_job_stops_without_writing_an_output() {
        let workspace = Workspace::new();
        let input = workspace.input(&fixture_pdf(2, Some("Sayfa")));
        let output = workspace.output("pdf");
        let spec = spec_for(JobKind::PdfMetadataStrip, serde_json::Value::Null);
        let inputs = vec![input.clone()];
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
            LopdfRunner.run(&request, &control),
            Err(RunError::Cancelled)
        ));
        assert!(!output.exists(), "a cancelled job wrote an output");
    }

    #[test]
    fn a_password_never_appears_in_a_formatted_spec() {
        // The spec lives in the job record for the job's whole life, so a
        // derived Debug would be one log line away from leaking it.
        let protect = spec_for(
            JobKind::PdfProtect,
            serde_json::json!({ "password": "çok-gizli", "owner_password": "sahip" }),
        );
        let unlock = spec_for(
            JobKind::PdfUnlock,
            serde_json::json!({ "password": "çok-gizli" }),
        );
        for spec in [protect, unlock] {
            let formatted = format!("{spec:?}");
            assert!(
                !formatted.contains("gizli") && !formatted.contains("sahip"),
                "a password reached a formatted spec: {formatted}"
            );
            assert!(formatted.contains("redacted"), "{formatted}");
        }
    }

    #[test]
    fn every_code_this_engine_reports_has_a_user_facing_message() {
        for code in [
            READ_FAILED,
            WRITE_FAILED,
            PASSWORD_REQUIRED,
            WRONG_PASSWORD,
            ALREADY_PROTECTED,
            ENCRYPTION_UNSUPPORTED,
            PAGE_UNAVAILABLE,
            NO_PAGES_LEFT,
            NO_TEXT_FOUND,
            OPERATION_FAILED,
        ] {
            let message = crate::error::job_error_message(code);
            assert_ne!(
                message,
                crate::error::job_error_message("A_CODE_THAT_DOES_NOT_EXIST"),
                "{code} falls through to the generic message"
            );
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
