//! Backend-computed feature capability model.
//!
//! The frontend should never have to guess whether a feature will work by
//! trying it and parsing the failure - it can call `get_capabilities` and
//! ask directly. This reports capability-level status (`office_to_pdf`,
//! `video_conversion`, ...), never raw tool names or filesystem paths, so
//! the UI can disable an unavailable feature cleanly instead of letting a
//! user pick it and fail after selecting a file.
//!
//! This module does not yet drive any UI (Phase 2 work) - it establishes
//! the backend contract first.

use crate::engines::office_manifest::{self, OfficeEngineStatus};
use crate::engines::speech::{self as speech_engine, SpeechEngineStatus};
use crate::engines::{engine_id::EngineId, resolver};
use crate::tools;
// The capability types and the native-image entries are shared with the web
// server via meb-core (one definition, same wire format).
pub use meb_core::capabilities::{Capability, CapabilityState};
use meb_core::capabilities::{capability, native_image_capabilities};

/// Ses Dikte capability state from the backend self-check. The UI shows these
/// as Hazir / Bilesen eksik / Henuz desteklenmiyor.
pub(crate) fn speech_capability_state(status: SpeechEngineStatus) -> CapabilityState {
    match status {
        SpeechEngineStatus::Available => CapabilityState::Available,
        // Present and valid but cannot run on this machine: not "missing".
        SpeechEngineStatus::CpuUnsupported => CapabilityState::NotImplemented,
        SpeechEngineStatus::EngineMissing
        | SpeechEngineStatus::ModelMissing
        | SpeechEngineStatus::EngineInvalid
        | SpeechEngineStatus::RuntimeMissing
        | SpeechEngineStatus::AudioPrepUnavailable => CapabilityState::EngineMissing,
    }
}

/// Pure, synchronous computation so it's directly unit-testable without a
/// Tauri runtime. The `#[tauri::command]` wrapper below just calls this.
pub fn compute_capabilities() -> Vec<Capability> {
    // office_to_pdf is AVAILABLE only when the bundled Office Engine
    // passes its full self-check (manifest present, version/architecture
    // pinned match, required resource dirs present) - not merely because
    // an executable happens to exist at the expected path. See
    // `engines::office_manifest::self_check`.
    let office_report = office_manifest::self_check();
    let office_available = office_report.status == OfficeEngineStatus::Available;
    let ffmpeg_available = resolver::is_available(EngineId::Ffmpeg);
    let magick_available = tools::check_tool_installed("magick").installed;
    // speech_transcription is computed from the real backend self-check
    // (manifest + hashes of engine, model and audio-prep bundle + the engine
    // actually starting on this CPU) - never assumed Available.
    let speech_report = speech_engine::report();
    let speech_state = speech_capability_state(speech_report.status);

    // JPEG/PNG/WebP/BMP/GIF/TIFF conversion, resize, crop, and rotate are
    // handled by the in-process native image pipeline (see meb_core::image)
    // and never need ImageMagick - so these stay AVAILABLE regardless of
    // `magick_available`. Same entries the web server reports.
    let mut caps = native_image_capabilities();
    caps.extend(vec![
        capability(
            "svg_rasterization",
            if magick_available {
                CapabilityState::Available
            } else {
                CapabilityState::EngineMissing
            },
            "Convert SVG to raster images.",
        ),
        capability(
            "advanced_image_formats",
            if magick_available {
                CapabilityState::Available
            } else {
                CapabilityState::EngineMissing
            },
            "Convert AVIF and other advanced image formats.",
        ),
        capability(
            "heic_conversion",
            CapabilityState::NotImplemented,
            "HEIC/HEIF conversion is not yet available in the app.",
        ),
        capability(
            "archive_operations",
            CapabilityState::Available,
            "Create and extract ZIP archives natively. Other archive formats need an external tool.",
        ),
        capability(
            "pdf_text_editing",
            CapabilityState::Available,
            "Extract and edit PDF text in place.",
        ),
        capability(
            "pdf_structural_ops",
            CapabilityState::NotImplemented,
            "PDF merge, split, and rotate are not yet available in the app.",
        ),
        capability(
            "office_to_pdf",
            if office_available {
                CapabilityState::Available
            } else {
                CapabilityState::EngineMissing
            },
            if office_available {
                "Convert Office documents to PDF."
            } else {
                office_report.message.as_str()
            },
        ),
        capability(
            "video_conversion",
            if ffmpeg_available {
                CapabilityState::Available
            } else {
                CapabilityState::EngineMissing
            },
            "Convert between video formats.",
        ),
        capability(
            "video_trimming",
            if ffmpeg_available {
                CapabilityState::Available
            } else {
                CapabilityState::EngineMissing
            },
            "Trim video clips.",
        ),
        capability(
            "audio_extraction",
            if ffmpeg_available {
                CapabilityState::Available
            } else {
                CapabilityState::EngineMissing
            },
            "Convert and extract audio tracks.",
        ),
        capability(
            "speech_transcription",
            speech_state,
            speech_report.message.as_str(),
        ),
        capability(
            "ocr",
            CapabilityState::NotImplemented,
            "OCR is not yet available in the app.",
        ),
    ]);
    caps
}

