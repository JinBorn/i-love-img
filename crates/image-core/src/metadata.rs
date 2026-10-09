//! Bounded metadata parsing and explicit JPEG/PNG/WebP container adaptation.
//!
//! EXIF values are TIFF bytes, without JPEG's `Exif\0\0` wrapper. Pixel-changing
//! paths must set both `orientation_normalized` and `converted_to_srgb` only
//! after doing that work. This prevents stale ICC profiles/orientations from
//! being attached to newly encoded pixels.

use image::ImageFormat;
use std::io::{Read, Write};

pub const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const XMP_JPEG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
// Also bound zero-length block bookkeeping: payload-byte limits alone do not
// constrain a malicious container containing millions of empty metadata blocks.
const MAX_CONTAINER_BLOCKS: usize = 65_536;

#[derive(Clone, Debug, Default)]
pub struct ImageMetadata {
    pub source_format: Option<ImageFormat>,
    pub icc_profile: Option<Vec<u8>>,
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    /// EXIF values 1..=8; zero means absent and is treated as 1.
    pub orientation: u8,
    pub jpeg_components: Option<u8>,
    pub adobe_inverted: bool,
    /// PNG gAMA value, expressed as the file gamma (e.g. 0.45455).
    pub png_gamma: Option<f64>,
    /// PNG cHRM white, red, green, blue x/y coordinates.
    pub png_chromaticities: Option<[f64; 8]>,
    pub png_srgb: bool,
    pub png_cicp: Option<[u8; 4]>,
    pub hdr: bool,
    pub warnings: Vec<String>,
    display_chunks: Vec<([u8; 4], Vec<u8>)>,
    native: Vec<NativeMetadata>,
}

#[derive(Clone, Debug)]
enum NativeMetadata {
    Jpeg(u8, Vec<u8>),
    Png([u8; 4], Vec<u8>),
}

#[derive(Clone, Debug)]
pub struct MetadataPolicy {
    pub keep_metadata: bool,
    pub orientation_normalized: bool,
    pub converted_to_srgb: bool,
    pub dimensions: (u32, u32),
    /// True for resizing, format conversion or any changed pixel encoding.
    /// Conservative removal of stale XMP/thumbnails is reported to the caller.
    pub pixels_changed: bool,
}

fn err(s: &str) -> String {
    format!("Invalid image metadata: {s}")
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().unwrap())
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b.try_into().unwrap())
}

fn bounded_copy(data: &[u8], used: &mut usize) -> Result<Vec<u8>, String> {
    *used = used
        .checked_add(data.len())
        .ok_or_else(|| err("metadata size overflow"))?;
    if *used > MAX_METADATA_BYTES {
        return Err(err("metadata exceeds 16 MiB limit"));
    }
    Ok(data.to_vec())
}

