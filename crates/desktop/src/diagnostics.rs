//! Opt-in native GUI diagnostics. Inactive unless its environment variable is set.
use crate::{
    settings::{Language, Settings},
    tool::ToolKind,
};
use eframe::egui;
use local_image_core::Format;
use serde::Deserialize;
use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant},
};

static DIAGNOSTIC_START: OnceLock<Instant> = OnceLock::new();

#[derive(Clone, Deserialize, Default)]
#[serde(default)]
pub struct Step {
    pub action: String,
    pub tool: Option<ToolKind>,
    pub resize_percent: Option<f64>,
    pub format: Option<String>,
    pub lossless: Option<bool>,
    pub jpeg_quality: Option<u8>,
    pub webp_quality: Option<u8>,
    pub strip_metadata: Option<bool>,
    pub gif_first_frame: Option<bool>,
    pub cancel_after_ms: Option<u64>,
}

impl Step {
    /// Navigation/actions are handled by the app. Applying a diagnostic step
    /// has the same tool boundary as editing that tool's visible preferences.
    pub fn apply(&self, settings: &mut Settings, tool: ToolKind) {
        let (jpeg_quality, webp_quality, strip_metadata) = match tool {
            ToolKind::Compress => {
                if let Some(lossless) = self.lossless {
                    settings.compress.lossless = lossless;
                }
                (
                    &mut settings.compress.jpeg_quality,
                    &mut settings.compress.webp_quality,
                    &mut settings.compress.strip_metadata,
                )
            }
            ToolKind::Resize => {
                if let Some(percent) = self.resize_percent {
                    settings.resize.mode = 3;
                    settings.resize.percent = percent;
                }
                (
                    &mut settings.resize.jpeg_quality,
                    &mut settings.resize.webp_quality,
                    &mut settings.resize.strip_metadata,
                )
            }
            ToolKind::Convert => {
                if let Some(format) = &self.format {
                    let format = match format.to_ascii_lowercase().as_str() {
                        "jpeg" | "jpg" => Some(Format::Jpeg),
                        "png" => Some(Format::Png),
                        "webp" => Some(Format::WebP),
                        "gif" => Some(Format::Gif),
                        _ => None,
                    };
                    if let Some(format) = format {
                        settings.convert.format = format;
                    }
                }
                if let Some(lossless) = self.lossless {
                    settings.convert.webp_lossless = lossless;
                }
                if let Some(first_frame) = self.gif_first_frame {
                    settings.convert.gif_first_frame = first_frame;
                }
                (
                    &mut settings.convert.jpeg_quality,
                    &mut settings.convert.webp_quality,
                    &mut settings.convert.strip_metadata,
                )
            }
        };
        if let Some(quality) = self.jpeg_quality {
            *jpeg_quality = quality;
        }
        if let Some(quality) = self.webp_quality {
            *webp_quality = quality;
        }
        if let Some(strip) = self.strip_metadata {
            *strip_metadata = strip;
        }
    }
}

#[derive(Deserialize)]
#[serde(default)]
pub struct Config {
    pub tool: Option<ToolKind>,
    pub input_paths: Vec<PathBuf>,
    pub output_directory: String,
    pub language: String,
    pub steps: Vec<Step>,
    pub capture_directory: PathBuf,
    pub idle_ms: u64,
    pub settle_ms: u64,
    pub exit_when_done: bool,
    /// Explicit opt-in for portable settings integration tests.
    pub use_saved_settings: bool,
    pub save_settings_on_exit: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            tool: None,
            input_paths: Vec::new(),
            output_directory: String::new(),
            language: "zh".into(),
            steps: Vec::new(),
            capture_directory: PathBuf::from("artifacts/gui"),
            idle_ms: 1500,
            settle_ms: 800,
            exit_when_done: true,
            use_saved_settings: false,
            save_settings_on_exit: false,
        }
    }
}

