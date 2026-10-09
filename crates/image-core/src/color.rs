//! Explicit, row-wise ICC conversion. No decoded RGB buffer is ever passed to
//! a CMYK profile. Alpha is retained independently of color channels.

use crate::metadata::{ImageMetadata, MAX_METADATA_BYTES};
use image::{DynamicImage, ImageBuffer};
use moxcms::{
    Chromaticity, CicpColorPrimaries, ColorPrimaries, ColorProfile, DataColorSpace, Layout,
    ParsingOptions, RenderingIntent, TransferCharacteristics, TransformOptions, XyY,
};

fn check_cancel(cancel: &dyn Fn() -> bool) -> Result<(), String> {
    if cancel() {
        Err("Cancelled".into())
    } else {
        Ok(())
    }
}
fn cms_error(e: impl std::fmt::Display) -> String {
    format!("Color conversion failed: {e}")
}
fn options() -> TransformOptions {
    TransformOptions {
        rendering_intent: RenderingIntent::RelativeColorimetric,
        // ICC TRCs describe the actual pixels. A contradictory/approximate
        // CICP hint must not replace those curves during the transform.
        allow_use_cicp_transfer: false,
        ..Default::default()
    }
}

fn parse_profile(icc: &[u8]) -> Result<ColorProfile, String> {
    if icc.len() > MAX_METADATA_BYTES {
        return Err("ICC profile exceeds 16 MiB limit".into());
    }
    let profile = ColorProfile::new_from_slice_with_options(
        icc,
        ParsingOptions {
            max_profile_size: MAX_METADATA_BYTES,
            max_allowed_clut_size: MAX_METADATA_BYTES,
            max_allowed_trc_size: 65_536,
        },
    )
    .map_err(cms_error)?;
    if profile.cicp.is_some_and(|c| {
        matches!(
            c.transfer_characteristics,
            TransferCharacteristics::Smpte2084 | TransferCharacteristics::Hlg
        )
    }) {
        return Err("HDR ICC profiles require tone mapping and are not supported".into());
    }
    Ok(profile)
}

/// Validate bounded ICC syntax and reject recognized HDR profiles even when a
/// pure optimization path would otherwise preserve the profile unchanged.
pub fn validate_sdr_profile(icc: &[u8]) -> Result<(), String> {
    parse_profile(icc).map(|_| ())
}

/// A real serialized sRGB profile for JPEG and WebP output. PNG uses its sRGB
/// chunk, which is valid for gray as well as RGB pixel encodings.
pub fn srgb_profile_bytes() -> Result<Vec<u8>, String> {
    let mut profile = ColorProfile::new_srgb();
    // ICC matrix/TRCs are sufficient; omit a redundant video range hint.
    profile.cicp = None;
    profile.encode().map_err(cms_error)
}