/// Extract only bounded metadata; never decode pixel buffers.
pub fn read_metadata(bytes: &[u8], format: ImageFormat) -> Result<ImageMetadata, String> {
    let mut m = ImageMetadata {
        source_format: Some(format),
        orientation: 1,
        ..Default::default()
    };
    let mut used = 0;
    match format {
        ImageFormat::Jpeg => {
            let mut icc_parts = Vec::new();
            for s in jpeg_segments(bytes)? {
                let p = &bytes[s.payload..s.end];
                match s.marker {
                    0xe1 if p.starts_with(b"Exif\0\0") => {
                        if m.exif.is_some() {
                            return Err(err("multiple EXIF blocks"));
                        }
                        m.exif = Some(bounded_copy(&p[6..], &mut used)?);
                    }
                    0xe1 if p.starts_with(XMP_JPEG) => {
                        m.xmp = Some(bounded_copy(&p[XMP_JPEG.len()..], &mut used)?)
                    }
                    0xe2 if p.starts_with(b"ICC_PROFILE\0") => {
                        if p.len() < 14 {
                            return Err(err("short JPEG ICC segment"));
                        }
                        icc_parts.push((p[12], p[13], bounded_copy(&p[14..], &mut used)?));
                    }
                    0xee if p.starts_with(b"Adobe") => {
                        if p.len() < 12 {
                            return Err(err("short Adobe JPEG color marker"));
                        }
                        m.adobe_inverted = true;
                    }
                    0xe2 if p.starts_with(b"MPF\0") => m.hdr = true,
                    0xe0..=0xef | 0xfe => {
                        if s.marker != 0xe0 && s.marker != 0xee {
                            m.native
                                .push(NativeMetadata::Jpeg(s.marker, bounded_copy(p, &mut used)?));
                        }
                    }
                    0xc0..=0xcf if !matches!(s.marker, 0xc4 | 0xc8 | 0xcc) => {
                        if p.len() < 6 {
                            return Err(err("short JPEG frame header"));
                        }
                        m.jpeg_components = Some(p[5]);
                    }
                    _ => {}
                }
            }
            if !icc_parts.is_empty() {
                icc_parts.sort_by_key(|p| p.0);
                let count = icc_parts[0].1 as usize;
                if count == 0
                    || count != icc_parts.len()
                    || icc_parts
                        .iter()
                        .enumerate()
                        .any(|(i, p)| p.0 as usize != i + 1 || p.1 as usize != count)
                {
                    return Err(err("incomplete or duplicated JPEG ICC segments"));
                }
                m.icc_profile = Some(icc_parts.into_iter().flat_map(|p| p.2).collect());
            }
        }
        ImageFormat::Png => {
            for c in png_chunks(bytes)? {
                let p = &bytes[c.payload..c.end - 4];
                match &c.kind {
                    b"iCCP" => {
                        if m.icc_profile.is_some() {
                            return Err(err("multiple PNG ICC profiles"));
                        }
                        let end_name = p
                            .iter()
                            .position(|&v| v == 0)
                            .ok_or_else(|| err("invalid PNG ICC name"))?;
                        if end_name == 0 || end_name > 79 || p.get(end_name + 1) != Some(&0) {
                            return Err(err("invalid PNG ICC compression"));
                        }
                        let mut decoded = Vec::new();
                        flate2::read::ZlibDecoder::new(&p[end_name + 2..])
                            .take((MAX_METADATA_BYTES + 1) as u64)
                            .read_to_end(&mut decoded)
                            .map_err(|e| err(&e.to_string()))?;
                        m.icc_profile = Some(bounded_copy(&decoded, &mut used)?);
                    }
                    b"eXIf" => {
                        if m.exif.is_some() {
                            return Err(err("multiple EXIF blocks"));
                        }
                        m.exif = Some(bounded_copy(p, &mut used)?);
                    }
                    b"sRGB" if p.len() == 1 && p[0] <= 3 => {
                        m.png_srgb = true;
                        m.display_chunks.push((c.kind, bounded_copy(p, &mut used)?));
                    }
                    b"gAMA" if p.len() == 4 => {
                        m.png_gamma = Some(be32(p) as f64 / 100_000.0);
                        m.display_chunks.push((c.kind, bounded_copy(p, &mut used)?));
                    }
                    b"cHRM" if p.len() == 32 => {
                        let mut xy = [0.; 8];
                        for (v, b) in xy.iter_mut().zip(p.as_chunks::<4>().0.iter()) {
                            *v = be32(b) as f64 / 100_000.0;
                        }
                        m.png_chromaticities = Some(xy);
                        m.display_chunks.push((c.kind, bounded_copy(p, &mut used)?));
                    }
                    b"cICP" if p.len() == 4 => {
                        m.png_cicp = Some(p.try_into().unwrap());
                        m.hdr |= matches!(p[1], 16 | 18);
                        m.display_chunks.push((c.kind, bounded_copy(p, &mut used)?));
                    }
                    b"sRGB" | b"gAMA" | b"cHRM" | b"cICP" => {
                        return Err(err("invalid PNG color metadata chunk"));
                    }
                    b"mDCv" | b"cLLI" => m.hdr = true,
                    b"sBIT" => m.display_chunks.push((c.kind, bounded_copy(p, &mut used)?)),
                    b"acTL" => return Err("Animated PNG is not supported".into()),
                    b"iTXt" if p.starts_with(b"XML:com.adobe.xmp\0") => {
                        m.xmp = Some(read_png_xmp(p, &mut used)?);
                    }
                    b"tEXt" | b"zTXt" | b"iTXt" | b"pHYs" | b"tIME" => m
                        .native
                        .push(NativeMetadata::Png(c.kind, bounded_copy(p, &mut used)?)),
                    _ => {}
                }
            }
        }
        ImageFormat::WebP => {
            for c in webp_chunks(bytes)? {
                let p = &bytes[c.payload..c.payload + c.len];
                match &c.kind {
                    b"VP8X" if p.len() != 10 => return Err(err("invalid WebP extended header")),
                    b"VP8X" if p[0] & 0x02 != 0 => {
                        return Err("Animated WebP is not supported".into());
                    }
                    b"ICCP" => {
                        if m.icc_profile.is_some() {
                            return Err(err("multiple WebP ICC profiles"));
                        }
                        m.icc_profile = Some(bounded_copy(p, &mut used)?);
                    }
                    b"EXIF" => {
                        if m.exif.is_some() {
                            return Err(err("multiple EXIF blocks"));
                        }
                        m.exif = Some(bounded_copy(
                            p.strip_prefix(b"Exif\0\0").unwrap_or(p),
                            &mut used,
                        )?);
                    }
                    b"XMP " => m.xmp = Some(bounded_copy(p, &mut used)?),
                    b"ANIM" | b"ANMF" => return Err("Animated WebP is not supported".into()),
                    _ => {}
                }
            }
        }
        ImageFormat::Gif => {}
        _ => return Err("Metadata adapter does not support this format".into()),
    }
    if let Some(exif) = &m.exif {
        m.orientation = exif_orientation(exif)?;
    }
    if m.hdr {
        return Err("HDR / multi-picture images are not supported".into());
    }
    if let Some(icc) = &m.icc_profile {
        crate::color::validate_sdr_profile(icc)?;
    }
    Ok(m)
}

