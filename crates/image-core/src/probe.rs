//! Allocation-bounded container inspection with small buffers and seeks.
//! Importing a batch never decodes images or reads whole files into RAM.
use crate::{CoreError, Format, ImageInfo, Result, metadata};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::AtomicBool,
};

fn invalid(message: &str) -> CoreError {
    CoreError::Image(message.into())
}
fn read_array<const N: usize>(reader: &mut impl Read) -> Result<[u8; N]> {
    let mut value = [0; N];
    reader.read_exact(&mut value)?;
    Ok(value)
}
fn payload(reader: &mut BufReader<File>, length: u64, used: &mut u64) -> Result<Vec<u8>> {
    *used = used.saturating_add(length);
    if *used > metadata::MAX_METADATA_BYTES as u64 {
        return Err(CoreError::Unsupported(
            "Metadata exceeds the 16 MiB safety limit".into(),
        ));
    }
    let mut bytes = vec![0; length as usize];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn account(length: u64, used: &mut u64) -> Result<()> {
    *used = used.saturating_add(length);
    if *used > metadata::MAX_METADATA_BYTES as u64 {
        return Err(CoreError::Unsupported(
            "Metadata exceeds the 16 MiB safety limit".into(),
        ));
    }
    Ok(())
}
fn base(format: Format, bytes: u64) -> ImageInfo {
    ImageInfo {
        format,
        width: 0,
        height: 0,
        bit_depth: 8,
        animated: false,
        bytes,
        orientation: 1,
        has_icc: false,
        has_metadata: false,
    }
}

pub fn probe(path: &Path) -> Result<ImageInfo> {
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut reader = BufReader::with_capacity(16 * 1024, file);
    let mut signature = [0u8; 12];
    let count = reader.read(&mut signature)?;
    reader.seek(SeekFrom::Start(0))?;
    let info = if count >= 3 && signature[..3] == [0xff, 0xd8, 0xff] {
        probe_jpeg(&mut reader, length)?
    } else if count >= 8 && &signature[..8] == b"\x89PNG\r\n\x1a\n" {
        probe_png(&mut reader, length)?
    } else if count >= 12 && &signature[..4] == b"RIFF" && &signature[8..12] == b"WEBP" {
        probe_webp(&mut reader, length)?
    } else if count >= 6 && (&signature[..6] == b"GIF87a" || &signature[..6] == b"GIF89a") {
        let gif = crate::gif_codec::inspect_gif(path, &AtomicBool::new(false))?;
        ImageInfo {
            format: Format::Gif,
            width: gif.width,
            height: gif.height,
            bit_depth: 8,
            animated: gif.frames > 1,
            bytes: length,
            orientation: 1,
            has_icc: gif.has_icc,
            has_metadata: gif.has_metadata,
        }
    } else {
        return Err(CoreError::Unsupported(
            "Supported inputs are JPEG, PNG, static WebP and GIF".into(),
        ));
    };
    if info.width == 0 || info.height == 0 {
        return Err(invalid("Missing or zero image dimensions"));
    }
    Ok(info)
}

fn probe_jpeg(reader: &mut BufReader<File>, length: u64) -> Result<ImageInfo> {
    let mut info = base(Format::Jpeg, length);
    reader.seek(SeekFrom::Start(2))?;
    let mut used = 0;
    let mut pending_marker = None;
    let mut saw_scan = false;
    let mut saw_frame = false;
    loop {
        let marker = if let Some(marker) = pending_marker.take() {
            marker
        } else {
            if read_array::<1>(reader)?[0] != 0xff {
                return Err(invalid("Invalid JPEG marker"));
            }
            loop {
                let b = read_array::<1>(reader)?[0];
                if b != 0xff {
                    break b;
                }
            }
        };
        if marker == 0xd9 {
            if !saw_scan {
                return Err(invalid("JPEG contains no image scan"));
            }
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            continue;
        }
        let size = u16::from_be_bytes(read_array(reader)?) as u64;
        if size < 2 {
            return Err(invalid("Invalid JPEG segment size"));
        }
        let size = size - 2;
        let end = reader
            .stream_position()?
            .checked_add(size)
            .ok_or_else(|| invalid("JPEG size overflow"))?;
        if end > length {
            return Err(invalid("Truncated JPEG segment"));
        }
        match marker {
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                if saw_frame {
                    return Err(invalid("Multiple JPEG frame headers are not supported"));
                }
                saw_frame = true;
                if size < 6 {
                    return Err(invalid("Truncated JPEG frame header"));
                }
                let p = read_array::<6>(reader)?;
                info.bit_depth = p[0];
                info.height = u16::from_be_bytes([p[1], p[2]]) as u32;
                info.width = u16::from_be_bytes([p[3], p[4]]) as u32;
                if p[0] != 8 {
                    return Err(CoreError::Unsupported(
                        "Only 8-bit JPEG is supported".into(),
                    ));
                }
            }
            0xe1 => {
                info.has_metadata = true;
                let p = payload(reader, size, &mut used)?;
                if p.starts_with(b"Exif\0\0") {
                    info.orientation =
                        metadata::exif_orientation(&p[6..]).map_err(CoreError::Image)?;
                }
                if p.windows(12).any(|w| w == b"hdr-gain-map")
                    || p.windows(11).any(|w| w == b"hdrgm:Versi")
                {
                    return Err(CoreError::Unsupported(
                        "HDR gain-map JPEG is not supported".into(),
                    ));
                }
            }
            0xe2 => {
                account(size, &mut used)?;
                let mut prefix = [0u8; 12];
                let n = size.min(12) as usize;
                reader.read_exact(&mut prefix[..n])?;
                if prefix[..n].starts_with(b"MPF\0") {
                    return Err(CoreError::Unsupported(
                        "Multi-picture/HDR JPEG is not supported".into(),
                    ));
                }
                if n == 12 && &prefix == b"ICC_PROFILE\0" {
                    info.has_icc = true;
                } else {
                    info.has_metadata = true;
                }
            }
            0xe3..=0xed | 0xef | 0xfe => {
                info.has_metadata = true;
                account(size, &mut used)?;
            }
            _ => {}
        }
        reader.seek(SeekFrom::Start(end))?;
        if marker == 0xda {
            saw_scan = true;
            pending_marker = Some(next_jpeg_marker(reader)?);
        }
    }
    Ok(info)
}