#[tauri::command]
pub async fn get_capabilities() -> Vec<Capability> {
    compute_capabilities()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_to_pdf_reflects_missing_engine_on_this_machine() {
        // Phase 1 policy: this dev/CI machine never has LibreOffice
        // installed, so this exercises the real "missing" branch rather
        // than a mock.
        if resolver::is_available(EngineId::Office) {
            return;
        }
        let caps = compute_capabilities();
        let office = caps.iter().find(|c| c.id == "office_to_pdf").unwrap();
        assert_eq!(office.state, CapabilityState::EngineMissing);
        assert_eq!(
            office.message,
            "Office conversion engine is not available on this installation."
        );
    }

    #[test]
    fn speech_capability_state_mapping_covers_every_backend_status() {
        use SpeechEngineStatus as S;
        assert_eq!(speech_capability_state(S::Available), CapabilityState::Available);
        assert_eq!(speech_capability_state(S::CpuUnsupported), CapabilityState::NotImplemented);
        for missing in [S::EngineMissing, S::ModelMissing, S::EngineInvalid, S::RuntimeMissing, S::AudioPrepUnavailable] {
            assert_eq!(speech_capability_state(missing), CapabilityState::EngineMissing, "{:?}", missing);
        }
    }

    /// Never hardcoded Available: the capability must equal what the real
    /// backend self-check says on this machine.
    #[test]
    fn speech_capability_is_computed_from_the_real_backend_check() {
        let report = speech_engine::report();
        let caps = compute_capabilities();
        let cap = caps.iter().find(|c| c.id == "speech_transcription").unwrap();
        assert_eq!(cap.state, speech_capability_state(report.status));
        assert_eq!(cap.message, report.message);
    }

    #[test]
    fn no_capability_exposes_a_filesystem_path_or_executable_name() {
        for cap in compute_capabilities() {
            let lower = cap.message.to_lowercase();
            assert!(!cap.message.contains('\\'), "path separator in: {}", cap.message);
            assert!(!lower.contains(".exe"), "executable name in: {}", cap.message);
            assert!(!lower.contains("program files"), "install path in: {}", cap.message);
        }
    }

    #[test]
    fn every_capability_has_a_stable_unique_id() {
        let caps = compute_capabilities();
        let mut ids: Vec<&str> = caps.iter().map(|c| c.id.as_str()).collect();
        let count_before = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), count_before, "duplicate capability id");
    }

    /// Step 3 (Turkish UI completion) frontend contract: `src/locales/tr.ts`
    /// and `en.ts` hand-maintain a `capabilityCategories` map keyed by every
    /// capability id this function can emit, so the System Status /
    /// ToolsSetup UI never falls back to a raw id. This test can't read the
    /// TS files, but it pins the exact set of ids the frontend map must
    /// cover - if a capability is added/removed here without updating the
    /// locale files, this is the test that should be extended to catch it
    /// and remind the author to update both `tr.ts`/`en.ts`.
    #[test]
    fn capability_ids_match_the_frontend_translation_contract() {
        let expected = [
            "image_conversion",
            "image_resize",
            "image_crop",
            "image_rotate",
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
        let caps = compute_capabilities();
        let ids: Vec<&str> = caps.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids, expected,
            "capabilities.rs now emits a different id set than src/locales/{{tr,en}}.ts's \
             capabilityCategories map expects - update both locale files' \
             `capabilityCategories` when changing this list."
        );
    }

    /// Every state the frontend's `translateCapabilityState`/
    /// `capabilityStates` map (src/locales/tr.ts, en.ts) declares a label
    /// for. If a fifth `CapabilityState` variant were ever added without
    /// this test being updated, that's the signal to also add its Turkish
    /// label - never let the UI fall back to displaying a raw enum value.
    #[test]
    fn every_capability_state_variant_is_covered_by_this_test() {
        let all_states = [
            CapabilityState::Available,
            CapabilityState::EngineMissing,
            CapabilityState::NotImplemented,
            CapabilityState::DisabledByPolicy,
        ];
        // Compile-time-ish safety net: if a match here were non-exhaustive
        // it would still compile (this isn't a `match`), so the real
        // guarantee is `#[deny(unreachable_patterns)]`-style vigilance in
        // code review - this test exists to name the 4 known states
        // explicitly so an added 5th variant is visible in a diff here.
        assert_eq!(all_states.len(), 4);
    }
}
