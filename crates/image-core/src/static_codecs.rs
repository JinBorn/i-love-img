//! Explicit still-image codecs. JPEG lossless optimization copies DCT
//! coefficients and quantization tables; it never decodes/requantizes pixels.
use crate::{CoreError, Format, Result, color, error::check_cancel, metadata::ImageMetadata};
use image::{DynamicImage, GenericImageView, ImageEncoder};
use std::{
    io::Cursor,
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) fn image_format(format: Format) -> image::ImageFormat {
    match format {
        Format::Jpeg => image::ImageFormat::Jpeg,
        Format::Png => image::ImageFormat::Png,
        Format::WebP => image::ImageFormat::WebP,
        Format::Gif => image::ImageFormat::Gif,
    }
}

/// The boolean reports CMYK conversion already performed by this decoder.
pub(crate) fn decode(
    bytes: &[u8],
    format: Format,
    metadata: &ImageMetadata,
    limit: u64,
    cancel: &AtomicBool,
) -> Result<(DynamicImage, bool)> {
    check_cancel(cancel)?;
    if format == Format::Jpeg && metadata.jpeg_components == Some(4) {
        return decode_cmyk(bytes, metadata, cancel).map(|im| (im, true));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image_format(format));
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(limit);
    reader.limits(limits);
    let image = reader.decode()?;
    check_cancel(cancel)?;
    if matches!(
        image.color(),
        image::ColorType::Rgb32F | image::ColorType::Rgba32F
    ) {
        return Err(CoreError::Unsupported(
            "HDR/floating-point images are not supported".into(),
        ));
    }
    Ok((image, false))
}

fn decode_cmyk(
    bytes: &[u8],
    metadata: &ImageMetadata,
    cancel: &AtomicBool,
) -> Result<DynamicImage> {
    let decode = || -> Result<DynamicImage> {
        let decoder = mozjpeg::Decompress::new_mem(bytes)?;
        let mut decoder = decoder.to_colorspace(mozjpeg::ColorSpace::JCS_CMYK)?;
        let (width, height) = (decoder.width() as u32, decoder.height() as u32);
        let raw = decoder.read_scanlines::<u8>()?;
        decoder.finish()?;
        check_cancel(cancel)?;
        if let Some(profile) = &metadata.icc_profile {
            return color::convert_cmyk8_to_srgb(
                &raw,
                width,
                height,
                profile,
                metadata.adobe_inverted,
                &|| cancel.load(Ordering::Relaxed),
            )
            .map_err(CoreError::Image);
        }
        // Untagged device CMYK has no defined colorimetry. Use the conventional
        // multiplicative approximation and report this choice in the pipeline.
        let mut rgb = image::RgbImage::new(width, height);
        for (i, (pixel, target)) in raw
            .as_chunks::<4>()
            .0
            .iter()
            .zip(rgb.pixels_mut())
            .enumerate()
        {
            if i % 65536 == 0 {
                check_cancel(cancel)?;
            }
            let ink = |c: usize| {
                if metadata.adobe_inverted {
                    255 - u32::from(pixel[c])
                } else {
                    u32::from(pixel[c])
                }
            };
            for c in 0..3 {
                target[c] = (((255 - ink(c)) * (255 - ink(3)) + 127) / 255) as u8;
            }
        }
        Ok(DynamicImage::ImageRgb8(rgb))
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode)).unwrap_or_else(|_| {
        Err(CoreError::Image(
            "JPEG decoder rejected malformed input".into(),
        ))
    })
}

pub(crate) fn encode_jpeg(
    rgb: &image::RgbImage,
    quality: u8,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    let encode = || -> Result<Vec<u8>> {
        check_cancel(cancel)?;
        let mut encoder = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_RGB);
        encoder.set_size(rgb.width() as usize, rgb.height() as usize);
        encoder.set_quality(f32::from(quality));
        // Avoid chroma subsampling damage to screenshots, text and colored edges.
        encoder.set_chroma_sampling_pixel_sizes((1, 1), (1, 1));
        let mut encoder = encoder.start_compress(Vec::new())?;
        for rows in rgb.as_raw().chunks(rgb.width() as usize * 3 * 32) {
            check_cancel(cancel)?;
            encoder.write_scanlines(rows)?;
        }
        let bytes = encoder.finish()?;
        check_cancel(cancel)?;
        Ok(bytes)
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(encode))
        .unwrap_or_else(|_| Err(CoreError::Image("JPEG encoder failed".into())))
}

pub(crate) fn encode_png(image: &DynamicImage, cancel: &AtomicBool) -> Result<Vec<u8>> {
    check_cancel(cancel)?;
    let mut bytes = Vec::new();
    let (width, height) = image.dimensions();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut bytes,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(image.as_bytes(), width, height, image.color().into())?;
    check_cancel(cancel)?;
    Ok(bytes)
}

