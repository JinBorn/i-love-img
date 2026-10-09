//! Task planning and format-aware processing; all writes target a caller-owned
//! staging file. Publication and collision handling belong to the batch layer.
use crate::{
    Compression, CoreError, Format, ImageInfo, MIB, ProcessOptions, ProcessReport, Result, color,
    error::check_cancel,
    gif_codec,
    metadata::{self, MetadataPolicy},
    pixels,
    probe::probe,
    static_codecs,
};
use image::DynamicImage;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

fn pure_lossless(info: &ImageInfo, options: &ProcessOptions, dimensions: (u32, u32)) -> bool {
    options.output_format(info) == info.format
        && dimensions == info.display_dimensions()
        && (matches!(info.format, Format::Png | Format::Gif)
            || options.compression == Compression::Lossless)
}

pub fn estimate_memory(info: &ImageInfo, options: &ProcessOptions) -> Result<u64> {
    options.validate()?;
    let dimensions = options
        .resize
        .dimensions(info.display_dimensions().0, info.display_dimensions().1)?;
    let output = options.output_format(info);
    check_dimensions(output, dimensions)?;
    if info.format == Format::Gif && output == Format::Gif {
        return Ok(gif_codec::estimate_gif_memory(
            (info.width, info.height),
            dimensions,
            dimensions != (info.width, info.height),
        )
        .saturating_add(64 * MIB));
    }
    let source_pixels = u64::from(info.width).saturating_mul(u64::from(info.height));
    let target_pixels = u64::from(dimensions.0).saturating_mul(u64::from(dimensions.1));
    // Includes decoded/oriented pixels, linear-light resize + alpha scratch,
    // color/codec workspaces and simultaneous encoded metadata candidates.
    // Conservative estimates are validated by release-platform measurements.
    let (src_factor, dst_factor) = if pure_lossless(info, options, dimensions) {
        match info.format {
            Format::Jpeg => (20, 0),
            Format::Png => (32, 0),
            _ => (24, 12),
        }
    } else if dimensions != info.display_dimensions() {
        (32, 24)
    } else {
        (28, 12)
    };
    Ok(source_pixels
        .saturating_mul(src_factor)
        .saturating_add(target_pixels.saturating_mul(dst_factor))
        .saturating_add(info.bytes.saturating_mul(3))
        .saturating_add(32 * MIB))
}

fn check_dimensions(format: Format, dimensions: (u32, u32)) -> Result<()> {
    let edge = match format {
        Format::Jpeg => 65500,
        Format::WebP => 16383,
        Format::Gif => 65535,
        Format::Png => 0x7fff_ffff,
    };
    if dimensions.0 == 0 || dimensions.1 == 0 || dimensions.0 > edge || dimensions.1 > edge {
        return Err(CoreError::Unsupported(format!(
            "{} output dimensions exceed the codec limit ({edge})",
            format.label()
        )));
    }
    Ok(())
}

fn read_input(path: &Path, expected: u64, limit: u64, cancel: &AtomicBool) -> Result<Vec<u8>> {
    check_cancel(cancel)?;
    if expected > limit / 3 || expected > usize::MAX as u64 {
        return Err(CoreError::MemoryBudget {
            estimated: expected.saturating_mul(3),
            limit,
        });
    }
    let mut file = File::open(path)?;
    if file.metadata()?.len() != expected {
        return Err(CoreError::SourceChanged(path.to_owned()));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected as usize)
        .map_err(|_| CoreError::MemoryBudget {
            estimated: expected.saturating_mul(3),
            limit,
        })?;
    let mut block = [0u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        if bytes.len() as u64 + n as u64 > expected {
            return Err(CoreError::SourceChanged(path.to_owned()));
        }
        bytes.extend_from_slice(&block[..n]);
    }
    if bytes.len() as u64 != expected {
        return Err(CoreError::SourceChanged(path.to_owned()));
    }
    Ok(bytes)
}

fn write_output(path: &Path, bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
    check_cancel(cancel)?;
    let mut file = File::create(path)?;
    for block in bytes.chunks(64 * 1024) {
        check_cancel(cancel)?;
        file.write_all(block)?;
    }
    file.flush()?;
    check_cancel(cancel)
}

