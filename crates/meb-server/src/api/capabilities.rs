//! `GET /api/v1/capabilities` - what THIS server's HTTP API can do.
//!
//! The answer is DERIVED, never written down twice: a capability is
//! AVAILABLE only if every job kind it needs has a runner in
//! `AppState::runners`. So an engine that is not wired in (or not
//! implemented yet) reports NOT_IMPLEMENTED here and is refused by
//! `POST /jobs` for the same reason, and wiring one up cannot leave this
//! endpoint behind.
//!
//! The native-image entries come from the same `meb_core` definition the
//! desktop app uses. Operations the native engine supports but this API does
//! not expose (crop, rotate), and engines the web server does not have
//! (archives, video, speech), are reported as NOT_IMPLEMENTED - never as
//! AVAILABLE - so the frontend's existing capability gating disables them.

use crate::session::Owner;
use crate::spec::JobKind;
use crate::AppState;
use axum::extract::State;
use axum::Json;
use meb_core::capabilities::{capability, native_image_capabilities, CapabilityState};
use meb_core::format::{OfficeFormat, SourceFormat};
use serde_json::{json, Value};

/// Which job kinds each capability id needs. A capability with an empty list
/// has no route into the job API at all and is always NOT_IMPLEMENTED.
///
/// The ids are the ones the desktop app already reports and the frontend
/// already gates on; they are deliberately unchanged.
const CAPABILITY_KINDS: &[(&str, &[JobKind])] = &[
    ("image_conversion", &[JobKind::ImageConvert]),
    ("image_resize", &[JobKind::ImageConvert]),
    // Crop and rotate are native-pipeline operations with no job kind yet.
    ("image_crop", &[]),
    ("image_rotate", &[]),
    (
        "pdf_structural_ops",
        &[
            JobKind::PdfMerge,
            JobKind::PdfSplit,
            JobKind::PdfCompress,
            JobKind::PdfRotate,
            JobKind::PdfWatermark,
        ],
    ),
    ("office_to_pdf", &[JobKind::OfficeConvert]),
    // PDF -> editable document. A capability of its own, because it is a
    // different (and experimental) operation - see `JobKind::PdfToOffice`.
    ("pdf_to_office", &[JobKind::PdfToOffice]),
    ("ocr", &[JobKind::PdfOcr]),
    // No job kind: these engines are not part of the web server.
    ("svg_rasterization", &[]),
    ("advanced_image_formats", &[]),
    ("heic_conversion", &[]),
    ("archive_operations", &[]),
    ("pdf_text_editing", &[]),
    ("video_conversion", &[]),
    ("video_trimming", &[]),
    ("audio_extraction", &[]),
    ("speech_transcription", &[]),
];

const WEB_PENDING_MESSAGE: &str = "Not yet available in the web version.";

pub async fn get_capabilities(State(state): State<AppState>, _owner: Owner) -> Json<Value> {
    // Messages for the native-image ids come from the shared definition, so
    // the desktop and the web describe the same engine the same way.
    let native = native_image_capabilities();
    let mut caps = Vec::with_capacity(CAPABILITY_KINDS.len());
    for (id, kinds) in CAPABILITY_KINDS {
        let available = !kinds.is_empty() && kinds.iter().all(|k| state.runners.supports(*k));
        let described = native.iter().find(|c| c.id == *id);
        caps.push(match (available, described) {
            (true, Some(c)) => c.clone(),
            (true, None) => capability(id, CapabilityState::Available, available_message(id)),
            (false, _) => capability(id, CapabilityState::NotImplemented, WEB_PENDING_MESSAGE),
        });
    }

    // The job kinds a client may actually post right now, so a UI can be
    // built from this response instead of from assumptions.
    let kinds: Vec<&str> = JobKind::ALL
        .into_iter()
        .filter(|k| state.runners.supports(*k))
        .map(JobKind::wire)
        .collect();

    let office_formats: Vec<&str> = OfficeFormat::ALL
        .into_iter()
        .map(OfficeFormat::canonical_extension)
        .collect();
    let documents_available = state.runners.supports(JobKind::OfficeConvert);

    // The exact per-input matrix, from the same definition `JobSpec::
    // from_request` validates against - so what is advertised and what is
    // accepted cannot drift apart. `outputs` below stays the flat union of
    // it, which is what the existing clients read.
    let office_conversions: Value = if documents_available {
        OfficeFormat::ALL
            .into_iter()
            .map(|source| {
                let targets: Vec<&str> = crate::spec::office_conversion_targets(source)
                    .into_iter()
                    .map(SourceFormat::canonical_extension)
                    .collect();
                (source.canonical_extension().to_string(), json!(targets))
            })
            .collect::<serde_json::Map<String, Value>>()
            .into()
    } else {
        json!({})
    };
    let pdf_outputs: Vec<&str> = {
        let mut outputs = Vec::new();
        if state.runners.supports(JobKind::PdfMerge) {
            outputs.push("pdf");
        }
        if state.runners.supports(JobKind::PdfToOffice) {
            outputs.extend(
                crate::spec::pdf_reconstruction_targets()
                    .into_iter()
                    .map(OfficeFormat::canonical_extension),
            );
        }
        outputs
    };

    // Operations that are wired, but cannot be relied on to preserve the
    // document. Published so a UI can label them before the user commits a
    // file to one, rather than leaving the honest label to the download
    // name alone.
    let experimental: Vec<&str> = JobKind::ALL
        .into_iter()
        .filter(|k| k.is_experimental() && state.runners.supports(*k))
        .map(JobKind::wire)
        .collect();

    Json(json!({
        "capabilities": caps,
        "kinds": kinds,
        "experimental_kinds": experimental,
        "limits": {
            "max_upload_bytes": state.config.max_upload_bytes,
            "session_quota_bytes": state.config.session_quota_bytes,
            "max_active_jobs": state.config.max_active_jobs_per_session,
            "max_inputs_per_job": crate::spec::MAX_INPUTS,
        },
        "formats": {
            "image": {
                "inputs": ["jpg", "jpeg", "png", "webp", "bmp", "gif", "tif", "tiff"],
                "outputs": ["jpg", "png", "webp", "bmp", "gif", "tiff"],
            },
            // Office files can be UPLOADED whatever the engine situation is
            // (that is an upload-admission question); what they can be
            // converted TO depends on the engine being wired in.
            "office": {
                "inputs": office_formats.clone(),
                "outputs": if documents_available {
                    let mut outputs = vec!["pdf"];
                    outputs.extend(office_formats.iter().copied());
                    outputs
                } else {
                    Vec::new()
                },
                // Which of those outputs each input may actually become:
                // a text document cannot become a spreadsheet, and no
                // format converts to itself.
                "conversions": office_conversions,
            },
            "pdf": {
                "inputs": ["pdf"],
                "outputs": pdf_outputs,
            }
        }
    }))
}

