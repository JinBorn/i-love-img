use image::{DynamicImage, ImageBuffer, ImageEncoder, Luma, RgbImage, Rgba, RgbaImage};
use local_image_core::{
    Compression, CoreError, Format, MIB, ProcessOptions, ResizeMode, ResizeSpec, probe,
    process_to_path,
};
use std::{fs, path::Path, sync::atomic::AtomicBool};

fn process(
    input: &Path,
    output: &Path,
    options: ProcessOptions,
) -> local_image_core::ProcessReport {
    process_to_path(input, output, &options, &AtomicBool::new(false)).unwrap()
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
    out
}

fn exif(orientation: u16) -> Vec<u8> {
    let private = b"private-location-marker\0";
    let mut out = b"II\x2a\0\x08\0\0\0".to_vec();
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&0x0112u16.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&orientation.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&0x010eu16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&(private.len() as u32).to_le_bytes());
    out.extend_from_slice(&38u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(private);
    out
}

fn insert_jpeg_exif(input: &[u8], orientation: u16) -> Vec<u8> {
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend(exif(orientation));
    let mut out = input[..2].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend(payload);
    out.extend_from_slice(&input[2..]);
    out
}

#[test]
fn png_16bit_pure_optimization_preserves_exact_samples_and_source() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("sixteen.png");
    let output = dir.path().join("out.png");
    let image: ImageBuffer<Luma<u16>, Vec<u16>> = ImageBuffer::from_fn(31, 17, |x, y| {
        Luma([((x * 1103 + y * 719 + 3) % 65536) as u16])
    });
    DynamicImage::ImageLuma16(image.clone())
        .save(&input)
        .unwrap();
    let original = fs::read(&input).unwrap();
    let report = process(&input, &output, ProcessOptions::default());
    let result = image::open(&output).unwrap();
    assert_eq!(result.color(), image::ColorType::L16);
    assert_eq!(result.into_luma16(), image);
    assert_eq!(fs::read(&input).unwrap(), original);
    assert_eq!((report.width, report.height), (31, 17));
}

#[test]
fn lossless_webp_retains_alpha_and_hidden_rgb() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.png");
    let output = dir.path().join("out.webp");
    let source = RgbaImage::from_raw(
        3,
        1,
        vec![11, 29, 217, 0, 100, 70, 40, 127, 200, 31, 5, 255],
    )
    .unwrap();
    source.save(&input).unwrap();
    process(
        &input,
        &output,
        ProcessOptions {
            format: Some(Format::WebP),
            compression: Compression::Lossless,
            ..Default::default()
        },
    );
    assert_eq!(image::open(&output).unwrap().into_rgba8(), source);
    assert_eq!(probe(&output).unwrap().format, Format::WebP);
}

#[test]
fn transparent_resize_preserves_foreground_and_geometry() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.png");
    let output = dir.path().join("out.png");
    RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 0])
        .unwrap()
        .save(&input)
        .unwrap();
    process(
        &input,
        &output,
        ProcessOptions {
            resize: ResizeSpec {
                mode: ResizeMode::LongestEdge(1),
                allow_upscale: false,
            },
            ..Default::default()
        },
    );
    let result = image::open(&output).unwrap().into_rgba8();
    assert_eq!(result.dimensions(), (1, 1));
    let p = result.get_pixel(0, 0);
    assert!(p[0] > 245 && p[1] < 10 && p[2] < 10, "{p:?}");
    assert!((110..=145).contains(&p[3]));
}

#[test]
fn jpeg_orientation_is_normalized_and_private_metadata_removed() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.jpg");
    let output = dir.path().join("out.jpg");
    RgbImage::from_fn(13, 7, |x, y| {
        image::Rgb([(x * 17) as u8, (y * 31) as u8, 90])
    })
    .save(&input)
    .unwrap();
    let original = insert_jpeg_exif(&fs::read(&input).unwrap(), 6);
    fs::write(&input, &original).unwrap();
    assert_eq!(probe(&input).unwrap().display_dimensions(), (7, 13));
    process(&input, &output, ProcessOptions::default());
    let result = probe(&output).unwrap();
    assert_eq!(
        (result.width, result.height, result.orientation),
        (7, 13, 1)
    );
    assert!(
        !fs::read(&output)
            .unwrap()
            .windows(23)
            .any(|s| s == b"private-location-marker")
    );
    assert_eq!(fs::read(&input).unwrap(), original);
}

