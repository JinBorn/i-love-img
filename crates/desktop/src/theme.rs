use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, TextStyle};

pub const INK: Color32 = Color32::from_rgb(35, 40, 49);
pub const MUTED: Color32 = Color32::from_rgb(113, 120, 135);
pub const ACCENT: Color32 = Color32::from_rgb(66, 103, 232);
pub const PALE: Color32 = Color32::from_rgb(237, 242, 255);
pub const LINE: Color32 = Color32::from_rgb(229, 232, 239);

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "noto-cjk".into(),
        FontData::from_static(include_bytes!("../../../assets/fonts/NotoSansSC-UI.otf")).into(),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .push("noto-cjk".into());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("noto-cjk".into());
    ctx.set_fonts(fonts);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.override_text_color = Some(INK);
    style.visuals.panel_fill = Color32::WHITE;
    style.visuals.window_fill = Color32::WHITE;
    style.visuals.extreme_bg_color = Color32::from_rgb(245, 247, 250);
    style.visuals.faint_bg_color = Color32::from_rgb(248, 249, 252);
    style.visuals.selection.bg_fill = PALE;
    style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT);
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(247, 248, 251);
    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(247, 248, 251);
    style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, LINE);
    style.visuals.widgets.hovered.bg_fill = PALE;
    style.visuals.widgets.hovered.weak_bg_fill = PALE;
    style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
    style.visuals.widgets.active.bg_fill = PALE;
    style.visuals.widgets.active.weak_bg_fill = PALE;
    style.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(6);
    style.visuals.widgets.open.corner_radius = egui::CornerRadius::same(6);
    style.spacing.item_spacing = egui::vec2(9.0, 10.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.spacing.interact_size.y = 30.0;
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(22.0));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(12.0));
    ctx.set_style(style);
}

pub fn label(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(egui::RichText::new(title).strong().size(15.0));
    if !subtitle.is_empty() {
        ui.label(egui::RichText::new(subtitle).small().color(MUTED));
    }
    ui.add_space(4.0);
}

pub fn primary(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    enabled: bool,
    size: egui::Vec2,
) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(text)
            .fill(ACCENT)
            .stroke(egui::Stroke::NONE)
            .corner_radius(8)
            .min_size(size),
    )
}

pub fn bytes(size: u64) -> String {
    if size >= 1024 * 1024 * 1024 {
        format!("{:.2} GiB", size as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if size >= 1024 * 1024 {
        format!("{:.2} MiB", size as f64 / (1024.0 * 1024.0))
    } else if size >= 1024 {
        format!("{:.1} KiB", size as f64 / 1024.0)
    } else {
        format!("{size} B")
    }
}
