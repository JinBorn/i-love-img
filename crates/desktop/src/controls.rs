use crate::theme::{ACCENT, INK, LINE, MUTED};
use eframe::egui::{self, Align, Color32, Layout, Response, RichText, Stroke};
use std::ops::RangeInclusive;

/// A full-width quality control with a directly editable value above the rail.
pub fn quality_slider(ui: &mut egui::Ui, label: &str, value: &mut u8) -> Response {
    labelled_slider(ui, label, value, 1..=100, "")
}

/// Uses the same interaction and visual treatment as the quality controls.
pub fn percent_slider(ui: &mut egui::Ui, label: &str, value: &mut f64) -> Response {
    labelled_slider(ui, label, value, 1.0..=200.0, " %")
}

fn labelled_slider<Num: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Num,
    range: RangeInclusive<Num>,
    suffix: &str,
) -> Response {
    ui.push_id(label, |ui| {
        ui.scope(|ui| {
            ui.spacing_mut().interact_size.y = 24.0;
            ui.spacing_mut().item_spacing.y = 2.0;
            let heading = ui.horizontal(|ui| {
                let label_response = ui.label(RichText::new(label).size(13.0).color(INK));
                let number = ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(
                        egui::DragValue::new(value)
                            .range(range.clone())
                            .speed(1.0)
                            .fixed_decimals(0)
                            .suffix(suffix),
                    )
                    .labelled_by(label_response.id)
                });
                (label_response.id, number.inner)
            });
            let (label_id, number_response) = heading.inner;
            let width = ui.available_width();
            ui.spacing_mut().slider_width = width;
            ui.spacing_mut().slider_rail_height = 4.0;
            let visuals = ui.visuals_mut();
            visuals.selection.bg_fill = ACCENT;
            visuals.widgets.inactive.bg_fill = Color32::from_rgb(231, 235, 242);
            visuals.widgets.inactive.fg_stroke = Stroke::new(1.5_f32, ACCENT);
            visuals.widgets.hovered.bg_fill = Color32::WHITE;
            visuals.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, ACCENT);
            visuals.widgets.active.bg_fill = Color32::WHITE;
            visuals.widgets.active.fg_stroke = Stroke::new(1.5_f32, ACCENT);
            visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, MUTED);
            // egui 0.32 derives a slider's circle radius from its interaction
            // height / 2.5. Keep the 24 px interaction target, but draw a 12 px
            // circle without growing it when hovered or dragged.
            for state in [
                &mut visuals.widgets.inactive,
                &mut visuals.widgets.hovered,
                &mut visuals.widgets.active,
                &mut visuals.widgets.noninteractive,
                &mut visuals.widgets.open,
            ] {
                state.expansion = -3.6;
                state.corner_radius = egui::CornerRadius::same(2);
            }
            let mut response = ui
                .add(
                    egui::Slider::new(value, range)
                        .show_value(false)
                        .trailing_fill(true)
                        .step_by(1.0)
                        .fixed_decimals(0),
                )
                .labelled_by(label_id);
            if number_response.changed() {
                response.mark_changed();
            }
            response.union(number_response)
        })
        .inner
    })
    .inner
}

/// A quiet progress rail; the caller keeps the count and status outside it.
pub fn thin_progress(ui: &mut egui::Ui, fraction: f32, width: f32, color: Color32) -> Response {
    ui.scope(|ui| {
        ui.visuals_mut().extreme_bg_color = LINE;
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(width.min(ui.available_width()).max(0.0))
                .desired_height(4.0)
                .corner_radius(2)
                .fill(color),
        )
    })
    .inner
}