fn source_profile(
    metadata: &ImageMetadata,
    grayscale: bool,
) -> Result<Option<ColorProfile>, String> {
    if metadata.hdr {
        return Err("HDR / multi-picture input is not supported".into());
    }
    // PNG 3 defines understood cICP as the highest-precedence color chunk,
    // including above an embedded ICC profile.
    if let Some(c) = metadata.png_cicp {
        if c[2] != 0 || c[3] != 1 {
            return Err("PNG cICP must describe full-range RGB pixels".into());
        }
        if matches!(c[1], 16 | 18) {
            return Err("HDR PNG input is not supported".into());
        }
        let primary = CicpColorPrimaries::try_from(c[0]).map_err(cms_error)?;
        let primaries = ColorPrimaries::try_from(primary).map_err(cms_error)?;
        let transfer = TransferCharacteristics::try_from(c[1]).map_err(cms_error)?;
        let curve = moxcms::ToneReprCurve::try_from(transfer).map_err(cms_error)?;
        let white = match primary {
            CicpColorPrimaries::Bt470M | CicpColorPrimaries::GenericFilm => {
                XyY::new(0.310, 0.316, 1.0)
            }
            CicpColorPrimaries::Smpte431 => XyY::new(0.314, 0.351, 1.0),
            CicpColorPrimaries::Xyz => {
                return Err("XYZ-coded PNG cICP is not supported as RGB".into());
            }
            _ => moxcms::WHITE_POINT_D65,
        };
        let mut profile = ColorProfile::new_srgb();
        // moxcms 0.7.9's convenience update returns false after a successful
        // update. Build the validated primaries/TRCs explicitly instead.
        profile.update_rgb_colorimetry(white, primaries);
        profile.red_trc = Some(curve.clone());
        profile.green_trc = Some(curve.clone());
        profile.blue_trc = Some(curve);
        profile.media_white_point = Some(white.to_xyzd());
        profile.cicp = None;
        return Ok(Some(profile));
    }
    if let Some(icc) = &metadata.icc_profile {
        return parse_profile(icc).map(Some);
    }
    if metadata.png_srgb {
        return Ok(None);
    }
    if metadata.png_gamma.is_none() && metadata.png_chromaticities.is_none() {
        return Ok(None);
    }
    let gamma = metadata.png_gamma.map(|g| 1.0 / g);
    if gamma.is_some_and(|g| !g.is_finite() || !(0.1..=10.0).contains(&g)) {
        return Err("Unsupported PNG gamma value".into());
    }
    if grayscale && metadata.png_chromaticities.is_none() {
        return Ok(gamma.map(|g| ColorProfile::new_gray_with_gamma(g as f32)));
    }
    let mut profile = ColorProfile::new_srgb();
    profile.cicp = None;
    if let Some(gamma) = gamma {
        let trc = moxcms::curve_from_gamma(gamma as f32);
        profile.red_trc = Some(trc.clone());
        profile.green_trc = Some(trc.clone());
        profile.blue_trc = Some(trc);
    }
    if let Some(xy) = metadata.png_chromaticities {
        if xy.as_chunks::<2>().0.iter().any(|p| {
            !p[0].is_finite()
                || !p[1].is_finite()
                || p[0] < 0.0
                || p[1] <= 0.0
                || p[0] + p[1] > 1.00001
        }) {
            return Err("Invalid PNG chromaticities".into());
        }
        let white = XyY::new(xy[0], xy[1], 1.0);
        profile.update_rgb_colorimetry(
            white,
            ColorPrimaries {
                red: Chromaticity {
                    x: xy[2] as f32,
                    y: xy[3] as f32,
                },
                green: Chromaticity {
                    x: xy[4] as f32,
                    y: xy[5] as f32,
                },
                blue: Chromaticity {
                    x: xy[6] as f32,
                    y: xy[7] as f32,
                },
            },
        );
        let m = profile.colorant_matrix().v;
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if !det.is_finite() || det.abs() < 1e-10 {
            return Err("Degenerate PNG chromaticities".into());
        }
        profile.media_white_point = Some(white.to_xyzd());
    }
    Ok(Some(profile))
}