pub(crate) fn optimize_png(bytes: &[u8], limit: u64, cancel: &AtomicBool) -> Result<Vec<u8>> {
    check_cancel(cancel)?;
    let mut options = oxipng::Options::from_preset(1);
    options.bit_depth_reduction = false;
    options.color_type_reduction = false;
    options.palette_reduction = false;
    options.grayscale_reduction = false;
    options.optimize_alpha = false;
    options.scale_16 = false;
    options.strip = oxipng::StripChunks::None;
    options.max_decompressed_size = Some(limit.min(usize::MAX as u64) as usize);
    options.timeout = Some(std::time::Duration::from_secs(20));
    let result = oxipng::optimize_from_memory(bytes, &options)
        .map_err(|e| CoreError::Image(e.to_string()))?;
    check_cancel(cancel)?;
    Ok(result)
}

pub(crate) fn encode_webp(
    image: &DynamicImage,
    quality: u8,
    lossless: bool,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    check_cancel(cancel)?;
    let rgba = image.to_rgba8();
    let encoder = webp::Encoder::from_rgba(&rgba, rgba.width(), rgba.height());
    let mut config = webp::WebPConfig::new()
        .map_err(|_| CoreError::Image("WebP configuration failed".into()))?;
    config.lossless = i32::from(lossless);
    config.quality = if lossless { 75.0 } else { f32::from(quality) };
    config.method = 4;
    config.thread_level = 0;
    // Preserve invisible RGB as well as displayed pixels in lossless mode.
    config.exact = i32::from(lossless);
    let encoded = encoder
        .encode_advanced(&config)
        .map_err(|e| CoreError::Image(format!("WebP: {e:?}")))?;
    check_cancel(cancel)?;
    Ok(encoded.to_vec())
}

