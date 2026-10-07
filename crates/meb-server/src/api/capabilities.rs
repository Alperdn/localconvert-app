//! `GET /api/v1/capabilities` - what THIS server's HTTP API can do.
//!
//! The native-image entries come from the same `meb_core` definition the
//! desktop app uses. Operations the native engine supports but this API
//! does not expose yet (crop, rotate), and every engine that is not wired
//! into the web server (Office, FFmpeg, speech, ...), are reported as
//! NOT_IMPLEMENTED - never as AVAILABLE - so the frontend's existing
//! capability gating disables them.

use crate::session::Owner;
use crate::AppState;
use axum::extract::State;
use axum::Json;
use meb_core::capabilities::{capability, native_image_capabilities, CapabilityState};
use serde_json::{json, Value};

/// Native-image capability ids reachable through `POST /api/v1/jobs`.
const WEB_EXPOSED_NATIVE: [&str; 2] = ["image_conversion", "image_resize"];

/// Ids the desktop app reports that the web server does not provide yet.
const NOT_IN_WEB_YET: [&str; 12] = [
    "svg_rasterization",
    "advanced_image_formats",
    "heic_conversion",
    "archive_operations",
    "pdf_text_editing",
    "pdf_structural_ops",
    "office_to_pdf",
    "video_conversion",
    "video_trimming",
    "audio_extraction",
    "speech_transcription",
    "ocr",
];

const WEB_PENDING_MESSAGE: &str = "Not yet available in the web version.";

pub async fn get_capabilities(State(state): State<AppState>, _owner: Owner) -> Json<Value> {
    let mut caps = native_image_capabilities();
    for cap in caps.iter_mut() {
        if !WEB_EXPOSED_NATIVE.contains(&cap.id.as_str()) {
            cap.state = CapabilityState::NotImplemented;
            cap.message = WEB_PENDING_MESSAGE.to_string();
        }
    }
    caps.extend(
        NOT_IN_WEB_YET
            .iter()
            .map(|id| capability(id, CapabilityState::NotImplemented, WEB_PENDING_MESSAGE)),
    );

    Json(json!({
        "capabilities": caps,
        "limits": {
            "max_upload_bytes": state.config.max_upload_bytes,
            "session_quota_bytes": state.config.session_quota_bytes,
            "max_active_jobs": state.config.max_active_jobs_per_session,
        },
        "formats": {
            "image": {
                "inputs": ["jpg", "jpeg", "png", "webp", "bmp", "gif", "tif", "tiff"],
                "outputs": ["jpg", "png", "webp", "bmp", "gif", "tiff"],
            }
        }
    }))
}