pub struct Diagnostics {
    created_at: Instant,
    pub config: Config,
    pub phase: u8,
    pub index: usize,
    pub deadline: Instant,
    pub step_started: Instant,
    pub screenshot_received: bool,
    pub complete: bool,
    pub last_frame: Instant,
    pub max_processing_update_ms: f64,
    pub max_processing_cpu_ms: f64,
    pub max_processing_tick_gap_ms: f64,
    pub cancel_requested: Option<Instant>,
    pub cancel_accepted_ms: Option<f64>,
    pub cancel_closed_ms: Option<f64>,
}

impl Diagnostics {
    pub fn from_environment() -> Option<Self> {
        let path = std::env::var_os("LOCAL_IMAGE_TOOLS_DIAGNOSTICS")?;
        let config: Config = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        fs::create_dir_all(&config.capture_directory).ok()?;
        let deadline = Instant::now() + Duration::from_millis(config.idle_ms);
        Some(Self {
            created_at: *DIAGNOSTIC_START.get_or_init(Instant::now),
            config,
            phase: 0,
            index: 0,
            deadline,
            step_started: Instant::now(),
            screenshot_received: false,
            complete: false,
            last_frame: Instant::now(),
            max_processing_update_ms: 0.0,
            max_processing_cpu_ms: 0.0,
            max_processing_tick_gap_ms: 0.0,
            cancel_requested: None,
            cancel_accepted_ms: None,
            cancel_closed_ms: None,
        })
    }
    pub fn settings(&self) -> Settings {
        Settings {
            language: if self.config.language == "en" {
                Language::English
            } else {
                Language::Chinese
            },
            output_directory: self.config.output_directory.clone(),
            ..Default::default()
        }
    }
    pub fn log(&self, mut value: serde_json::Value) {
        if let Some(object) = value.as_object_mut() {
            object.insert("process_id".into(), serde_json::json!(std::process::id()));
            let process_role = "parent";
            #[cfg(windows)]
            let process_role = if crate::primer::is_child() {
                "primer-child"
            } else {
                process_role
            };
            object.insert("process_role".into(), serde_json::json!(process_role));
            object.insert(
                "diagnostic_elapsed_ms".into(),
                serde_json::json!(self.created_at.elapsed().as_secs_f64() * 1000.0),
            );
        }
        if let Ok(file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.config.capture_directory.join("run.jsonl"))
        {
            // serde_json::Value's Display writes many tiny fragments. Buffer
            // them so a thousand-row diagnostic snapshot does not issue tens
            // of thousands of synchronous file writes on the UI thread.
            let mut file = BufWriter::with_capacity(64 * 1024, file);
            let _ = writeln!(file, "{value}");
            let _ = file.flush();
        }
    }
    pub fn capture(&mut self, ctx: &egui::Context, name: String) {
        self.screenshot_received = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(name)));
    }
    pub fn receive_screenshots(&mut self, ctx: &egui::Context) {
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Screenshot {
                user_data, image, ..
            } = event
            {
                let Some(name) = user_data
                    .data
                    .as_ref()
                    .and_then(|d| d.downcast_ref::<String>())
                else {
                    continue;
                };
                let path = self.config.capture_directory.join(format!("{name}.png"));
                let pixels: Vec<u8> = image
                    .pixels
                    .iter()
                    .flat_map(|pixel| pixel.to_array())
                    .collect();
                let result = image::save_buffer(
                    &path,
                    &pixels,
                    image.width() as u32,
                    image.height() as u32,
                    image::ColorType::Rgba8,
                );
                self.log(serde_json::json!({"event":"screenshot","path":path,"ok":result.is_ok(),"error":result.err().map(|e|e.to_string())}));
                self.screenshot_received = true;
            }
        }
    }
}

pub fn record_renderer(cc: &eframe::CreationContext<'_>, diagnostics: &Diagnostics) {
    use eframe::glow::HasContext;
    if let Some(gl) = &cc.gl {
        // The native creation callback runs with its GL context current.
        let (vendor, renderer, version) = unsafe {
            (
                gl.get_parameter_string(eframe::glow::VENDOR),
                gl.get_parameter_string(eframe::glow::RENDERER),
                gl.get_parameter_string(eframe::glow::VERSION),
            )
        };
        diagnostics.log(serde_json::json!({"event":"renderer","vendor":vendor,"renderer":renderer,"version":version}));
    }
}

