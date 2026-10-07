//! Shared capability vocabulary (moved from `src-tauri/src/capabilities.rs`).
//!
//! The types and the native-image entries live here so the desktop app and
//! the web server report the native image engine from ONE definition. Each
//! host still composes its own full list: the desktop adds Office/FFmpeg/
//! speech self-checks, the web server reports what its HTTP API actually
//! exposes. The wire format (`SCREAMING_SNAKE_CASE` states) is unchanged.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CapabilityState {
    Available,
    EngineMissing,
    NotImplemented,
    #[allow(dead_code)]
    DisabledByPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct Capability {
    pub id: String,
    pub state: CapabilityState,
    /// Safe to show in the UI as-is - never a path or executable name.
    pub message: String,
}

pub fn capability(id: &str, state: CapabilityState, message: &str) -> Capability {
    Capability {
        id: id.to_string(),
        state,
        message: message.to_string(),
    }
}

/// JPEG/PNG/WebP/BMP/GIF/TIFF conversion, resize, crop, and rotate are
/// handled by the in-process native image pipeline (see `crate::image`) and
/// never need ImageMagick or any other external tool - so these are
/// AVAILABLE whenever this crate is linked in.
pub fn native_image_capabilities() -> Vec<Capability> {
    vec![
        capability(
            "image_conversion",
            CapabilityState::Available,
            "Convert between common image formats (JPEG, PNG, WebP, BMP, GIF, TIFF).",
        ),
        capability("image_resize", CapabilityState::Available, "Resize images."),
        capability("image_crop", CapabilityState::Available, "Crop images."),
        capability(
            "image_rotate",
            CapabilityState::Available,
            "Rotate images in 90-degree steps.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_image_capabilities_are_available_and_in_stable_order() {
        let caps = native_image_capabilities();
        let ids: Vec<&str> = caps.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "image_conversion",
                "image_resize",
                "image_crop",
                "image_rotate"
            ]
        );
        assert!(caps.iter().all(|c| c.state == CapabilityState::Available));
    }

    #[test]
    fn state_serializes_in_the_existing_wire_format() {
        let json = serde_json::to_string(&CapabilityState::EngineMissing).unwrap();
        assert_eq!(json, "\"ENGINE_MISSING\"");
    }
}
