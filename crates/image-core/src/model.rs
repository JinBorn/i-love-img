use crate::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    Jpeg,
    Png,
    WebP,
    Gif,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::WebP => "webp",
            Self::Gif => "gif",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::WebP => "WebP",
            Self::Gif => "GIF",
        }
    }
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "webp" => Some(Self::WebP),
            "gif" => Some(Self::Gif),
            _ => None,
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Compression {
    #[default]
    Auto,
    Lossless,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum ResizeMode {
    #[default]
    Keep,
    Bounds {
        width: u32,
        height: u32,
    },
    LongestEdge(u32),
    Percent(f64),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ResizeSpec {
    pub mode: ResizeMode,
    pub allow_upscale: bool,
}

impl ResizeSpec {
    pub fn dimensions(&self, width: u32, height: u32) -> Result<(u32, u32)> {
        if width == 0 || height == 0 {
            return Err(CoreError::Invalid(
                "Image dimensions must be positive".into(),
            ));
        }
        let mut scale = match self.mode {
            ResizeMode::Keep => 1.0,
            ResizeMode::Bounds {
                width: w,
                height: h,
            } => {
                if w == 0 || h == 0 {
                    return Err(CoreError::Invalid(
                        "Width and height must be positive".into(),
                    ));
                }
                (f64::from(w) / f64::from(width)).min(f64::from(h) / f64::from(height))
            }
            ResizeMode::LongestEdge(edge) => {
                if edge == 0 {
                    return Err(CoreError::Invalid("Longest edge must be positive".into()));
                }
                f64::from(edge) / f64::from(width.max(height))
            }
            ResizeMode::Percent(percent) => {
                if !percent.is_finite() || percent <= 0.0 {
                    return Err(CoreError::Invalid(
                        "Percentage must be finite and positive".into(),
                    ));
                }
                percent / 100.0
            }
        };
        if !self.allow_upscale {
            scale = scale.min(1.0);
        }
        let w = (f64::from(width) * scale).round().max(1.0);
        let h = (f64::from(height) * scale).round().max(1.0);
        if !w.is_finite() || !h.is_finite() || w > f64::from(u32::MAX) || h > f64::from(u32::MAX) {
            return Err(CoreError::Invalid(
                "Requested dimensions are too large".into(),
            ));
        }
        Ok((w as u32, h as u32))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessOptions {
    pub format: Option<Format>,
    pub resize: ResizeSpec,
    pub compression: Compression,
    pub jpeg_quality: u8,
    pub webp_quality: u8,
    pub strip_metadata: bool,
    pub matte: [u8; 3],
    pub gif_first_frame: bool,
    pub memory_limit_bytes: u64,
}

impl Default for ProcessOptions {
    fn default() -> Self {
        Self {
            format: None,
            resize: ResizeSpec::default(),
            compression: Compression::Auto,
            jpeg_quality: 85,
            webp_quality: 80,
            strip_metadata: true,
            matte: [255; 3],
            gif_first_frame: false,
            memory_limit_bytes: 1536 * MIB,
        }
    }
}

impl ProcessOptions {
    pub fn validate(&self) -> Result<()> {
        if !(1..=100).contains(&self.jpeg_quality) || !(1..=100).contains(&self.webp_quality) {
            return Err(CoreError::Invalid(
                "Quality must be between 1 and 100".into(),
            ));
        }
        if self.memory_limit_bytes < 16 * MIB {
            return Err(CoreError::Invalid(
                "Memory budget must be at least 16 MiB".into(),
            ));
        }
        self.resize.dimensions(1, 1)?;
        Ok(())
    }
    pub fn output_format(&self, info: &ImageInfo) -> Format {
        self.format.unwrap_or(info.format)
    }
    pub fn next_step(&self) -> Self {
        Self {
            format: None,
            resize: ResizeSpec::default(),
            compression: Compression::Auto,
            jpeg_quality: 85,
            webp_quality: 80,
            gif_first_frame: false,
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub animated: bool,
    pub bytes: u64,
    /// EXIF orientation (1..=8); raw width/height are stored above.
    pub orientation: u8,
    pub has_icc: bool,
    pub has_metadata: bool,
}

impl ImageInfo {
    pub fn display_dimensions(&self) -> (u32, u32) {
        if matches!(self.orientation, 5..=8) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessReport {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub elapsed_ms: u64,
    pub warnings: Vec<String>,
    pub unchanged: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fit_mixed_orientations_without_upscaling() {
        let fit = ResizeSpec {
            mode: ResizeMode::Bounds {
                width: 1920,
                height: 1080,
            },
            allow_upscale: false,
        };
        assert_eq!(fit.dimensions(4000, 3000).unwrap(), (1440, 1080));
        assert_eq!(fit.dimensions(3000, 4000).unwrap(), (810, 1080));
        assert_eq!(fit.dimensions(100, 50).unwrap(), (100, 50));
    }
    #[test]
    fn invalid_or_overflowing_dimensions_are_rejected() {
        for percent in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e100] {
            assert!(
                ResizeSpec {
                    mode: ResizeMode::Percent(percent),
                    allow_upscale: true
                }
                .dimensions(4000, 3000)
                .is_err()
            );
        }
    }
    #[test]
    fn continuing_does_not_repeat_geometry_or_format() {
        let options = ProcessOptions {
            format: Some(Format::WebP),
            resize: ResizeSpec {
                mode: ResizeMode::Percent(50.0),
                allow_upscale: false,
            },
            strip_metadata: false,
            matte: [1, 2, 3],
            ..Default::default()
        };
        let next = options.next_step();
        assert_eq!(next.resize.mode, ResizeMode::Keep);
        assert_eq!(next.format, None);
        assert!(!next.strip_metadata);
        assert_eq!(next.matte, [1, 2, 3]);
    }
}
