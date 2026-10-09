use gif::{DisposalMethod, Encoder, Frame, Repeat};
use image::{AnimationDecoder, ImageDecoder, Rgba, RgbaImage};
use local_image_core::{
    CoreError, MIB,
    gif_codec::{GifOptions, decode_first_frame, encode_static_gif, inspect_gif, transcode_gif},
};
use std::{borrow::Cow, fs, io::BufReader, path::Path, sync::atomic::AtomicBool};

fn fixture(path: &Path, looping: Option<Repeat>) {
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(
            &mut bytes,
            3,
            2,
            &[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255],
        )
        .unwrap();
        if let Some(repeat) = looping {
            encoder.set_repeat(repeat).unwrap();
        }
        encoder
            .write_raw_extension(gif::AnyExtension(0xfe), &[b"private camera note"])
            .unwrap();
        let frames = [
            Frame {
                width: 3,
                height: 2,
                buffer: Cow::Borrowed(&[1, 1, 0, 1, 1, 0]),
                transparent: Some(0),
                dispose: DisposalMethod::Keep,
                delay: 0,
                ..Frame::default()
            },
            Frame {
                left: 1,
                width: 1,
                height: 2,
                buffer: Cow::Borrowed(&[1, 1]),
                palette: Some(vec![0, 0, 0, 255, 0, 255]),
                transparent: Some(0),
                dispose: DisposalMethod::Previous,
                delay: 3,
                ..Frame::default()
            },
            Frame {
                left: 2,
                top: 1,
                width: 1,
                height: 1,
                buffer: Cow::Borrowed(&[3]),
                transparent: Some(0),
                dispose: DisposalMethod::Background,
                delay: 17,
                ..Frame::default()
            },
            Frame {
                width: 1,
                height: 1,
                buffer: Cow::Borrowed(&[2]),
                transparent: Some(0),
                dispose: DisposalMethod::Keep,
                delay: 65535,
                ..Frame::default()
            },
        ];
        for frame in &frames {
            encoder.write_frame(frame).unwrap();
        }
    }
    // Encoder's public API does not expose these logical-screen fields.
    bytes[11] = 3;
    bytes[12] = 49;
    fs::write(path, bytes).unwrap();
}

fn rendered(path: &Path) -> Vec<(RgbaImage, image::Delay)> {
    let decoder =
        image::codecs::gif::GifDecoder::new(BufReader::new(fs::File::open(path).unwrap())).unwrap();
    decoder
        .into_frames()
        .map(|frame| {
            let frame = frame.unwrap();
            let delay = frame.delay();
            (frame.into_buffer(), delay)
        })
        .collect()
}

#[test]
fn lossless_preserves_full_playback_local_palettes_delays_and_looping() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("animation.gif");
    let output = dir.path().join("result.gif");
    fixture(&input, Some(Repeat::Finite(5)));
    let options = GifOptions {
        preserve_metadata: true,
        ..Default::default()
    };
    let summary = transcode_gif(&input, &output, &options, &AtomicBool::new(false)).unwrap();
    assert_eq!(summary.frames, 4);
    assert_eq!(summary.duration_cs, 65555);
    assert_eq!(summary.repeat, Some(5));
    assert_eq!(rendered(&input), rendered(&output));
    let input_bytes = fs::read(&input).unwrap();
    let output_bytes = fs::read(&output).unwrap();
    assert_eq!(&input_bytes[11..13], &output_bytes[11..13]);
    assert!(
        output_bytes
            .windows(19)
            .any(|w| w == b"private camera note")
    );

    let mut before = gif::DecodeOptions::new()
        .read_info(&input_bytes[..])
        .unwrap();
    let mut after = gif::DecodeOptions::new()
        .read_info(&output_bytes[..])
        .unwrap();
    while let Some(left) = before.read_next_frame().unwrap() {
        let right = after.read_next_frame().unwrap().unwrap();
        assert_eq!(
            left.buffer, right.buffer,
            "indices must not be re-quantized"
        );
        assert_eq!(left.palette, right.palette);
        assert_eq!(
            (
                left.left,
                left.top,
                left.width,
                left.height,
                left.transparent,
                left.dispose,
                left.delay
            ),
            (
                right.left,
                right.top,
                right.width,
                right.height,
                right.transparent,
                right.dispose,
                right.delay
            )
        );
    }
    assert!(after.read_next_frame().unwrap().is_none());
}