/// Update metadata on an already encoded output. No source APP/ancillary data
/// are blindly copied: private data are opt-in and display metadata are explicit.
pub fn rewrite_metadata(
    encoded: &[u8],
    format: ImageFormat,
    source: &ImageMetadata,
    policy: &MetadataPolicy,
) -> Result<(Vec<u8>, Vec<String>), String> {
    let mut warnings = source.warnings.clone();
    let icc = if policy.converted_to_srgb {
        Some(crate::color::srgb_profile_bytes()?)
    } else {
        source.icc_profile.clone()
    };
    let orientation = if policy.orientation_normalized {
        1
    } else {
        source.orientation.max(1)
    };
    let mut exif = if policy.keep_metadata {
        match &source.exif {
            Some(raw) => Some(update_exif(raw, policy, orientation)?),
            None if orientation != 1 => Some(minimal_exif(orientation)),
            None => None,
        }
    } else if orientation != 1 {
        Some(minimal_exif(orientation))
    } else {
        None
    };
    let mut xmp = if policy.keep_metadata
        && !policy.pixels_changed
        && !policy.converted_to_srgb
        && !policy.orientation_normalized
    {
        source.xmp.as_deref()
    } else {
        if policy.keep_metadata && source.xmp.is_some() {
            warnings.push(
                "XMP was omitted because its geometry/color fields cannot safely be updated".into(),
            );
        }
        None
    };
    if format == ImageFormat::Jpeg {
        if exif.as_ref().is_some_and(|p| p.len() + 6 > 65_533) {
            warnings.push(
                "EXIF exceeds the JPEG segment limit; only required orientation was retained"
                    .into(),
            );
            exif = if orientation != 1 {
                Some(minimal_exif(orientation))
            } else {
                None
            };
        }
        if xmp.is_some_and(|p| p.len() + XMP_JPEG.len() > 65_533) {
            warnings.push("XMP exceeds the JPEG segment limit and was omitted".into());
            xmp = None;
        }
    }
    let native_ok =
        policy.keep_metadata && !policy.pixels_changed && source.source_format == Some(format);
    if policy.keep_metadata && !source.native.is_empty() && !native_ok {
        warnings
            .push("Format-specific metadata that cannot be safely transferred were omitted".into());
    }
    let output = match format {
        ImageFormat::Jpeg => rewrite_jpeg(
            encoded,
            icc.as_deref(),
            exif.as_deref(),
            xmp,
            source,
            native_ok,
        )?,
        ImageFormat::Png => rewrite_png(
            encoded,
            icc.as_deref(),
            exif.as_deref(),
            xmp,
            source,
            policy,
            native_ok,
        )?,
        ImageFormat::WebP => rewrite_webp(
            encoded,
            icc.as_deref(),
            exif.as_deref(),
            xmp,
            policy.dimensions,
        )?,
        ImageFormat::Gif => {
            if icc.is_some() || exif.is_some() || xmp.is_some() {
                warnings.push("GIF cannot preserve ICC/EXIF/XMP metadata".into());
            }
            encoded.to_vec()
        }
        _ => return Err("Unsupported metadata output format".into()),
    };
    Ok((output, warnings))
}

#[derive(Clone, Copy)]
struct JpegSegment {
    marker: u8,
    start: usize,
    payload: usize,
    end: usize,
}

/// Walk all scans, including metadata between scans. Stopping at the first SOS
/// would allow EXIF after that scan to survive privacy stripping.
fn jpeg_segments(data: &[u8]) -> Result<Vec<JpegSegment>, String> {
    if !data.starts_with(&[0xff, 0xd8]) {
        return Err(err("JPEG signature"));
    }
    let mut segments = Vec::new();
    let mut p = 2;
    while p < data.len() {
        if segments.len() >= MAX_CONTAINER_BLOCKS {
            return Err(err("too many JPEG segments"));
        }
        let start = p;
        if data[p] != 0xff {
            return Err(err("JPEG marker"));
        }
        while data.get(p) == Some(&0xff) {
            p += 1;
        }
        let marker = *data.get(p).ok_or_else(|| err("truncated JPEG marker"))?;
        p += 1;
        if marker == 0xd9 {
            segments.push(JpegSegment {
                marker,
                start,
                payload: p,
                end: p,
            });
            return Ok(segments);
        }
        if matches!(marker, 0x00 | 0xd8 | 0xd0..=0xd7) {
            return Err(err("unexpected JPEG marker"));
        }
        if marker == 0x01 {
            segments.push(JpegSegment {
                marker,
                start,
                payload: p,
                end: p,
            });
            continue;
        }
        let len_bytes = data
            .get(p..p + 2)
            .ok_or_else(|| err("truncated JPEG length"))?;
        let len = u16::from_be_bytes(len_bytes.try_into().unwrap()) as usize;
        if len < 2 {
            return Err(err("JPEG segment length"));
        }
        let payload = p + 2;
        p = p
            .checked_add(len)
            .filter(|&v| v <= data.len())
            .ok_or_else(|| err("JPEG segment out of range"))?;
        if marker == 0xda {
            while p < data.len() {
                if data[p] != 0xff {
                    p += 1;
                    continue;
                }
                let mut q = p + 1;
                while data.get(q) == Some(&0xff) {
                    q += 1;
                }
                let next = *data.get(q).ok_or_else(|| err("truncated JPEG scan"))?;
                if next == 0 || (0xd0..=0xd7).contains(&next) {
                    p = q + 1;
                } else {
                    break;
                }
            }
        }
        segments.push(JpegSegment {
            marker,
            start,
            payload,
            end: p,
        });
    }
    Err(err("JPEG end marker missing"))
}