fn error_from_metadata(error: String, cancel: &AtomicBool) -> CoreError {
    if cancel.load(Ordering::Relaxed) {
        CoreError::Cancelled
    } else {
        CoreError::Image(error)
    }
}

pub fn process_to_path(
    input: &Path,
    output: &Path,
    options: &ProcessOptions,
    cancel: &AtomicBool,
) -> Result<ProcessReport> {
    let started = Instant::now();
    check_cancel(cancel)?;
    options.validate()?;
    let input_canonical = fs::canonicalize(input)?;
    if input == output
        || fs::canonicalize(output).ok().as_ref() == Some(&input_canonical)
        || (output.exists() && same_file::is_same_file(input, output)?)
    {
        return Err(CoreError::Invalid(
            "The output staging path must differ from the source".into(),
        ));
    }
    let info = probe(input)?;
    let estimated = estimate_memory(&info, options)?;
    if estimated > options.memory_limit_bytes {
        return Err(CoreError::MemoryBudget {
            estimated,
            limit: options.memory_limit_bytes,
        });
    }
    let dimensions = options
        .resize
        .dimensions(info.display_dimensions().0, info.display_dimensions().1)?;
    let format = options.output_format(&info);
    let pure = pure_lossless(&info, options, dimensions);
    if format == Format::Jpeg && options.compression == Compression::Lossless && !pure {
        return Err(CoreError::Invalid("Lossless JPEG optimization requires a JPEG input without resizing; choose standard compression for new JPEG pixels".into()));
    }
    if info.animated && format != Format::Gif && !options.gif_first_frame {
        return Err(CoreError::Invalid(
            "Exporting an animated GIF to a static format requires explicit Export first frame"
                .into(),
        ));
    }
    if info.format == Format::Gif && format == Format::Gif {
        let gif_options = gif_codec::GifOptions {
            resize: (dimensions != (info.width, info.height)).then_some(dimensions),
            matte: options.matte,
            preserve_metadata: !options.strip_metadata,
            memory_limit_bytes: options.memory_limit_bytes,
        };
        let summary = gif_codec::transcode_gif(input, output, &gif_options, cancel)?;
        let mut unchanged = false;
        let mut output_bytes = fs::metadata(output)?.len();
        if pure && (!options.strip_metadata || !info.has_metadata) && output_bytes >= info.bytes {
            check_cancel(cancel)?;
            fs::copy(input, output)?;
            check_cancel(cancel)?;
            output_bytes = info.bytes;
            unchanged = true;
        }
        return Ok(ProcessReport {
            format,
            width: summary.width,
            height: summary.height,
            input_bytes: info.bytes,
            output_bytes,
            elapsed_ms: started.elapsed().as_millis() as u64,
            warnings: summary.warnings,
            unchanged,
        });
    }
    if info.format == Format::Gif {
        let mut image = DynamicImage::ImageRgba8(gif_codec::decode_first_frame(
            input,
            options.memory_limit_bytes,
            cancel,
        )?);
        image = pixels::resize_image(image, dimensions.0, dimensions.1, cancel)?;
        let metadata = metadata::ImageMetadata::default();
        let mut warnings = vec![];
        if info.animated {
            warnings.push(
                "Only the explicitly requested first displayed GIF frame was exported".into(),
            );
        }
        if info.has_metadata && !options.strip_metadata {
            warnings
                .push("GIF-specific metadata cannot be represented in this static output".into());
        }
        let encoded = encode_pixels(image, format, options, cancel)?;
        let policy = MetadataPolicy {
            keep_metadata: false,
            orientation_normalized: true,
            converted_to_srgb: true,
            dimensions,
            pixels_changed: true,
        };
        let (encoded, metadata_warnings) = metadata::rewrite_metadata(
            &encoded,
            static_codecs::image_format(format),
            &metadata,
            &policy,
        )
        .map_err(|e| error_from_metadata(e, cancel))?;
        warnings.extend(metadata_warnings);
        write_output(output, &encoded, cancel)?;
        return Ok(ProcessReport {
            format,
            width: dimensions.0,
            height: dimensions.1,
            input_bytes: info.bytes,
            output_bytes: encoded.len() as u64,
            elapsed_ms: started.elapsed().as_millis() as u64,
            warnings,
            unchanged: false,
        });
    }

    let source_bytes = read_input(input, info.bytes, options.memory_limit_bytes, cancel)?;
    let source_metadata =
        metadata::read_metadata(&source_bytes, static_codecs::image_format(info.format))
            .map_err(|e| error_from_metadata(e, cancel))?;
    if source_metadata.orientation.max(1) != info.orientation
        || source_metadata.icc_profile.is_some() != info.has_icc
    {
        return Err(CoreError::SourceChanged(input.to_owned()));
    }
    if source_metadata.hdr {
        return Err(CoreError::Unsupported(
            "HDR/multi-picture input is not supported".into(),
        ));
    }
    let mut warnings = source_metadata.warnings.clone();
    let converted = !pure;
    let orientation_normalized = !pure;
    let mut encoded = if pure && info.format == Format::Jpeg {
        static_codecs::optimize_jpeg(
            &source_bytes,
            source_metadata.adobe_inverted,
            options.memory_limit_bytes,
            cancel,
        )?
    } else if pure && info.format == Format::Png {
        static_codecs::optimize_png(&source_bytes, options.memory_limit_bytes, cancel)?
    } else {
        let (mut image, already_srgb) = static_codecs::decode(
            &source_bytes,
            info.format,
            &source_metadata,
            options.memory_limit_bytes,
            cancel,
        )?;
        if already_srgb && source_metadata.icc_profile.is_none() {
            warnings.push("Untagged CMYK JPEG uses a generic CMYK color interpretation".into());
        }
        if !pure {
            if !already_srgb {
                image = color::to_srgb(image, &source_metadata, &|| cancel.load(Ordering::Relaxed))
                    .map_err(|e| error_from_metadata(e, cancel))?;
            }
            pixels::orient(&mut image, source_metadata.orientation);
            check_cancel(cancel)?;
            image = pixels::resize_image(image, dimensions.0, dimensions.1, cancel)?;
        }
        if info.bit_depth > 8 && format != Format::Png {
            warnings
                .push("The selected format uses 8-bit output; source precision was reduced".into());
        }
        if format == Format::Gif {
            let gif_options = gif_codec::GifOptions {
                resize: None,
                matte: options.matte,
                preserve_metadata: !options.strip_metadata,
                memory_limit_bytes: options.memory_limit_bytes,
            };
            let summary =
                gif_codec::encode_static_gif(&image.into_rgba8(), output, &gif_options, cancel)?;
            warnings.extend(summary.warnings);
            if !options.strip_metadata && (info.has_metadata || info.has_icc) {
                warnings.push("Source metadata cannot be represented safely in the new indexed GIF; pixels were converted to sRGB".into());
            }
            return Ok(ProcessReport {
                format,
                width: summary.width,
                height: summary.height,
                input_bytes: info.bytes,
                output_bytes: fs::metadata(output)?.len(),
                elapsed_ms: started.elapsed().as_millis() as u64,
                warnings,
                unchanged: false,
            });
        }
        encode_pixels(image, format, options, cancel)?
    };
    check_cancel(cancel)?;
    let policy = MetadataPolicy {
        keep_metadata: !options.strip_metadata,
        orientation_normalized,
        converted_to_srgb: converted,
        dimensions: if pure {
            (info.width, info.height)
        } else {
            dimensions
        },
        pixels_changed: !pure,
    };
    let (rewritten, metadata_warnings) = metadata::rewrite_metadata(
        &encoded,
        static_codecs::image_format(format),
        &source_metadata,
        &policy,
    )
    .map_err(|e| error_from_metadata(e, cancel))?;
    encoded = rewritten;
    warnings.extend(metadata_warnings);
    let mut unchanged = false;
    // A fallback must satisfy metadata and color policy too. Rewriting the
    // source candidate prevents privacy metadata from returning with a copy.
    let source_compatible = pure
        || (format == info.format
            && dimensions == info.display_dimensions()
            && source_metadata.orientation == 1
            && source_metadata.icc_profile.is_none()
            && source_metadata.png_gamma.is_none()
            && source_metadata.png_chromaticities.is_none()
            && source_metadata.png_cicp.is_none()
            && source_metadata.jpeg_components != Some(4));
    if source_compatible && encoded.len() >= source_bytes.len() {
        let (candidate, _) = metadata::rewrite_metadata(
            &source_bytes,
            static_codecs::image_format(format),
            &source_metadata,
            &policy,
        )
        .map_err(|e| error_from_metadata(e, cancel))?;
        if candidate.len() <= encoded.len() {
            unchanged = candidate == source_bytes;
            encoded = candidate;
        }
    }
    check_cancel(cancel)?;
    write_output(output, &encoded, cancel)?;
    Ok(ProcessReport {
        format,
        width: dimensions.0,
        height: dimensions.1,
        input_bytes: info.bytes,
        output_bytes: encoded.len() as u64,
        elapsed_ms: started.elapsed().as_millis() as u64,
        warnings,
        unchanged,
    })
}

