use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Response, Sense, Stroke, Ui, Vec2,
};

use super::{
    design_tokens::{self as tokens, InterfaceMetrics},
    pixel_snap,
    toolbar::{ToolState, UiCommand, color32},
};
use crate::{
    ink::{EraserSize, InkColor, PenWidth},
    settings::{LogLevel, PalmSizePreset},
};

const COLORS: [InkColor; 6] = [
    InkColor::Red,
    InkColor::Yellow,
    InkColor::Blue,
    InkColor::Green,
    InkColor::Black,
    InkColor::White,
];
const PEN_WIDTHS: [PenWidth; 4] = [PenWidth::Px4, PenWidth::Px6, PenWidth::Px8, PenWidth::Px16];
const ERASER_SIZES: [EraserSize; 3] = [EraserSize::Px24, EraserSize::Px48, EraserSize::Px72];

/// 颜色和画笔粗细选择项在不同界面中的排列方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectorOrientation {
    Horizontal,
    Vertical,
}

impl SelectorOrientation {
    /// 按指定方向创建隔离布局，并返回选择器内容的结果。
    fn show<R>(self, ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
        match self {
            Self::Horizontal => ui.horizontal(add_contents).inner,
            Self::Vertical => ui.vertical(add_contents).inner,
        }
    }

    /// 返回指定选项数量在当前排列方向下需要的弹层内容宽度。
    const fn popup_width(self, option_count: usize, metrics: InterfaceMetrics) -> f32 {
        match self {
            Self::Horizontal => {
                option_count as f32 * metrics.touch_target
                    + (option_count - 1) as f32 * metrics.space_2
            }
            Self::Vertical => metrics.touch_target,
        }
    }
}

/// 返回颜色选择器在指定排列方向下需要的内容宽度。
pub(super) const fn color_selector_width(
    orientation: SelectorOrientation,
    metrics: InterfaceMetrics,
) -> f32 {
    orientation.popup_width(COLORS.len(), metrics)
}

/// 返回画笔粗细选择器在指定排列方向下需要的内容宽度。
pub(super) const fn pen_width_selector_width(
    orientation: SelectorOrientation,
    metrics: InterfaceMetrics,
) -> f32 {
    orientation.popup_width(PEN_WIDTHS.len(), metrics)
}

/// 绘制共用工具偏好，并按入口决定是否显示自然笔锋开关。
pub fn render_tool_preferences(
    ui: &mut Ui,
    tools: ToolState,
    metrics: InterfaceMetrics,
    readable_mode: bool,
    show_natural_taper: bool,
) -> Option<UiCommand> {
    let color_command = render_color_selector(
        ui,
        tools.color,
        SelectorOrientation::Horizontal,
        metrics,
        readable_mode,
    );
    let pen_width_command = render_pen_width_selector(
        ui,
        tools.pen_width,
        SelectorOrientation::Horizontal,
        metrics,
        readable_mode,
    );
    let mut command = color_command.or(pen_width_command);
    let eraser_size_command =
        render_eraser_size_selector(ui, tools.eraser_size, metrics, readable_mode);
    if command.is_none() {
        command = eraser_size_command;
    }
    if show_natural_taper {
        ui.add_space(metrics.space_2);
        let mut natural_taper_enabled = tools.natural_taper_enabled;
        if ui
            .add_sized(
                [ui.available_width(), metrics.touch_target],
                egui::Checkbox::new(&mut natural_taper_enabled, "自然笔锋"),
            )
            .changed()
            && command.is_none()
        {
            command = Some(UiCommand::SetNaturalTaperEnabled(natural_taper_enabled));
        }
    }
    command
}

/// 绘制四档日志级别的紧凑选择按钮，并返回当前帧的设置命令。
pub(super) fn render_log_level_selector(
    ui: &mut Ui,
    selected: LogLevel,
    metrics: InterfaceMetrics,
) -> Option<UiCommand> {
    section_label(ui, "日志级别", metrics);
    let mut command = None;
    ui.horizontal(|ui| {
        for level in LogLevel::ALL {
            if ui
                .add_sized(
                    Vec2::splat(metrics.touch_target),
                    egui::Button::selectable(selected == level, level.label()),
                )
                .clicked()
            {
                command = Some(UiCommand::SetLogLevel(level));
            }
        }
    });
    command
}