/// Entropy data is scanned in the existing small BufReader buffer. Metadata is
/// legal between scans; stopping at the first SOS misses ICC and orientation.
fn next_jpeg_marker(reader: &mut BufReader<File>) -> Result<u8> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(invalid("JPEG end marker missing"));
        }
        let found = available.iter().position(|&b| b == 0xff);
        let count = found.unwrap_or(available.len());
        reader.consume(count);
        if found.is_none() {
            continue;
        }
        let _ = read_array::<1>(reader)?;
        let marker = loop {
            let b = read_array::<1>(reader)?[0];
            if b != 0xff {
                break b;
            }
        };
        if marker == 0 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        return Ok(marker);
    }
}

fn probe_png(reader: &mut BufReader<File>, length: u64) -> Result<ImageInfo> {
    let mut info = base(Format::Png, length);
    reader.seek(SeekFrom::Start(8))?;
    let mut used = 0;
    let mut first = true;
    loop {
        let size = u32::from_be_bytes(read_array(reader)?) as u64;
        let kind = read_array::<4>(reader)?;
        let end = reader
            .stream_position()?
            .checked_add(size + 4)
            .ok_or_else(|| invalid("PNG size overflow"))?;
        if end > length {
            return Err(invalid("Truncated PNG chunk"));
        }
        if first && (&kind != b"IHDR" || size != 13) {
            return Err(invalid("PNG must begin with IHDR"));
        }
        if !first && &kind == b"IHDR" {
            return Err(invalid("PNG contains multiple IHDR chunks"));
        }
        first = false;
        match &kind {
            b"IHDR" => {
                if size != 13 {
                    return Err(invalid("Invalid PNG IHDR size"));
                }
                let p = read_array::<13>(reader)?;
                info.width = u32::from_be_bytes(p[0..4].try_into().unwrap());
                info.height = u32::from_be_bytes(p[4..8].try_into().unwrap());
                info.bit_depth = p[8];
            }
            b"acTL" | b"fcTL" | b"fdAT" => {
                return Err(CoreError::Unsupported(
                    "Animated PNG (APNG) is not supported".into(),
                ));
            }
            b"iCCP" => {
                info.has_icc = true;
                account(size, &mut used)?;
            }
            b"eXIf" => {
                info.has_metadata = true;
                let p = payload(reader, size, &mut used)?;
                info.orientation = metadata::exif_orientation(&p).map_err(CoreError::Image)?;
            }
            b"cICP" => {
                if size != 4 {
                    return Err(invalid("Invalid PNG cICP"));
                }
                let p = read_array::<4>(reader)?;
                if matches!(p[1], 16 | 18) {
                    return Err(CoreError::Unsupported("HDR PNG is not supported".into()));
                }
            }
            b"cLLI" | b"mDCv" => {
                return Err(CoreError::Unsupported(
                    "HDR PNG metadata is not supported".into(),
                ));
            }
            b"IDAT" | b"PLTE" | b"tRNS" | b"gAMA" | b"cHRM" | b"sRGB" => {}
            b"IEND" => {
                if size != 0 {
                    return Err(invalid("Invalid PNG end marker"));
                }
                break;
            }
            _ => {
                info.has_metadata = true;
                account(size, &mut used)?;
            }
        }
        reader.seek(SeekFrom::Start(end))?;
    }
    Ok(info)
}

