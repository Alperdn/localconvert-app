//! Bounded image decoding - guards against decompression bombs by checking
//! declared dimensions *before* allocating pixel buffers for the full
//! decode.

use super::error::{ImageNativeError, ImageNativeErrorKind};
use super::{MAX_DECODED_PIXELS, MAX_DIMENSION};
use image::DynamicImage;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

pub fn decode_bounded(path: &Path) -> Result<DynamicImage, ImageNativeError> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string()))?
        .with_guessed_format()
        .map_err(|e| ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string()))?;

    if reader.format().is_none() {
        return Err(ImageNativeError::new(ImageNativeErrorKind::UnsupportedFormat));
    }

    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string()))?;
    check_dimensions(width, height)?;

    // Re-open: `into_dimensions()` consumes the reader without allocating
    // pixel data, so a second open is the simplest way to then decode the
    // full image now that bounds are confirmed safe.
    let reader = image::ImageReader::open(path)
        .map_err(|e| ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string()))?
        .with_guessed_format()
        .map_err(|e| ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string()))?;

    reader.decode().map_err(ImageNativeError::from)
}

/// Reject animated GIF and multi-page TIFF input up front, rather than
/// letting `decode_bounded` silently decode just the first frame/page and
/// have that get written out as if it were a complete conversion (Step 3
/// fix pass, item 5). Single-frame GIF and single-page TIFF - the common
/// case - are unaffected and still convert normally.
pub fn check_single_frame(path: &Path) -> Result<(), ImageNativeError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "gif" => check_gif_single_frame(path),
        "tif" | "tiff" => check_tiff_single_page(path),
        _ => Ok(()),
    }
}

fn open_bounded(path: &Path) -> Result<BufReader<File>, ImageNativeError> {
    let file = File::open(path)
        .map_err(|e| ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string()))?;
    Ok(BufReader::new(file))
}

fn check_gif_single_frame(path: &Path) -> Result<(), ImageNativeError> {
    use image::codecs::gif::GifDecoder;
    use image::AnimationDecoder;

    let decoder = GifDecoder::new(open_bounded(path)?)
        .map_err(|e| ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string()))?;
    let mut frames = decoder.into_frames();
    // Only decode as many frames as needed to know whether there's more
    // than one - never the whole animation.
    let has_first_frame = frames.next().is_some();
    if has_first_frame && frames.next().is_some() {
        return Err(ImageNativeError::new(ImageNativeErrorKind::MultiFrameUnsupported));
    }
    Ok(())
}

fn check_tiff_single_page(path: &Path) -> Result<(), ImageNativeError> {
    let decoder = tiff::decoder::Decoder::new(open_bounded(path)?)
        .map_err(|e| ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string()))?;
    // `more_images()` reports whether another IFD (page) follows - it does
    // not decode pixel data, so this stays cheap regardless of page count.
    if decoder.more_images() {
        return Err(ImageNativeError::new(ImageNativeErrorKind::MultiFrameUnsupported));
    }
    Ok(())
}

pub fn check_dimensions(width: u32, height: u32) -> Result<(), ImageNativeError> {
    if width == 0 || height == 0 {
        return Err(ImageNativeError::new(ImageNativeErrorKind::InvalidDimensions));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(
            ImageNativeError::new(ImageNativeErrorKind::TooLarge).with_detail(format!("{}x{}", width, height))
        );
    }
    let pixels = width as u64 * height as u64;
    if pixels > MAX_DECODED_PIXELS {
        return Err(ImageNativeError::new(ImageNativeErrorKind::TooLarge).with_detail(format!("{} px", pixels)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_dimensions() {
        assert!(check_dimensions(0, 100).is_err());
        assert!(check_dimensions(100, 0).is_err());
    }

    #[test]
    fn rejects_oversized_dimensions() {
        assert!(check_dimensions(MAX_DIMENSION + 1, 100).is_err());
        assert!(check_dimensions(100, MAX_DIMENSION + 1).is_err());
    }

    #[test]
    fn rejects_excessive_pixel_count_within_dimension_bounds() {
        // Both under MAX_DIMENSION individually, but the product blows the
        // pixel-count budget - this is the actual decompression-bomb shape.
        let side = (MAX_DECODED_PIXELS as f64).sqrt() as u32 + 2000;
        if side <= MAX_DIMENSION {
            assert!(check_dimensions(side, side).is_err());
        }
    }

    #[test]
    fn accepts_reasonable_dimensions() {
        assert!(check_dimensions(1920, 1080).is_ok());
    }

    #[test]
    fn decode_bounded_rejects_nonexistent_file() {
        let result = decode_bounded(Path::new("Z:\\definitely\\not\\a\\real\\path.png"));
        assert!(result.is_err());
    }
}
