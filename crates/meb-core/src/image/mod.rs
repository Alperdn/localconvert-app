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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

    /// The one extension this pipeline writes for the format. Server-side
    /// file names are built from this, never from a client-supplied name.
    pub fn canonical_extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::WebP => "webp",
            Self::Bmp => "bmp",
            Self::Gif => "gif",
            Self::Tiff => "tiff",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::WebP => "image/webp",
            Self::Bmp => "image/bmp",
            Self::Gif => "image/gif",
            Self::Tiff => "image/tiff",
        }
    }

    fn from_image_format(f: image::ImageFormat) -> Option<Self> {
        match f {
            image::ImageFormat::Jpeg => Some(Self::Jpeg),
            image::ImageFormat::Png => Some(Self::Png),
            image::ImageFormat::WebP => Some(Self::WebP),
            image::ImageFormat::Bmp => Some(Self::Bmp),
            image::ImageFormat::Gif => Some(Self::Gif),
            image::ImageFormat::Tiff => Some(Self::Tiff),
            _ => None,
        }
    }
}

/// Identifies a native format from the file's leading bytes (magic
/// numbers) - never from an extension or a client-declared MIME type.
/// `None` means "not a format this pipeline handles" (or too few bytes).
pub fn sniff_format(head: &[u8]) -> Option<NativeImageFormat> {
    image::guess_format(head)
        .ok()
        .and_then(NativeImageFormat::from_image_format)
}

/// What an upload-time probe learned about a file without decoding its
/// pixel data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageProbe {
    pub format: NativeImageFormat,
    pub width: u32,
    pub height: u32,
}

/// Cheap structural validation of an untrusted file that is *claimed* to be
/// `format`: the content must sniff as that format, its declared dimensions
/// must pass the same bomb guards as a real conversion
/// (`decode::check_dimensions`), and multi-frame GIF/TIFF is rejected the
/// same way `convert_file` rejects it. Pixel data is never decoded, so this
/// stays fast for large files; a file whose header is fine but whose body
/// is corrupt is only caught by the real conversion (and fails that job).
pub fn probe_file(path: &Path, format: NativeImageFormat) -> Result<ImageProbe, ImageNativeError> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| {
            ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string())
        })?
        .with_guessed_format()
        .map_err(|e| {
            ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string())
        })?;
    let sniffed = reader
        .format()
        .and_then(NativeImageFormat::from_image_format)
        .ok_or_else(|| ImageNativeError::new(ImageNativeErrorKind::UnsupportedFormat))?;
    if sniffed != format {
        return Err(
            ImageNativeError::new(ImageNativeErrorKind::UnsupportedFormat)
                .with_detail(format!("content is {:?}, expected {:?}", sniffed, format)),
        );
    }
    let (width, height) = reader.into_dimensions().map_err(|e| {
        ImageNativeError::new(ImageNativeErrorKind::DecodeFailed).with_detail(e.to_string())
    })?;
    decode::check_dimensions(width, height)?;
    decode::check_single_frame_for(path, format)?;
    Ok(ImageProbe {
        format,
        width,
        height,
    })
}

/// Pipeline stage boundaries reported to `PipelineHooks::on_stage`. These
/// are the only points where progress is real, so they are the only
/// points reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineStage {
    Decoded,
    Transformed,
    Encoded,
    Written,
}

/// Cooperative cancellation + progress for long-running callers (the web
/// job worker). Cancellation is checked only BETWEEN stages: a single
/// decode/resize/encode call is not interruptible, so a cancel requested
/// mid-decode takes effect when that decode returns. The pixel limits above
/// bound how long that can be.
pub struct PipelineHooks<'a> {
    pub is_cancelled: &'a (dyn Fn() -> bool + Sync),
    pub on_stage: &'a (dyn Fn(PipelineStage) + Sync),
}

impl PipelineHooks<'static> {
    pub fn none() -> Self {
        PipelineHooks {
            is_cancelled: &|| false,
            on_stage: &|_| {},
        }
    }
}