/// 绘制固定快速颜色选择器，并返回用户选中的颜色命令。
pub(super) fn render_color_selector(
    ui: &mut Ui,
    selected: InkColor,
    orientation: SelectorOrientation,
    metrics: InterfaceMetrics,
    readable_mode: bool,
) -> Option<UiCommand> {
    let mut command = None;
    section_label(ui, "画笔颜色", metrics);
    orientation.show(ui, |ui| {
        for color in COLORS {
            if selection_button(
                ui,
                color_label(color),
                SelectionVisual::Color(color32(color)),
                selected == color,
                metrics,
                readable_mode,
            )
            .clicked()
            {
                command = Some(UiCommand::SetColor(color));
            }
        }
    });
    command
}

/// 绘制固定画笔粗细选择器，并用不同线宽预览各档位。
pub(super) fn render_pen_width_selector(
    ui: &mut Ui,
    selected: PenWidth,
    orientation: SelectorOrientation,
    metrics: InterfaceMetrics,
    readable_mode: bool,
) -> Option<UiCommand> {
    let mut command = None;
    section_label(ui, "画笔粗细", metrics);
    orientation.show(ui, |ui| {
        for width in PEN_WIDTHS {
            if selection_button(
                ui,
                pen_width_label(width),
                SelectionVisual::PenWidth(width),
                selected == width,
                metrics,
                readable_mode,
            )
            .clicked()
            {
                command = Some(UiCommand::SetPenWidth(width));
            }
        }
    });
    command
}

/// 绘制固定区域橡皮擦大小选择器。
fn render_eraser_size_selector(
    ui: &mut Ui,
    selected: EraserSize,
    metrics: InterfaceMetrics,
    readable_mode: bool,
) -> Option<UiCommand> {
    let mut command = None;
    section_label(ui, "橡皮擦大小", metrics);
    ui.horizontal(|ui| {
        for size in ERASER_SIZES {
            if selection_button(
                ui,
                eraser_size_label(size),
                SelectionVisual::EraserSize(size),
                selected == size,
                metrics,
                readable_mode,
            )
            .clicked()
            {
                command = Some(UiCommand::SetEraserSize(size));
            }
        }
    });
    command
}

/// 绘制设置分组使用的紧凑标题。
fn section_label(ui: &mut Ui, label: &str, metrics: InterfaceMetrics) {
    ui.label(
        egui::RichText::new(label)
            .size(metrics.option_text)
            .color(tokens::COLOR_TEXT_SECONDARY),
    );
}

/// 绘制固定触摸尺寸的色样或数值选择按钮。
fn selection_button(
    ui: &mut Ui,
    label: &str,
    visual: SelectionVisual,
    selected: bool,
    metrics: InterfaceMetrics,
    readable_mode: bool,
) -> Response {
    let size = Vec2::splat(metrics.touch_target);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let (fill, border) = tokens::button_colors(readable_mode, selected, &response, ui.is_enabled());
    pixel_snap::paint_pixel_aligned_rect(
        ui,
        rect,
        CornerRadius::same(metrics.button_radius),
        fill,
        Stroke::new(1.0, border),
    );

    let center = Pos2::new(
        rect.center().x,
        rect.top() + metrics.space_3 + metrics.icon_size / 2.0,
    );
    draw_selection_visual(ui, center, visual, metrics, readable_mode);
    ui.painter().text(
        Pos2::new(rect.center().x, rect.bottom() - metrics.space_3),
        Align2::CENTER_BOTTOM,
        label,
        FontId::proportional(metrics.option_text),
        tokens::COLOR_TEXT_PRIMARY,
    );
    response
}