#[test]
fn resized_animation_preserves_disposal_transparency_delays_and_infinite_loop() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.gif");
    let output = dir.path().join("output.gif");
    fixture(&input, Some(Repeat::Infinite));
    let summary = transcode_gif(
        &input,
        &output,
        &GifOptions {
            resize: Some((6, 4)),
            ..Default::default()
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(summary.repeat, Some(0));
    assert!(!summary.warnings.is_empty());
    let source = rendered(&input);
    let result = rendered(&output);
    assert_eq!(source.len(), result.len());
    for ((_, expected_delay), (pixels, actual_delay)) in source.iter().zip(&result) {
        assert_eq!(expected_delay, actual_delay);
        assert_eq!(pixels.dimensions(), (6, 4));
    }
    // Corners unaffected by interpolation distinguish keep/previous/background.
    assert!(result[1].0.get_pixel(2, 1)[2] > result[0].0.get_pixel(2, 1)[2]);
    assert!(result[2].0.get_pixel(2, 1)[0] > result[2].0.get_pixel(2, 1)[2]);
    assert!(result[2].0.get_pixel(5, 3)[2] > 200);
    assert_eq!(result[3].0.get_pixel(5, 3)[3], 0);
    assert!(result[3].0.get_pixel(0, 0)[1] > 200);
}

#[test]
fn first_frame_composes_offsets_and_transparency() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("partial.gif");
    {
        let mut file = fs::File::create(&input).unwrap();
        let mut encoder = Encoder::new(&mut file, 3, 2, &[0, 0, 0, 250, 10, 20]).unwrap();
        encoder
            .write_frame(&Frame {
                left: 1,
                top: 1,
                width: 1,
                height: 1,
                buffer: Cow::Borrowed(&[1]),
                transparent: Some(0),
                ..Frame::default()
            })
            .unwrap();
    }
    let rgba = decode_first_frame(&input, 32 * MIB, &AtomicBool::new(false)).unwrap();
    assert_eq!(rgba.dimensions(), (3, 2));
    assert_eq!(rgba.get_pixel(1, 1), &Rgba([250, 10, 20, 255]));
    assert_eq!(rgba.get_pixel(0, 0)[3], 0);
}

#[test]
fn conversion_keeps_zero_alpha_and_composites_partial_alpha_over_matte() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("converted.gif");
    let pixels =
        RgbaImage::from_raw(3, 1, vec![230, 10, 20, 0, 200, 100, 0, 128, 2, 3, 4, 255]).unwrap();
    encode_static_gif(
        &pixels,
        &output,
        &GifOptions {
            matte: [0, 0, 200],
            ..Default::default()
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    let decoded = rendered(&output).remove(0).0;
    assert_eq!(decoded.get_pixel(0, 0)[3], 0);
    assert_eq!(decoded.get_pixel(1, 0), &Rgba([100, 50, 100, 255]));
    assert_eq!(decoded.get_pixel(2, 0), &Rgba([2, 3, 4, 255]));
}

#[test]
fn quantization_never_turns_opaque_colors_transparent() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("many-colors.gif");
    let mut pixels = RgbaImage::new(257, 4);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = Rgba([
            x as u8,
            (y * 61) as u8,
            (x * 17 + y * 19) as u8,
            if x == 0 { 0 } else { 255 },
        ]);
    }
    encode_static_gif(
        &pixels,
        &output,
        &GifOptions::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let decoded = rendered(&output).remove(0).0;
    for (x, _, pixel) in decoded.enumerate_pixels() {
        assert_eq!(pixel[3], if x == 0 { 0 } else { 255 });
    }
}

#[test]
fn no_loop_extension_remains_single_play_and_metadata_is_stripped() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.gif");
    let output = dir.path().join("output.gif");
    fixture(&input, None);
    let before = inspect_gif(&input, &AtomicBool::new(false)).unwrap();
    assert!(before.has_metadata);
    let report = transcode_gif(
        &input,
        &output,
        &GifOptions::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(report.repeat, None);
    let after = inspect_gif(&output, &AtomicBool::new(false)).unwrap();
    assert!(!after.has_metadata);
    assert_eq!(after.repeat, None);
    assert_eq!(rendered(&input), rendered(&output));
}

#[test]
fn budget_and_cancellation_are_checked_before_output_creation() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.gif");
    let output = dir.path().join("output.gif");
    fixture(&input, None);
    let error = transcode_gif(
        &input,
        &output,
        &GifOptions::default(),
        &AtomicBool::new(true),
    )
    .unwrap_err();
    assert!(matches!(error, CoreError::Cancelled));
    assert!(!output.exists());
    let error = transcode_gif(
        &input,
        &output,
        &GifOptions {
            memory_limit_bytes: 1,
            ..Default::default()
        },
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(matches!(error, CoreError::MemoryBudget { .. }));
    assert!(!output.exists());
}