fn transform_rgb8(
    pixels: &mut [u8],
    width: u32,
    channels: usize,
    profile: &ColorProfile,
    cancel: &dyn Fn() -> bool,
) -> Result<(), String> {
    check_cancel(cancel)?;
    if profile.color_space != DataColorSpace::Rgb {
        return Err("RGB pixels do not match the embedded ICC color space".into());
    }
    let row = (width as usize)
        .checked_mul(channels)
        .filter(|&n| n > 0)
        .ok_or("Invalid color conversion row length")?;
    if !pixels.len().is_multiple_of(row) {
        return Err("Invalid RGB pixel buffer length".into());
    }
    let layout = if channels == 4 {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    let transform = profile
        .create_transform_8bit(layout, &ColorProfile::new_srgb(), layout, options())
        .map_err(cms_error)?;
    let mut scratch = vec![0; row];
    for dst in pixels.chunks_exact_mut(row) {
        check_cancel(cancel)?;
        scratch.copy_from_slice(dst);
        transform.transform(&scratch, dst).map_err(cms_error)?;
        if channels == 4 {
            for (s, d) in scratch
                .as_chunks::<4>()
                .0
                .iter()
                .zip(dst.as_chunks_mut::<4>().0.iter_mut())
            {
                d[3] = s[3];
            }
        }
    }
    Ok(())
}

fn transform_rgb16(
    pixels: &mut [u16],
    width: u32,
    channels: usize,
    profile: &ColorProfile,
    cancel: &dyn Fn() -> bool,
) -> Result<(), String> {
    check_cancel(cancel)?;
    if profile.color_space != DataColorSpace::Rgb {
        return Err("RGB pixels do not match the embedded ICC color space".into());
    }
    let row = (width as usize)
        .checked_mul(channels)
        .filter(|&n| n > 0)
        .ok_or("Invalid color conversion row length")?;
    if !pixels.len().is_multiple_of(row) {
        return Err("Invalid RGB16 buffer length".into());
    }
    let layout = if channels == 4 {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    let transform = profile
        .create_transform_16bit(layout, &ColorProfile::new_srgb(), layout, options())
        .map_err(cms_error)?;
    let mut scratch = vec![0; row];
    for dst in pixels.chunks_exact_mut(row) {
        check_cancel(cancel)?;
        scratch.copy_from_slice(dst);
        transform.transform(&scratch, dst).map_err(cms_error)?;
        if channels == 4 {
            for (s, d) in scratch
                .as_chunks::<4>()
                .0
                .iter()
                .zip(dst.as_chunks_mut::<4>().0.iter_mut())
            {
                d[3] = s[3];
            }
        }
    }
    Ok(())
}

/// Convert an existing RGBA8 buffer in place with only one scratch row.
/// Suitable for a composited GIF frame. ICC must describe RGB channels.
pub fn rgba8_to_srgb(
    pixels: &mut [u8],
    width: u32,
    icc: &[u8],
    cancel: &dyn Fn() -> bool,
) -> Result<(), String> {
    transform_rgb8(pixels, width, 4, &parse_profile(icc)?, cancel)
}

/// Convert decoded RGB/gray pixels to sRGB, retaining 16-bit precision and alpha.
/// The caller applies EXIF orientation and then encodes exactly this result.
/// An untagged image uses the conventional sRGB assumption. A CMYK ICC must go
/// through `convert_cmyk8_to_srgb` before an image decoder can discard its inks.
pub fn to_srgb(
    mut image: DynamicImage,
    metadata: &ImageMetadata,
    cancel: &dyn Fn() -> bool,
) -> Result<DynamicImage, String> {
    check_cancel(cancel)?;
    if matches!(
        image,
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)
    ) {
        return Err("Floating-point / HDR pixels are not supported".into());
    }
    let gray = matches!(
        image,
        DynamicImage::ImageLuma8(_)
            | DynamicImage::ImageLumaA8(_)
            | DynamicImage::ImageLuma16(_)
            | DynamicImage::ImageLumaA16(_)
    );
    let Some(profile) = source_profile(metadata, gray)? else {
        return Ok(image);
    };
    if profile.color_space == DataColorSpace::Cmyk {
        return Err(
            "CMYK ICC conversion requires original CMYK pixels; RGB fallback is forbidden".into(),
        );
    }
    if profile.color_space == DataColorSpace::Gray {
        return convert_gray(image, &profile, cancel);
    }
    if profile.color_space != DataColorSpace::Rgb {
        return Err("Unsupported ICC source color space".into());
    }
    // PNG grayscale with RGB chromaticity hints represents neutral RGB samples;
    // expand before applying the matrix. RGB ICC attached to decoded gray JPEG
    // can be interpreted the same way without treating an ink channel as alpha.
    image = match image {
        DynamicImage::ImageLuma8(p) => {
            DynamicImage::ImageRgb8(DynamicImage::ImageLuma8(p).into_rgb8())
        }
        DynamicImage::ImageLumaA8(p) => {
            DynamicImage::ImageRgba8(DynamicImage::ImageLumaA8(p).into_rgba8())
        }
        DynamicImage::ImageLuma16(p) => {
            DynamicImage::ImageRgb16(DynamicImage::ImageLuma16(p).into_rgb16())
        }
        DynamicImage::ImageLumaA16(p) => {
            DynamicImage::ImageRgba16(DynamicImage::ImageLumaA16(p).into_rgba16())
        }
        p => p,
    };
    match &mut image {
        DynamicImage::ImageRgb8(p) => {
            let width = p.width();
            transform_rgb8(p.as_mut(), width, 3, &profile, cancel)?;
        }
        DynamicImage::ImageRgba8(p) => {
            let width = p.width();
            transform_rgb8(p.as_mut(), width, 4, &profile, cancel)?;
        }
        DynamicImage::ImageRgb16(p) => {
            let width = p.width();
            transform_rgb16(p.as_mut(), width, 3, &profile, cancel)?;
        }
        DynamicImage::ImageRgba16(p) => {
            let width = p.width();
            transform_rgb16(p.as_mut(), width, 4, &profile, cancel)?;
        }
        _ => return Err("Unsupported pixel type for ICC conversion".into()),
    }
    Ok(image)
}

fn convert_gray(
    image: DynamicImage,
    profile: &ColorProfile,
    cancel: &dyn Fn() -> bool,
) -> Result<DynamicImage, String> {
    let (w, h) = (image.width(), image.height());
    macro_rules! gray {
        ($buffer:expr,$depth:ident,$out:ident,$alpha:expr) => {{
            let buffer = $buffer;
            let src_channels = if $alpha { 2 } else { 1 };
            let dst_channels = if $alpha { 4 } else { 3 };
            let transform = profile
                .$depth(
                    if $alpha {
                        Layout::GrayAlpha
                    } else {
                        Layout::Gray
                    },
                    &ColorProfile::new_srgb(),
                    if $alpha { Layout::Rgba } else { Layout::Rgb },
                    options(),
                )
                .map_err(cms_error)?;
            let len = (w as usize)
                .checked_mul(h as usize)
                .and_then(|n| n.checked_mul(dst_channels))
                .ok_or("Color buffer size overflow")?;
            let mut dst = vec![0; len];
            for (s, d) in buffer
                .as_raw()
                .chunks_exact(w as usize * src_channels)
                .zip(dst.chunks_exact_mut(w as usize * dst_channels))
            {
                check_cancel(cancel)?;
                transform.transform(s, d).map_err(cms_error)?;
                if $alpha {
                    for (s, d) in s
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .zip(d.as_chunks_mut::<4>().0.iter_mut())
                    {
                        d[3] = s[1];
                    }
                }
            }
            Ok(DynamicImage::$out(
                ImageBuffer::from_raw(w, h, dst).ok_or("Invalid transformed buffer size")?,
            ))
        }};
    }
    match image {
        DynamicImage::ImageLuma8(p) => gray!(p, create_transform_8bit, ImageRgb8, false),
        DynamicImage::ImageLumaA8(p) => gray!(p, create_transform_8bit, ImageRgba8, true),
        DynamicImage::ImageLuma16(p) => gray!(p, create_transform_16bit, ImageRgb16, false),
        DynamicImage::ImageLumaA16(p) => gray!(p, create_transform_16bit, ImageRgba16, true),
        _ => Err("Gray ICC profile does not match RGB pixel data".into()),
    }
}

/// Convert original CMYK samples, optionally undoing Adobe's inverted storage.
/// With a nonempty profile this is an actual CMYK→PCS→sRGB transform. Empty ICC
/// uses the conventional untagged subtractive fallback; callers must warn that
/// no device-specific color profile was available.
pub fn convert_cmyk8_to_srgb(
    pixels: &[u8],
    width: u32,
    height: u32,
    icc: &[u8],
    inverted: bool,
    cancel: &dyn Fn() -> bool,
) -> Result<DynamicImage, String> {
    check_cancel(cancel)?;
    let count = (width as usize)
        .checked_mul(height as usize)
        .filter(|&n| n > 0)
        .ok_or("Invalid CMYK dimensions")?;
    if count.checked_mul(4) != Some(pixels.len()) {
        return Err("Invalid CMYK pixel buffer length".into());
    }
    let profile = if icc.is_empty() {
        None
    } else {
        Some(parse_profile(icc)?)
    };
    let transform = match &profile {
        Some(p) if p.color_space != DataColorSpace::Cmyk => {
            return Err("CMYK pixels require a CMYK ICC profile".into());
        }
        Some(p) => Some(
            p.create_transform_8bit(
                Layout::Rgba,
                &ColorProfile::new_srgb(),
                Layout::Rgb,
                options(),
            )
            .map_err(cms_error)?,
        ),
        None => None,
    };
    let mut dst = vec![0; count.checked_mul(3).ok_or("CMYK output size overflow")?];
    let mut row = vec![0; width as usize * 4];
    for (s, d) in pixels
        .chunks_exact(width as usize * 4)
        .zip(dst.chunks_exact_mut(width as usize * 3))
    {
        check_cancel(cancel)?;
        let source = if inverted {
            for (a, b) in s.iter().zip(&mut row) {
                *b = 255 - *a;
            }
            &row[..]
        } else {
            s
        };
        if let Some(transform) = &transform {
            transform.transform(source, d).map_err(cms_error)?;
        } else {
            for (c, rgb) in source
                .as_chunks::<4>()
                .0
                .iter()
                .zip(d.as_chunks_mut::<3>().0.iter_mut())
            {
                for i in 0..3 {
                    rgb[i] = (((255 - c[i]) as u16 * (255 - c[3]) as u16 + 127) / 255) as u8;
                }
            }
        }
    }
    Ok(DynamicImage::ImageRgb8(
        ImageBuffer::from_raw(width, height, dst).ok_or("Invalid CMYK output buffer")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// Original test-only ICC: a two-point CMYK CLUT whose corners describe
    /// ideal subtractive sRGB inks. No vendor profile or third-party asset.
    fn synthetic_cmyk_profile() -> Vec<u8> {
        use moxcms::{
            ColorProfile, DataColorSpace, LocalizableString, LutDataType, LutStore, LutType,
            LutWarehouse, Matrix3d, ProfileClass, ProfileText,
        };
        let mut clut = Vec::new();
        for c in 0..=1 {
            for m in 0..=1 {
                for y in 0..=1 {
                    for k in 0..=1 {
                        let rgb = [
                            ((1 - c) * (1 - k)) as f64,
                            ((1 - m) * (1 - k)) as f64,
                            ((1 - y) * (1 - k)) as f64,
                        ];
                        // D50-adapted sRGB primary matrix, followed by CIE XYZ→Lab.
                        let xyz = [
                            0.4360747 * rgb[0] + 0.3850649 * rgb[1] + 0.1430804 * rgb[2],
                            0.2225045 * rgb[0] + 0.7168786 * rgb[1] + 0.0606169 * rgb[2],
                            0.0139322 * rgb[0] + 0.0971045 * rgb[1] + 0.7141733 * rgb[2],
                        ];
                        let f = |v: f64| {
                            if v > 216.0 / 24389.0 {
                                v.cbrt()
                            } else {
                                (24389.0 / 27.0 * v + 16.0) / 116.0
                            }
                        };
                        let (x, y, z) = (f(xyz[0] / 0.96422), f(xyz[1]), f(xyz[2] / 0.82521));
                        let lab = [
                            (116.0 * y - 16.0) / 100.0,
                            (500.0 * (x - y) + 128.0) / 255.0,
                            (200.0 * (y - z) + 128.0) / 255.0,
                        ];
                        for v in lab {
                            clut.push((v.clamp(0.0, 1.0) * 65535.0).round() as u16);
                        }
                    }
                }
            }
        }
        let lut = LutWarehouse::Lut(LutDataType {
            num_input_channels: 4,
            num_output_channels: 3,
            num_clut_grid_points: 2,
            matrix: Matrix3d {
                v: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            },
            num_input_table_entries: 2,
            num_output_table_entries: 2,
            input_table: LutStore::Store16([0, u16::MAX].repeat(4)),
            clut_table: LutStore::Store16(clut),
            output_table: LutStore::Store16([0, u16::MAX].repeat(3)),
            lut_type: LutType::Lut16,
        });
        let mut profile = ColorProfile::default();
        profile.color_space = DataColorSpace::Cmyk;
        profile.pcs = DataColorSpace::Lab;
        profile.profile_class = ProfileClass::InputDevice;
        profile.white_point = moxcms::WHITE_POINT_D50.to_xyzd();
        profile.media_white_point = Some(profile.white_point);
        profile.description = Some(ProfileText::Localizable(vec![LocalizableString::new(
            "en".into(),
            "US".into(),
            "Original synthetic CMYK test profile".into(),
        )]));
        profile.copyright = Some(ProfileText::Localizable(vec![LocalizableString::new(
            "en".into(),
            "US".into(),
            "Original test data, licensed under MIT".into(),
        )]));
        profile.lut_a_to_b_perceptual = Some(lut.clone());
        profile.lut_a_to_b_colorimetric = Some(lut);
        profile.encode().unwrap()
    }

    #[test]
    fn real_cmyk_icc_maps_white_black_and_red_and_handles_adobe_inversion() {
        let icc = synthetic_cmyk_profile();
        assert_eq!(
            ColorProfile::new_from_slice(&icc).unwrap().color_space,
            DataColorSpace::Cmyk
        );
        let cmyk = [0, 0, 0, 0, 0, 0, 0, 255, 0, 255, 255, 0];
        let rgb = convert_cmyk8_to_srgb(&cmyk, 3, 1, &icc, false, &|| false)
            .unwrap()
            .into_rgb8();
        let inverted: Vec<u8> = cmyk.iter().map(|v| 255 - v).collect();
        let inverse_rgb = convert_cmyk8_to_srgb(&inverted, 3, 1, &icc, true, &|| false)
            .unwrap()
            .into_rgb8();
        assert_eq!(rgb, inverse_rgb);
        for (actual, expected) in rgb
            .as_raw()
            .iter()
            .zip([255u8, 255, 255, 0, 0, 0, 255, 0, 0])
        {
            assert!(
                actual.abs_diff(expected) <= 3,
                "CMYK ICC pixels: {:?}",
                rgb.as_raw()
            );
        }
    }

    #[test]
    fn cmyk_jpeg_icc_pipeline_exports_correct_srgb_png_with_and_without_adobe() {
        use crate::{Compression, Format, ProcessOptions};
        use std::sync::atomic::AtomicBool;
        for adobe in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let input = directory.path().join("cmyk.jpg");
            let output = directory.path().join("srgb.png");
            let mut inks = Vec::with_capacity(48 * 16 * 4);
            for _ in 0..16 {
                for x in 0..48 {
                    let pixel = match x / 16 {
                        0 => [0u8, 0, 0, 0],
                        1 => [0, 0, 0, 255],
                        _ => [0, 255, 255, 0],
                    };
                    inks.extend(pixel.map(|v| if adobe { 255 - v } else { v }));
                }
            }
            let mut encoder = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_CMYK);
            encoder.set_size(48, 16);
            encoder.set_quality(100.0);
            let mut encoder = encoder.start_compress(Vec::new()).unwrap();
            // mozjpeg 0.10.13's write_icc_profile emits zero-based sequence
            // numbers. Write valid ICC APP2 segments explicitly (1..=count).
            let profile = synthetic_cmyk_profile();
            let chunks = profile.chunks(65_519);
            let count = chunks.len();
            assert!(count <= 255);
            for (index, chunk) in chunks.enumerate() {
                let mut payload = b"ICC_PROFILE\0".to_vec();
                payload.extend_from_slice(&[(index + 1) as u8, count as u8]);
                payload.extend_from_slice(chunk);
                encoder.write_marker(mozjpeg::Marker::APP(2), &payload);
            }
            encoder.write_scanlines(&inks).unwrap();
            let mut jpeg = encoder.finish().unwrap();
            if !adobe {
                // Our encoder generates APP14 by default. Removing its marker
                // makes these non-inverted CMYK samples the standard raw case.
                let mut p = 2;
                let mut removed = false;
                while p + 4 <= jpeg.len() {
                    assert_eq!(jpeg[p], 0xff);
                    let marker = jpeg[p + 1];
                    if marker == 0xda {
                        break;
                    }
                    let len = u16::from_be_bytes([jpeg[p + 2], jpeg[p + 3]]) as usize;
                    if marker == 0xee && jpeg[p + 4..].starts_with(b"Adobe") {
                        jpeg.drain(p..p + len + 2);
                        removed = true;
                        break;
                    }
                    p += len + 2;
                }
                assert!(removed, "fixture encoder did not produce Adobe APP14");
            }
            std::fs::write(&input, &jpeg).unwrap();
            let metadata = crate::metadata::read_metadata(&jpeg, image::ImageFormat::Jpeg).unwrap();
            assert_eq!(metadata.adobe_inverted, adobe);
            let options = ProcessOptions {
                format: Some(Format::Png),
                ..Default::default()
            };
            let report = crate::pipeline::process_to_path(
                &input,
                &output,
                &options,
                &AtomicBool::new(false),
            )
            .unwrap();
            assert!(!report.warnings.iter().any(|w| w.contains("Untagged CMYK")));
            let result = image::open(&output).unwrap().into_rgb8();
            assert_eq!(result.dimensions(), (48, 16));
            for (x, expected) in [(8, [255u8, 255, 255]), (24, [0, 0, 0]), (40, [255, 0, 0])] {
                for (actual, expected) in result.get_pixel(x, 8).0.iter().zip(expected) {
                    assert!(
                        actual.abs_diff(expected) <= 4,
                        "adobe={adobe}, x={x}: {:?}",
                        result.get_pixel(x, 8)
                    );
                }
            }
            let metadata = crate::metadata::read_metadata(
                &std::fs::read(output).unwrap(),
                image::ImageFormat::Png,
            )
            .unwrap();
            assert!(metadata.png_srgb);
            assert!(
                metadata.icc_profile.is_none(),
                "old CMYK ICC must not be attached to RGB pixels"
            );

            // Exercise the coefficient-only optimizer too. Inspect its Adobe
            // marker directly, so a size fallback cannot hide a wrong marker.
            let optimized = crate::static_codecs::optimize_jpeg(
                &jpeg,
                adobe,
                options.memory_limit_bytes,
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(
                crate::metadata::read_metadata(&optimized, image::ImageFormat::Jpeg)
                    .unwrap()
                    .adobe_inverted,
                adobe
            );
            let lossless = directory.path().join("lossless.jpg");
            let lossless_png = directory.path().join("lossless-srgb.png");
            let lossless_options = ProcessOptions {
                format: Some(Format::Jpeg),
                compression: Compression::Lossless,
                ..Default::default()
            };
            crate::pipeline::process_to_path(
                &input,
                &lossless,
                &lossless_options,
                &AtomicBool::new(false),
            )
            .unwrap();
            let optimized_metadata = crate::metadata::read_metadata(
                &std::fs::read(&lossless).unwrap(),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
            assert_eq!(
                optimized_metadata.adobe_inverted, adobe,
                "lossless JPEG changed CMYK interpretation"
            );
            assert!(
                optimized_metadata.icc_profile.is_some(),
                "lossless JPEG lost the original CMYK ICC"
            );
            crate::pipeline::process_to_path(
                &lossless,
                &lossless_png,
                &options,
                &AtomicBool::new(false),
            )
            .unwrap();
            let result = image::open(&lossless_png).unwrap().into_rgb8();
            assert_eq!(result.dimensions(), (48, 16));
            for (x, expected) in [(8, [255u8, 255, 255]), (24, [0, 0, 0]), (40, [255, 0, 0])] {
                for (actual, expected) in result.get_pixel(x, 8).0.iter().zip(expected) {
                    assert!(
                        actual.abs_diff(expected) <= 4,
                        "after lossless JPEG: adobe={adobe}, x={x}: {:?}",
                        result.get_pixel(x, 8)
                    );
                }
            }
        }
    }
    #[test]
    fn p3_conversion_changes_color_and_keeps_alpha() {
        let icc = ColorProfile::new_display_p3().encode().unwrap();
        let mut pixels = vec![210, 100, 40, 0, 100, 180, 100, 77];
        let before = pixels.clone();
        rgba8_to_srgb(&mut pixels, 2, &icc, &|| false).unwrap();
        assert_ne!(&pixels[..3], &before[..3]);
        // Independent D65 Display-P3→XYZ→sRGB matrix + IEC transfer result.
        for (actual, expected) in pixels[..3].iter().zip([226u8, 91, 2]) {
            assert!(
                actual.abs_diff(expected) <= 2,
                "unexpected P3 transform: {pixels:?}"
            );
        }
        assert_eq!(pixels[3], 0);
        assert_eq!(pixels[7], 77);
    }
    #[test]
    fn conversion_is_cancellable_before_mutation() {
        let icc = srgb_profile_bytes().unwrap();
        let mut pixel = [1, 2, 3, 4];
        assert_eq!(
            rgba8_to_srgb(&mut pixel, 1, &icc, &|| true).unwrap_err(),
            "Cancelled"
        );
        assert_eq!(pixel, [1, 2, 3, 4]);
    }
    #[test]
    fn encoded_srgb_profile_roundtrips() {
        assert_eq!(
            parse_profile(&srgb_profile_bytes().unwrap())
                .unwrap()
                .color_space,
            DataColorSpace::Rgb
        );
    }
    #[test]
    fn preserve_sixteen_bit_alpha() {
        let pixels = ImageBuffer::<Rgba<u16>, Vec<u16>>::from_pixel(
            1,
            1,
            Rgba([30_000, 20_000, 10_000, 12_345]),
        );
        let mut metadata = ImageMetadata::default();
        metadata.icc_profile = Some(ColorProfile::new_display_p3().encode().unwrap());
        let output = to_srgb(DynamicImage::ImageRgba16(pixels), &metadata, &|| false).unwrap();
        let DynamicImage::ImageRgba16(output) = output else {
            panic!("16-bit precision was lost")
        };
        assert_eq!(output.get_pixel(0, 0).0[3], 12_345);
    }
    #[test]
    fn linear_png_gamma_is_actually_converted() {
        let mut metadata = ImageMetadata::default();
        metadata.png_gamma = Some(1.0);
        let image =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([128, 128, 128, 255])));
        let result = to_srgb(image, &metadata, &|| false).unwrap().into_rgba8();
        assert!((185..=190).contains(&result.get_pixel(0, 0).0[0]));
    }
    #[test]
    fn cmyk_inversion_and_untagged_fallback() {
        let normal = convert_cmyk8_to_srgb(&[0, 255, 255, 0], 1, 1, &[], false, &|| false)
            .unwrap()
            .into_rgb8();
        let inverted = convert_cmyk8_to_srgb(&[255, 0, 0, 255], 1, 1, &[], true, &|| false)
            .unwrap()
            .into_rgb8();
        assert_eq!(normal, inverted);
        assert_eq!(normal.get_pixel(0, 0).0, [255, 0, 0]);
    }
    #[test]
    fn png_srgb_and_display_p3_cicp_are_converted() {
        for (primaries, expected) in [(1, [210u8, 100, 40]), (12, [226u8, 91, 2])] {
            // A conflicting lower-priority ICC must not override cICP.
            let mut metadata = ImageMetadata::default();
            metadata.png_cicp = Some([primaries, 13, 0, 1]);
            metadata.icc_profile = Some(
                if primaries == 1 {
                    ColorProfile::new_display_p3()
                } else {
                    ColorProfile::new_srgb()
                }
                .encode()
                .unwrap(),
            );
            let image =
                DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([210, 100, 40, 77])));
            let result = to_srgb(image, &metadata, &|| false).unwrap().into_rgba8();
            for (actual, expected) in result.get_pixel(0, 0).0[..3].iter().zip(expected) {
                assert!(
                    actual.abs_diff(expected) <= 2,
                    "cICP primaries {primaries}: {result:?}"
                );
            }
            assert_eq!(result.get_pixel(0, 0).0[3], 77);
        }
    }
}
