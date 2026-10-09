//! Streaming GIF processing. Only the current indexed frame, compositing canvas,
//! and the rectangle needed by `Previous` disposal are retained.
//!
//! Indexed re-encoding preserves playback semantics, but is not a promise that
//! every output is smaller. Resizing and creating a GIF require quantization.

use crate::{CoreError, Result, error::check_cancel};
use gif::{ColorOutput, DecodeOptions, DisposalMethod, Encoder, Frame, MemoryLimit};
use image::{DynamicImage, Rgba, RgbaImage};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write},
    num::NonZeroU64,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone)]
pub struct GifOptions {
    /// Exact dimensions already calculated by the shared aspect-ratio planner.
    pub resize: Option<(u32, u32)>,
    pub matte: [u8; 3],
    pub preserve_metadata: bool,
    pub memory_limit_bytes: u64,
}

impl Default for GifOptions {
    fn default() -> Self {
        Self {
            resize: None,
            matte: [255; 3],
            preserve_metadata: false,
            memory_limit_bytes: 1536 * crate::MIB,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GifSummary {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub duration_cs: u64,
    /// None means no looping extension (one playback); zero means infinite.
    pub repeat: Option<u16>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GifInfo {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub repeat: Option<u16>,
    pub has_icc: bool,
    pub has_metadata: bool,
    /// Cumulative framed extension bytes, capped at 16 MiB across the file.
    pub metadata_bytes: u64,
}

/// A conservative, frame-count-independent upper bound for working buffers.
pub fn estimate_gif_memory(source: (u32, u32), target: (u32, u32), resize: bool) -> u64 {
    let src = u64::from(source.0).saturating_mul(u64::from(source.1));
    let dst = u64::from(target.0).saturating_mul(u64::from(target.1));
    if resize {
        src.saturating_mul(56)
            .saturating_add(dst.saturating_mul(40))
            .saturating_add(8 * crate::MIB)
    } else {
        src.saturating_mul(12).saturating_add(8 * crate::MIB)
    }
}

fn budget(estimated: u64, limit: u64) -> Result<()> {
    if estimated > limit {
        Err(CoreError::MemoryBudget { estimated, limit })
    } else {
        Ok(())
    }
}

fn dimensions(width: u32, height: u32) -> Result<(u16, u16)> {
    if width == 0 || height == 0 || width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err(CoreError::Invalid(
            "GIF dimensions must be between 1 and 65535".into(),
        ));
    }
    Ok((width as u16, height as u16))
}

fn codec_error(error: impl std::fmt::Display, cancel: &AtomicBool) -> CoreError {
    if cancel.load(Ordering::Relaxed) {
        CoreError::Cancelled
    } else {
        CoreError::Image(format!("GIF: {error}"))
    }
}

struct CancelRead<'a, R> {
    inner: R,
    cancel: &'a AtomicBool,
}
impl<R: Read> Read for CancelRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Do not use Interrupted: read_exact would retry forever after cancellation.
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("GIF cancelled"));
        }
        self.inner.read(buf)
    }
}

fn decoder<'a>(
    input: &Path,
    limit: u64,
    cancel: &'a AtomicBool,
) -> Result<gif::Decoder<CancelRead<'a, File>>> {
    check_cancel(cancel)?;
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::Indexed);
    options.set_memory_limit(MemoryLimit::Bytes(
        NonZeroU64::new(limit)
            .ok_or_else(|| CoreError::Invalid("Zero GIF memory budget".into()))?,
    ));
    options.check_frame_consistency(true);
    // Keep the decoder's default end-code policy. In gif 0.14.2 its strict
    // setting mistakes a row boundary for missing EOI on valid interlaced
    // images. The block scanner still requires complete sub-blocks/trailer,
    // and pixel decoding rejects incomplete frame data.
    options
        .read_info(CancelRead {
            inner: File::open(input)?,
            cancel,
        })
        .map_err(|e| codec_error(e, cancel))
}