fn jpeg_app(out: &mut Vec<u8>, marker: u8, payload: &[u8]) -> Result<(), String> {
    let size = u16::try_from(payload.len() + 2)
        .map_err(|_| err("metadata too large for JPEG APP segment"))?;
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

fn rewrite_jpeg(
    data: &[u8],
    icc: Option<&[u8]>,
    exif: Option<&[u8]>,
    xmp: Option<&[u8]>,
    source: &ImageMetadata,
    native_ok: bool,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&[0xff, 0xd8]);
    let segments = jpeg_segments(data)?;
    // A JFIF APP0 header belongs immediately after SOI. Keep the encoder's
    // current color-space hints ahead of our inserted metadata.
    for s in &segments {
        let p = &data[s.payload..s.end];
        if s.marker == 0xe0 && p.starts_with(b"JFIF\0") && p.len() >= 14 {
            let mut jfif = p[..14].to_vec();
            jfif[12] = 0;
            jfif[13] = 0;
            jpeg_app(&mut out, 0xe0, &jfif)?;
        } else if s.marker == 0xee && p.starts_with(b"Adobe") {
            if p.len() < 12 {
                return Err(err("short Adobe JPEG color marker"));
            }
            // Only the 12 standard bytes affect color interpretation. Keeping
            // an arbitrary trailing payload would bypass privacy stripping.
            jpeg_app(&mut out, 0xee, &p[..12])?;
        }
    }
    if let Some(icc) = icc {
        let count = icc.len().div_ceil(65_519);
        if count > 255 {
            return Err(err("ICC profile too large for JPEG"));
        }
        for (i, part) in icc.chunks(65_519).enumerate() {
            let mut payload = b"ICC_PROFILE\0".to_vec();
            payload.extend_from_slice(&[(i + 1) as u8, count as u8]);
            payload.extend_from_slice(part);
            jpeg_app(&mut out, 0xe2, &payload)?;
        }
    }
    if let Some(exif) = exif {
        let mut p = b"Exif\0\0".to_vec();
        p.extend_from_slice(exif);
        jpeg_app(&mut out, 0xe1, &p)?;
    }
    if let Some(xmp) = xmp {
        let mut p = XMP_JPEG.to_vec();
        p.extend_from_slice(xmp);
        jpeg_app(&mut out, 0xe1, &p)?;
    }
    if native_ok {
        for n in &source.native {
            if let NativeMetadata::Jpeg(marker, payload) = n {
                jpeg_app(&mut out, *marker, payload)?;
            }
        }
    }
    for s in segments {
        match s.marker {
            0xe0..=0xef | 0xfe => {}
            _ => out.extend_from_slice(&data[s.start..s.end]),
        }
    }
    Ok(out)
}

struct PngChunk {
    kind: [u8; 4],
    start: usize,
    payload: usize,
    end: usize,
}