#[test]
fn interlaced_indices_are_reencoded_in_correct_row_order() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("interlaced.gif");
    let output = dir.path().join("result.gif");
    {
        let mut file = fs::File::create(&input).unwrap();
        let mut encoder =
            Encoder::new(&mut file, 2, 5, &[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]).unwrap();
        // GIF's interlace passes visit rows 0, 4, 2, 1, 3 at this height.
        encoder
            .write_frame(&Frame {
                width: 2,
                height: 5,
                interlaced: true,
                buffer: Cow::Borrowed(&[0, 0, 0, 1, 2, 2, 1, 1, 3, 3]),
                ..Default::default()
            })
            .unwrap();
    }
    transcode_gif(
        &input,
        &output,
        &GifOptions::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(rendered(&input), rendered(&output));
    let pixels = rendered(&output).remove(0).0;
    assert_eq!(pixels.get_pixel(0, 1), &Rgba([255, 0, 0, 255]));
    assert_eq!(pixels.get_pixel(0, 2), &Rgba([0, 255, 0, 255]));
    assert_eq!(pixels.get_pixel(0, 3), &Rgba([0, 0, 255, 255]));
}

#[test]
fn opaque_logical_background_is_used_for_partial_first_frame() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("background.gif");
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(&mut bytes, 2, 1, &[255, 0, 0, 0, 0, 255]).unwrap();
        encoder
            .write_frame(&Frame {
                width: 1,
                height: 1,
                buffer: Cow::Borrowed(&[0]),
                ..Default::default()
            })
            .unwrap();
    }
    bytes[11] = 1;
    fs::write(&input, bytes).unwrap();
    let image = decode_first_frame(&input, 32 * MIB, &AtomicBool::new(false)).unwrap();
    assert_eq!(image.get_pixel(0, 0), &Rgba([255, 0, 0, 255]));
    assert_eq!(image.get_pixel(1, 0), &Rgba([0, 0, 255, 255]));
}

#[test]
fn local_palette_only_gif_does_not_gain_a_global_background() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("local-only.gif");
    let output = dir.path().join("result.gif");
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(&mut bytes, 2, 2, &[]).unwrap();
        encoder
            .write_frame(&Frame {
                width: 1,
                height: 1,
                palette: Some(vec![200, 0, 20, 0, 0, 0]),
                buffer: Cow::Borrowed(&[0]),
                ..Default::default()
            })
            .unwrap();
    }
    // Remove the encoder's synthesized two-color global table.
    bytes[10] &= 0x7f;
    bytes.drain(13..19);
    fs::write(&input, bytes).unwrap();
    transcode_gif(
        &input,
        &output,
        &GifOptions::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(fs::read(&output).unwrap()[10] & 0x80, 0);
    assert_eq!(rendered(&input), rendered(&output));
    assert_eq!(
        decode_first_frame(&output, 32 * MIB, &AtomicBool::new(false))
            .unwrap()
            .get_pixel(1, 1)[3],
        0
    );
}

#[test]
fn embedded_icc_is_preserved_losslessly_and_converted_for_resize_and_first_frame() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("p3.gif");
    let optimized = dir.path().join("optimized.gif");
    let resized = dir.path().join("resized.gif");
    let icc = moxcms::ColorProfile::new_display_p3().encode().unwrap();
    {
        let mut file = fs::File::create(&input).unwrap();
        let mut encoder = Encoder::new(&mut file, 2, 2, &[210, 100, 40, 0, 0, 0]).unwrap();
        let mut chunks: Vec<&[u8]> = vec![b"ICCRGBG1012"];
        chunks.extend(icc.chunks(255));
        encoder
            .write_raw_extension(gif::AnyExtension(0xff), &chunks)
            .unwrap();
        encoder
            .write_frame(&Frame {
                width: 2,
                height: 2,
                buffer: Cow::Borrowed(&[0, 0, 0, 0]),
                ..Default::default()
            })
            .unwrap();
    }
    let cancel = AtomicBool::new(false);
    assert!(inspect_gif(&input, &cancel).unwrap().has_icc);
    transcode_gif(&input, &optimized, &GifOptions::default(), &cancel).unwrap();
    assert!(inspect_gif(&optimized, &cancel).unwrap().has_icc);
    assert_eq!(rendered(&input), rendered(&optimized));
    let first = decode_first_frame(&input, 32 * MIB, &cancel).unwrap();
    assert_ne!(&first.get_pixel(0, 0).0[..3], &[210, 100, 40]);
    transcode_gif(
        &input,
        &resized,
        &GifOptions {
            resize: Some((1, 1)),
            ..Default::default()
        },
        &cancel,
    )
    .unwrap();
    assert!(!inspect_gif(&resized, &cancel).unwrap().has_icc);
    let converted = rendered(&resized).remove(0).0;
    for channel in 0..3 {
        assert!(
            (i16::from(first.get_pixel(0, 0)[channel])
                - i16::from(converted.get_pixel(0, 0)[channel]))
            .abs()
                <= 1
        );
    }
}