/// Scanner for framed GIF blocks. Extensions and compressed data are consumed
/// in sub-blocks of at most 255 bytes, never accumulated across frames.
struct Blocks<'a> {
    reader: BufReader<File>,
    header: [u8; 13],
    global_palette: Vec<u8>,
    info: GifInfo,
    icc_profile: Option<Vec<u8>>,
    capture_icc: bool,
    working_budget: Option<(u64, u64)>,
    cancel: &'a AtomicBool,
}

enum Record {
    Image,
    End,
}

impl<'a> Blocks<'a> {
    fn open(path: &Path, cancel: &'a AtomicBool) -> Result<Self> {
        check_cancel(cancel)?;
        let mut reader = BufReader::new(File::open(path)?);
        let mut header = [0; 13];
        reader.read_exact(&mut header)?;
        if &header[..6] != b"GIF87a" && &header[..6] != b"GIF89a" {
            return Err(CoreError::Invalid("Invalid GIF signature".into()));
        }
        let width = u16::from_le_bytes([header[6], header[7]]) as u32;
        let height = u16::from_le_bytes([header[8], header[9]]) as u32;
        dimensions(width, height)?;
        let mut global_palette = vec![
            0;
            if header[10] & 0x80 != 0 {
                3 * (2usize << (header[10] & 7))
            } else {
                0
            }
        ];
        reader.read_exact(&mut global_palette)?;
        Ok(Self {
            reader,
            header,
            global_palette,
            info: GifInfo {
                width,
                height,
                frames: 0,
                repeat: None,
                has_icc: false,
                has_metadata: false,
                metadata_bytes: 0,
            },
            icc_profile: None,
            capture_icc: true,
            working_budget: None,
            cancel,
        })
    }

    fn byte(&mut self) -> Result<u8> {
        check_cancel(self.cancel)?;
        let mut byte = [0];
        self.reader.read_exact(&mut byte)?;
        Ok(byte[0])
    }

    fn skip(&mut self, mut count: usize) -> Result<()> {
        let mut scratch = [0; 1024];
        while count > 0 {
            check_cancel(self.cancel)?;
            let len = count.min(scratch.len());
            self.reader.read_exact(&mut scratch[..len])?;
            count -= len;
        }
        Ok(())
    }

    fn account_extension(&mut self, bytes: u64) -> Result<()> {
        self.info.metadata_bytes = self.info.metadata_bytes.saturating_add(bytes);
        if self.info.metadata_bytes > crate::metadata::MAX_METADATA_BYTES as u64 {
            return Err(CoreError::Unsupported(
                "Cumulative GIF extension data exceeds the 16 MiB safety limit".into(),
            ));
        }
        if let Some((working, limit)) = self.working_budget {
            budget(
                working.saturating_add(self.info.metadata_bytes.saturating_mul(4)),
                limit,
            )?;
        }
        Ok(())
    }

