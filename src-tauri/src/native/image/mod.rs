//! Native (pure-Rust, in-process) raster image pipeline. Replaces the
//! external `magick` (ImageMagick) subprocess for the common formats:
//! JPEG, PNG, WebP, BMP, GIF (single-frame), TIFF (single-page).
//!
//! Explicitly NOT covered here (still ImageMagick/engine-backed or
//! unimplemented - see `docs`/final report for the full classification):
//! - SVG rasterization (stays ImageMagick-backed, see `converter::convert_vector`)
//! - HEIC/HEIF, AVIF, PSD (NOT_IMPLEMENTED)
//! - animated GIF, multi-page TIFF as *input* to any output-producing
//!   operation below: rejected with `IMAGE_MULTI_FRAME_UNSUPPORTED`
//!   (`decode::check_single_frame`) rather than silently converting only
//!   the first frame/page and presenting that as a complete conversion
//!   (Step 3 fix pass, item 5). `thumbnail()` is the one exception - it's
//!   preview-only and showing just the first frame there is fine.

pub mod decode;
pub mod encode;
pub mod error;
pub mod metadata;
pub mod transform;

pub use error::{ImageNativeError, ImageNativeErrorKind};

use image::DynamicImage;
use std::path::Path;

/// Decompression-bomb guard: refuse to fully decode an image whose pixel
/// count exceeds this, regardless of file size. ~40 megapixels covers any
/// normal photo/scan; a file claiming more is either malicious or not
/// something this app needs to open.
pub const MAX_DECODED_PIXELS: u64 = 40_000_000;
/// Independent per-axis sanity bound, checked before the pixel-count
/// multiplication (which itself cannot overflow u64, but a single axis of
/// e.g. 4 billion is nonsensical well before that check runs).
pub const MAX_DIMENSION: u32 = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeImageFormat {
    Jpeg,
    Png,
    WebP,
    Bmp,
    Gif,
    Tiff,
}

impl NativeImageFormat {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "webp" => Some(Self::WebP),
            "bmp" => Some(Self::Bmp),
            "gif" => Some(Self::Gif),
            "tif" | "tiff" => Some(Self::Tiff),
            _ => None,
        }
    }

    /// Whether both decoding *and* encoding this format are handled by the
    /// native pipeline (as opposed to only one direction, or not at all).
    pub fn is_native(ext: &str) -> bool {
        Self::from_extension(ext).is_some()
    }
}

#[derive(Debug, Clone, Default)]
pub struct NativeConvertOptions {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub quality: Option<u8>,
    pub png_compression_level: Option<u8>,
}

/// Full pipeline: bounded-decode -> optional resize -> encode -> write.
/// Used for straight format conversion (with an optional resize baked in,
/// matching the old `convert_image` behavior).
pub fn convert_file(
    input: &Path,
    output: &Path,
    target_format: NativeImageFormat,
    options: &NativeConvertOptions,
) -> Result<(), ImageNativeError> {
    decode::check_single_frame(input)?;
    let mut img = decode::decode_bounded(input)?;

    if let (Some(w), Some(h)) = (options.width, options.height) {
        img = transform::resize(&img, w, h)?;
    }

    write_encoded(&img, output, target_format, options)
}

pub fn resize_file(
    input: &Path,
    output: &Path,
    target_format: NativeImageFormat,
    width: u32,
    height: u32,
    quality: Option<u8>,
) -> Result<(), ImageNativeError> {
    decode::check_single_frame(input)?;
    let img = decode::decode_bounded(input)?;
    let resized = transform::resize(&img, width, height)?;
    write_encoded(
        &resized,
        output,
        target_format,
        &NativeConvertOptions {
            quality,
            ..Default::default()
        },
    )
}

