use crate::{
    settings::Language,
    theme::{ACCENT, INK, LINE, MUTED},
    tool::ToolKind,
};
use eframe::egui::{self, Color32, FontId, Pos2, RichText, Sense, Stroke, Vec2};

const TOOLS: [ToolKind; 3] = [ToolKind::Compress, ToolKind::Resize, ToolKind::Convert];

pub fn title(tool: ToolKind, language: Language) -> &'static str {
    match tool {
        ToolKind::Compress => language.text("压缩图片", "Compress images"),
        ToolKind::Resize => language.text("调整尺寸", "Resize images"),
        ToolKind::Convert => language.text("转换格式", "Convert images"),
    }
}

pub fn description(tool: ToolKind, language: Language) -> &'static str {
    match tool {
        ToolKind::Compress => language.text(
            "优化图片文件体积，保持原有像素尺寸和格式。",
            "Reduce image file sizes while keeping their dimensions and formats.",
        ),
        ToolKind::Resize => language.text(
            "按像素或比例批量调整尺寸，保持宽高比与原有格式。",
            "Resize by pixels or percentage, keeping aspect ratios and original formats.",
        ),
        ToolKind::Convert => language.text(
            "批量转换 JPG、PNG、WebP 和 GIF，保持原有像素尺寸。",
            "Convert between JPG, PNG, WebP and GIF, keeping the original dimensions.",
        ),
    }
}

pub fn action_label(tool: ToolKind, language: Language) -> &'static str {
    match tool {
        ToolKind::Compress => language.text("压缩图片", "Compress images"),
        ToolKind::Resize => language.text("调整尺寸", "Resize images"),
        ToolKind::Convert => language.text("转换格式", "Convert images"),
    }
}

pub fn color(tool: ToolKind) -> Color32 {
    match tool {
        ToolKind::Compress => Color32::from_rgb(230, 171, 37),
        ToolKind::Resize => Color32::from_rgb(51, 127, 216),
        ToolKind::Convert => Color32::from_rgb(133, 85, 195),
    }
}

fn pale_color(tool: ToolKind) -> Color32 {
    match tool {
        ToolKind::Compress => Color32::from_rgb(255, 247, 226),
        ToolKind::Resize => Color32::from_rgb(235, 244, 255),
        ToolKind::Convert => Color32::from_rgb(245, 238, 255),
    }
}

pub fn icon(ui: &mut egui::Ui, tool: ToolKind, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_icon(ui.painter(), rect, tool);
}

fn paint_icon(painter: &egui::Painter, rect: egui::Rect, tool: ToolKind) {
    let size = rect.width().min(rect.height());
    painter.rect_filled(rect, (size * 0.24).round() as u8, pale_color(tool));
    let point = |x: f32, y: f32| rect.left_top() + egui::vec2(size * x, size * y);
    let stroke = Stroke::new((size * 0.039).max(1.6), color(tool));
    let arrow = |from: Pos2, to: Pos2| {
        painter.line_segment([from, to], stroke);
        let direction = (from - to).normalized();
        let perpendicular = egui::vec2(-direction.y, direction.x);
        let length = size * 0.11;
        painter.line_segment(
            [to, to + direction * length + perpendicular * length * 0.7],
            stroke,
        );
        painter.line_segment(
            [to, to + direction * length - perpendicular * length * 0.7],
            stroke,
        );
    };
    match tool {
        ToolKind::Compress => {
            arrow(point(0.23, 0.23), point(0.43, 0.43));
            arrow(point(0.77, 0.23), point(0.57, 0.43));
            arrow(point(0.23, 0.77), point(0.43, 0.57));
            arrow(point(0.77, 0.77), point(0.57, 0.57));
        }
        ToolKind::Resize => {
            painter.line_segment([point(0.24, 0.40), point(0.24, 0.76)], stroke);
            painter.line_segment([point(0.24, 0.76), point(0.60, 0.76)], stroke);
            painter.line_segment([point(0.24, 0.40), point(0.42, 0.40)], stroke);
            painter.line_segment([point(0.60, 0.76), point(0.60, 0.58)], stroke);
            arrow(point(0.47, 0.53), point(0.77, 0.23));
            painter.line_segment([point(0.58, 0.23), point(0.77, 0.23)], stroke);
            painter.line_segment([point(0.77, 0.23), point(0.77, 0.42)], stroke);
        }
        ToolKind::Convert => {
            arrow(point(0.24, 0.35), point(0.76, 0.35));
            arrow(point(0.76, 0.65), point(0.24, 0.65));
            painter.line_segment([point(0.24, 0.35), point(0.24, 0.49)], stroke);
            painter.line_segment([point(0.76, 0.65), point(0.76, 0.51)], stroke);
        }
    }
}