    fn next(
        &mut self,
        mut output: Option<&mut dyn Write>,
        preserve_metadata: bool,
        transformed: bool,
    ) -> Result<Record> {
        loop {
            match self.byte()? {
                0x3b => {
                    // A raw-source fallback may copy only an entirely validated
                    // container; otherwise a private trailing payload returns.
                    check_cancel(self.cancel)?;
                    let mut trailing = [0u8; 1];
                    if self.reader.read(&mut trailing)? != 0 {
                        return Err(CoreError::Invalid(
                            "GIF contains trailing data after its trailer".into(),
                        ));
                    }
                    return Ok(Record::End);
                }
                0x2c => {
                    let mut desc = [0; 9];
                    self.reader.read_exact(&mut desc)?;
                    let left = u16::from_le_bytes([desc[0], desc[1]]) as u32;
                    let top = u16::from_le_bytes([desc[2], desc[3]]) as u32;
                    let width = u16::from_le_bytes([desc[4], desc[5]]) as u32;
                    let height = u16::from_le_bytes([desc[6], desc[7]]) as u32;
                    if width == 0
                        || height == 0
                        || left + width > self.info.width
                        || top + height > self.info.height
                    {
                        return Err(CoreError::Invalid(
                            "GIF frame rectangle is outside its logical canvas".into(),
                        ));
                    }
                    if desc[8] & 0x80 != 0 {
                        self.skip(3 * (2usize << (desc[8] & 7)))?;
                    }
                    self.byte()?; // Minimum LZW code size; validated by the decoder.
                    loop {
                        let n = self.byte()?;
                        if n == 0 {
                            break;
                        }
                        self.skip(n as usize)?;
                    }
                    self.info.frames = self
                        .info
                        .frames
                        .checked_add(1)
                        .ok_or_else(|| CoreError::Invalid("Too many GIF frames".into()))?;
                    return Ok(Record::Image);
                }
                0x21 => {
                    let label = self.byte()?;
                    if label == 0x01 {
                        return Err(CoreError::Unsupported(
                            "GIF plain-text rendering extensions are not supported".into(),
                        ));
                    }
                    let len = self.byte()?;
                    self.account_extension(u64::from(len) + 3)?;
                    let mut data = [0; 255];
                    self.reader.read_exact(&mut data[..len as usize])?;
                    let name = &data[..len as usize];
                    let looping =
                        label == 0xff && (name == b"NETSCAPE2.0" || name == b"ANIMEXTS1.0");
                    let icc = label == 0xff && name == b"ICCRGBG1012";
                    self.info.has_icc |= icc;
                    self.info.has_metadata |= label != 0xf9 && !looping;
                    if icc && self.capture_icc {
                        self.icc_profile = Some(Vec::new());
                    }
                    let keep =
                        label != 0xf9 && (looping || (!transformed && (icc || preserve_metadata)));
                    if keep && let Some(writer) = output.as_deref_mut() {
                        writer.write_all(&[0x21, label, len])?;
                        writer.write_all(name)?;
                    }
                    if len != 0 {
                        let mut subblock = 0usize;
                        loop {
                            let size = self.byte()?;
                            self.account_extension(u64::from(size) + 1)?;
                            self.reader.read_exact(&mut data[..size as usize])?;
                            if icc && self.capture_icc {
                                let profile =
                                    self.icc_profile.as_mut().expect("ICC initialized above");
                                if profile.len() + usize::from(size) > 16 * 1024 * 1024 {
                                    return Err(CoreError::Unsupported(
                                        "GIF ICC profile exceeds 16 MiB".into(),
                                    ));
                                }
                                profile.extend_from_slice(&data[..size as usize]);
                            }
                            if looping && subblock == 0 && size == 3 && data[0] == 1 {
                                self.info.repeat = Some(u16::from_le_bytes([data[1], data[2]]));
                            }
                            if keep && let Some(writer) = output.as_deref_mut() {
                                writer.write_all(&[size])?;
                                writer.write_all(&data[..size as usize])?;
                            }
                            if size == 0 {
                                break;
                            }
                            subblock += 1;
                        }
                    }
                    if icc && self.capture_icc {
                        check_cancel(self.cancel)?;
                        // Validate every occurrence, including profiles later
                        // replaced by another ICC block and pure-lossless jobs.
                        crate::color::validate_sdr_profile(
                            self.icc_profile.as_deref().unwrap_or_default(),
                        )
                        .map_err(CoreError::Unsupported)?;
                    }
                }
                block => {
                    return Err(CoreError::Invalid(format!(
                        "Unknown GIF block 0x{block:02x}"
                    )));
                }
            }
        }
    }
}

