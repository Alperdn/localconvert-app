//! Encoding + the alpha/color policy for formats that can't carry an alpha
//! channel.
//!
//! Policy (documented, deliberate - see CLAUDE.md Step 2 section C):
//! converting an image with transparency to a format with no alpha channel
//! (JPEG, BMP) flattens it onto an **opaque white background**, never a
//! silent black/garbage background. This is applied consistently rather
//! than left to whatever the underlying encoder happens to do with the
//! alpha byte.

use super::error::{ImageNativeError, ImageNativeErrorKind};
use super::NativeImageFormat;
use image::{DynamicImage, ImageBuffer, ImageEncoder, Rgb, RgbImage};

/// Flattens any alpha channel onto an opaque white background. A no-op
/// (aside from the RGB conversion) for images that never had alpha.
pub fn flatten_to_white(img: &DynamicImage) -> RgbImage {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out: RgbImage = ImageBuffer::new(w, h);
    for (x, y, px) in rgba.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        let a = a as f32 / 255.0;
        let blend = |channel: u8| -> u8 { (channel as f32 * a + 255.0 * (1.0 - a)).round() as u8 };
        out.put_pixel(x, y, Rgb([blend(r), blend(g), blend(b)]));
    }
    out
}

pub struct EncodeOptions {
    /// 1-100. Applies to JPEG. WebP encoding in this pipeline is lossless
    /// (pure-Rust `image-webp` has no lossy encoder), so this is ignored
    /// there - see `encode_to_bytes`.
    pub quality: Option<u8>,
    /// 0-9 zlib compression effort for PNG. Higher = smaller/slower.
    pub png_compression_level: Option<u8>,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            quality: None,
            png_compression_level: None,
        }
    }
}

pub fn encode_to_bytes(
    img: &DynamicImage,
    format: NativeImageFormat,
    options: &EncodeOptions,
) -> Result<Vec<u8>, ImageNativeError> {
    let mut buf: Vec<u8> = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buf);

    match format {
        NativeImageFormat::Jpeg => {
            let rgb = flatten_to_white(img);
            let quality = options.quality.unwrap_or(85).clamp(1, 100);
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, quality);
            encoder
                .write_image(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| {
                    ImageNativeError::new(ImageNativeErrorKind::EncodeFailed)
                        .with_detail(e.to_string())
                })?;
        }
        NativeImageFormat::Png => {
            let compression = match options.png_compression_level.unwrap_or(6) {
                0..=2 => image::codecs::png::CompressionType::Fast,
                7..=9 => image::codecs::png::CompressionType::Best,
                _ => image::codecs::png::CompressionType::Default,
            };
            let encoder = image::codecs::png::PngEncoder::new_with_quality(
                &mut cursor,
                compression,
                image::codecs::png::FilterType::Adaptive,
            );
            img.write_with_encoder(encoder).map_err(|e| {
                ImageNativeError::new(ImageNativeErrorKind::EncodeFailed).with_detail(e.to_string())
            })?;
        }
        NativeImageFormat::WebP => {
            // Pure-Rust `image-webp` only implements lossless WebP encoding
            // (no libwebp in the dependency tree). Quality is accepted in
            // the UI/API for forward compatibility but has no effect here -
            // documented in the Step 2 final report.
            let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut cursor);
            img.write_with_encoder(encoder).map_err(|e| {
                ImageNativeError::new(ImageNativeErrorKind::EncodeFailed).with_detail(e.to_string())
            })?;
        }
        NativeImageFormat::Bmp => {
            let rgb = flatten_to_white(img);
            let encoder = image::codecs::bmp::BmpEncoder::new(&mut cursor);
            encoder
                .write_image(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| {
                    ImageNativeError::new(ImageNativeErrorKind::EncodeFailed)
                        .with_detail(e.to_string())
                })?;
        }
        NativeImageFormat::Gif => {
            let rgba = img.to_rgba8();
            image::codecs::gif::GifEncoder::new(&mut cursor)
                .write_image(
                    rgba.as_raw(),
                    rgba.width(),
                    rgba.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| {
                    ImageNativeError::new(ImageNativeErrorKind::EncodeFailed)
                        .with_detail(e.to_string())
                })?;
        }
        NativeImageFormat::Tiff => {
            let encoder = image::codecs::tiff::TiffEncoder::new(&mut cursor);
            img.write_with_encoder(encoder).map_err(|e| {
                ImageNativeError::new(ImageNativeErrorKind::EncodeFailed).with_detail(e.to_string())
            })?;
        }
    }

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn flatten_to_white_makes_fully_transparent_pixels_white() {
        let mut img: image::RgbaImage = ImageBuffer::new(2, 2);
        img.put_pixel(0, 0, Rgba([10, 20, 30, 0])); // fully transparent, dark RGB
        img.put_pixel(1, 0, Rgba([200, 0, 0, 255])); // fully opaque red
        let dyn_img = DynamicImage::ImageRgba8(img);

        let flattened = flatten_to_white(&dyn_img);
        assert_eq!(flattened.get_pixel(0, 0), &Rgb([255, 255, 255]));
        assert_eq!(flattened.get_pixel(1, 0), &Rgb([200, 0, 0]));
    }

    #[test]
    fn flatten_to_white_blends_partial_alpha() {
        let mut img: image::RgbaImage = ImageBuffer::new(1, 1);
        img.put_pixel(0, 0, Rgba([0, 0, 0, 128])); // ~50% black
        let dyn_img = DynamicImage::ImageRgba8(img);
        let flattened = flatten_to_white(&dyn_img);
        let Rgb([r, g, b]) = *flattened.get_pixel(0, 0);
        // Should land roughly halfway between black and white, not stay black.
        assert!(r > 100 && r < 155, "r={}", r);
        assert_eq!(r, g);
        assert_eq!(g, b);
    }
}
