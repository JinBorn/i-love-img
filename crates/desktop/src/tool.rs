use local_image_core::Format;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Compress,
    Resize,
    Convert,
}

impl ToolKind {
    pub const ALL: [Self; 3] = [Self::Compress, Self::Resize, Self::Convert];

    pub const fn index(self) -> usize {
        match self {
            Self::Compress => 0,
            Self::Resize => 1,
            Self::Convert => 2,
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Compress => "compress",
            Self::Resize => "resize",
            Self::Convert => "convert",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompressPreferences {
    pub lossless: bool,
    pub jpeg_quality: u8,
    pub webp_quality: u8,
    pub strip_metadata: bool,
}

impl Default for CompressPreferences {
    fn default() -> Self {
        Self {
            lossless: false,
            jpeg_quality: 85,
            webp_quality: 80,
            strip_metadata: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResizePreferences {
    /// 1 = bounding dimensions, 2 = longest edge, 3 = percentage.
    pub mode: u8,
    pub width: u32,
    pub height: u32,
    pub longest_edge: u32,
    pub percent: f64,
    pub allow_upscale: bool,
    pub jpeg_quality: u8,
    pub webp_quality: u8,
    pub strip_metadata: bool,
    pub background: [u8; 3],
}

impl Default for ResizePreferences {
    fn default() -> Self {
        Self {
            mode: 2,
            width: 1920,
            height: 1080,
            longest_edge: 1920,
            percent: 50.0,
            allow_upscale: false,
            jpeg_quality: 95,
            webp_quality: 90,
            strip_metadata: true,
            background: [255; 3],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvertPreferences {
    pub format: Format,
    pub webp_lossless: bool,
    pub jpeg_quality: u8,
    pub webp_quality: u8,
    pub strip_metadata: bool,
    pub gif_first_frame: bool,
    pub background: [u8; 3],
}

impl Default for ConvertPreferences {
    fn default() -> Self {
        Self {
            format: Format::Jpeg,
            webp_lossless: false,
            jpeg_quality: 85,
            webp_quality: 80,
            strip_metadata: true,
            gif_first_frame: false,
            background: [255; 3],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolPreferences {
    Compress(CompressPreferences),
    Resize(ResizePreferences),
    Convert(ConvertPreferences),
}

impl ToolPreferences {
    pub const fn tool(&self) -> ToolKind {
        match self {
            Self::Compress(_) => ToolKind::Compress,
            Self::Resize(_) => ToolKind::Resize,
            Self::Convert(_) => ToolKind::Convert,
        }
    }
}