#[test]
fn lossless_jpeg_keeps_orientation_without_cropping_odd_edges() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.jpg");
    let output = dir.path().join("out.jpg");
    RgbImage::from_pixel(13, 7, image::Rgb([121, 70, 41]))
        .save(&input)
        .unwrap();
    let original = insert_jpeg_exif(&fs::read(&input).unwrap(), 6);
    fs::write(&input, &original).unwrap();
    process(
        &input,
        &output,
        ProcessOptions {
            compression: Compression::Lossless,
            ..Default::default()
        },
    );
    let result = probe(&output).unwrap();
    assert_eq!(
        (result.width, result.height, result.orientation),
        (13, 7, 6)
    );
    assert_eq!(
        image::open(&input).unwrap().into_rgb8(),
        image::open(&output).unwrap().into_rgb8()
    );
    assert!(
        !fs::read(&output)
            .unwrap()
            .windows(23)
            .any(|s| s == b"private-location-marker")
    );
}

#[test]
fn png_fallback_does_not_restore_private_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.png");
    let output = dir.path().join("out.png");
    RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 255]))
        .save(&input)
        .unwrap();
    let mut bytes = fs::read(&input).unwrap();
    let chunk = png_chunk(b"eXIf", &exif(1));
    bytes.splice(33..33, chunk);
    fs::write(&input, &bytes).unwrap();
    process(&input, &output, ProcessOptions::default());
    let encoded = fs::read(&output).unwrap();
    assert!(!encoded.windows(23).any(|s| s == b"private-location-marker"));
    assert_eq!(
        image::open(&output).unwrap().into_rgba8().get_pixel(0, 0).0,
        [1, 2, 3, 255]
    );
    let retained = dir.path().join("retained.png");
    process(
        &input,
        &retained,
        ProcessOptions {
            strip_metadata: false,
            ..Default::default()
        },
    );
    assert!(
        fs::read(retained)
            .unwrap()
            .windows(23)
            .any(|s| s == b"private-location-marker")
    );
}

#[test]
fn p3_conversion_uses_real_icc_and_updates_output_profile() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("p3.png");
    let output = dir.path().join("srgb.webp");
    let icc = moxcms::ColorProfile::new_display_p3().encode().unwrap();
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
    encoder.set_icc_profile(icc.clone()).unwrap();
    encoder
        .write_image(&[210, 100, 40, 255], 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    fs::write(&input, bytes).unwrap();
    process(
        &input,
        &output,
        ProcessOptions {
            format: Some(Format::WebP),
            compression: Compression::Lossless,
            ..Default::default()
        },
    );
    let result = image::open(&output).unwrap().into_rgba8();
    let p = result.get_pixel(0, 0);
    for (a, b) in p.0[..3].iter().zip([226u8, 91, 2]) {
        assert!(a.abs_diff(b) <= 2, "{p:?}");
    }
    let encoded = fs::read(&output).unwrap();
    assert!(!encoded.windows(icc.len()).any(|s| s == icc));
    assert!(probe(&output).unwrap().has_icc);
}

#[test]
fn combination_resize_convert_and_selected_matte_are_applied() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.png");
    let output = dir.path().join("out.jpg");
    RgbaImage::from_pixel(32, 16, Rgba([255, 0, 0, 0]))
        .save(&input)
        .unwrap();
    process(
        &input,
        &output,
        ProcessOptions {
            format: Some(Format::Jpeg),
            matte: [20, 100, 180],
            jpeg_quality: 100,
            resize: ResizeSpec {
                mode: ResizeMode::LongestEdge(8),
                allow_upscale: false,
            },
            ..Default::default()
        },
    );
    let image = image::open(output).unwrap().into_rgb8();
    assert_eq!(image.dimensions(), (8, 4));
    for (a, b) in image.get_pixel(3, 2).0.iter().zip([20u8, 100, 180]) {
        assert!(a.abs_diff(b) < 4);
    }
}