fn card(ui: &mut egui::Ui, tool: ToolKind, language: Language, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 228.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title(tool, language))
    });
    let hovered = response.hovered() || response.has_focus();
    ui.painter().rect_filled(
        rect,
        14,
        if hovered {
            pale_color(tool)
        } else {
            Color32::WHITE
        },
    );
    ui.painter().rect_stroke(
        rect,
        14,
        Stroke::new(
            if hovered { 1.5_f32 } else { 1.0_f32 },
            if hovered { color(tool) } else { LINE },
        ),
        egui::StrokeKind::Inside,
    );
    let origin = rect.left_top() + egui::vec2(24.0, 23.0);
    paint_icon(
        ui.painter(),
        egui::Rect::from_min_size(origin, Vec2::splat(54.0)),
        tool,
    );
    let label = ui.painter().layout(
        title(tool, language).to_owned(),
        FontId::proportional(21.0),
        INK,
        width - 48.0,
    );
    ui.painter()
        .galley(origin + egui::vec2(0.0, 71.0), label, INK);
    let description = ui.painter().layout(
        description(tool, language).to_owned(),
        FontId::proportional(14.0),
        MUTED,
        width - 48.0,
    );
    ui.painter()
        .galley(origin + egui::vec2(0.0, 109.0), description, MUTED);
    let arrow_end = rect.right_bottom() - egui::vec2(24.0, 24.0);
    let arrow_start = arrow_end - egui::vec2(16.0, 0.0);
    let stroke = Stroke::new(1.7_f32, color(tool));
    ui.painter().line_segment([arrow_start, arrow_end], stroke);
    ui.painter()
        .line_segment([arrow_end - egui::vec2(5.0, 5.0), arrow_end], stroke);
    ui.painter()
        .line_segment([arrow_end - egui::vec2(5.0, -5.0), arrow_end], stroke);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

pub fn home(
    ctx: &egui::Context,
    language: Language,
    search: &mut String,
    pending_imports: usize,
) -> Option<ToolKind> {
    let mut selected = None;
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(Color32::from_rgb(247, 249, 251)).inner_margin(egui::Margin::symmetric(32, 24)))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().id_salt("tool-gallery").show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.set_max_width(1060.0);
                    ui.add_space(31.0);
                    ui.label(RichText::new(language.text("每次选择一个工具，轻松处理一批图片", "Simple tools for your everyday images")).size(31.0).strong());
                    ui.add_space(10.0);
                    ui.label(RichText::new(language.text(
                        "压缩、调整尺寸或转换格式。各自独立，按需使用。",
                        "Compress, resize or convert. Choose a tool and get straight to work.",
                    )).size(16.0).color(MUTED));
                    ui.add_space(27.0);
                    ui.add_sized(
                        [330.0_f32.min(ui.available_width()), 38.0],
                        egui::TextEdit::singleline(search).hint_text(language.text("搜索工具", "Search tools")).margin(egui::vec2(12.0, 9.0)),
                    );
                    ui.add_space(24.0);
                    if pending_imports > 0 {
                        let notice = if language == Language::Chinese {
                            format!("已接收 {pending_imports} 个文件或文件夹，选择工具后导入。")
                        } else {
                            format!("{pending_imports} files or folders waiting. Choose a tool to import them.")
                        };
                        ui.label(RichText::new(notice).color(INK));
                        ui.add_space(17.0);
                    }
                    let query = search.trim().to_lowercase();
                    let visible: Vec<_> = TOOLS.into_iter().filter(|tool| {
                        query.is_empty()
                            || title(*tool, language).to_lowercase().contains(&query)
                            || description(*tool, language).to_lowercase().contains(&query)
                            || title(*tool, Language::English).to_lowercase().contains(&query)
                            || title(*tool, Language::Chinese).contains(&query)
                    }).collect();
                    let available = ui.available_width();
                    let columns = if available >= 870.0 { 3 } else if available >= 570.0 { 2 } else { 1 };
                    let gap = 18.0;
                    let card_width = (available - gap * (columns - 1) as f32) / columns as f32;
                    for row in visible.chunks(columns) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            for &tool in row {
                                if card(ui, tool, language, card_width) {
                                    selected = Some(tool);
                                }
                            }
                        });
                        ui.add_space(8.0);
                    }
                    if visible.is_empty() {
                        ui.add_space(22.0);
                        ui.label(RichText::new(language.text("没有找到匹配的工具。", "No matching tools found.")).color(MUTED));
                        if ui.link(language.text("显示全部工具", "Show all tools")).clicked() {
                            search.clear();
                        }
                        ui.add_space(24.0);
                    }
                    ui.add_space(30.0);
                    ui.label(RichText::new(language.text(
                        "完全本地处理  ·  支持批量操作  ·  原图始终保留",
                        "Entirely on your device  ·  Batch processing  ·  Originals always preserved",
                    )).size(13.0).color(MUTED));
                    ui.add_space(20.0);
                });
            });
        });
    selected
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandingAction {
    None,
    ChooseImages,
    ChooseFolder,
}