fn read_png_xmp(payload: &[u8], used: &mut usize) -> Result<Vec<u8>, String> {
    let mut p = b"XML:com.adobe.xmp\0".len();
    let flag = *payload.get(p).ok_or_else(|| err("truncated PNG XMP"))?;
    let method = *payload.get(p + 1).ok_or_else(|| err("truncated PNG XMP"))?;
    p += 2;
    if flag > 1 || method != 0 {
        return Err(err("invalid PNG XMP compression"));
    }
    for _ in 0..2 {
        let len = payload
            .get(p..)
            .and_then(|s| s.iter().position(|&c| c == 0))
            .ok_or_else(|| err("invalid PNG XMP text header"))?;
        p += len + 1;
    }
    if flag == 0 {
        bounded_copy(&payload[p..], used)
    } else {
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(&payload[p..])
            .take((MAX_METADATA_BYTES + 1) as u64)
            .read_to_end(&mut decoded)
            .map_err(|e| err(&e.to_string()))?;
        bounded_copy(&decoded, used)
    }
}
fn png_chunks(data: &[u8]) -> Result<Vec<PngChunk>, String> {
    if !data.starts_with(PNG_SIGNATURE) {
        return Err(err("PNG signature"));
    }
    let mut p = 8;
    let mut chunks = Vec::new();
    while p < data.len() {
        if chunks.len() >= MAX_CONTAINER_BLOCKS {
            return Err(err("too many PNG chunks"));
        }
        let h = data
            .get(p..p + 8)
            .ok_or_else(|| err("truncated PNG chunk"))?;
        let len = be32(&h[..4]) as usize;
        let end = p
            .checked_add(12)
            .and_then(|n| n.checked_add(len))
            .filter(|&v| v <= data.len())
            .ok_or_else(|| err("PNG chunk out of range"))?;
        let kind: [u8; 4] = h[4..].try_into().unwrap();
        if crc32fast::hash(&data[p + 4..end - 4]) != be32(&data[end - 4..end]) {
            return Err(err("PNG chunk CRC mismatch"));
        }
        chunks.push(PngChunk {
            kind,
            start: p,
            payload: p + 8,
            end,
        });
        p = end;
        if kind == *b"IEND" {
            return Ok(chunks);
        }
    }
    Err(err("PNG end chunk missing"))
}
fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) -> Result<(), String> {
    let len = u32::try_from(payload.len()).map_err(|_| err("PNG chunk too large"))?;
    out.extend_from_slice(&len.to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    let crc = crc32fast::hash(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
    Ok(())
}
fn rewrite_png(
    data: &[u8],
    icc: Option<&[u8]>,
    exif: Option<&[u8]>,
    xmp: Option<&[u8]>,
    source: &ImageMetadata,
    policy: &MetadataPolicy,
    native_ok: bool,
) -> Result<Vec<u8>, String> {
    let mut out = PNG_SIGNATURE.to_vec();
    for c in png_chunks(data)? {
        match &c.kind {
            b"IHDR" => {
                out.extend_from_slice(&data[c.start..c.end]);
                if policy.converted_to_srgb {
                    // The sRGB chunk describes RGB and gray outputs without an RGB-only ICC mismatch.
                    png_chunk(&mut out, b"sRGB", &[0])?;
                } else {
                    if let Some(icc) = icc {
                        let mut z = flate2::write::ZlibEncoder::new(
                            Vec::new(),
                            flate2::Compression::fast(),
                        );
                        z.write_all(icc).map_err(|e| e.to_string())?;
                        let compressed = z.finish().map_err(|e| e.to_string())?;
                        let mut p = b"ICC\0\0".to_vec();
                        p.extend_from_slice(&compressed);
                        png_chunk(&mut out, b"iCCP", &p)?;
                    }
                    for (kind, p) in &source.display_chunks {
                        if *kind == *b"sRGB" && icc.is_some() {
                            continue;
                        }
                        png_chunk(&mut out, kind, p)?;
                    }
                }
                if let Some(exif) = exif {
                    png_chunk(&mut out, b"eXIf", exif)?;
                }
                if let Some(xmp) = xmp {
                    let mut p = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec();
                    p.extend_from_slice(xmp);
                    png_chunk(&mut out, b"iTXt", &p)?;
                }
                if native_ok {
                    for n in &source.native {
                        if let NativeMetadata::Png(k, p) = n {
                            png_chunk(&mut out, k, p)?;
                        }
                    }
                }
            }
            b"PLTE" | b"tRNS" | b"IDAT" | b"IEND" => out.extend_from_slice(&data[c.start..c.end]),
            // All old metadata, including unknown private chunks, are intentionally filtered.
            _ => {}
        }
    }
    Ok(out)
}

struct WebpChunk {
    kind: [u8; 4],
    payload: usize,
    len: usize,
    start: usize,
    end: usize,
}
fn webp_chunks(data: &[u8]) -> Result<Vec<WebpChunk>, String> {
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return Err(err("WebP signature"));
    }
    let size = le32(&data[4..8]) as usize;
    let limit = size
        .checked_add(8)
        .filter(|&v| v <= data.len())
        .ok_or_else(|| err("WebP RIFF size"))?;
    let mut p = 12;
    let mut chunks = Vec::new();
    while p < limit {
        if chunks.len() >= MAX_CONTAINER_BLOCKS {
            return Err(err("too many WebP chunks"));
        }
        let h = data
            .get(p..p + 8)
            .filter(|_| p + 8 <= limit)
            .ok_or_else(|| err("truncated WebP chunk"))?;
        let len = le32(&h[4..]) as usize;
        let end = p
            .checked_add(8)
            .and_then(|n| n.checked_add(len))
            .and_then(|n| n.checked_add(len & 1))
            .filter(|&v| v <= limit)
            .ok_or_else(|| err("WebP chunk out of range"))?;
        chunks.push(WebpChunk {
            kind: h[..4].try_into().unwrap(),
            payload: p + 8,
            len,
            start: p,
            end,
        });
        p = end;
    }
    Ok(chunks)
}
fn webp_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) -> Result<(), String> {
    out.extend_from_slice(kind);
    out.extend_from_slice(
        &u32::try_from(payload.len())
            .map_err(|_| err("WebP metadata too large"))?
            .to_le_bytes(),
    );
    out.extend_from_slice(payload);
    if payload.len() & 1 != 0 {
        out.push(0);
    }
    Ok(())
}
fn rewrite_webp(
    data: &[u8],
    icc: Option<&[u8]>,
    exif: Option<&[u8]>,
    xmp: Option<&[u8]>,
    dimensions: (u32, u32),
) -> Result<Vec<u8>, String> {
    let chunks = webp_chunks(data)?;
    let mut out = b"RIFF\0\0\0\0WEBP".to_vec();
    let mut flags = 0u8;
    for c in &chunks {
        match &c.kind {
            b"VP8X" if c.len == 10 => flags = data[c.payload] & 0x12,
            b"ALPH" => flags |= 0x10,
            b"VP8L" if c.len >= 5 => {
                if data[c.payload + 4] & 0x10 != 0 {
                    flags |= 0x10;
                }
            }
            b"ANIM" | b"ANMF" => return Err("Animated WebP is not supported".into()),
            _ => {}
        }
    }
    if icc.is_some() {
        flags |= 0x20;
    }
    if exif.is_some() {
        flags |= 0x08;
    }
    if xmp.is_some() {
        flags |= 0x04;
    }
    if flags != 0 {
        let (w, h) = dimensions;
        if w == 0 || h == 0 || w > 1 << 24 || h > 1 << 24 {
            return Err(err("WebP canvas dimensions"));
        }
        let mut p = vec![flags, 0, 0, 0];
        p.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
        p.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
        webp_chunk(&mut out, b"VP8X", &p)?;
    }
    if let Some(icc) = icc {
        webp_chunk(&mut out, b"ICCP", icc)?;
    }
    for c in chunks {
        if matches!(&c.kind, b"ALPH" | b"VP8 " | b"VP8L") {
            out.extend_from_slice(&data[c.start..c.end]);
        }
    }
    if let Some(exif) = exif {
        webp_chunk(&mut out, b"EXIF", exif)?;
    }
    if let Some(xmp) = xmp {
        webp_chunk(&mut out, b"XMP ", xmp)?;
    }
    let size = u32::try_from(out.len() - 8).map_err(|_| err("WebP RIFF too large"))?;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Ok(out)
}