pub fn crop_file(
    input: &Path,
    output: &Path,
    target_format: NativeImageFormat,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(), ImageNativeError> {
    decode::check_single_frame(input)?;
    let img = decode::decode_bounded(input)?;
    let cropped = transform::crop(&img, x, y, width, height)?;
    write_encoded(&cropped, output, target_format, &NativeConvertOptions::default())
}

pub fn rotate_file(
    input: &Path,
    output: &Path,
    target_format: NativeImageFormat,
    degrees: i32,
) -> Result<(), ImageNativeError> {
    decode::check_single_frame(input)?;
    let img = decode::decode_bounded(input)?;
    let rotated = transform::rotate(&img, degrees);
    write_encoded(&rotated, output, target_format, &NativeConvertOptions::default())
}

pub fn compress_file(
    input: &Path,
    output: &Path,
    target_format: NativeImageFormat,
    quality: u8,
) -> Result<(), ImageNativeError> {
    decode::check_single_frame(input)?;
    let img = decode::decode_bounded(input)?;
    write_encoded(
        &img,
        output,
        target_format,
        &NativeConvertOptions {
            quality: Some(quality),
            ..Default::default()
        },
    )
}

/// Bounded-decode + downscale-to-fit thumbnail, used by the preview path.
/// Never upscales.
pub fn thumbnail(input: &Path, max_dimension: u32) -> Result<DynamicImage, ImageNativeError> {
    let img = decode::decode_bounded(input)?;
    if img.width() <= max_dimension && img.height() <= max_dimension {
        return Ok(img);
    }
    Ok(img.resize(max_dimension, max_dimension, image::imageops::FilterType::Triangle))
}

fn write_encoded(
    img: &DynamicImage,
    output: &Path,
    format: NativeImageFormat,
    options: &NativeConvertOptions,
) -> Result<(), ImageNativeError> {
    let bytes = encode::encode_to_bytes(
        img,
        format,
        &encode::EncodeOptions {
            quality: options.quality,
            png_compression_level: options.png_compression_level,
        },
    )?;
    std::fs::write(output, bytes)
        .map_err(|e| ImageNativeError::new(ImageNativeErrorKind::EncodeFailed).with_detail(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lc_native_image_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_sample_png(path: &Path, with_alpha: bool) {
        if with_alpha {
            let mut img: RgbaImage = image::ImageBuffer::new(6, 4);
            for x in 0..6 {
                for y in 0..4 {
                    img.put_pixel(x, y, Rgba([10, 20, 30, if x < 3 { 0 } else { 255 }]));
                }
            }
            DynamicImage::ImageRgba8(img).save(path).unwrap();
        } else {
            let mut img: RgbImage = image::ImageBuffer::new(6, 4);
            for x in 0..6 {
                for y in 0..4 {
                    img.put_pixel(x, y, Rgb([x as u8 * 10, y as u8 * 10, 5]));
                }
            }
            DynamicImage::ImageRgb8(img).save(path).unwrap();
        }
    }

    #[test]
    fn png_to_jpeg_round_trip() {
        let dir = temp_dir();
        let src = dir.join("in.png");
        let dst = dir.join("out.jpg");
        write_sample_png(&src, false);

        convert_file(&src, &dst, NativeImageFormat::Jpeg, &NativeConvertOptions::default()).unwrap();
        assert!(dst.exists());
        let decoded = image::open(&dst).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (6, 4));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn jpeg_to_png_round_trip() {
        let dir = temp_dir();
        let src = dir.join("in.jpg");
        let dst = dir.join("out.png");
        let img: RgbImage = image::ImageBuffer::from_fn(6, 4, |x, y| Rgb([x as u8 * 10, y as u8 * 10, 5]));
        DynamicImage::ImageRgb8(img).save(&src).unwrap();

        convert_file(&src, &dst, NativeImageFormat::Png, &NativeConvertOptions::default()).unwrap();
        assert!(dst.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn webp_round_trip() {
        let dir = temp_dir();
        let src = dir.join("in.png");
        let dst = dir.join("out.webp");
        write_sample_png(&src, false);

        convert_file(&src, &dst, NativeImageFormat::WebP, &NativeConvertOptions::default()).unwrap();
        let decoded = image::open(&dst).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (6, 4));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resize_produces_exact_requested_dimensions() {
        let dir = temp_dir();
        let src = dir.join("in.png");
        let dst = dir.join("out.png");
        write_sample_png(&src, false);

        resize_file(&src, &dst, NativeImageFormat::Png, 12, 8, None).unwrap();
        let decoded = image::open(&dst).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (12, 8));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crop_out_of_bounds_is_rejected() {
        let dir = temp_dir();
        let src = dir.join("in.png");
        let dst = dir.join("out.png");
        write_sample_png(&src, false);

        let result = crop_file(&src, &dst, NativeImageFormat::Png, 0, 0, 999, 999);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), "INVALID_DIMENSIONS");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_90_swaps_output_dimensions() {
        let dir = temp_dir();
        let src = dir.join("in.png");
        let dst = dir.join("out.png");
        write_sample_png(&src, false); // 6x4

        rotate_file(&src, &dst, NativeImageFormat::Png, 90).unwrap();
        let decoded = image::open(&dst).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (4, 6));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn alpha_png_to_jpeg_produces_deterministic_white_background() {
        let dir = temp_dir();
        let src = dir.join("in.png");
        let dst = dir.join("out.jpg");
        write_sample_png(&src, true);

        convert_file(&src, &dst, NativeImageFormat::Jpeg, &NativeConvertOptions::default()).unwrap();
        let decoded = image::open(&dst).unwrap().to_rgb8();
        // Left half was fully transparent (10,20,30,0) - must come out
        // white-ish, not black and not the raw (10,20,30) source color.
        let px = decoded.get_pixel(0, 0);
        assert!(px[0] > 200 && px[1] > 200 && px[2] > 200, "{:?}", px);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_file_is_rejected_with_structured_error() {
        let dir = temp_dir();
        let bogus = dir.join("not_an_image.png");
        std::fs::write(&bogus, b"this is not image data").unwrap();
        let dst = dir.join("out.jpg");

        let result = convert_file(&bogus, &dst, NativeImageFormat::Jpeg, &NativeConvertOptions::default());
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_declared_dimensions_are_rejected_without_full_decode() {
        assert!(decode::check_dimensions(MAX_DIMENSION + 1, 10).is_err());
    }

    #[test]
    fn unsupported_extension_returns_none_from_from_extension() {
        assert!(NativeImageFormat::from_extension("heic").is_none());
        assert!(NativeImageFormat::from_extension("psd").is_none());
        assert!(NativeImageFormat::from_extension("avif").is_none());
    }

    #[test]
    fn common_formats_are_recognized_as_native() {
        for ext in ["jpg", "jpeg", "png", "webp", "bmp", "gif", "tiff", "tif"] {
            assert!(NativeImageFormat::is_native(ext), "{} should be native", ext);
        }
    }

    #[test]
    fn animated_gif_is_rejected_not_silently_truncated() {
        let dir = temp_dir();
        let src = dir.join("in.gif");
        let dst = dir.join("out.png");

        let frame1: RgbaImage = image::ImageBuffer::from_pixel(4, 4, Rgba([255, 0, 0, 255]));
        let frame2: RgbaImage = image::ImageBuffer::from_pixel(4, 4, Rgba([0, 255, 0, 255]));
        {
            let file = std::fs::File::create(&src).unwrap();
            let mut encoder = image::codecs::gif::GifEncoder::new(file);
            encoder
                .encode_frames(vec![
                    image::Frame::new(frame1),
                    image::Frame::new(frame2),
                ])
                .unwrap();
        }

        let result = convert_file(&src, &dst, NativeImageFormat::Png, &NativeConvertOptions::default());
        assert!(result.is_err(), "animated GIF must not silently convert only the first frame");
        assert_eq!(result.unwrap_err().code(), "IMAGE_MULTI_FRAME_UNSUPPORTED");
        assert!(!dst.exists(), "no partial output should be written for a rejected multi-frame input");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_frame_gif_still_converts_normally() {
        let dir = temp_dir();
        let src = dir.join("in.gif");
        let dst = dir.join("out.png");

        let frame: RgbaImage = image::ImageBuffer::from_pixel(4, 4, Rgba([255, 0, 0, 255]));
        {
            let file = std::fs::File::create(&src).unwrap();
            let mut encoder = image::codecs::gif::GifEncoder::new(file);
            encoder.encode_frames(vec![image::Frame::new(frame)]).unwrap();
        }

        convert_file(&src, &dst, NativeImageFormat::Png, &NativeConvertOptions::default()).unwrap();
        assert!(dst.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thumbnail_never_upscales() {
        let dir = temp_dir();
        let src = dir.join("in.png");
        write_sample_png(&src, false); // 6x4
        let thumb = thumbnail(&src, 200).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (6, 4));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