fn encode_pixels(
    image: DynamicImage,
    format: Format,
    options: &ProcessOptions,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    match format {
        Format::Jpeg => static_codecs::encode_jpeg(
            &pixels::flatten_alpha(image, options.matte),
            options.jpeg_quality,
            cancel,
        ),
        Format::Png => {
            let encoded = static_codecs::encode_png(&image, cancel)?;
            drop(image);
            static_codecs::optimize_png(&encoded, options.memory_limit_bytes, cancel)
        }
        Format::WebP => static_codecs::encode_webp(
            &image,
            options.webp_quality,
            options.compression == Compression::Lossless,
            cancel,
        ),
        Format::Gif => Err(CoreError::Invalid(
            "GIF output must use the streaming GIF encoder".into(),
        )),
    }
}

/// A selected-image preview shares color/orientation handling with processing.
/// This separate 384 MiB allowance fits the GUI reserve outside batch budgets.
pub fn preview(path: &Path, max_edge: u32) -> Result<image::RgbaImage> {
    if max_edge == 0 {
        return Err(CoreError::Invalid("Preview size must be positive".into()));
    }
    let info = probe(path)?;
    let limit = 384 * MIB;
    let estimated = u64::from(info.width)
        .saturating_mul(u64::from(info.height))
        .saturating_mul(if info.bit_depth > 8 { 20 } else { 12 })
        .saturating_add(info.bytes.saturating_mul(2))
        .saturating_add(16 * MIB);
    if estimated > limit {
        return Err(CoreError::MemoryBudget { estimated, limit });
    }
    let cancel = AtomicBool::new(false);
    let mut image = if info.format == Format::Gif {
        DynamicImage::ImageRgba8(gif_codec::decode_first_frame(path, limit, &cancel)?)
    } else {
        let bytes = read_input(path, info.bytes, limit, &cancel)?;
        let metadata = metadata::read_metadata(&bytes, static_codecs::image_format(info.format))
            .map_err(CoreError::Image)?;
        if metadata.hdr {
            return Err(CoreError::Unsupported(
                "HDR preview is not supported".into(),
            ));
        }
        let (mut image, already_srgb) =
            static_codecs::decode(&bytes, info.format, &metadata, limit, &cancel)?;
        if !already_srgb {
            image = color::to_srgb(image, &metadata, &|| false).map_err(CoreError::Image)?;
        }
        pixels::orient(&mut image, metadata.orientation);
        image
    };
    if image.width().max(image.height()) > max_edge {
        let scale = f64::from(max_edge) / f64::from(image.width().max(image.height()));
        let width = (f64::from(image.width()) * scale).round().max(1.0) as u32;
        let height = (f64::from(image.height()) * scale).round().max(1.0) as u32;
        // Preview uses the original integer precision to avoid a full extra
        // linear 16-bit copy. Alpha is still premultiplied by the resizer.
        let mut target = DynamicImage::new(width, height, image.color());
        fast_image_resize::Resizer::new()
            .resize(&image, &mut target, None)
            .map_err(|e| CoreError::Image(e.to_string()))?;
        image = target;
    }
    Ok(image.into_rgba8())
}