fn probe_webp(reader: &mut BufReader<File>, length: u64) -> Result<ImageInfo> {
    let mut info = base(Format::WebP, length);
    reader.seek(SeekFrom::Start(4))?;
    let container_end = u32::from_le_bytes(read_array(reader)?) as u64 + 8;
    if container_end > length || container_end < 20 {
        return Err(invalid("Truncated WebP container"));
    }
    reader.seek(SeekFrom::Start(12))?;
    let mut used = 0;
    let mut has_pixels = false;
    let mut has_extended = false;
    while reader.stream_position()? < container_end {
        let kind = read_array::<4>(reader)?;
        let size = u32::from_le_bytes(read_array(reader)?) as u64;
        let end = reader
            .stream_position()?
            .checked_add(size + (size & 1))
            .ok_or_else(|| invalid("WebP size overflow"))?;
        if end > container_end {
            return Err(invalid("Truncated WebP chunk"));
        }
        match &kind {
            b"VP8X" => {
                if size != 10 || has_extended || has_pixels || reader.stream_position()? != 20 {
                    return Err(invalid("WebP must have at most one leading VP8X chunk"));
                }
                has_extended = true;
                let p = read_array::<10>(reader)?;
                if p[0] & 2 != 0 {
                    return Err(CoreError::Unsupported(
                        "Animated WebP is not supported".into(),
                    ));
                }
                info.width = 1 + u32::from_le_bytes([p[4], p[5], p[6], 0]);
                info.height = 1 + u32::from_le_bytes([p[7], p[8], p[9], 0]);
            }
            b"VP8 " => {
                if has_pixels {
                    return Err(invalid("Multiple static WebP image chunks"));
                }
                if size < 10 {
                    return Err(invalid("Truncated WebP VP8 frame"));
                }
                let p = read_array::<10>(reader)?;
                if p[3..6] != [0x9d, 0x01, 0x2a] {
                    return Err(invalid("Invalid WebP frame header"));
                }
                let width = (u16::from_le_bytes([p[6], p[7]]) & 0x3fff) as u32;
                let height = (u16::from_le_bytes([p[8], p[9]]) & 0x3fff) as u32;
                if has_extended && (info.width, info.height) != (width, height) {
                    return Err(invalid("WebP canvas and encoded frame dimensions differ"));
                }
                info.width = width;
                info.height = height;
                has_pixels = true;
            }
            b"VP8L" => {
                if has_pixels {
                    return Err(invalid("Multiple static WebP image chunks"));
                }
                if size < 5 {
                    return Err(invalid("Truncated lossless WebP frame"));
                }
                let p = read_array::<5>(reader)?;
                if p[0] != 0x2f {
                    return Err(invalid("Invalid lossless WebP frame"));
                }
                let bits = u32::from_le_bytes(p[1..5].try_into().unwrap());
                let width = (bits & 0x3fff) + 1;
                let height = ((bits >> 14) & 0x3fff) + 1;
                if has_extended && (info.width, info.height) != (width, height) {
                    return Err(invalid("WebP canvas and encoded frame dimensions differ"));
                }
                info.width = width;
                info.height = height;
                has_pixels = true;
            }
            b"ANIM" | b"ANMF" => {
                return Err(CoreError::Unsupported(
                    "Animated WebP is not supported".into(),
                ));
            }
            b"ICCP" => {
                info.has_icc = true;
                account(size, &mut used)?;
            }
            b"EXIF" => {
                info.has_metadata = true;
                let p = payload(reader, size, &mut used)?;
                info.orientation =
                    metadata::exif_orientation(p.strip_prefix(b"Exif\0\0").unwrap_or(&p))
                        .map_err(CoreError::Image)?;
            }
            b"ALPH" => {}
            _ => {
                info.has_metadata = true;
                account(size, &mut used)?;
            }
        }
        reader.seek(SeekFrom::Start(end))?;
    }
    if !has_pixels {
        return Err(invalid("WebP contains no static image"));
    }
    Ok(info)
}