#[test]
fn budget_and_cancellation_are_checked_before_output_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.png");
    let output = dir.path().join("out.png");
    RgbaImage::from_pixel(64, 64, Rgba([1, 2, 3, 4]))
        .save(&input)
        .unwrap();
    fs::write(&output, b"sentinel").unwrap();
    let error = process_to_path(
        &input,
        &output,
        &ProcessOptions {
            memory_limit_bytes: 16 * MIB,
            ..Default::default()
        },
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(matches!(error, CoreError::MemoryBudget { .. }));
    assert_eq!(fs::read(&output).unwrap(), b"sentinel");
    let error = process_to_path(
        &input,
        &output,
        &ProcessOptions::default(),
        &AtomicBool::new(true),
    )
    .unwrap_err();
    assert!(matches!(error, CoreError::Cancelled));
    assert_eq!(fs::read(&output).unwrap(), b"sentinel");
    assert!(
        process_to_path(
            &input,
            &input,
            &ProcessOptions::default(),
            &AtomicBool::new(false)
        )
        .is_err()
    );
}

#[test]
fn unsupported_animation_containers_are_not_flattened_silently() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("animated.png");
    RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 255]))
        .save(&png)
        .unwrap();
    let mut bytes = fs::read(&png).unwrap();
    bytes.splice(33..33, png_chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
    fs::write(&png, bytes).unwrap();
    assert!(matches!(probe(&png), Err(CoreError::Unsupported(_))));
    let webp = dir.path().join("animated.webp");
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&22u32.to_le_bytes());
    bytes.extend_from_slice(b"WEBPVP8X");
    bytes.extend_from_slice(&10u32.to_le_bytes());
    bytes.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    fs::write(&webp, bytes).unwrap();
    assert!(matches!(probe(&webp), Err(CoreError::Unsupported(_))));
}

#[test]
fn explicit_lossless_jpeg_cannot_be_satisfied_by_a_lossy_resize() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.jpg");
    let output = dir.path().join("out.jpg");
    RgbImage::from_pixel(20, 10, image::Rgb([10, 40, 80]))
        .save(&input)
        .unwrap();
    let options = ProcessOptions {
        compression: Compression::Lossless,
        resize: ResizeSpec {
            mode: ResizeMode::Percent(50.0),
            allow_upscale: false,
        },
        ..Default::default()
    };
    assert!(matches!(
        process_to_path(&input, &output, &options, &AtomicBool::new(false)),
        Err(CoreError::Invalid(_))
    ));
}

#[test]
fn webp_canvas_must_match_frame_before_any_pixel_allocation() {
    let dir = tempfile::tempdir().unwrap();
    for lossless in [false, true] {
        let pixels = vec![100u8; 64 * 32 * 3];
        let encoded = webp::Encoder::from_rgb(&pixels, 64, 32)
            .encode_simple(lossless, 80.0)
            .unwrap();
        let mut bytes = encoded.to_vec();
        // The codec payload is valid 64x32; the forged extended header claims
        // 1x1. It must fail at probe rather than allocating the real frame.
        let mut extended = b"VP8X".to_vec();
        extended.extend_from_slice(&10u32.to_le_bytes());
        extended.extend_from_slice(&[0; 10]);
        bytes.splice(12..12, extended);
        let len = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());
        let path = dir.path().join(format!("mismatch-{lossless}.webp"));
        fs::write(&path, bytes).unwrap();
        assert!(matches!(probe(&path), Err(CoreError::Image(_))));
    }
}

