use crate::tool::{
    CompressPreferences, ConvertPreferences, ResizePreferences, ToolKind, ToolPreferences,
};
use local_image_core::{Compression, Format, ProcessOptions, ResizeMode, ResizeSpec};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Language {
    #[default]
    Chinese,
    English,
}

impl Language {
    pub fn text<'a>(self, chinese: &'a str, english: &'a str) -> &'a str {
        if self == Self::Chinese {
            chinese
        } else {
            english
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub language: Language,
    pub output_directory: String,
    pub recursive: bool,
    pub compress: CompressPreferences,
    pub resize: ResizePreferences,
    pub convert: ConvertPreferences,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Language::Chinese,
            output_directory: String::new(),
            recursive: true,
            compress: CompressPreferences::default(),
            resize: ResizePreferences::default(),
            convert: ConvertPreferences::default(),
        }
    }
}

impl Settings {
    /// Build each operation from a fresh whitelist. Fields from other tools can
    /// never resize, convert, discard animation, or select JPEG lossless mode.
    pub fn options(&self, tool: ToolKind) -> ProcessOptions {
        match tool {
            ToolKind::Compress => ProcessOptions {
                compression: if self.compress.lossless {
                    Compression::Lossless
                } else {
                    Compression::Auto
                },
                jpeg_quality: self.compress.jpeg_quality,
                webp_quality: self.compress.webp_quality,
                strip_metadata: self.compress.strip_metadata,
                ..ProcessOptions::default()
            },
            ToolKind::Resize => ProcessOptions {
                resize: ResizeSpec {
                    mode: match self.resize.mode {
                        1 => ResizeMode::Bounds {
                            width: self.resize.width,
                            height: self.resize.height,
                        },
                        3 => ResizeMode::Percent(self.resize.percent),
                        _ => ResizeMode::LongestEdge(self.resize.longest_edge),
                    },
                    allow_upscale: self.resize.allow_upscale,
                },
                jpeg_quality: self.resize.jpeg_quality,
                webp_quality: self.resize.webp_quality,
                strip_metadata: self.resize.strip_metadata,
                matte: self.resize.background,
                ..ProcessOptions::default()
            },
            ToolKind::Convert => ProcessOptions {
                format: Some(self.convert.format),
                compression: if self.convert.format == Format::WebP && self.convert.webp_lossless {
                    Compression::Lossless
                } else {
                    Compression::Auto
                },
                jpeg_quality: self.convert.jpeg_quality,
                webp_quality: self.convert.webp_quality,
                strip_metadata: self.convert.strip_metadata,
                gif_first_frame: self.convert.gif_first_frame,
                matte: self.convert.background,
                ..ProcessOptions::default()
            },
        }
    }

    pub fn reset_tool(&mut self, tool: ToolKind) {
        match tool {
            ToolKind::Compress => self.compress = CompressPreferences::default(),
            ToolKind::Resize => self.resize = ResizePreferences::default(),
            ToolKind::Convert => self.convert = ConvertPreferences::default(),
        }
    }

    pub fn snapshot(&self, tool: ToolKind) -> ToolPreferences {
        match tool {
            ToolKind::Compress => ToolPreferences::Compress(self.compress.clone()),
            ToolKind::Resize => ToolPreferences::Resize(self.resize.clone()),
            ToolKind::Convert => ToolPreferences::Convert(self.convert.clone()),
        }
    }

    /// A historical operation restores only its own tool, never another tool's
    /// preferences or shared language, output directory, or import behavior.
    pub fn restore_tool(&mut self, preferences: &ToolPreferences) {
        match preferences {
            ToolPreferences::Compress(value) => self.compress = value.clone(),
            ToolPreferences::Resize(value) => self.resize = value.clone(),
            ToolPreferences::Convert(value) => self.convert = value.clone(),
        }
    }
}

#[derive(Serialize)]
struct PersistedSettings<'a> {
    schema_version: u32,
    #[serde(flatten)]
    settings: &'a Settings,
}

/// The unversioned 0.1.1 layout is read only by the migration boundary. It is
/// never used as an operation draft or serialized back to disk.
#[derive(Deserialize)]
#[serde(default)]
struct LegacySettings {
    language: Language,
    output_directory: String,
    recursive: bool,
    resize_mode: u8,
    width: u32,
    height: u32,
    longest_edge: u32,
    percent: f64,
    allow_upscale: bool,
    format: u8,
    jpeg_lossless: bool,
    jpeg_quality: u8,
    webp_quality: u8,
    strip_metadata: bool,
    gif_first_frame: bool,
    background: [u8; 3],
}

