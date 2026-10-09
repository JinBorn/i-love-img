#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod controls;
mod diagnostics;
mod graphics;
mod preview;
#[cfg(windows)]
mod primer;
mod settings;
mod theme;
mod tool;
mod tool_gallery;
mod worker;

fn main() -> eframe::Result {
    #[cfg(windows)]
    primer::run_before_graphics();

    let diagnostic = std::env::var_os("LOCAL_IMAGE_TOOLS_DIAGNOSTICS").is_some();
    let mut window_size = [1220.0_f32, 820.0_f32];
    if diagnostic {
        if let Ok(size) = std::env::var("LIT_DIAGNOSTIC_WINDOW_SIZE") {
            let values: Vec<f32> = size
                .split(['x', ','])
                .filter_map(|v| v.parse().ok())
                .collect();
            if values.len() == 2 && values.iter().all(|v| v.is_finite() && *v >= 200.0) {
                window_size = [values[0], values[1]];
            }
        }
        if let Some(scale) = std::env::var("LIT_DIAGNOSTIC_SCALE")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite() && (0.25..=3.0).contains(value))
        {
            window_size = [window_size[0] * scale, window_size[1] * scale];
        }
    }
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon.png"))
        .expect("bundled application icon");
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("本地图像工具 · Local Image Tools")
            .with_inner_size(window_size)
            .with_min_inner_size(if diagnostic {
                [200.0, 200.0]
            } else {
                [1000.0, 720.0]
            })
            .with_icon(icon),
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        vsync: true,
        ..Default::default()
    };
    #[cfg(windows)]
    if primer::is_child() {
        return primer::run_child(options);
    }
    let paths = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .collect();
    eframe::run_native(
        "Local Image Tools",
        options,
        Box::new(move |creation| {
            graphics::initialize(creation);
            if std::env::var_os("LOCAL_IMAGE_TOOLS_DIAGNOSTICS").is_some()
                && std::env::var_os("LOCAL_IMAGE_TOOLS_DIAGNOSTICS_MINIMAL").is_some()
            {
                Ok(Box::new(diagnostics::MinimalProbe::new(creation)))
            } else {
                Ok(Box::new(app::ImageToolsApp::new(creation, paths)))
            }
        }),
    )
}