#[test]
fn jpeg_metadata_after_image_scans_controls_geometry_and_color() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("late.jpg");
    let output = dir.path().join("out.png");
    RgbImage::from_pixel(13, 7, image::Rgb([210, 100, 40]))
        .save(&input)
        .unwrap();
    let source = fs::read(&input).unwrap();
    let mut bytes = source[..source.len() - 2].to_vec();
    let mut exif_payload = b"Exif\0\0".to_vec();
    exif_payload.extend(exif(6));
    bytes.extend_from_slice(&[0xff, 0xe1]);
    bytes.extend_from_slice(&((exif_payload.len() + 2) as u16).to_be_bytes());
    bytes.extend(exif_payload);
    let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
    let mut icc = b"ICC_PROFILE\0\x01\x01".to_vec();
    icc.extend(profile);
    bytes.extend_from_slice(&[0xff, 0xe2]);
    bytes.extend_from_slice(&((icc.len() + 2) as u16).to_be_bytes());
    bytes.extend(icc);
    bytes.extend_from_slice(&[0xff, 0xd9]);
    fs::write(&input, bytes).unwrap();
    let info = probe(&input).unwrap();
    assert_eq!(info.display_dimensions(), (7, 13));
    assert!(info.has_icc);
    process(
        &input,
        &output,
        ProcessOptions {
            format: Some(Format::Png),
            ..Default::default()
        },
    );
    let result = image::open(&output).unwrap().into_rgb8();
    assert_eq!(result.dimensions(), (7, 13));
    // The source JPEG incurs at most a few units of quantization error. A
    // metadata-only re-tag would leave green near100 and blue near40.
    let pixel = result.get_pixel(3, 3);
    assert!(
        pixel[0] > 218 && pixel[1] < 97 && pixel[2] < 12,
        "{pixel:?}"
    );
}

#[test]
fn hard_link_output_cannot_modify_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source.png");
    let alias = dir.path().join("alias.png");
    RgbaImage::from_pixel(3, 2, Rgba([4, 5, 6, 255]))
        .save(&input)
        .unwrap();
    let original = fs::read(&input).unwrap();
    fs::hard_link(&input, &alias).unwrap();
    let error = process_to_path(
        &input,
        &alias,
        &ProcessOptions::default(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(matches!(error, CoreError::Invalid(_)));
    assert_eq!(fs::read(&input).unwrap(), original);
}

#[test]
fn duplicate_dimension_headers_cannot_replace_the_budgeted_geometry() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("double.png");
    RgbaImage::from_pixel(64, 32, Rgba([4, 5, 6, 255]))
        .save(&png)
        .unwrap();
    let mut bytes = fs::read(&png).unwrap();
    let mut second = bytes[16..29].to_vec();
    second[..4].copy_from_slice(&1u32.to_be_bytes());
    second[4..8].copy_from_slice(&1u32.to_be_bytes());
    bytes.splice(33..33, png_chunk(b"IHDR", &second));
    fs::write(&png, bytes).unwrap();
    assert!(matches!(probe(&png), Err(CoreError::Image(_))));

    let jpeg = dir.path().join("double.jpg");
    RgbImage::from_pixel(64, 32, image::Rgb([4, 5, 6]))
        .save(&jpeg)
        .unwrap();
    let bytes = fs::read(&jpeg).unwrap();
    let mut i = 2;
    let mut duplicate = Vec::new();
    while i + 4 < bytes.len() {
        let marker = bytes[i + 1];
        if marker == 0xda {
            break;
        }
        let size = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            duplicate = bytes[i..i + size + 2].to_vec();
            duplicate[5..7].copy_from_slice(&1u16.to_be_bytes());
            duplicate[7..9].copy_from_slice(&1u16.to_be_bytes());
            break;
        }
        i += size + 2;
    }
    assert!(!duplicate.is_empty());
    let mut forged = bytes[..bytes.len() - 2].to_vec();
    forged.extend(duplicate);
    forged.extend_from_slice(&[0xff, 0xd9]);
    fs::write(&jpeg, forged).unwrap();
    assert!(matches!(probe(&jpeg), Err(CoreError::Image(_))));
}