impl Default for LegacySettings {
    fn default() -> Self {
        Self {
            language: Language::Chinese,
            output_directory: String::new(),
            recursive: true,
            resize_mode: 0,
            width: 1920,
            height: 1080,
            longest_edge: 1920,
            percent: 50.0,
            allow_upscale: false,
            format: 0,
            jpeg_lossless: false,
            jpeg_quality: 85,
            webp_quality: 80,
            strip_metadata: true,
            gif_first_frame: false,
            background: [255; 3],
        }
    }
}

impl From<LegacySettings> for Settings {
    fn from(legacy: LegacySettings) -> Self {
        Self {
            language: legacy.language,
            output_directory: legacy.output_directory,
            recursive: legacy.recursive,
            compress: CompressPreferences {
                lossless: legacy.jpeg_lossless,
                jpeg_quality: legacy.jpeg_quality,
                webp_quality: legacy.webp_quality,
                strip_metadata: legacy.strip_metadata,
            },
            resize: ResizePreferences {
                mode: if matches!(legacy.resize_mode, 1..=3) {
                    legacy.resize_mode
                } else {
                    2
                },
                width: legacy.width,
                height: legacy.height,
                longest_edge: if legacy.resize_mode == 0 {
                    1920
                } else {
                    legacy.longest_edge
                },
                percent: legacy.percent,
                allow_upscale: legacy.allow_upscale,
                jpeg_quality: legacy.jpeg_quality,
                webp_quality: legacy.webp_quality,
                strip_metadata: legacy.strip_metadata,
                background: legacy.background,
            },
            convert: ConvertPreferences {
                format: match legacy.format {
                    2 => Format::Png,
                    3 => Format::WebP,
                    4 => Format::Gif,
                    _ => Format::Jpeg,
                },
                // The old global lossless flag belongs only to Compress.
                webp_lossless: false,
                jpeg_quality: legacy.jpeg_quality,
                webp_quality: legacy.webp_quality,
                strip_metadata: legacy.strip_metadata,
                gif_first_frame: legacy.gif_first_frame,
                background: legacy.background,
            },
        }
    }
}

fn decode_settings(bytes: &[u8]) -> Result<Settings, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("Settings must be a JSON object".into());
    }
    match value.get("schema_version") {
        Some(version) if version.as_u64() == Some(2) => {
            serde_json::from_value(value).map_err(|e| e.to_string())
        }
        Some(version) => Err(format!("Unsupported settings schema version: {version}")),
        None => {
            if ["compress", "resize", "convert"]
                .iter()
                .any(|field| value.get(field).is_some())
            {
                return Err("Tool preferences require settings schema_version 2".into());
            }
            serde_json::from_value::<LegacySettings>(value)
                .map(Settings::from)
                .map_err(|e| e.to_string())
        }
    }
}

/// A selected storage location never falls back to another location after an error.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    pub path: PathBuf,
    pub portable_root: Option<PathBuf>,
}

pub struct LoadedSettings {
    pub settings: Settings,
    pub warning: Option<String>,
}

impl SettingsStore {
    pub fn discover() -> Result<Self, String> {
        let user_path = directories::ProjectDirs::from("org", "LocalImageTools", "LocalImageTools")
            .map(|p| p.config_dir().join("settings.json"));
        #[cfg(windows)]
        {
            let executable = std::env::current_exe()
                .map_err(|e| format!("Cannot locate the application executable: {e}"))?;
            let root = executable
                .parent()
                .ok_or("Cannot locate the application directory")?;
            Self::discover_at(root, user_path)
        }
        #[cfg(not(windows))]
        {
            Ok(Self {
                path: user_path.ok_or("Cannot locate settings directory")?,
                portable_root: None,
            })
        }
    }