/// 绘制色样、画笔线宽或橡皮擦直径的视觉预览。
fn draw_selection_visual(
    ui: &Ui,
    center: Pos2,
    visual: SelectionVisual,
    metrics: InterfaceMetrics,
    readable_mode: bool,
) {
    match visual {
        SelectionVisual::Color(color) => {
            ui.painter()
                .circle_filled(center, metrics.icon_size / 2.0, color);
            ui.painter().circle_stroke(
                center,
                metrics.icon_size / 2.0,
                Stroke::new(
                    1.0,
                    tokens::material_palette(readable_mode, tokens::MaterialRole::Control).border,
                ),
            );
        }
        SelectionVisual::PenWidth(width) => {
            let visual_width = match width {
                PenWidth::Px4 => 1.0,
                PenWidth::Px6 => 1.5,
                PenWidth::Px8 => 2.0,
                PenWidth::Px16 => 4.0,
            };
            pixel_snap::paint_pixel_aligned_line(
                ui.painter(),
                [
                    center - egui::vec2(metrics.icon_size / 2.0, 0.0),
                    center + egui::vec2(metrics.icon_size / 2.0, 0.0),
                ],
                Stroke::new(visual_width, tokens::COLOR_TEXT_SECONDARY),
            );
        }
        SelectionVisual::EraserSize(size) => {
            let radius = match size {
                EraserSize::Px24 => metrics.space_2,
                EraserSize::Px48 => metrics.space_3,
                EraserSize::Px72 => metrics.space_4,
            };
            ui.painter().circle_stroke(
                center,
                radius,
                Stroke::new(metrics.points(2.0), tokens::COLOR_TEXT_SECONDARY),
            );
        }
    }
}

/// 返回固定快速颜色的中文标签。
const fn color_label(color: InkColor) -> &'static str {
    match color {
        InkColor::Red => "红",
        InkColor::Yellow => "黄",
        InkColor::Blue => "蓝",
        InkColor::Green => "绿",
        InkColor::Black => "黑",
        InkColor::White => "白",
    }
}

/// 返回画笔粗细档位标签。
const fn pen_width_label(width: PenWidth) -> &'static str {
    match width {
        PenWidth::Px4 => "4px",
        PenWidth::Px6 => "6px",
        PenWidth::Px8 => "8px",
        PenWidth::Px16 => "16px",
    }
}

/// 返回橡皮擦直径档位标签。
const fn eraser_size_label(size: EraserSize) -> &'static str {
    match size {
        EraserSize::Px24 => "24px",
        EraserSize::Px48 => "48px",
        EraserSize::Px72 => "72px",
    }
}

#[derive(Debug, Clone, Copy)]
enum SelectionVisual {
    Color(Color32),
    PenWidth(PenWidth),
    EraserSize(EraserSize),
}

/// 绘制设置页行式选项行：固定标签列加控件区，行高为完整触摸高度。
fn selection_row(
    ui: &mut Ui,
    label: &str,
    metrics: InterfaceMetrics,
    add_contents: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(metrics.diagnostic_label_width, metrics.touch_target),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(label)
                        .size(metrics.option_text)
                        .color(tokens::COLOR_TEXT_PRIMARY),
                );
            },
        );
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), metrics.touch_target),
            Layout::left_to_right(Align::Center),
            add_contents,
        );
    });
}

/// 绘制设置页行式的画笔颜色选择行：圆形色块，命令与旧选择器一致。
pub(super) fn render_color_selector_row(
    ui: &mut Ui,
    selected: InkColor,
    metrics: InterfaceMetrics,
    _readable_mode: bool,
) -> Option<UiCommand> {
    let mut command = None;
    selection_row(ui, "画笔颜色", metrics, |ui| {
        for color in COLORS {
            if color_swatch_button(ui, color, selected == color, metrics).clicked()
                && command.is_none()
            {
                command = Some(UiCommand::SetColor(color));
            }
        }
    });
    command
}

/// 绘制设置页行式布局中的圆形颜色色块，选中态使用加粗主色描边。
fn color_swatch_button(
    ui: &mut Ui,
    color: InkColor,
    selected: bool,
    metrics: InterfaceMetrics,
) -> Response {
    let diameter = metrics.space_6 * 2.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let center = rect.center();
    let radius = metrics.space_6 - 1.0;
    ui.painter().circle_filled(center, radius, color32(color));
    let stroke = if selected {
        Stroke::new(metrics.points(2.0), tokens::COLOR_PRIMARY)
    } else if response.hovered() || response.is_pointer_button_down_on() {
        Stroke::new(1.0, tokens::COLOR_TEXT_SECONDARY)
    } else {
        Stroke::new(1.0, tokens::COLOR_BORDER_INPUT)
    };
    ui.painter().circle_stroke(center, radius, stroke);
    response.on_hover_text(color_label(color))
}

