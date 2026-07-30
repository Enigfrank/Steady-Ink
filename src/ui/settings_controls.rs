use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Response, Sense, Stroke, Ui, Vec2};

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

/// 绘制小、标准、大三档手掌尺寸选择器。
pub(super) fn render_palm_size_selector(
    ui: &mut Ui,
    selected: PalmSizePreset,
    metrics: InterfaceMetrics,
) -> Option<UiCommand> {
    section_label(ui, "手掌尺寸", metrics);
    let mut command = None;
    ui.horizontal(|ui| {
        for preset in PalmSizePreset::ALL {
            if ui
                .add_sized(
                    Vec2::splat(metrics.touch_target),
                    egui::Button::selectable(selected == preset, preset.label()),
                )
                .clicked()
            {
                command = Some(UiCommand::SetPalmSizePreset(preset));
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