#[test]
fn truncated_data_is_not_reported_as_a_successful_gif() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("broken.gif");
    let output = dir.path().join("result.gif");
    fixture(&input, None);
    let mut bytes = fs::read(&input).unwrap();
    bytes.truncate(bytes.len() - 4);
    fs::write(&input, bytes).unwrap();
    assert!(
        transcode_gif(
            &input,
            &output,
            &GifOptions::default(),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(!output.exists());
}

#[test]
fn long_animation_is_processed_sequentially_without_duration_or_frame_loss() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("long.gif");
    let output = dir.path().join("long-result.gif");
    {
        let mut file = fs::File::create(&input).unwrap();
        let mut encoder = Encoder::new(&mut file, 32, 32, &[0, 0, 0, 255, 255, 255]).unwrap();
        encoder.set_repeat(Repeat::Finite(2)).unwrap();
        for index in 0..2000 {
            let frame = Frame {
                width: 32,
                height: 32,
                buffer: Cow::Owned(vec![(index % 2) as u8; 1024]),
                delay: 1,
                dispose: DisposalMethod::Keep,
                ..Default::default()
            };
            encoder.write_frame(&frame).unwrap();
        }
    }
    let result = transcode_gif(
        &input,
        &output,
        &GifOptions {
            resize: Some((16, 16)),
            memory_limit_bytes: 16 * MIB,
            ..Default::default()
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        (result.frames, result.duration_cs, result.repeat),
        (2000, 2000, Some(2))
    );
    let decoder =
        image::codecs::gif::GifDecoder::new(BufReader::new(fs::File::open(output).unwrap()))
            .unwrap();
    assert_eq!(decoder.dimensions(), (16, 16));
    assert_eq!(
        decoder.into_frames().filter(|frame| frame.is_ok()).count(),
        2000
    );
}

#[test]
fn cancellation_during_a_running_animation_stops_at_a_safe_boundary() {
    use std::{
        sync::{Arc, atomic::Ordering},
        thread,
        time::{Duration, Instant},
    };
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("long.gif");
    let output = dir.path().join("partial.gif");
    {
        let mut file = fs::File::create(&input).unwrap();
        let mut encoder = Encoder::new(&mut file, 64, 64, &[0, 0, 0, 255, 255, 255]).unwrap();
        let frame = Frame {
            width: 64,
            height: 64,
            buffer: Cow::Owned(vec![1; 4096]),
            delay: 1,
            ..Default::default()
        };
        for _ in 0..2000 {
            encoder.write_frame(&frame).unwrap();
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let worker_output = output.clone();
    let worker = thread::spawn(move || {
        transcode_gif(
            &input,
            &worker_output,
            &GifOptions {
                resize: Some((32, 32)),
                ..Default::default()
            },
            &worker_cancel,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while !output.exists() && !worker.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert!(output.exists(), "the worker should have entered encoding");
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(worker.join().unwrap(), Err(CoreError::Cancelled)));
}

#[test]
fn streaming_previews_share_compositing_and_stop_without_collecting_the_animation() {
    use local_image_core::gif_codec::preview_frames;
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("preview.gif");
    fixture(&input, Some(Repeat::Finite(5)));
    let expected = rendered(&input);
    let mut count = 0;
    preview_frames(&input, 1024, 32 * MIB, |pixels, delay_ms| {
        assert_eq!(pixels, expected[count].0);
        assert_eq!(delay_ms, [0, 30, 170, 655350][count]);
        count += 1;
        true
    })
    .unwrap();
    assert_eq!(
        count, 4,
        "the core emits one pass; the UI owns looping previews"
    );
    count = 0;
    preview_frames(&input, 2, 32 * MIB, |pixels, _| {
        assert_eq!(pixels.dimensions(), (2, 1));
        count += 1;
        count < 2
    })
    .unwrap();
    assert_eq!(count, 2);
    let mut called = false;
    assert!(matches!(
        preview_frames(&input, 1024, 1, |_, _| {
            called = true;
            true
        }),
        Err(CoreError::MemoryBudget { .. })
    ));
    assert!(!called);
}

fn write_profile_gif(path: &Path, profiles: &[&[u8]]) {
    let mut file = fs::File::create(path).unwrap();
    let mut encoder = Encoder::new(&mut file, 1, 1, &[210, 100, 40, 0, 0, 0]).unwrap();
    for profile in profiles {
        let mut chunks = vec![&b"ICCRGBG1012"[..]];
        chunks.extend(profile.chunks(255));
        encoder
            .write_raw_extension(gif::AnyExtension(0xff), &chunks)
            .unwrap();
    }
    encoder
        .write_frame(&Frame {
            width: 1,
            height: 1,
            buffer: Cow::Borrowed(&[0]),
            ..Default::default()
        })
        .unwrap();
}

#[test]
fn trailer_payload_is_rejected_before_a_source_fallback_can_reintroduce_it() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("trailing.gif");
    let output = dir.path().join("result.gif");
    let hex = "47494638376101000100800000000000ffffff2c00000000010001000002024401003b";
    let mut bytes = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();
    fs::write(&input, &bytes).unwrap();
    assert_eq!(
        inspect_gif(&input, &AtomicBool::new(false)).unwrap().frames,
        1
    );
    for trailing in [&b"GPS"[..], &[0][..], &b"\n"[..]] {
        let original = bytes.len();
        bytes.extend_from_slice(trailing);
        fs::write(&input, &bytes).unwrap();
        let error = inspect_gif(&input, &AtomicBool::new(false)).unwrap_err();
        assert!(error.to_string().contains("trailing data"));
        assert!(
            transcode_gif(
                &input,
                &output,
                &GifOptions::default(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert!(!output.exists());
        bytes.truncate(original);
    }
}

#[test]
fn cumulative_comments_are_limited_even_when_each_extension_is_small_enough() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("comments.gif");
    let output = dir.path().join("result.gif");
    {
        let mut file = fs::File::create(&input).unwrap();
        let mut encoder = Encoder::new(&mut file, 1, 1, &[0, 0, 0, 255, 255, 255]).unwrap();
        let payload = vec![b'p'; 8 * 1024 * 1024 + 1024];
        for _ in 0..2 {
            encoder
                .write_raw_extension(gif::AnyExtension(0xfe), &[&payload])
                .unwrap();
        }
        encoder
            .write_frame(&Frame {
                width: 1,
                height: 1,
                buffer: Cow::Borrowed(&[0]),
                ..Default::default()
            })
            .unwrap();
    }
    let cancel = AtomicBool::new(false);
    assert!(
        inspect_gif(&input, &cancel)
            .unwrap_err()
            .to_string()
            .contains("Cumulative GIF extension")
    );
    assert!(
        transcode_gif(&input, &output, &GifOptions::default(), &cancel)
            .unwrap_err()
            .to_string()
            .contains("Cumulative GIF extension")
    );
    assert!(!output.exists());
}

#[test]
fn duplicate_icc_payloads_share_the_cumulative_extension_limit() {
    let dir = tempfile::tempdir().unwrap();
    let single = dir.path().join("single-profile.gif");
    let duplicate = dir.path().join("duplicate-profile.gif");
    let mut profile = moxcms::ColorProfile::new_srgb().encode().unwrap();
    profile.resize(8 * 1024 * 1024 + 1024, 0);
    let length = profile.len() as u32;
    profile[..4].copy_from_slice(&length.to_be_bytes());
    write_profile_gif(&single, &[&profile]);
    assert!(
        inspect_gif(&single, &AtomicBool::new(false))
            .unwrap()
            .has_icc
    );
    write_profile_gif(&duplicate, &[&profile, &profile]);
    assert!(
        inspect_gif(&duplicate, &AtomicBool::new(false))
            .unwrap_err()
            .to_string()
            .contains("Cumulative GIF extension")
    );
}

#[test]
fn pure_lossless_rejects_hdr_and_invalid_icc_including_replaced_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("unsupported-icc.gif");
    let output = dir.path().join("result.gif");
    let hdr = moxcms::ColorProfile::new_display_p3_pq().encode().unwrap();
    let sdr = moxcms::ColorProfile::new_srgb().encode().unwrap();
    let invalid = &b"not an ICC profile"[..];
    for profiles in [vec![&hdr[..]], vec![&hdr[..], &sdr[..]], vec![invalid]] {
        write_profile_gif(&input, &profiles);
        let error = transcode_gif(
            &input,
            &output,
            &GifOptions::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(matches!(error, CoreError::Unsupported(_)));
        assert!(!output.exists());
        assert!(inspect_gif(&input, &AtomicBool::new(false)).is_err());
    }
}