pub fn inspect_gif(input: &Path, cancel: &AtomicBool) -> Result<GifInfo> {
    let mut blocks = Blocks::open(input, cancel)?;
    while let Record::Image = blocks.next(None, false, false)? {}
    if blocks.info.frames == 0 {
        return Err(CoreError::Invalid("GIF contains no image frames".into()));
    }
    Ok(blocks.info)
}

/// gif::Encoder does not expose logical-screen background/aspect fields. Keep
/// those bytes while preserving exactly the encoder's palette-size bits.
struct HeaderWriter<'a, W> {
    inner: W,
    offset: usize,
    background: u8,
    aspect: u8,
    color_flags: u8,
    no_global_palette: bool,
    cancel: &'a AtomicBool,
}
impl<W: Write> Write for HeaderWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("GIF cancelled"));
        }
        // gif 0.14 emits a padded black two-entry global table even when given
        // an empty palette. Suppress that table and its flag to avoid inventing
        // a logical background color for a source with local palettes only.
        if self.no_global_palette && (13..19).contains(&self.offset) {
            let consumed = bytes.len().min(19 - self.offset);
            self.offset += consumed;
            return Ok(consumed);
        }
        if self.offset >= 13 {
            return self.inner.write(bytes);
        }
        let n = bytes.len().min(13 - self.offset);
        let mut header_part = [0; 13];
        header_part[..n].copy_from_slice(&bytes[..n]);
        for (index, value) in header_part[..n].iter_mut().enumerate() {
            match self.offset + index {
                10 => {
                    *value = if self.no_global_palette {
                        self.color_flags & 0x78
                    } else {
                        (*value & 0x87) | (self.color_flags & 0x78)
                    }
                }
                11 => *value = self.background,
                12 => *value = self.aspect,
                _ => {}
            }
        }
        let written = self.inner.write(&header_part[..n])?;
        self.offset += written;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct Canvas {
    pixels: RgbaImage,
    previous: Vec<u8>,
    global_background: [u8; 4],
    initialized: bool,
}
impl Canvas {
    fn new(width: u32, height: u32, palette: &[u8], background: u8) -> Self {
        let index = usize::from(background) * 3;
        let global_background = if let Some(rgb) = palette.get(index..index + 3) {
            [rgb[0], rgb[1], rgb[2], 255]
        } else {
            [0; 4]
        };
        Self {
            pixels: RgbaImage::new(width, height),
            previous: Vec::new(),
            global_background,
            initialized: false,
        }
    }