    #[cfg(any(windows, test))]
    fn discover_at(root: &Path, user_path: Option<PathBuf>) -> Result<Self, String> {
        let root = normalized_path(root)?;
        let metadata = fs::metadata(&root)
            .map_err(|e| path_error("Cannot inspect application directory", &root, e))?;
        if !metadata.is_dir() {
            return Err(format!(
                "Application path is not a directory: {}",
                root.display()
            ));
        }
        let marker = root.join("portable.flag");
        // symlink_metadata distinguishes an absent marker from a broken marker link.
        // Once any marker is present, its errors cannot select the user profile instead.
        match fs::symlink_metadata(&marker) {
            Ok(_) => {
                let metadata = fs::metadata(&marker)
                    .map_err(|e| path_error("Cannot read portable marker", &marker, e))?;
                if !metadata.is_file() {
                    return Err(format!(
                        "Portable marker is not a file: {}",
                        marker.display()
                    ));
                }
                let directory = root.join("Data");
                match fs::symlink_metadata(&directory) {
                    Ok(_) => {
                        let metadata = fs::metadata(&directory).map_err(|e| {
                            path_error("Cannot inspect portable data directory", &directory, e)
                        })?;
                        if !metadata.is_dir() {
                            return Err(format!(
                                "Portable data path is not a directory: {}",
                                directory.display()
                            ));
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(path_error(
                            "Cannot inspect portable data directory",
                            &directory,
                            e,
                        ));
                    }
                }
                Ok(Self {
                    path: directory.join("settings.json"),
                    portable_root: Some(root),
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self {
                path: user_path.ok_or("Cannot locate settings directory")?,
                portable_root: None,
            }),
            Err(e) => Err(path_error("Cannot inspect portable marker", &marker, e)),
        }
    }

    pub fn is_portable(&self) -> bool {
        self.portable_root.is_some()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn defaults(&self) -> Settings {
        Settings {
            output_directory: if self.is_portable() {
                "Output".into()
            } else {
                String::new()
            },
            ..Settings::default()
        }
    }

    pub fn load(&self) -> Result<LoadedSettings, String> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(LoadedSettings {
                    settings: self.defaults(),
                    warning: None,
                });
            }
            Err(e) => return Err(path_error("Cannot read settings", &self.path, e)),
        };
        let mut settings = decode_settings(&bytes)
            .map_err(|e| format!("Cannot parse settings at {}: {e}", self.path.display()))?;
        let mut warning = None;
        if self.is_portable() {
            if settings.output_directory.trim().is_empty() {
                settings.output_directory = self.defaults().output_directory;
            } else {
                settings.output_directory = self.stored_output(&settings.output_directory)?;
                let output = Path::new(&settings.output_directory);
                // Relative choices, including explicit '..', travel with the application.
                // External absolute paths stay machine-specific; do not recreate an old
                // machine's missing directory after moving a portable package.
                if output.is_absolute() {
                    let usable = match fs::metadata(output) {
                        Ok(metadata) => metadata.is_dir(),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
                        Err(e) => {
                            return Err(path_error(
                                "Cannot inspect saved output directory",
                                output,
                                e,
                            ));
                        }
                    };
                    if !usable {
                        warning = Some(format!(
                            "Saved output directory is unavailable: {}. Output now uses the portable Output folder.",
                            output.display()
                        ));
                        settings.output_directory = self.defaults().output_directory;
                    }
                }
            }
        }
        Ok(LoadedSettings { settings, warning })
    }

    pub fn save(&self, settings: &Settings) -> Result<(), String> {
        let mut stored = settings.clone();
        if self.is_portable() && !stored.output_directory.trim().is_empty() {
            stored.output_directory = self.stored_output(&stored.output_directory)?;
        }
        let bytes = serde_json::to_vec_pretty(&PersistedSettings {
            schema_version: 2,
            settings: &stored,
        })
        .map_err(|e| e.to_string())?;
        let parent = self
            .path
            .parent()
            .ok_or("Cannot locate settings directory")?;
        ensure_writable_if_present(parent)?;
        ensure_writable_if_present(&self.path)?;
        fs::create_dir_all(parent)
            .map_err(|e| path_error("Cannot create settings directory", parent, e))?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".settings-")
            .suffix(".tmp")
            .tempfile_in(parent)
            .map_err(|e| path_error("Cannot create temporary settings", parent, e))?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.flush())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|e| path_error("Cannot write settings", &self.path, e))?;
        // Persist replaces the destination without first deleting it, including on
        // Windows. Every instance has its own temporary file; failure keeps the old JSON.
        temporary
            .persist(&self.path)
            .map_err(|e| path_error("Cannot replace settings", &self.path, e.error))?;
        Ok(())
    }

    /// GUI output paths use the executable directory in portable mode. Input paths
    /// and the CLI retain their own caller-relative semantics; no process chdir is used.
    pub fn resolve_output(&self, value: &str) -> Result<PathBuf, String> {
        if value.trim().is_empty() {
            return Err("Choose an output directory".into());
        }
        let path = normalized_path(Path::new(value))?;
        if path.is_absolute() {
            return Ok(path);
        }
        let base = match &self.portable_root {
            Some(root) => root.clone(),
            None => std::env::current_dir()
                .map_err(|e| format!("Cannot locate working directory: {e}"))?,
        };
        normalized_path(&base.join(path))
    }

    fn stored_output(&self, value: &str) -> Result<String, String> {
        let path = normalized_path(Path::new(value))?;
        let path = if let Some(root) = &self.portable_root {
            let root = normalized_path(root)?;
            relative_descendant(&path, &root).unwrap_or(path)
        } else {
            path
        };
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| "Output directory is not valid Unicode".into())
    }
}