/// Message for an available capability the shared native-image definition
/// does not describe (the document engines).
fn available_message(id: &str) -> &'static str {
    match id {
        "pdf_structural_ops" => "Merge, split, compress, rotate and watermark PDF files.",
        "office_to_pdf" => "Convert Word, Excel and PowerPoint files to PDF.",
        "pdf_to_office" => {
            "Rebuild an editable Word document from a PDF. Experimental              (deneysel): layout and tables are often only approximated, and              a scanned PDF yields little usable structure."
        }
        "ocr" => "Add a searchable text layer to scanned PDF files.",
        _ => "Available.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_of(caps: &[meb_core::capabilities::Capability], id: &str) -> CapabilityState {
        caps.iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("{id} is not reported at all"))
            .state
    }

    #[test]
    fn every_capability_id_is_reported_exactly_once() {
        let mut ids: Vec<&str> = CAPABILITY_KINDS.iter().map(|(id, _)| *id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "a capability id is listed twice");
    }

    #[test]
    fn every_job_kind_is_reachable_through_some_capability() {
        // Otherwise a kind could be wired up and still never appear as
        // available to a client.
        for kind in JobKind::ALL {
            assert!(
                CAPABILITY_KINDS
                    .iter()
                    .any(|(_, kinds)| kinds.contains(&kind)),
                "job kind {} belongs to no capability",
                kind.wire()
            );
        }
    }

    /// The derivation itself, without an HTTP request: an id is available
    /// only when every kind it needs has a runner.
    #[test]
    fn availability_follows_the_registered_runners() {
        let describe = |supports: &dyn Fn(JobKind) -> bool| -> Vec<(&'static str, bool)> {
            CAPABILITY_KINDS
                .iter()
                .map(|(id, kinds)| {
                    (*id, !kinds.is_empty() && kinds.iter().all(|k| supports(*k)))
                })
                .collect()
        };

        // Production today: the in-process image pipeline only.
        let image_only = describe(&|k| k == JobKind::ImageConvert);
        for (id, available) in &image_only {
            let expected = matches!(*id, "image_conversion" | "image_resize");
            assert_eq!(available, &expected, "{id}");
        }

        // With every kind wired, the document capabilities turn available -
        // and the ones with no job kind stay unavailable.
        let all_wired = describe(&|_| true);
        assert!(all_wired
            .iter()
            .any(|(id, a)| *id == "pdf_structural_ops" && *a));
        assert!(all_wired.iter().any(|(id, a)| *id == "ocr" && *a));
        assert!(all_wired
            .iter()
            .any(|(id, a)| *id == "archive_operations" && !*a));
        assert!(all_wired.iter().any(|(id, a)| *id == "image_crop" && !*a));
    }

    #[test]
    fn native_image_messages_are_reused_not_restated() {
        // The two exposed native ids must be described by `meb_core`, so the
        // desktop app and the web server never drift apart in wording.
        let native = native_image_capabilities();
        for id in ["image_conversion", "image_resize"] {
            assert_eq!(state_of(&native, id), CapabilityState::Available);
        }
    }
}
