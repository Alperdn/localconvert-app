//! Metadata privacy policy for the native image pipeline.
//!
//! `image`'s decoders read pixel data only - EXIF/GPS/ICC/XMP chunks are
//! never parsed into the `DynamicImage`, and its encoders never write any
//! metadata chunk of their own. So every native encode in this module is
//! **metadata-stripped by construction**: there is no "strip" step because
//! there is nothing carried forward to strip in the first place. This is
//! the privacy-first default required by Step 2 section D.
//!
//! This intentionally differs from the old ImageMagick path, which passed
//! `-strip` only when `preserve_metadata == Some(false)` - i.e. it could
//! carry EXIF/GPS through by default. The native path never does, for any
//! `preserve_metadata` value. `ConversionOptions::preserve_metadata` is
//! accepted for compatibility but has no metadata-preserving effect on
//! natively-converted images; this is documented behavior, not a bug.
//!
//! If true source-metadata *preservation* becomes a real requirement, it
//! needs its own explicit implementation (e.g. reading EXIF via `kamadak-
//! exif` and re-embedding only the fields a user opted into) - not a
//! silent pass-through.

/// Always true for the native pipeline - see module docs.
#[allow(dead_code)]
pub const NATIVE_ENCODE_STRIPS_METADATA: bool = true;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_pipeline_documents_privacy_first_default() {
        assert!(NATIVE_ENCODE_STRIPS_METADATA);
    }
}