/// 绘制设置页行式的画笔粗细选择行，复用线宽预览方块按钮。
pub(super) fn render_pen_width_selector_row(
    ui: &mut Ui,
    selected: PenWidth,
    metrics: InterfaceMetrics,
    readable_mode: bool,
) -> Option<UiCommand> {
    let mut command = None;
    selection_row(ui, "画笔粗细", metrics, |ui| {
        for width in PEN_WIDTHS {
            if selection_button(
                ui,
                pen_width_label(width),
                SelectionVisual::PenWidth(width),
                selected == width,
                metrics,
                readable_mode,
            )
            .clicked()
                && command.is_none()
            {
                command = Some(UiCommand::SetPenWidth(width));
            }
        }
    });
    command
}

/// 绘制设置页行式的橡皮擦大小选择行，复用圆环预览方块按钮。
pub(super) fn render_eraser_size_selector_row(
    ui: &mut Ui,
    selected: EraserSize,
    metrics: InterfaceMetrics,
    readable_mode: bool,
) -> Option<UiCommand> {
    let mut command = None;
    selection_row(ui, "橡皮擦大小", metrics, |ui| {
        for size in ERASER_SIZES {
            if selection_button(
                ui,
                eraser_size_label(size),
                SelectionVisual::EraserSize(size),
                selected == size,
                metrics,
                readable_mode,
            )
            .clicked()
                && command.is_none()
            {
                command = Some(UiCommand::SetEraserSize(size));
            }
        }
    });
    command
}

/// 绘制设置页行式的手掌尺寸选择行。
pub(super) fn render_palm_size_selector_row(
    ui: &mut Ui,
    selected: PalmSizePreset,
    metrics: InterfaceMetrics,
) -> Option<UiCommand> {
    let mut command = None;
    selection_row(ui, "手掌尺寸", metrics, |ui| {
        for preset in PalmSizePreset::ALL {
            if ui
                .add_sized(
                    Vec2::splat(metrics.touch_target),
                    egui::Button::selectable(selected == preset, preset.label()),
                )
                .clicked()
                && command.is_none()
            {
                command = Some(UiCommand::SetPalmSizePreset(preset));
            }
        }
    });
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 断言形状中的全部文本都完整落在视口内。
    fn assert_text_inside_viewport(shape: &egui::Shape, viewport: egui::Rect) {
        match shape {
            egui::Shape::Text(text) => {
                let bounds = text.visual_bounding_rect();
                assert!(
                    viewport.expand(1.1).contains_rect(bounds),
                    "文本 {:?} 越出视口: {bounds:?}",
                    text.galley.job.text
                );
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    assert_text_inside_viewport(shape, viewport);
                }
            }
            _ => {}
        }
    }

    /// 验证四种行式选择器在设置页 528px 内容宽内全部放下。
    #[test]
    fn settings_row_selectors_fit_content_width() {
        let metrics = tokens::SETTINGS_METRICS;
        let content_width = 528.0 - metrics.diagnostic_label_width - metrics.space_2;
        let color_width = COLORS.len() as f32 * (metrics.space_6 * 2.0)
            + (COLORS.len() - 1) as f32 * metrics.space_2;
        let pen_width = PEN_WIDTHS.len() as f32 * metrics.touch_target
            + (PEN_WIDTHS.len() - 1) as f32 * metrics.space_2;
        let eraser_width = ERASER_SIZES.len() as f32 * metrics.touch_target
            + (ERASER_SIZES.len() - 1) as f32 * metrics.space_2;
        let palm_width = PalmSizePreset::ALL.len() as f32 * metrics.touch_target
            + (PalmSizePreset::ALL.len() - 1) as f32 * metrics.space_2;

        assert!(
            color_width <= content_width,
            "画笔颜色行溢出: {color_width} > {content_width}"
        );
        assert!(
            pen_width <= content_width,
            "画笔粗细行溢出: {pen_width} > {content_width}"
        );
        assert!(
            eraser_width <= content_width,
            "橡皮擦大小行溢出: {eraser_width} > {content_width}"
        );
        assert!(
            palm_width <= content_width,
            "手掌尺寸行溢出: {palm_width} > {content_width}"
        );
    }

    /// 验证行式颜色选择器在 560×640 设置视口内完整渲染，无文本越界。
    #[test]
    fn color_selector_row_renders_within_settings_viewport() {
        let context = egui::Context::default();
        crate::ui::configure_context(&context);
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(560.0, 640.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ =
                    render_color_selector_row(ui, InkColor::Red, tokens::SETTINGS_METRICS, false);
            },
        );
        let viewport = context.content_rect();
        for clipped in &output.shapes {
            assert_text_inside_viewport(&clipped.shape, viewport);
        }
    }
}