pub fn landing(
    ctx: &egui::Context,
    language: Language,
    tool: ToolKind,
    busy: bool,
    recursive: &mut bool,
) -> LandingAction {
    let mut action = LandingAction::None;
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(Color32::from_rgb(247, 249, 251))
                .inner_margin(egui::Margin::symmetric(32, 28)),
        )
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("tool-landing")
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(850.0);
                        ui.add_space(20.0);
                        icon(ui, tool, 52.0);
                        ui.add_space(15.0);
                        ui.label(RichText::new(title(tool, language)).size(34.0).strong());
                        ui.add_space(9.0);
                        ui.label(
                            RichText::new(description(tool, language))
                                .size(17.0)
                                .color(MUTED),
                        );
                        ui.add_space(28.0);
                        if ui
                            .add_enabled(
                                !busy,
                                egui::Button::new(
                                    RichText::new(language.text("选择图片", "Select images"))
                                        .size(21.0)
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(ACCENT)
                                .stroke(Stroke::NONE)
                                .corner_radius(8)
                                .min_size(egui::vec2(280.0_f32.min(ui.available_width()), 60.0)),
                            )
                            .clicked()
                        {
                            action = LandingAction::ChooseImages;
                        }
                        ui.add_space(12.0);
                        if ui
                            .add_enabled(
                                !busy,
                                egui::Button::new(
                                    language.text("选择整个文件夹", "Choose a folder"),
                                )
                                .min_size(egui::vec2(190.0, 37.0)),
                            )
                            .clicked()
                        {
                            action = LandingAction::ChooseFolder;
                        }
                        ui.add_space(3.0);
                        ui.add_enabled(
                            !busy,
                            egui::Checkbox::new(
                                recursive,
                                language.text("包括子目录", "Include subfolders"),
                            ),
                        );
                        ui.add_space(15.0);
                        if busy {
                            ui.spinner();
                            ui.label(
                                RichText::new(language.text("正在准备图片…", "Preparing images…"))
                                    .color(MUTED),
                            );
                        } else {
                            ui.label(
                                RichText::new(language.text(
                                    "也可以将图片或文件夹拖到这里",
                                    "or drop images or folders here",
                                ))
                                .color(MUTED),
                            );
                        }
                        ui.add_space(24.0);
                        ui.label(
                            RichText::new(language.text(
                                "JPG / JPEG · PNG · GIF · 静态 WebP",
                                "JPG / JPEG · PNG · GIF · Static WebP",
                            ))
                            .small()
                            .color(MUTED),
                        );
                        ui.add_space(9.0);
                        ui.label(
                            RichText::new(language.text(
                                "批量处理，无需上传。原图始终保留。",
                                "Batch processing, without uploads. Originals always preserved.",
                            ))
                            .small()
                            .color(MUTED),
                        );
                        ui.add_space(24.0);
                    });
                });
        });
    action
}