fn checkpoint(hooks: &PipelineHooks<'_>) -> Result<(), ImageNativeError> {
    if (hooks.is_cancelled)() {
        Err(ImageNativeError::new(ImageNativeErrorKind::Cancelled))
    } else {
        Ok(())
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
    convert_file_with_hooks(
        input,
        output,
        target_format,
        options,
        &PipelineHooks::none(),
    )
}

/// `convert_file` with cancellation checkpoints and stage reporting. The
/// pipeline itself (frame check, bounded decode, resize, encode policy) is
/// the same code path - `convert_file` is this with no-op hooks. On
/// cancellation nothing is written to `output`.
pub fn convert_file_with_hooks(
    input: &Path,
    output: &Path,
    target_format: NativeImageFormat,
    options: &NativeConvertOptions,
    hooks: &PipelineHooks<'_>,
) -> Result<(), ImageNativeError> {
    checkpoint(hooks)?;
    decode::check_single_frame(input)?;
    let mut img = decode::decode_bounded(input)?;
    (hooks.on_stage)(PipelineStage::Decoded);
    checkpoint(hooks)?;

    if let (Some(w), Some(h)) = (options.width, options.height) {
        img = transform::resize(&img, w, h)?;
    }
    (hooks.on_stage)(PipelineStage::Transformed);
    checkpoint(hooks)?;

    let bytes = encode_bytes(&img, target_format, options)?;
    (hooks.on_stage)(PipelineStage::Encoded);
    checkpoint(hooks)?;

    write_bytes(output, bytes)?;
    (hooks.on_stage)(PipelineStage::Written);
    Ok(())
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
    write_encoded(
        &cropped,
        output,
        target_format,
        &NativeConvertOptions::default(),
    )
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
    write_encoded(
        &rotated,
        output,
        target_format,
        &NativeConvertOptions::default(),
    )
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
    Ok(img.resize(
        max_dimension,
        max_dimension,
        image::imageops::FilterType::Triangle,
    ))
}

fn write_encoded(
    img: &DynamicImage,
    output: &Path,
    format: NativeImageFormat,
    options: &NativeConvertOptions,
) -> Result<(), ImageNativeError> {
    let bytes = encode_bytes(img, format, options)?;
    write_bytes(output, bytes)
}

fn encode_bytes(
    img: &DynamicImage,
    format: NativeImageFormat,
    options: &NativeConvertOptions,
) -> Result<Vec<u8>, ImageNativeError> {
    encode::encode_to_bytes(
        img,
        format,
        &encode::EncodeOptions {
            quality: options.quality,
            png_compression_level: options.png_compression_level,
        },
    )
}

fn write_bytes(output: &Path, bytes: Vec<u8>) -> Result<(), ImageNativeError> {
    std::fs::write(output, bytes).map_err(|e| {
        ImageNativeError::new(ImageNativeErrorKind::EncodeFailed).with_detail(e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn temp_dir() -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lc_native_image_test_{}", uuid::Uuid::new_v4()));
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

        convert_file(
            &src,
            &dst,
            NativeImageFormat::Jpeg,
            &NativeConvertOptions::default(),
        )
        .unwrap();
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
        let img: RgbImage =
            image::ImageBuffer::from_fn(6, 4, |x, y| Rgb([x as u8 * 10, y as u8 * 10, 5]));
        DynamicImage::ImageRgb8(img).save(&src).unwrap();

        convert_file(
            &src,
            &dst,
            NativeImageFormat::Png,
            &NativeConvertOptions::default(),
        )
        .unwrap();
        assert!(dst.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn webp_round_trip() {
        let dir = temp_dir();
        let src = dir.join("in.png");
        let dst = dir.join("out.webp");
        write_sample_png(&src, false);

        convert_file(
            &src,
            &dst,
            NativeImageFormat::WebP,
            &NativeConvertOptions::default(),
        )
        .unwrap();
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

        convert_file(
            &src,
            &dst,
            NativeImageFormat::Jpeg,
            &NativeConvertOptions::default(),
        )
        .unwrap();
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

        let result = convert_file(
            &bogus,
            &dst,
            NativeImageFormat::Jpeg,
            &NativeConvertOptions::default(),
        );
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
            assert!(
                NativeImageFormat::is_native(ext),
                "{} should be native",
                ext
            );
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
                .encode_frames(vec![image::Frame::new(frame1), image::Frame::new(frame2)])
                .unwrap();
        }

        let result = convert_file(
            &src,
            &dst,
            NativeImageFormat::Png,
            &NativeConvertOptions::default(),
        );
        assert!(
            result.is_err(),
            "animated GIF must not silently convert only the first frame"
        );
        assert_eq!(result.unwrap_err().code(), "IMAGE_MULTI_FRAME_UNSUPPORTED");
        assert!(
            !dst.exists(),
            "no partial output should be written for a rejected multi-frame input"
        );

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
            encoder
                .encode_frames(vec![image::Frame::new(frame)])
                .unwrap();
        }

        convert_file(
            &src,
            &dst,
            NativeImageFormat::Png,
            &NativeConvertOptions::default(),
        )
        .unwrap();
        assert!(dst.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sniff_identifies_formats_by_content_not_name() {
        let dir = temp_dir();
        let src = dir.join("photo.txt"); // misleading name on purpose
        let img: RgbImage = image::ImageBuffer::from_fn(4, 4, |_, _| Rgb([1, 2, 3]));
        DynamicImage::ImageRgb8(img)
            .save_with_format(&src, image::ImageFormat::Jpeg)
            .unwrap();
        let head = std::fs::read(&src).unwrap();
        assert_eq!(sniff_format(&head), Some(NativeImageFormat::Jpeg));
        assert_eq!(sniff_format(b"%PDF-1.7 not an image"), None);
        assert_eq!(sniff_format(b""), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_accepts_a_valid_image_and_rejects_a_format_mismatch() {
        let dir = temp_dir();
        let src = dir.join("in.png");
        write_sample_png(&src, false);
        let probe = probe_file(&src, NativeImageFormat::Png).unwrap();
        assert_eq!((probe.width, probe.height), (6, 4));
        let err = probe_file(&src, NativeImageFormat::Jpeg).unwrap_err();
        assert_eq!(err.code(), "UNSUPPORTED_IMAGE_FORMAT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_rejects_animated_gif_like_the_converter_does() {
        let dir = temp_dir();
        let src = dir.join("anim.bin"); // extension irrelevant: format is explicit
        let f1: RgbaImage = image::ImageBuffer::from_pixel(4, 4, Rgba([255, 0, 0, 255]));
        let f2: RgbaImage = image::ImageBuffer::from_pixel(4, 4, Rgba([0, 255, 0, 255]));
        {
            let file = std::fs::File::create(&src).unwrap();
            let mut encoder = image::codecs::gif::GifEncoder::new(file);
            encoder
                .encode_frames(vec![image::Frame::new(f1), image::Frame::new(f2)])
                .unwrap();
        }
        let err = probe_file(&src, NativeImageFormat::Gif).unwrap_err();
        assert_eq!(err.code(), "IMAGE_MULTI_FRAME_UNSUPPORTED");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_before_start_writes_nothing() {
        let dir = temp_dir();
        let src = dir.join("in.png");
        let dst = dir.join("out.jpg");
        write_sample_png(&src, false);
        let hooks = PipelineHooks {
            is_cancelled: &|| true,
            on_stage: &|_| {},
        };
        let err = convert_file_with_hooks(
            &src,
            &dst,
            NativeImageFormat::Jpeg,
            &NativeConvertOptions::default(),
            &hooks,
        )
        .unwrap_err();
        assert_eq!(err.code(), "CANCELLED");
        assert!(!dst.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_after_decode_stops_before_writing() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = temp_dir();
        let src = dir.join("in.png");
        let dst = dir.join("out.jpg");
        write_sample_png(&src, false);
        let flag = AtomicBool::new(false);
        let on_stage = |s: PipelineStage| {
            if s == PipelineStage::Decoded {
                flag.store(true, Ordering::SeqCst);
            }
        };
        let is_cancelled = || flag.load(Ordering::SeqCst);
        let hooks = PipelineHooks {
            is_cancelled: &is_cancelled,
            on_stage: &on_stage,
        };
        let err = convert_file_with_hooks(
            &src,
            &dst,
            NativeImageFormat::Jpeg,
            &NativeConvertOptions::default(),
            &hooks,
        )
        .unwrap_err();
        assert_eq!(err.code(), "CANCELLED");
        assert!(!dst.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hooks_report_every_stage_in_order_on_success() {
        use std::sync::Mutex;
        let dir = temp_dir();
        let src = dir.join("in.png");
        let dst = dir.join("out.png");
        write_sample_png(&src, false);
        let seen = Mutex::new(Vec::new());
        let on_stage = |s: PipelineStage| seen.lock().unwrap().push(s);
        let hooks = PipelineHooks {
            is_cancelled: &|| false,
            on_stage: &on_stage,
        };
        convert_file_with_hooks(
            &src,
            &dst,
            NativeImageFormat::Png,
            &NativeConvertOptions::default(),
            &hooks,
        )
        .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                PipelineStage::Decoded,
                PipelineStage::Transformed,
                PipelineStage::Encoded,
                PipelineStage::Written
            ]
        );
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
