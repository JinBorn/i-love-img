//! Pixel operations shared by the still-image and animation pipelines.
use crate::{CoreError, Result, error::check_cancel};
use fast_image_resize::{Resizer, create_srgb_mapper};
use image::{DynamicImage, GenericImageView, Rgb, RgbImage};
use std::sync::atomic::AtomicBool;

/// Resize in linear-light, 16-bit RGBA. The resizer premultiplies alpha before
/// filtering, so hidden RGB values cannot create dark/colored fringes.
pub(crate) fn resize_image(
    image: DynamicImage,
    width: u32,
    height: u32,
    cancel: &AtomicBool,
) -> Result<DynamicImage> {
    check_cancel(cancel)?;
    if image.dimensions() == (width, height) {
        return Ok(image);
    }
    if width == 0 || height == 0 {
        return Err(CoreError::Invalid(
            "Resize dimensions must be positive".into(),
        ));
    }
    let keep_16 = image.color().bits_per_pixel() / u16::from(image.color().channel_count()) == 16;
    let mut source = DynamicImage::ImageRgba16(image.into_rgba16());
    let mapper = create_srgb_mapper();
    mapper
        .forward_map_inplace(&mut source)
        .map_err(|e| CoreError::Image(e.to_string()))?;
    check_cancel(cancel)?;
    let mut target = DynamicImage::ImageRgba16(image::ImageBuffer::new(width, height));
    Resizer::new()
        .resize(&source, &mut target, None)
        .map_err(|e| CoreError::Image(e.to_string()))?;
    drop(source);
    check_cancel(cancel)?;
    mapper
        .backward_map_inplace(&mut target)
        .map_err(|e| CoreError::Image(e.to_string()))?;
    check_cancel(cancel)?;
    if keep_16 {
        Ok(target)
    } else {
        Ok(DynamicImage::ImageRgba8(target.into_rgba8()))
    }
}

/// Flatten transparent pixels against the user-selected sRGB background.
pub(crate) fn flatten_alpha(image: DynamicImage, matte: [u8; 3]) -> RgbImage {
    if !image.color().has_alpha() {
        return image.into_rgb8();
    }
    let rgba = image.into_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (source, target) in rgba.pixels().zip(rgb.pixels_mut()) {
        let alpha = u32::from(source[3]);
        let mut pixel = [0; 3];
        for c in 0..3 {
            pixel[c] = ((u32::from(source[c]) * alpha + u32::from(matte[c]) * (255 - alpha) + 127)
                / 255) as u8;
        }
        *target = Rgb(pixel);
    }
    rgb
}

pub(crate) fn orient(image: &mut DynamicImage, orientation: u8) {
    if let Some(orientation) = image::metadata::Orientation::from_exif(orientation) {
        image.apply_orientation(orientation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transparent_resize_does_not_mix_hidden_rgb() {
        let source = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 0]).unwrap();
        let result = resize_image(
            DynamicImage::ImageRgba8(source),
            1,
            1,
            &AtomicBool::new(false),
        )
        .unwrap()
        .into_rgba8();
        let p = result.get_pixel(0, 0);
        assert!(p[0] > 245 && p[1] < 10 && p[2] < 10, "{p:?}");
        assert!((110..=145).contains(&p[3]));
    }
    #[test]
    fn matte_is_applied_before_dropping_alpha() {
        let source = image::RgbaImage::from_raw(1, 1, vec![100, 0, 255, 0]).unwrap();
        assert_eq!(
            flatten_alpha(DynamicImage::ImageRgba8(source), [17, 29, 42])
                .get_pixel(0, 0)
                .0,
            [17, 29, 42]
        );
    }
}