    fn draw(
        &mut self,
        frame: &Frame<'_>,
        global_palette: &[u8],
        cancel: &AtomicBool,
    ) -> Result<()> {
        if !self.initialized {
            if frame.transparent.is_none() {
                self.pixels
                    .pixels_mut()
                    .for_each(|p| *p = Rgba(self.global_background));
            }
            self.initialized = true;
        }
        let palette = frame.palette.as_deref().unwrap_or(global_palette);
        self.previous.clear();
        let canvas_width = self.pixels.width() as usize;
        let width = usize::from(frame.width);
        if frame.dispose == DisposalMethod::Previous {
            self.previous.reserve(width * usize::from(frame.height) * 4);
        }
        for row in 0..usize::from(frame.height) {
            check_cancel(cancel)?;
            let start =
                ((usize::from(frame.top) + row) * canvas_width + usize::from(frame.left)) * 4;
            let destination = &mut self.pixels.as_mut()[start..start + width * 4];
            if frame.dispose == DisposalMethod::Previous {
                self.previous.extend_from_slice(destination);
            }
            for (pixel, &index) in destination
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(&frame.buffer[row * width..(row + 1) * width])
            {
                if Some(index) == frame.transparent {
                    continue;
                }
                let rgb = palette
                    .get(usize::from(index) * 3..usize::from(index) * 3 + 3)
                    .ok_or_else(|| {
                        CoreError::Invalid("GIF references an absent palette color".into())
                    })?;
                pixel.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        Ok(())
    }

    fn dispose(&mut self, frame: &Frame<'_>, cancel: &AtomicBool) -> Result<()> {
        if !matches!(
            frame.dispose,
            DisposalMethod::Background | DisposalMethod::Previous
        ) {
            return Ok(());
        }
        let background = if frame.transparent.is_some() {
            [0; 4]
        } else {
            self.global_background
        };
        let canvas_width = self.pixels.width() as usize;
        let width = usize::from(frame.width);
        for row in 0..usize::from(frame.height) {
            check_cancel(cancel)?;
            let start =
                ((usize::from(frame.top) + row) * canvas_width + usize::from(frame.left)) * 4;
            let destination = &mut self.pixels.as_mut()[start..start + width * 4];
            if frame.dispose == DisposalMethod::Previous {
                destination.copy_from_slice(&self.previous[row * width * 4..(row + 1) * width * 4]);
            } else {
                destination
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .for_each(|p| p.copy_from_slice(&background));
            }
        }
        Ok(())
    }
}

/// Explicit caller opt-in is required before using this for animated-to-static
/// conversion. This function itself is also used by bounded first-frame previews.
pub fn decode_first_frame(
    input: &Path,
    memory_limit_bytes: u64,
    cancel: &AtomicBool,
) -> Result<RgbaImage> {
    let mut blocks = Blocks::open(input, cancel)?;
    blocks.working_budget = Some((
        estimate_gif_memory((blocks.info.width, blocks.info.height), (0, 0), false),
        memory_limit_bytes,
    ));
    budget(
        estimate_gif_memory((blocks.info.width, blocks.info.height), (0, 0), false),
        memory_limit_bytes,
    )?;
    while let Record::Image = blocks.next(None, false, false)? {}
    budget(
        estimate_gif_memory((blocks.info.width, blocks.info.height), (0, 0), false)
            .saturating_add(blocks.info.metadata_bytes.saturating_mul(4)),
        memory_limit_bytes,
    )?;
    let mut reader = decoder(input, memory_limit_bytes, cancel)?;
    let mut canvas = Canvas::new(
        blocks.info.width,
        blocks.info.height,
        &blocks.global_palette,
        blocks.header[11],
    );
    let frame = reader
        .read_next_frame()
        .map_err(|e| codec_error(e, cancel))?
        .ok_or_else(|| CoreError::Invalid("GIF contains no frames".into()))?;
    canvas.draw(frame, &blocks.global_palette, cancel)?;
    if let Some(icc) = &blocks.icc_profile {
        convert_color(&mut canvas.pixels, icc, cancel)?;
    }
    check_cancel(cancel)?;
    Ok(canvas.pixels)
}

/// Render one complete playback to bounded preview images. The callback owns
/// only the current preview; returning false stops playback successfully.
/// Delays are reported in milliseconds without browser-specific clamping.
///
/// The caller may start another pass for a deliberately looping preview. This
/// operation never retains a history of frames and does not change the source's
/// finite/infinite loop setting. Preview filtering uses premultiplied RGBA8;
/// final image resizing uses the shared higher-precision processing path.
pub fn preview_frames(
    input: &Path,
    max_edge: u32,
    memory_limit_bytes: u64,
    mut callback: impl FnMut(RgbaImage, u32) -> bool,
) -> Result<()> {
    if max_edge == 0 {
        return Err(CoreError::Invalid("Preview size must be positive".into()));
    }
    let cancel = AtomicBool::new(false);
    let mut blocks = Blocks::open(input, &cancel)?;
    let source = (blocks.info.width, blocks.info.height);
    let scale = (f64::from(max_edge) / f64::from(source.0.max(source.1))).min(1.0);
    let target = (
        (f64::from(source.0) * scale).round().max(1.0) as u32,
        (f64::from(source.1) * scale).round().max(1.0) as u32,
    );
    let estimated = u64::from(source.0) * u64::from(source.1) * 24
        + u64::from(target.0) * u64::from(target.1) * 16
        + 8 * crate::MIB;
    budget(estimated, memory_limit_bytes)?;
    blocks.working_budget = Some((estimated, memory_limit_bytes));
    while let Record::Image = blocks.next(None, false, false)? {}
    budget(
        estimated.saturating_add(blocks.info.metadata_bytes.saturating_mul(4)),
        memory_limit_bytes,
    )?;
    if blocks.info.frames == 0 {
        return Err(CoreError::Invalid("GIF contains no image frames".into()));
    }
    let mut reader = decoder(input, memory_limit_bytes, &cancel)?;
    let mut canvas = Canvas::new(
        source.0,
        source.1,
        &blocks.global_palette,
        blocks.header[11],
    );
    let mut resizer = fast_image_resize::Resizer::new();
    while let Some(frame) = reader
        .read_next_frame()
        .map_err(|e| codec_error(e, &cancel))?
    {
        canvas.draw(frame, &blocks.global_palette, &cancel)?;
        let mut pixels = canvas.pixels.clone();
        if let Some(icc) = &blocks.icc_profile {
            convert_color(&mut pixels, icc, &cancel)?;
        }
        if source != target {
            let src = DynamicImage::ImageRgba8(pixels);
            let mut dst = DynamicImage::new_rgba8(target.0, target.1);
            resizer
                .resize(&src, &mut dst, None)
                .map_err(|e| codec_error(e, &cancel))?;
            pixels = dst.into_rgba8();
        }
        if !callback(pixels, u32::from(frame.delay) * 10) {
            return Ok(());
        }
        canvas.dispose(frame, &cancel)?;
    }
    Ok(())
}

fn convert_color(pixels: &mut RgbaImage, icc: &[u8], cancel: &AtomicBool) -> Result<()> {
    let width = pixels.width();
    crate::color::rgba8_to_srgb(pixels.as_mut(), width, icc, &|| {
        cancel.load(Ordering::Relaxed)
    })
    .map_err(|e| codec_error(e, cancel))
}

/// Quantization with a reserved transparent entry: no opaque pixel can become
/// transparent, even when NeuQuant sees more than 256 source colors.
fn quantize(mut pixels: RgbaImage, matte: [u8; 3], cancel: &AtomicBool) -> Result<Frame<'static>> {
    let (width, height) = dimensions(pixels.width(), pixels.height())?;
    let mut transparent = false;
    let mut colors = BTreeMap::<[u8; 3], u8>::new();
    let mut too_many = false;
    for row in pixels.rows_mut() {
        check_cancel(cancel)?;
        for pixel in row {
            let alpha = u32::from(pixel[3]);
            if alpha == 0 {
                transparent = true;
                pixel.0 = [0; 4];
                continue;
            }
            for channel in 0..3 {
                pixel[channel] = ((u32::from(pixel[channel]) * alpha
                    + u32::from(matte[channel]) * (255 - alpha)
                    + 127)
                    / 255) as u8;
            }
            pixel[3] = 255;
            if !too_many {
                let rgb = [pixel[0], pixel[1], pixel[2]];
                let next = colors.len();
                if next < 256 {
                    colors.entry(rgb).or_insert(next as u8);
                } else if !colors.contains_key(&rgb) {
                    too_many = true;
                }
            }
        }
    }
    let count = 256 - usize::from(transparent);
    let mut indices = Vec::with_capacity(usize::from(width) * usize::from(height));
    let mut palette;
    if !too_many && colors.len() <= count {
        palette = vec![0; (colors.len() + usize::from(transparent)).max(2) * 3];
        for (rgb, &index) in &colors {
            palette[usize::from(index) * 3..usize::from(index) * 3 + 3].copy_from_slice(rgb);
        }
        // Use slot 255 uniformly, including exact-palette paths.
        if transparent {
            palette.resize(768, 0);
        }
        for row in pixels.rows() {
            check_cancel(cancel)?;
            for pixel in row {
                indices.push(if pixel[3] == 0 {
                    255
                } else {
                    colors[&[pixel[0], pixel[1], pixel[2]]]
                });
            }
        }
    } else {
        let quantizer = color_quant::NeuQuant::new(10, count, pixels.as_raw());
        check_cancel(cancel)?;
        palette = quantizer.color_map_rgb();
        if transparent {
            palette.resize(768, 0);
        }
        for row in pixels.rows() {
            check_cancel(cancel)?;
            for pixel in row {
                indices.push(if pixel[3] == 0 {
                    255
                } else {
                    quantizer.index_of(&pixel.0) as u8
                });
            }
        }
    }
    Ok(Frame {
        width,
        height,
        buffer: Cow::Owned(indices),
        palette: Some(palette),
        transparent: transparent.then_some(255),
        dispose: DisposalMethod::Background,
        ..Frame::default()
    })
}

pub fn encode_static_gif(
    image: &RgbaImage,
    output: &Path,
    options: &GifOptions,
    cancel: &AtomicBool,
) -> Result<GifSummary> {
    check_cancel(cancel)?;
    let (width, height) = dimensions(image.width(), image.height())?;
    budget(
        estimate_gif_memory((0, 0), image.dimensions(), true),
        options.memory_limit_bytes,
    )?;
    let frame = quantize(image.clone(), options.matte, cancel)?;
    let file = File::create(output)?;
    let writer = HeaderWriter {
        inner: BufWriter::new(file),
        offset: 0,
        background: 0,
        aspect: 0,
        color_flags: 0,
        no_global_palette: true,
        cancel,
    };
    let mut encoder =
        Encoder::new(writer, width, height, &[]).map_err(|e| codec_error(e, cancel))?;
    encoder
        .write_frame(&frame)
        .map_err(|e| codec_error(e, cancel))?;
    let mut writer = encoder.into_inner().map_err(|e| codec_error(e, cancel))?;
    writer.flush().map_err(|e| codec_error(e, cancel))?;
    check_cancel(cancel)?;
    Ok(GifSummary { width: width.into(), height: height.into(), frames: 1, warnings: vec!["Creating GIF uses at most 256 colors and binary transparency; partial alpha is composited over the selected matte.".into()], ..Default::default() })
}

pub fn transcode_gif(
    input: &Path,
    output: &Path,
    options: &GifOptions,
    cancel: &AtomicBool,
) -> Result<GifSummary> {
    let result = transcode_inner(input, output, options, cancel);
    if cancel.load(Ordering::Relaxed) {
        Err(CoreError::Cancelled)
    } else {
        result
    }
}

fn transcode_inner(
    input: &Path,
    output: &Path,
    options: &GifOptions,
    cancel: &AtomicBool,
) -> Result<GifSummary> {
    check_cancel(cancel)?;
    let mut blocks = Blocks::open(input, cancel)?;
    let original = (blocks.info.width, blocks.info.height);
    let target = options.resize.unwrap_or(original);
    let (width, height) = dimensions(target.0, target.1)?;
    let transformed = target != original;
    blocks.working_budget = Some((
        estimate_gif_memory(original, target, transformed),
        options.memory_limit_bytes,
    ));
    budget(
        estimate_gif_memory(original, target, transformed),
        options.memory_limit_bytes,
    )?;
    // Read metadata without decoding pixels, including legal late extensions.
    while let Record::Image = blocks.next(None, false, false)? {}
    budget(
        estimate_gif_memory(original, target, transformed)
            .saturating_add(blocks.info.metadata_bytes.saturating_mul(4)),
        options.memory_limit_bytes,
    )?;
    let icc_profile = if transformed {
        blocks.icc_profile.take()
    } else {
        None
    };
    let had_metadata = blocks.info.has_metadata;
    blocks = Blocks::open(input, cancel)?;
    blocks.capture_icc = false;
    let mut reader = decoder(input, options.memory_limit_bytes, cancel)?;
    let writer = HeaderWriter {
        inner: BufWriter::new(File::create(output)?),
        offset: 0,
        background: if transformed { 0 } else { blocks.header[11] },
        aspect: blocks.header[12],
        color_flags: blocks.header[10],
        no_global_palette: transformed || blocks.global_palette.is_empty(),
        cancel,
    };
    let palette = if transformed {
        &[][..]
    } else {
        blocks.global_palette.as_slice()
    };
    let mut encoder =
        Encoder::new(writer, width, height, palette).map_err(|e| codec_error(e, cancel))?;
    let mut canvas = transformed.then(|| {
        Canvas::new(
            original.0,
            original.1,
            &blocks.global_palette,
            blocks.header[11],
        )
    });
    let mut summary = GifSummary {
        width: target.0,
        height: target.1,
        ..Default::default()
    };
    while let Record::Image = blocks.next(
        Some(encoder.get_mut()),
        options.preserve_metadata,
        transformed,
    )? {
        check_cancel(cancel)?;
        let frame = reader
            .read_next_frame()
            .map_err(|e| codec_error(e, cancel))?
            .ok_or_else(|| CoreError::Invalid("GIF image-block/frame count differs".into()))?;
        summary.frames += 1;
        summary.duration_cs = summary
            .duration_cs
            .checked_add(u64::from(frame.delay))
            .ok_or_else(|| CoreError::Invalid("GIF duration overflow".into()))?;
        if let Some(canvas) = &mut canvas {
            canvas.draw(frame, &blocks.global_palette, cancel)?;
            let mut rendered = canvas.pixels.clone();
            if let Some(icc) = &icc_profile {
                convert_color(&mut rendered, icc, cancel)?;
            }
            let resized = crate::pixels::resize_image(
                DynamicImage::ImageRgba8(rendered),
                target.0,
                target.1,
                cancel,
            )?
            .into_rgba8();
            let mut encoded = quantize(resized, options.matte, cancel)?;
            encoded.delay = frame.delay;
            encoded.needs_user_input = frame.needs_user_input;
            encoder
                .write_frame(&encoded)
                .map_err(|e| codec_error(e, cancel))?;
            canvas.dispose(frame, cancel)?;
        } else {
            let palette = frame.palette.as_deref().unwrap_or(&blocks.global_palette);
            for row in frame.buffer.chunks(usize::from(frame.width)) {
                check_cancel(cancel)?;
                if row.iter().any(|&index| {
                    Some(index) != frame.transparent && usize::from(index) >= palette.len() / 3
                }) {
                    return Err(CoreError::Invalid(
                        "GIF references an absent palette color".into(),
                    ));
                }
            }
            encoder
                .write_frame(frame)
                .map_err(|e| codec_error(e, cancel))?;
        }
    }
    if summary.frames == 0 {
        return Err(CoreError::Invalid("GIF contains no frames".into()));
    }
    // Finish decoding to catch a malformed final image and late decoder errors.
    if reader
        .read_next_frame()
        .map_err(|e| codec_error(e, cancel))?
        .is_some()
    {
        return Err(CoreError::Invalid(
            "GIF has unexpected trailing frame".into(),
        ));
    }
    let mut writer = encoder.into_inner().map_err(|e| codec_error(e, cancel))?;
    writer.flush().map_err(|e| codec_error(e, cancel))?;
    check_cancel(cancel)?;
    summary.repeat = blocks.info.repeat;
    if transformed {
        summary.warnings.push("GIF resizing requires palette quantization and binary transparency; frame delays and looping are preserved.".into());
    }
    if transformed && options.preserve_metadata && had_metadata {
        summary.warnings.push("Ancillary GIF metadata is omitted after resizing; embedded color is converted to sRGB.".into());
    }
    Ok(summary)
}