#[derive(Clone, Copy)]
struct Tiff {
    little: bool,
}
impl Tiff {
    fn new(b: &[u8]) -> Result<Self, String> {
        match b.get(..4) {
            Some(b"II\x2a\0") => Ok(Self { little: true }),
            Some(b"MM\0\x2a") => Ok(Self { little: false }),
            _ => Err(err("EXIF TIFF header")),
        }
    }
    fn u16(self, b: &[u8], p: usize) -> Result<u16, String> {
        let a = b
            .get(p..p + 2)
            .ok_or_else(|| err("EXIF offset"))?
            .try_into()
            .unwrap();
        Ok(if self.little {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        })
    }
    fn u32(self, b: &[u8], p: usize) -> Result<u32, String> {
        let a = b
            .get(p..p + 4)
            .ok_or_else(|| err("EXIF offset"))?
            .try_into()
            .unwrap();
        Ok(if self.little {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }
    fn write16(self, b: &mut [u8], p: usize, v: u16) {
        let v = if self.little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        b[p..p + 2].copy_from_slice(&v);
    }
    fn write32(self, b: &mut [u8], p: usize, v: u32) {
        let v = if self.little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        b[p..p + 4].copy_from_slice(&v);
    }
    fn entries(self, b: &[u8], ifd: usize) -> Result<(Vec<usize>, usize), String> {
        let n = self.u16(b, ifd)? as usize;
        let end = ifd
            .checked_add(2)
            .and_then(|v| v.checked_add(n * 12))
            .ok_or_else(|| err("EXIF directory overflow"))?;
        if end.checked_add(4).filter(|&v| v <= b.len()).is_none() {
            return Err(err("EXIF directory out of range"));
        }
        Ok(((0..n).map(|i| ifd + 2 + i * 12).collect(), end))
    }
}

pub fn exif_orientation(exif: &[u8]) -> Result<u8, String> {
    let t = Tiff::new(exif)?;
    let ifd = t.u32(exif, 4)? as usize;
    if ifd == 0 {
        return Ok(1);
    }
    for p in t.entries(exif, ifd)?.0 {
        if t.u16(exif, p)? == 0x0112 {
            if t.u16(exif, p + 2)? != 3 || t.u32(exif, p + 4)? != 1 {
                return Err(err("EXIF orientation type"));
            }
            let v = t.u16(exif, p + 8)?;
            if !(1..=8).contains(&v) {
                return Err(err("EXIF orientation value"));
            }
            return Ok(v as u8);
        }
    }
    Ok(1)
}

pub fn minimal_exif(orientation: u8) -> Vec<u8> {
    let mut b = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x01\0\0\0\0\0\0\0".to_vec();
    b[18] = orientation.clamp(1, 8);
    b
}

fn update_exif(raw: &[u8], policy: &MetadataPolicy, orientation: u8) -> Result<Vec<u8>, String> {
    let t = Tiff::new(raw)?;
    let mut b = raw.to_vec();
    let ifd = t.u32(&b, 4)? as usize;
    if ifd == 0 {
        return Ok(b);
    }
    let (entries, next) = t.entries(&b, ifd)?;
    let mut exif_ifd = None;
    for p in entries {
        let tag = t.u16(&b, p)?;
        if tag == 0x8769 && t.u16(&b, p + 2)? == 4 && t.u32(&b, p + 4)? == 1 {
            exif_ifd = Some(t.u32(&b, p + 8)? as usize);
        }
        let value = match tag {
            0x0112 => Some(orientation as u32),
            0x0100 if policy.pixels_changed => Some(policy.dimensions.0),
            0x0101 if policy.pixels_changed => Some(policy.dimensions.1),
            _ => None,
        };
        if let Some(v) = value {
            update_scalar(t, &mut b, p, v)?;
        }
    }
    if policy.pixels_changed || policy.orientation_normalized {
        t.write32(&mut b, next, 0);
    }
    if let Some(ifd) = exif_ifd.filter(|&p| p != 0) {
        for p in t.entries(&b, ifd)?.0 {
            let value = match t.u16(&b, p)? {
                0xa002 if policy.pixels_changed => Some(policy.dimensions.0),
                0xa003 if policy.pixels_changed => Some(policy.dimensions.1),
                0xa001 if policy.converted_to_srgb => Some(1),
                _ => None,
            };
            if let Some(v) = value {
                update_scalar(t, &mut b, p, v)?;
            }
        }
    }
    Ok(b)
}
fn update_scalar(t: Tiff, b: &mut [u8], p: usize, v: u32) -> Result<(), String> {
    if t.u32(b, p + 4)? != 1 {
        return Err(err("EXIF scalar count"));
    }
    match t.u16(b, p + 2)? {
        3 if v <= u16::MAX as u32 => {
            t.write16(b, p + 8, v as u16);
            b[p + 10..p + 12].fill(0);
        }
        3 | 4 => {
            t.write16(b, p + 2, 4);
            t.write32(b, p + 8, v);
        }
        _ => return Err(err("EXIF scalar type")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> MetadataPolicy {
        MetadataPolicy {
            keep_metadata: false,
            orientation_normalized: false,
            converted_to_srgb: false,
            dimensions: (1, 1),
            pixels_changed: false,
        }
    }
    #[test]
    fn orientations_and_invalid_offsets() {
        for value in 1..=8 {
            assert_eq!(exif_orientation(&minimal_exif(value)).unwrap(), value);
        }
        let mut bad = minimal_exif(6);
        bad[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(exif_orientation(&bad).is_err());
    }
    #[test]
    fn stripping_keeps_only_required_orientation() {
        let mut jpg = vec![0xff, 0xd8];
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(&minimal_exif(6));
        exif.extend_from_slice(b"PRIVATE GPS DATA");
        jpeg_app(&mut jpg, 0xe1, &exif).unwrap();
        jpeg_app(&mut jpg, 0xfe, b"private comment").unwrap();
        jpg.extend_from_slice(&[0xff, 0xd9]);
        let source = read_metadata(&jpg, ImageFormat::Jpeg).unwrap();
        let (out, _) = rewrite_metadata(&jpg, ImageFormat::Jpeg, &source, &policy()).unwrap();
        let m = read_metadata(&out, ImageFormat::Jpeg).unwrap();
        assert_eq!(m.orientation, 6);
        assert_eq!(m.exif.unwrap(), minimal_exif(6));
        assert!(!out.windows(7).any(|v| v == b"PRIVATE"));
        let mut p = policy();
        p.orientation_normalized = true;
        let (out, _) = rewrite_metadata(&jpg, ImageFormat::Jpeg, &source, &p).unwrap();
        assert!(
            read_metadata(&out, ImageFormat::Jpeg)
                .unwrap()
                .exif
                .is_none()
        );
    }
    #[test]
    fn metadata_between_jpeg_scans_is_removed() {
        let mut jpg = vec![0xff, 0xd8, 0xff, 0xda, 0, 2, 0x22, 0xff, 0, 0x33];
        jpeg_app(&mut jpg, 0xfe, b"secret").unwrap();
        jpg.extend_from_slice(&[0xff, 0xd9]);
        let source = read_metadata(&jpg, ImageFormat::Jpeg).unwrap();
        let (out, _) = rewrite_metadata(&jpg, ImageFormat::Jpeg, &source, &policy()).unwrap();
        assert!(!out.windows(6).any(|v| v == b"secret"));
        assert!(out.windows(4).any(|v| v == [0x22, 0xff, 0, 0x33]));
    }
    #[test]
    fn adobe_color_hint_cannot_smuggle_private_trailing_bytes() {
        let mut jpeg = vec![0xff, 0xd8];
        let standard = b"Adobe\0\x64\0\0\0\0\0";
        let mut payload = standard.to_vec();
        payload.extend_from_slice(b"PRIVATE GPS");
        jpeg_app(&mut jpeg, 0xee, &payload).unwrap();
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        let source = read_metadata(&jpeg, ImageFormat::Jpeg).unwrap();
        assert!(source.adobe_inverted);
        let (output, _) = rewrite_metadata(&jpeg, ImageFormat::Jpeg, &source, &policy()).unwrap();
        assert!(!output.windows(7).any(|b| b == b"PRIVATE"));
        let adobe = jpeg_segments(&output)
            .unwrap()
            .into_iter()
            .find(|s| s.marker == 0xee)
            .unwrap();
        assert_eq!(&output[adobe.payload..adobe.end], standard);
    }
    #[test]
    fn png_crc_is_validated() {
        let mut b = PNG_SIGNATURE.to_vec();
        png_chunk(&mut b, b"IEND", &[]).unwrap();
        assert!(png_chunks(&b).is_ok());
        b[12] ^= 1;
        assert!(png_chunks(&b).is_err());
    }
    #[test]
    fn webp_exif_round_trip_and_flags() {
        let mut webp = b"RIFF\0\0\0\0WEBP".to_vec();
        webp_chunk(&mut webp, b"VP8L", &[0x2f, 0, 0, 0, 0]).unwrap();
        let len = (webp.len() - 8) as u32;
        webp[4..8].copy_from_slice(&len.to_le_bytes());
        let source = ImageMetadata {
            orientation: 8,
            ..Default::default()
        };
        let (out, _) = rewrite_metadata(&webp, ImageFormat::WebP, &source, &policy()).unwrap();
        assert_eq!(
            read_metadata(&out, ImageFormat::WebP).unwrap().orientation,
            8
        );
        assert_eq!(out[20] & 0x08, 0x08);
    }

    #[test]
    fn exif_preservation_updates_geometry_color_and_thumbnail_link() {
        // IFD0: orientation, ImageWidth (SHORT), ExifIFD pointer. IFD1 points
        // at a stale thumbnail; the EXIF IFD contains pixel width/height/color.
        let mut exif = b"II\x2a\0\x08\0\0\0".to_vec();
        exif.extend_from_slice(&3u16.to_le_bytes());
        fn field(b: &mut Vec<u8>, tag: u16, kind: u16, value: u32) {
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(&kind.to_le_bytes());
            b.extend_from_slice(&1u32.to_le_bytes());
            b.extend_from_slice(&value.to_le_bytes());
        }
        field(&mut exif, 0x0112, 3, 6);
        field(&mut exif, 0x0100, 3, 800);
        field(&mut exif, 0x8769, 4, 50);
        exif.extend_from_slice(&92u32.to_le_bytes());
        exif.extend_from_slice(&3u16.to_le_bytes());
        field(&mut exif, 0xa002, 4, 800);
        field(&mut exif, 0xa003, 4, 600);
        field(&mut exif, 0xa001, 3, 65535);
        exif.extend_from_slice(&0u32.to_le_bytes());
        let mut p = policy();
        p.keep_metadata = true;
        p.pixels_changed = true;
        p.converted_to_srgb = true;
        p.orientation_normalized = true;
        p.dimensions = (70_000, 80_000);
        let output = update_exif(&exif, &p, 1).unwrap();
        let t = Tiff::new(&output).unwrap();
        assert_eq!(exif_orientation(&output).unwrap(), 1);
        assert_eq!(t.u16(&output, 24).unwrap(), 4);
        assert_eq!(t.u32(&output, 30).unwrap(), 70_000);
        assert_eq!(t.u32(&output, 46).unwrap(), 0);
        assert_eq!(t.u32(&output, 60).unwrap(), 70_000);
        assert_eq!(t.u32(&output, 72).unwrap(), 80_000);
        assert_eq!(t.u16(&output, 84).unwrap(), 1);
    }

    #[test]
    fn png_profile_replacement_and_xmp_privacy_are_explicit() {
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[20, 40, 60, 77], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        let original = ImageMetadata {
            icc_profile: Some(moxcms::ColorProfile::new_display_p3().encode().unwrap()),
            exif: Some(minimal_exif(6)),
            orientation: 6,
            xmp: Some(b"<xmp>private</xmp>".to_vec()),
            ..Default::default()
        };
        let mut p = policy();
        p.keep_metadata = true;
        let (with_metadata, _) = rewrite_metadata(&png, ImageFormat::Png, &original, &p).unwrap();
        let parsed = read_metadata(&with_metadata, ImageFormat::Png).unwrap();
        assert_eq!(parsed.icc_profile, original.icc_profile);
        assert_eq!(parsed.xmp, original.xmp);
        p.keep_metadata = false;
        p.converted_to_srgb = true;
        p.orientation_normalized = true;
        let (output, _) = rewrite_metadata(&png, ImageFormat::Png, &parsed, &p).unwrap();
        let parsed = read_metadata(&output, ImageFormat::Png).unwrap();
        assert!(parsed.png_srgb);
        assert!(parsed.icc_profile.is_none());
        assert!(parsed.exif.is_none());
        assert!(parsed.xmp.is_none());
        // The adapter changes the container only; it never accidentally touches
        // decoded alpha/pixels or claims to perform the color conversion itself.
        assert_eq!(
            image::load_from_memory(&output)
                .unwrap()
                .into_rgba8()
                .as_raw(),
            &[20, 40, 60, 77]
        );
    }

    #[test]
    fn compressed_metadata_bomb_is_rejected() {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        let block = [0u8; 8192];
        for _ in 0..=MAX_METADATA_BYTES / block.len() {
            z.write_all(&block).unwrap();
        }
        let mut p = b"ICC\0\0".to_vec();
        p.extend_from_slice(&z.finish().unwrap());
        let mut png = PNG_SIGNATURE.to_vec();
        png_chunk(&mut png, b"iCCP", &p).unwrap();
        png_chunk(&mut png, b"IEND", &[]).unwrap();
        assert!(
            read_metadata(&png, ImageFormat::Png)
                .unwrap_err()
                .contains("16 MiB")
        );
    }
    #[test]
    fn trailing_private_data_after_container_end_cannot_return_via_rewrite() {
        let jpeg = vec![0xff, 0xd8, 0xff, 0xd9];
        let mut png = PNG_SIGNATURE.to_vec();
        png_chunk(&mut png, b"IEND", &[]).unwrap();
        let mut webp = b"RIFF\0\0\0\0WEBP".to_vec();
        webp_chunk(&mut webp, b"VP8L", &[0x2f, 0, 0, 0, 0]).unwrap();
        let size = (webp.len() - 8) as u32;
        webp[4..8].copy_from_slice(&size.to_le_bytes());
        for (format, original) in [
            (ImageFormat::Jpeg, jpeg),
            (ImageFormat::Png, png),
            (ImageFormat::WebP, webp),
        ] {
            let mut with_trailer = original.clone();
            with_trailer.extend_from_slice(b"PRIVATE GPS TRAILING PAYLOAD");
            let source = read_metadata(&with_trailer, format).unwrap();
            let (output, _) = rewrite_metadata(&with_trailer, format, &source, &policy()).unwrap();
            assert_eq!(
                output, original,
                "{format:?} retained bytes after its container end"
            );
        }
    }
}