/// Same eframe/GL window without application fonts, widgets, or worker threads.
/// Only enabled together with LOCAL_IMAGE_TOOLS_DIAGNOSTICS.
pub struct MinimalProbe {
    diagnostics: Diagnostics,
}
impl MinimalProbe {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let diagnostics = Diagnostics::from_environment().expect("valid diagnostic configuration");
        record_renderer(cc, &diagnostics);
        Self { diagnostics }
    }
}
impl eframe::App for MinimalProbe {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Native renderer memory probe");
        });
        self.diagnostics.receive_screenshots(ctx);
        if self.diagnostics.phase == 0 && Instant::now() >= self.diagnostics.deadline {
            self.diagnostics.log(
                serde_json::json!({"event":"ready","font_atlas":ctx.fonts(|f|f.font_image_size())}),
            );
            self.diagnostics.capture(ctx, "00-minimal".into());
            self.diagnostics.phase = 1;
        } else if self.diagnostics.phase == 1 && self.diagnostics.screenshot_received {
            self.diagnostics.complete = true;
            self.diagnostics.phase = 6;
            self.diagnostics
                .log(serde_json::json!({"event":"complete"}));
            if self.diagnostics.config.exit_when_done {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if !self.diagnostics.complete {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_actions_and_tool_selection_deserialize_without_mutating_defaults() {
        let default: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(default.tool, None);
        assert!(!default.use_saved_settings);
        assert!(!default.save_settings_on_exit);
        for action in ["process", "continue", "readjust", "open", "home"] {
            let step: Step = serde_json::from_value(serde_json::json!({
                "action":action,"tool":"resize"
            }))
            .unwrap();
            assert_eq!(step.action, action);
            assert_eq!(step.tool, Some(ToolKind::Resize));
            let mut settings = Settings::default();
            step.apply(&mut settings, ToolKind::Resize);
            assert_eq!(settings, Settings::default());
        }
    }

    #[test]
    fn diagnostic_parameters_apply_only_to_the_selected_profile() {
        let step = Step {
            resize_percent: Some(25.0),
            format: Some("webp".into()),
            lossless: Some(true),
            jpeg_quality: Some(61),
            webp_quality: Some(62),
            strip_metadata: Some(false),
            gif_first_frame: Some(true),
            ..Default::default()
        };
        let original = Settings {
            language: Language::English,
            output_directory: "Output/testing".into(),
            recursive: false,
            ..Default::default()
        };
        for tool in ToolKind::ALL {
            let mut settings = original.clone();
            step.apply(&mut settings, tool);
            for other in ToolKind::ALL.into_iter().filter(|other| *other != tool) {
                assert_eq!(settings.snapshot(other), original.snapshot(other));
            }
            assert_eq!(settings.language, original.language);
            assert_eq!(settings.output_directory, original.output_directory);
            assert_eq!(settings.recursive, original.recursive);
            let options = settings.options(tool);
            assert_eq!(options.jpeg_quality, 61);
            assert_eq!(options.webp_quality, 62);
            assert!(!options.strip_metadata);
            match tool {
                ToolKind::Compress => {
                    assert_eq!(options.format, None);
                    assert_eq!(options.resize.mode, local_image_core::ResizeMode::Keep);
                    assert!(!options.gif_first_frame);
                }
                ToolKind::Resize => {
                    assert_eq!(options.format, None);
                    assert_eq!(
                        options.resize.mode,
                        local_image_core::ResizeMode::Percent(25.0)
                    );
                    assert_eq!(options.compression, local_image_core::Compression::Auto);
                    assert!(!options.gif_first_frame);
                }
                ToolKind::Convert => {
                    assert_eq!(options.format, Some(Format::WebP));
                    assert_eq!(options.resize.mode, local_image_core::ResizeMode::Keep);
                    assert!(options.gif_first_frame);
                }
            }
        }
    }
}
