//! Structured native-image errors. Mirrors the shape of
//! `engines::error::EngineError` (stable `code()`, safe `user_message()`)
//! but for the in-process image pipeline, which has no "engine" to name.
//!
//! `user_message()` never contains a filesystem path or raw decoder
//! internals - those belong only in the `Debug` derive, which this app
//! never logs to disk.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageNativeErrorKind {
    UnsupportedFormat,
    TooLarge,
    DecodeFailed,
    EncodeFailed,
    InvalidDimensions,
    /// The native pipeline only round-trips a single frame/page. Rather than
    /// silently converting just the first frame of an animated GIF or the
    /// first page of a multi-page TIFF and presenting that as a complete
    /// conversion, this rejects the file up front (Step 3 fix pass, item 5 -
    /// prefer a loud, structured error over silent data loss).
    MultiFrameUnsupported,
    /// A caller-requested cooperative cancellation (see
    /// `super::PipelineHooks`). Never produced by plain `convert_file`.
    Cancelled,
}

impl ImageNativeErrorKind {
    pub fn code(self) -> &'static str {
        match self {
            ImageNativeErrorKind::UnsupportedFormat => "UNSUPPORTED_IMAGE_FORMAT",
            ImageNativeErrorKind::TooLarge => "IMAGE_TOO_LARGE",
            ImageNativeErrorKind::DecodeFailed => "IMAGE_DECODE_FAILED",
            ImageNativeErrorKind::EncodeFailed => "IMAGE_ENCODE_FAILED",
            ImageNativeErrorKind::InvalidDimensions => "INVALID_DIMENSIONS",
            ImageNativeErrorKind::MultiFrameUnsupported => "IMAGE_MULTI_FRAME_UNSUPPORTED",
            ImageNativeErrorKind::Cancelled => "CANCELLED",
        }
    }

    fn default_message(self) -> &'static str {
        match self {
            ImageNativeErrorKind::UnsupportedFormat => "This image format is not supported yet.",
            ImageNativeErrorKind::TooLarge => "Image exceeds safe processing limits.",
            ImageNativeErrorKind::DecodeFailed => "Could not read this image file.",
            ImageNativeErrorKind::EncodeFailed => "Could not write the converted image.",
            ImageNativeErrorKind::InvalidDimensions => "The requested dimensions are invalid.",
            ImageNativeErrorKind::MultiFrameUnsupported => {
                "Animated GIF and multi-page TIFF are not supported yet - only single-frame images convert."
            }
            ImageNativeErrorKind::Cancelled => "The operation was cancelled.",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageNativeError {
    kind: ImageNativeErrorKind,
    user_message: String,
    technical_detail: Option<String>,
}

impl ImageNativeError {
    pub fn new(kind: ImageNativeErrorKind) -> Self {
        Self {
            kind,
            user_message: kind.default_message().to_string(),
            technical_detail: None,
        }
    }

    /// In-memory-only technical detail (decoder error text, dimensions).
    /// Never included in `Display`/`to_string()`.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.technical_detail = Some(detail.into());
        self
    }

    pub fn kind(&self) -> ImageNativeErrorKind {
        self.kind
    }

    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    pub fn user_message(&self) -> &str {
        &self.user_message
    }
}

impl fmt::Display for ImageNativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.user_message)
    }
}

impl From<ImageNativeError> for String {
    fn from(e: ImageNativeError) -> String {
        e.to_string()
    }
}

impl From<image::ImageError> for ImageNativeError {
    fn from(e: image::ImageError) -> Self {
        use image::ImageError;
        let kind = match e {
            ImageError::Unsupported(_) => ImageNativeErrorKind::UnsupportedFormat,
            ImageError::Decoding(_) => ImageNativeErrorKind::DecodeFailed,
            ImageError::Encoding(_) => ImageNativeErrorKind::EncodeFailed,
            ImageError::Parameter(_) => ImageNativeErrorKind::InvalidDimensions,
            ImageError::Limits(_) => ImageNativeErrorKind::TooLarge,
            ImageError::IoError(_) => ImageNativeErrorKind::DecodeFailed,
        };
        ImageNativeError::new(kind).with_detail(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_match_the_documented_set() {
        assert_eq!(
            ImageNativeError::new(ImageNativeErrorKind::UnsupportedFormat).code(),
            "UNSUPPORTED_IMAGE_FORMAT"
        );
        assert_eq!(
            ImageNativeError::new(ImageNativeErrorKind::TooLarge).code(),
            "IMAGE_TOO_LARGE"
        );
        assert_eq!(
            ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).code(),
            "IMAGE_DECODE_FAILED"
        );
        assert_eq!(
            ImageNativeError::new(ImageNativeErrorKind::EncodeFailed).code(),
            "IMAGE_ENCODE_FAILED"
        );
        assert_eq!(
            ImageNativeError::new(ImageNativeErrorKind::InvalidDimensions).code(),
            "INVALID_DIMENSIONS"
        );
        assert_eq!(
            ImageNativeError::new(ImageNativeErrorKind::MultiFrameUnsupported).code(),
            "IMAGE_MULTI_FRAME_UNSUPPORTED"
        );
    }

    #[test]
    fn technical_detail_never_appears_in_display() {
        let err = ImageNativeError::new(ImageNativeErrorKind::DecodeFailed)
            .with_detail("C:\\Users\\alice\\secret\\input.png: garbage header");
        let rendered = err.to_string();
        assert!(!rendered.contains("alice"));
        assert!(!rendered.contains("secret"));
    }
}
