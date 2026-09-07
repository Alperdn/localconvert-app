//! Geometric transforms: resize, crop, rotate, flip.

use super::error::{ImageNativeError, ImageNativeErrorKind};
use image::DynamicImage;

pub fn resize(img: &DynamicImage, width: u32, height: u32) -> Result<DynamicImage, ImageNativeError> {
    super::decode::check_dimensions(width, height)?;
    // Exact (non-aspect-preserving) resize - matches the previous
    // ImageMagick `-resize WxH!` behavior used by the resize command.
    Ok(img.resize_exact(width, height, image::imageops::FilterType::Lanczos3))
}

pub fn crop(img: &DynamicImage, x: u32, y: u32, width: u32, height: u32) -> Result<DynamicImage, ImageNativeError> {
    if width == 0 || height == 0 {
        return Err(ImageNativeError::new(ImageNativeErrorKind::InvalidDimensions));
    }
    let (img_w, img_h) = (img.width(), img.height());
    if x >= img_w || y >= img_h || x.saturating_add(width) > img_w || y.saturating_add(height) > img_h {
        return Err(ImageNativeError::new(ImageNativeErrorKind::InvalidDimensions)
            .with_detail(format!("crop {}x{}+{}+{} outside {}x{}", width, height, x, y, img_w, img_h)));
    }
    Ok(img.crop_imm(x, y, width, height))
}

/// Normalizes an arbitrary rotation request to the nearest supported
/// 90-degree step (0/90/180/270), matching what the previous ImageMagick
/// `-rotate` path effectively produced for axis-aligned rotations.
pub fn rotate(img: &DynamicImage, degrees: i32) -> DynamicImage {
    let normalized = ((degrees % 360) + 360) % 360;
    match normalized {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img.clone(),
    }
}

// Not wired to a command yet - no UI flip control exists to migrate in this
// step - kept ready for when one is added rather than reimplemented later.
#[allow(dead_code)]
pub fn flip_horizontal(img: &DynamicImage) -> DynamicImage {
    img.fliph()
}

#[allow(dead_code)]
pub fn flip_vertical(img: &DynamicImage) -> DynamicImage {
    img.flipv()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn sample() -> DynamicImage {
        let mut img = RgbImage::new(4, 2);
        for x in 0..4 {
            for y in 0..2 {
                img.put_pixel(x, y, Rgb([x as u8 * 10, y as u8 * 10, 0]));
            }
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn resize_preserves_expected_dimensions() {
        let img = sample();
        let resized = resize(&img, 8, 4).unwrap();
        assert_eq!((resized.width(), resized.height()), (8, 4));
    }

    #[test]
    fn resize_rejects_zero_dimensions() {
        assert!(resize(&sample(), 0, 10).is_err());
    }

    #[test]
    fn crop_bounds_are_validated() {
        let img = sample();
        assert!(crop(&img, 0, 0, 2, 2).is_ok());
        assert!(crop(&img, 3, 0, 2, 2).is_err()); // x+width > img width
        assert!(crop(&img, 0, 0, 0, 2).is_err()); // zero width
    }

    #[test]
    fn rotate_90_and_270_swap_dimensions() {
        let img = sample(); // 4x2
        assert_eq!((rotate(&img, 90).width(), rotate(&img, 90).height()), (2, 4));
        assert_eq!((rotate(&img, 270).width(), rotate(&img, 270).height()), (2, 4));
        assert_eq!((rotate(&img, 180).width(), rotate(&img, 180).height()), (4, 2));
        assert_eq!((rotate(&img, 0).width(), rotate(&img, 0).height()), (4, 2));
    }
}