/// Coefficient-domain optimization with RAII cleanup even when libjpeg raises
/// an error or the progress callback observes cancellation. mozjpeg-sys is built
/// with its `unwinding` feature and all callbacks use the C-unwind ABI.
pub(crate) fn optimize_jpeg(
    bytes: &[u8],
    preserve_adobe: bool,
    limit: u64,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    use mozjpeg_sys as ffi;
    struct State {
        decoder: Box<ffi::jpeg_decompress_struct>,
        encoder: Box<ffi::jpeg_compress_struct>,
        _decode_error: Box<ffi::jpeg_error_mgr>,
        _encode_error: Box<ffi::jpeg_error_mgr>,
        _progress: Box<ffi::jpeg_progress_mgr>,
        output: *mut u8,
        output_len: libc::c_ulong,
    }
    impl Drop for State {
        fn drop(&mut self) {
            unsafe {
                if !self.encoder.common.mem.is_null() {
                    ffi::jpeg_destroy_compress(&mut self.encoder);
                }
                if !self.decoder.common.mem.is_null() {
                    ffi::jpeg_destroy_decompress(&mut self.decoder);
                }
                if !self.output.is_null() {
                    libc::free(self.output.cast());
                }
            }
        }
    }
    unsafe extern "C-unwind" fn fail(info: &mut ffi::jpeg_common_struct) {
        let error = unsafe { &*info.err };
        let mut buffer = [0u8; 80];
        if let Some(format) = error.format_message {
            // mozjpeg-sys declares libjpeg's output buffer as an immutable
            // reference; the C contract is a writable JMSG_LENGTH_MAX buffer.
            let format: unsafe extern "C-unwind" fn(&mut ffi::jpeg_common_struct, &mut [u8; 80]) =
                unsafe { std::mem::transmute(format) };
            unsafe { format(info, &mut buffer) };
        }
        let message = String::from_utf8_lossy(buffer.split(|&b| b == 0).next().unwrap_or(&[]));
        std::panic::resume_unwind(Box::new(format!("JPEG codec error: {message}")));
    }
    unsafe extern "C-unwind" fn quiet(_: &mut ffi::jpeg_common_struct, _: libc::c_int) {}
    unsafe extern "C-unwind" fn progress(info: &mut ffi::jpeg_common_struct) {
        if !info.client_data.is_null()
            && unsafe { &*info.client_data.cast::<AtomicBool>() }.load(Ordering::Relaxed)
        {
            std::panic::resume_unwind(Box::new(CoreError::Cancelled));
        }
    }
    check_cancel(cancel)?;
    let transcode = || -> Result<Vec<u8>> {
        unsafe {
            let mut de = Box::new(std::mem::zeroed::<ffi::jpeg_error_mgr>());
            let mut en = Box::new(std::mem::zeroed::<ffi::jpeg_error_mgr>());
            ffi::jpeg_std_error(&mut de);
            ffi::jpeg_std_error(&mut en);
            de.error_exit = Some(fail);
            en.error_exit = Some(fail);
            de.emit_message = Some(quiet);
            en.emit_message = Some(quiet);
            let mut monitor = Box::new(std::mem::zeroed::<ffi::jpeg_progress_mgr>());
            monitor.progress_monitor = Some(progress);
            let mut decoder = Box::new(std::mem::zeroed::<ffi::jpeg_decompress_struct>());
            let mut encoder = Box::new(std::mem::zeroed::<ffi::jpeg_compress_struct>());
            decoder.common.err = &mut *de;
            encoder.common.err = &mut *en;
            let mut state = State {
                decoder,
                encoder,
                _decode_error: de,
                _encode_error: en,
                _progress: monitor,
                output: std::ptr::null_mut(),
                output_len: 0,
            };
            ffi::jpeg_create_decompress(&mut *state.decoder);
            ffi::jpeg_create_compress(&mut *state.encoder);
            state.decoder.common.client_data = (cancel as *const AtomicBool).cast_mut().cast();
            state.encoder.common.client_data = (cancel as *const AtomicBool).cast_mut().cast();
            state.decoder.common.progress = &mut *state._progress;
            state.encoder.common.progress = &mut *state._progress;
            (*state.decoder.common.mem).max_memory_to_use =
                limit.min(libc::c_long::MAX as u64) as libc::c_long;
            (*state.encoder.common.mem).max_memory_to_use =
                limit.min(libc::c_long::MAX as u64) as libc::c_long;
            ffi::jpeg_mem_src(
                &mut state.decoder,
                bytes.as_ptr(),
                bytes.len() as libc::c_ulong,
            );
            if ffi::jpeg_read_header(&mut state.decoder, 1) != 1 {
                return Err(CoreError::Image("JPEG header is incomplete".into()));
            }
            let coefficients = ffi::jpeg_read_coefficients(&mut state.decoder);
            check_cancel(cancel)?;
            ffi::jpeg_mem_dest(&mut state.encoder, &mut state.output, &mut state.output_len);
            ffi::jpeg_copy_critical_parameters(&state.decoder, &mut state.encoder);
            // Adding APP14 to untagged CMYK would invert the interpretation of
            // otherwise unchanged coefficients. Preserve the source convention.
            state.encoder.write_Adobe_marker = i32::from(preserve_adobe);
            state.encoder.optimize_coding = 1;
            // Scan generation depends on OPTIMIZE_SCANS; set the policy first.
            // Changing it after jpeg_simple_progression invalidates that script.
            ffi::jpeg_c_set_bool_param(
                &mut state.encoder,
                ffi::J_BOOLEAN_PARAM::JBOOLEAN_OPTIMIZE_SCANS,
                0,
            );
            ffi::jpeg_simple_progression(&mut state.encoder);
            ffi::jpeg_write_coefficients(&mut state.encoder, coefficients);
            ffi::jpeg_finish_compress(&mut state.encoder);
            ffi::jpeg_finish_decompress(&mut state.decoder);
            check_cancel(cancel)?;
            if state.output.is_null() || state.output_len == 0 {
                return Err(CoreError::Image("JPEG encoder produced no data".into()));
            }
            Ok(std::slice::from_raw_parts(state.output, state.output_len as usize).to_vec())
        }
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(transcode)) {
        Ok(result) => result,
        Err(error) => {
            if cancel.load(Ordering::Relaxed) {
                return Err(CoreError::Cancelled);
            }
            let message = error
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "JPEG coefficient transcoding failed".into());
            Err(CoreError::Image(message))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn quantization_tables(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut tables = Vec::new();
        let mut i = 2;
        while i + 4 < bytes.len() {
            if bytes[i] != 0xff {
                break;
            }
            let marker = bytes[i + 1];
            if marker == 0xda || marker == 0xd9 {
                break;
            }
            let n = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if marker == 0xdb {
                tables.push(bytes[i + 4..i + 2 + n].to_vec());
            }
            i += 2 + n;
        }
        tables.sort();
        tables
    }
    #[test]
    fn coefficient_transcoding_preserves_pixels_and_quantization() {
        let pixels = image::RgbImage::from_fn(37, 29, |x, y| {
            image::Rgb([
                ((x * 51 + y * 37) % 256) as u8,
                ((x * 23 + y * 17) % 256) as u8,
                ((x * 7 + y * 61) % 256) as u8,
            ])
        });
        let input = encode_jpeg(&pixels, 78, &AtomicBool::new(false)).unwrap();
        // Call the optimizer directly: pipeline fallback must not mask errors.
        let optimized =
            optimize_jpeg(&input, false, 128 * crate::MIB, &AtomicBool::new(false)).unwrap();
        assert_eq!(quantization_tables(&input), quantization_tables(&optimized));
        let a = image::load_from_memory(&input).unwrap().into_rgb8();
        let b = image::load_from_memory(&optimized).unwrap().into_rgb8();
        assert_eq!(a, b);
    }
    #[test]
    fn malformed_jpeg_returns_error_without_cross_boundary_panic() {
        let damaged = [0xff, 0xd8, 0xff, 0xdb, 0, 1, 0, 0xff, 0xd9];
        let result = std::panic::catch_unwind(|| {
            optimize_jpeg(&damaged, false, 64 * crate::MIB, &AtomicBool::new(false))
        });
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
}