fn path_error(action: &str, path: &Path, error: io::Error) -> String {
    format!("{action} at {}: {error}", path.display())
}

fn ensure_writable_if_present(path: &Path) -> Result<(), String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.permissions().readonly() => {
            Err(format!("Settings path is read-only: {}", path.display()))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(path_error("Cannot inspect settings path", path, e)),
    }
}

/// Lexically normalize without requiring an output directory to exist. Windows
/// drive-relative/root-relative and device namespace paths have ambiguous or
/// non-directory semantics, so they are not accepted as output destinations.
fn normalized_path(path: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    if path.has_root() && !matches!(path.components().next(), Some(Component::Prefix(_))) {
        return Err(
            "Use a full drive path or a relative output path, not a root-relative path".into(),
        );
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                #[cfg(windows)]
                {
                    use std::{ffi::OsString, path::Prefix};
                    if !path.has_root() {
                        return Err(
                            "Drive-relative output paths such as C:folder are not supported".into(),
                        );
                    }
                    match prefix.kind() {
                        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                            normalized.push(format!("{}:", char::from(drive).to_ascii_uppercase()));
                        }
                        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                            let mut root = OsString::from("\\\\");
                            root.push(server);
                            root.push("\\");
                            root.push(share);
                            normalized.push(root);
                        }
                        _ => {
                            return Err(
                                "Windows device paths are not supported as output directories"
                                    .into(),
                            );
                        }
                    }
                }
                #[cfg(not(windows))]
                normalized.push(prefix.as_os_str());
            }
            Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
            }
            Component::Normal(name) => normalized.push(name),
        }
    }
    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    Ok(normalized)
}

fn relative_descendant(path: &Path, root: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || !root.is_absolute() {
        return None;
    }
    let mut suffix = path.components();
    for base in root.components() {
        let candidate = suffix.next()?;
        #[cfg(windows)]
        let equal = candidate.as_os_str().to_string_lossy().to_lowercase()
            == base.as_os_str().to_string_lossy().to_lowercase();
        #[cfg(not(windows))]
        let equal = candidate == base;
        if !equal {
            return None;
        }
    }
    let relative: PathBuf = suffix.collect();
    Some(if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portable(root: &Path) -> SettingsStore {
        SettingsStore {
            path: root.join("Data/settings.json"),
            portable_root: Some(root.to_owned()),
        }
    }

    #[test]
    fn discovery_selects_marker_without_creating_data_or_using_user_settings() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("application");
        fs::create_dir(&root).unwrap();
        let user = directory.path().join("user/settings.json");
        let normal = SettingsStore::discover_at(&root, Some(user.clone())).unwrap();
        assert!(!normal.is_portable());
        assert_eq!(normal.path(), user);
        assert!(normal.defaults().output_directory.is_empty());

        fs::write(root.join("portable.flag"), []).unwrap();
        let store = SettingsStore::discover_at(&root, Some(user.clone())).unwrap();
        assert!(store.is_portable());
        assert_eq!(store.path(), root.join("Data/settings.json"));
        assert_eq!(store.load().unwrap().settings.output_directory, "Output");
        assert!(!root.join("Data").exists());
        assert!(!user.exists());
    }

    #[test]
    fn invalid_marker_or_data_never_selects_user_storage() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("portable.flag");
        let user = directory.path().join("user/settings.json");
        fs::create_dir(&marker).unwrap();
        assert!(SettingsStore::discover_at(directory.path(), Some(user.clone())).is_err());
        fs::remove_dir(&marker).unwrap();
        fs::write(&marker, []).unwrap();
        fs::write(directory.path().join("Data"), b"not a directory").unwrap();
        assert!(SettingsStore::discover_at(directory.path(), Some(user.clone())).is_err());
        assert!(
            SettingsStore::discover_at(&directory.path().join("missing"), Some(user.clone()))
                .is_err()
        );
        assert!(!user.exists());
    }

    #[test]
    fn portable_settings_and_descendant_output_follow_relocated_directory() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original");
        fs::create_dir(&original).unwrap();
        let store = portable(&original);
        let settings = Settings {
            language: Language::English,
            compress: CompressPreferences {
                jpeg_quality: 73,
                ..Default::default()
            },
            resize: ResizePreferences {
                jpeg_quality: 96,
                percent: 37.0,
                ..Default::default()
            },
            convert: ConvertPreferences {
                format: Format::WebP,
                webp_quality: 82,
                ..Default::default()
            },
            output_directory: original.join("Pictures/finished").to_str().unwrap().into(),
            ..store.defaults()
        };
        store.save(&settings).unwrap();
        let serialized: Settings =
            serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(
            Path::new(&serialized.output_directory),
            Path::new("Pictures/finished")
        );

        let moved = directory.path().join("moved");
        fs::rename(&original, &moved).unwrap();
        let moved_store = portable(&moved);
        let loaded = moved_store.load().unwrap();
        assert_eq!(loaded.settings.language, Language::English);
        assert_eq!(loaded.settings.compress.jpeg_quality, 73);
        assert_eq!(loaded.settings.resize, settings.resize);
        assert_eq!(loaded.settings.convert, settings.convert);
        assert!(loaded.warning.is_none());
        assert_eq!(
            moved_store
                .resolve_output(&loaded.settings.output_directory)
                .unwrap(),
            moved.join("Pictures/finished")
        );
        assert!(!original.exists());
    }

    #[test]
    fn missing_external_output_resets_without_creating_or_rewriting_anything() {
        let directory = tempfile::tempdir().unwrap();
        let store = portable(&directory.path().join("application"));
        let external = directory.path().join("old-machine/finished");
        let settings = Settings {
            output_directory: external.to_str().unwrap().into(),
            ..store.defaults()
        };
        store.save(&settings).unwrap();
        let before = fs::read(store.path()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.settings.output_directory, "Output");
        assert!(loaded.warning.is_some());
        assert!(!external.exists());
        assert_eq!(fs::read(store.path()).unwrap(), before);

        fs::create_dir_all(&external).unwrap();
        let available = store.load().unwrap();
        assert_eq!(Path::new(&available.settings.output_directory), external);
        assert!(available.warning.is_none());
    }

    #[test]
    fn portable_relative_outputs_ignore_cwd_and_allow_explicit_parent_directory() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("USB/tool");
        let store = portable(&root);
        let cwd = std::env::current_dir().unwrap();
        assert_ne!(root, cwd);
        assert_eq!(store.resolve_output("Output").unwrap(), root.join("Output"));
        assert_eq!(
            store.resolve_output("../Pictures/./finished").unwrap(),
            directory.path().join("USB/Pictures/finished")
        );
        assert_eq!(std::env::current_dir().unwrap(), cwd);
        assert!(store.resolve_output("   ").is_err());

        let installed = SettingsStore {
            path: directory.path().join("settings.json"),
            portable_root: None,
        };
        assert_eq!(
            installed.resolve_output("Output").unwrap(),
            cwd.join("Output")
        );
    }

    #[test]
    fn malformed_settings_are_reported_and_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let store = portable(directory.path());
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        let malformed = b"{\"language\":";
        fs::write(store.path(), malformed).unwrap();
        assert!(store.load().is_err());
        assert_eq!(fs::read(store.path()).unwrap(), malformed);
    }

    #[test]
    fn tool_defaults_have_distinct_and_valid_operations() {
        let settings = Settings::default();
        assert_eq!(ToolKind::ALL.map(ToolKind::index), [0, 1, 2]);
        assert_eq!(
            ToolKind::ALL.map(ToolKind::id),
            ["compress", "resize", "convert"]
        );
        let compress = settings.options(ToolKind::Compress);
        assert_eq!(compress.format, None);
        assert_eq!(compress.resize, ResizeSpec::default());
        assert_eq!((compress.jpeg_quality, compress.webp_quality), (85, 80));
        let resize = settings.options(ToolKind::Resize);
        assert_eq!(resize.format, None);
        assert_eq!(resize.resize.mode, ResizeMode::LongestEdge(1920));
        assert!(!resize.resize.allow_upscale);
        assert_eq!((resize.jpeg_quality, resize.webp_quality), (95, 90));
        let convert = settings.options(ToolKind::Convert);
        assert_eq!(convert.format, Some(Format::Jpeg));
        assert_eq!(convert.resize, ResizeSpec::default());
        assert_eq!((convert.jpeg_quality, convert.webp_quality), (85, 80));
        for tool in ToolKind::ALL {
            settings.options(tool).validate().unwrap();
        }
    }

    #[test]
    fn editing_other_tools_cannot_change_effective_options() {
        let original = Settings::default();
        for tool in ToolKind::ALL {
            let expected = original.options(tool);
            let mut changed = original.clone();
            if tool != ToolKind::Compress {
                changed.compress = CompressPreferences {
                    lossless: true,
                    jpeg_quality: 1,
                    webp_quality: 2,
                    strip_metadata: false,
                };
            }
            if tool != ToolKind::Resize {
                changed.resize = ResizePreferences {
                    mode: 3,
                    percent: 400.0,
                    allow_upscale: true,
                    jpeg_quality: 3,
                    webp_quality: 4,
                    strip_metadata: false,
                    background: [1, 2, 3],
                    ..Default::default()
                };
            }
            if tool != ToolKind::Convert {
                changed.convert = ConvertPreferences {
                    format: Format::WebP,
                    webp_lossless: true,
                    jpeg_quality: 5,
                    webp_quality: 6,
                    strip_metadata: false,
                    gif_first_frame: true,
                    background: [4, 5, 6],
                };
            }
            assert_eq!(
                changed.options(tool),
                expected,
                "unrelated preferences leaked into {tool:?}"
            );
        }
    }

    #[test]
    fn webp_lossless_never_becomes_jpeg_lossless_conversion() {
        let mut settings = Settings {
            compress: CompressPreferences {
                lossless: true,
                ..Default::default()
            },
            convert: ConvertPreferences {
                webp_lossless: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            settings.options(ToolKind::Convert).compression,
            Compression::Auto
        );
        settings.convert.format = Format::WebP;
        assert_eq!(
            settings.options(ToolKind::Convert).compression,
            Compression::Lossless
        );
        assert_eq!(
            settings.options(ToolKind::Resize).compression,
            Compression::Auto
        );
    }

    #[test]
    fn tool_snapshots_restore_only_their_own_preferences() {
        let original = Settings {
            language: Language::English,
            output_directory: "Output/custom".into(),
            recursive: false,
            compress: CompressPreferences {
                jpeg_quality: 61,
                lossless: true,
                ..Default::default()
            },
            resize: ResizePreferences {
                mode: 3,
                percent: 31.0,
                ..Default::default()
            },
            convert: ConvertPreferences {
                format: Format::Png,
                background: [11, 22, 33],
                ..Default::default()
            },
        };
        for tool in ToolKind::ALL {
            let snapshot = original.snapshot(tool);
            assert_eq!(snapshot.tool(), tool);
            let mut changed = original.clone();
            changed.reset_tool(tool);
            assert_eq!(changed.snapshot(tool), Settings::default().snapshot(tool));
            for other in ToolKind::ALL.into_iter().filter(|other| *other != tool) {
                assert_eq!(changed.snapshot(other), original.snapshot(other));
            }
            assert_eq!(changed.language, original.language);
            assert_eq!(changed.output_directory, original.output_directory);
            assert_eq!(changed.recursive, original.recursive);
            changed.restore_tool(&snapshot);
            assert_eq!(changed, original);
        }
    }

    #[test]
    fn legacy_migration_preserves_preferences_without_rewriting_until_save() {
        let directory = tempfile::tempdir().unwrap();
        let store = portable(directory.path());
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        let legacy = serde_json::json!({
            "language":"English", "output_directory":directory.path().join("Pictures"),
            "recursive":false, "resize_mode":3, "width":640, "height":480,
            "longest_edge":800, "percent":25.0, "allow_upscale":true,
            "format":3, "jpeg_lossless":true, "webp_lossless":true,
            "jpeg_quality":72, "webp_quality":68, "strip_metadata":false,
            "gif_first_frame":true, "background":[11,22,33]
        });
        let before = serde_json::to_vec(&legacy).unwrap();
        fs::write(store.path(), &before).unwrap();
        let loaded = store.load().unwrap();
        assert!(loaded.warning.is_none());
        assert_eq!(fs::read(store.path()).unwrap(), before);
        let settings = loaded.settings;
        assert_eq!(settings.language, Language::English);
        assert_eq!(settings.output_directory, "Pictures");
        assert!(!settings.recursive);
        assert!(settings.compress.lossless);
        assert_eq!(settings.compress.jpeg_quality, 72);
        assert_eq!(settings.resize.mode, 3);
        assert_eq!(settings.resize.percent, 25.0);
        assert_eq!(settings.resize.jpeg_quality, 72);
        assert_eq!(settings.convert.format, Format::WebP);
        assert!(!settings.convert.webp_lossless);
        assert!(settings.convert.gif_first_frame);
        assert_eq!(settings.convert.background, [11, 22, 33]);
        assert_eq!(
            settings.options(ToolKind::Compress).resize,
            ResizeSpec::default()
        );
        assert_eq!(settings.options(ToolKind::Compress).format, None);
        assert!(!settings.options(ToolKind::Compress).gif_first_frame);
        assert_eq!(settings.options(ToolKind::Resize).format, None);
        assert_eq!(
            settings.options(ToolKind::Resize).compression,
            Compression::Auto
        );
        assert!(!settings.options(ToolKind::Resize).gif_first_frame);
        assert_eq!(
            settings.options(ToolKind::Convert).resize,
            ResizeSpec::default()
        );
        store.save(&settings).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(saved["schema_version"], 2);
        assert!(saved.get("resize_mode").is_none());
        assert!(saved.get("jpeg_lossless").is_none());
        assert!(saved.get("jpeg_quality").is_none());
        assert_eq!(saved["compress"]["lossless"], true);
        assert_eq!(store.load().unwrap().settings, settings);
    }

    #[test]
    fn legacy_keep_settings_get_actionable_tool_defaults() {
        let settings = decode_settings(
            br#"{
            "resize_mode":0,"longest_edge":400,"format":0,
            "jpeg_lossless":true,"webp_lossless":true
        }"#,
        )
        .unwrap();
        assert_eq!(settings.resize.mode, 2);
        assert_eq!(settings.resize.longest_edge, 1920);
        assert_eq!(settings.convert.format, Format::Jpeg);
        assert!(settings.compress.lossless);
        assert!(!settings.convert.webp_lossless);
    }

    #[test]
    fn unknown_or_missing_typed_schema_is_reported_without_replacing_json() {
        let directory = tempfile::tempdir().unwrap();
        let store = portable(directory.path());
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        for value in [
            serde_json::json!({"schema_version":1}),
            serde_json::json!({"schema_version":3,"compress":{"jpeg_quality":34}}),
            serde_json::json!({"schema_version":"2"}),
            serde_json::json!({"schema_version":null}),
            serde_json::json!({"compress":{"jpeg_quality":34}}),
        ] {
            let bytes = serde_json::to_vec(&value).unwrap();
            fs::write(store.path(), &bytes).unwrap();
            assert!(
                store.load().is_err(),
                "accepted unsupported schema: {value}"
            );
            assert_eq!(fs::read(store.path()).unwrap(), bytes);
        }
    }

    #[test]
    fn leftover_flattened_fields_cannot_change_version_two_operations() {
        let settings = decode_settings(
            br#"{
            "schema_version":2,"resize_mode":3,"percent":1,
            "format":3,"jpeg_lossless":true,"gif_first_frame":true,
            "resize":{"mode":3,"percent":25},
            "convert":{"format":"Png"}
        }"#,
        )
        .unwrap();
        assert_eq!(
            settings.options(ToolKind::Compress),
            Settings::default().options(ToolKind::Compress)
        );
        assert_eq!(settings.options(ToolKind::Resize).format, None);
        assert_eq!(
            settings.options(ToolKind::Resize).resize.mode,
            ResizeMode::Percent(25.0)
        );
        assert_eq!(
            settings.options(ToolKind::Convert).format,
            Some(Format::Png)
        );
        assert_eq!(
            settings.options(ToolKind::Convert).resize,
            ResizeSpec::default()
        );
        assert!(!settings.options(ToolKind::Convert).gif_first_frame);
    }

    #[test]
    fn actual_processing_obeys_tool_boundaries_after_other_tools_are_edited() {
        use std::sync::atomic::AtomicBool;
        let directory = tempfile::tempdir().unwrap();
        let image = image::RgbImage::from_fn(64, 48, |x, y| {
            image::Rgb([(x * 3) as u8, (y * 5) as u8, ((x + y) * 2) as u8])
        });
        let settings = Settings {
            compress: CompressPreferences {
                lossless: true,
                ..Default::default()
            },
            resize: ResizePreferences {
                mode: 3,
                percent: 50.0,
                ..Default::default()
            },
            // Stale WebP lossless is intentionally set while choosing JPEG.
            convert: ConvertPreferences {
                format: Format::Jpeg,
                webp_lossless: true,
                gif_first_frame: true,
                ..Default::default()
            },
            ..Default::default()
        };
        for extension in ["png", "jpg"] {
            let source = directory.path().join(format!("source.{extension}"));
            image.save(&source).unwrap();
            let original = fs::read(&source).unwrap();
            let source_format = local_image_core::probe(&source).unwrap().format;
            for tool in ToolKind::ALL {
                let destination = directory
                    .path()
                    .join(format!("{extension}-{}.tmp", tool.id()));
                let options = settings.options(tool);
                let report = local_image_core::process_to_path(
                    &source,
                    &destination,
                    &options,
                    &AtomicBool::new(false),
                )
                .unwrap();
                let expected_format = if tool == ToolKind::Convert {
                    Format::Jpeg
                } else {
                    source_format
                };
                let expected_dimensions = if tool == ToolKind::Resize {
                    (32, 24)
                } else {
                    (64, 48)
                };
                assert_eq!(report.format, expected_format, "{extension} {tool:?}");
                assert_eq!(
                    (report.width, report.height),
                    expected_dimensions,
                    "{extension} {tool:?}"
                );
                let output = image::ImageReader::open(&destination)
                    .unwrap()
                    .with_guessed_format()
                    .unwrap()
                    .decode()
                    .unwrap();
                assert_eq!((output.width(), output.height()), expected_dimensions);
            }
            assert_eq!(fs::read(&source).unwrap(), original);
        }
    }

    #[test]
    fn read_only_settings_return_error_without_removing_previous_json() {
        let directory = tempfile::tempdir().unwrap();
        let store = portable(directory.path());
        store.save(&store.defaults()).unwrap();
        let before = fs::read(store.path()).unwrap();
        let permissions = fs::metadata(store.path()).unwrap().permissions();
        let mut read_only = permissions.clone();
        read_only.set_readonly(true);
        fs::set_permissions(store.path(), read_only).unwrap();
        let changed = Settings {
            language: Language::English,
            ..store.defaults()
        };
        let result = store.save(&changed);
        let after = fs::read(store.path()).unwrap();
        fs::set_permissions(store.path(), permissions).unwrap();
        assert!(result.is_err());
        assert_eq!(before, after);
        assert_eq!(
            fs::read_dir(store.path().parent().unwrap())
                .unwrap()
                .count(),
            1
        );
    }

    #[cfg(windows)]
    #[test]
    fn failed_windows_replace_preserves_existing_settings_and_cleans_temporary() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory = tempfile::tempdir().unwrap();
        let store = portable(directory.path());
        store.save(&store.defaults()).unwrap();
        let before = fs::read(store.path()).unwrap();
        // Permit reads while refusing replacement/deletion of the existing file.
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(store.path())
            .unwrap();
        let changed = Settings {
            language: Language::English,
            ..store.defaults()
        };
        let result = store.save(&changed);
        assert!(result.is_err());
        assert_eq!(fs::read(store.path()).unwrap(), before);
        assert_eq!(
            fs::read_dir(store.path().parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        drop(locked);
        store.save(&changed).unwrap();
        assert_eq!(store.load().unwrap().settings.language, Language::English);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_reject_ambiguous_and_device_forms() {
        let store = portable(Path::new(r"E:\Portable\Tool"));
        for path in [
            r"C:folder",
            r"C:",
            r"\folder",
            r"/folder",
            r"\\.\C:\folder",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\folder",
        ] {
            assert!(store.resolve_output(path).is_err(), "accepted {path}");
        }
        assert_eq!(
            store.resolve_output(r"C:\Pictures").unwrap(),
            PathBuf::from(r"C:\Pictures")
        );
        assert_eq!(
            store.resolve_output(r"\\server\share\Pictures").unwrap(),
            PathBuf::from(r"\\server\share\Pictures")
        );
        assert_eq!(
            store.resolve_output(r"\\?\C:\Pictures").unwrap(),
            PathBuf::from(r"C:\Pictures")
        );
        assert_eq!(
            store
                .resolve_output(r"\\?\UNC\server\share\Pictures")
                .unwrap(),
            PathBuf::from(r"\\server\share\Pictures")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_descendants_match_case_and_extended_prefixes_across_drive_moves() {
        let first = portable(Path::new(r"E:\Portable\Tool"));
        let saved = first
            .stored_output(r"\\?\e:\PORTABLE\tool\Pictures\finished")
            .unwrap();
        assert_eq!(Path::new(&saved), Path::new(r"Pictures\finished"));
        let moved = portable(Path::new(r"F:\Moved\Tool"));
        assert_eq!(
            moved.resolve_output(&saved).unwrap(),
            PathBuf::from(r"F:\Moved\Tool\Pictures\finished")
        );
        assert_eq!(
            first
                .stored_output(r"E:\Portable\Toolbox\Pictures")
                .unwrap(),
            r"E:\Portable\Toolbox\Pictures"
        );
    }
}
