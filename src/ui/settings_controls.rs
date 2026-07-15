use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Response, Sense, Stroke, StrokeKind, Ui, Vec2,
};

use super::{
    design_tokens as tokens,
    toolbar::{ToolState, UiCommand, color32},
};
use crate::ink::{EraserSize, InkColor, PenWidth};

const COLORS: [InkColor; 6] = [
    InkColor::Red,
    InkColor::Yellow,
    InkColor::Blue,
    InkColor::Green,
    InkColor::Black,
    InkColor::White,
];
const PEN_WIDTHS: [PenWidth; 4] = [PenWidth::Px4, PenWidth::Px8, PenWidth::Px16, PenWidth::Px24];
const ERASER_SIZES: [EraserSize; 3] = [EraserSize::Px24, EraserSize::Px48, EraserSize::Px72];

/// 绘制颜色、画笔粗细和区域橡皮擦大小三组共用选择控件。
pub fn render_tool_preferences(ui: &mut Ui, tools: ToolState) -> Option<UiCommand> {
    let mut command = None;

    section_label(ui, "画笔颜色");
    ui.horizontal_wrapped(|ui| {
        for color in COLORS {
            if selection_button(
                ui,
                color_label(color),
                SelectionVisual::Color(color32(color)),
                tools.color == color,
            )
            .clicked()
            {
                command = Some(UiCommand::SetColor(color));
            }
        }
    });

    section_label(ui, "画笔粗细");
    ui.horizontal_wrapped(|ui| {
        for width in PEN_WIDTHS {
            if selection_button(
                ui,
                pen_width_label(width),
                SelectionVisual::PenWidth(width),
                tools.pen_width == width,
            )
            .clicked()
            {
                command = Some(UiCommand::SetPenWidth(width));
            }
        }
    });

    section_label(ui, "橡皮擦大小");
    ui.horizontal_wrapped(|ui| {
        for size in ERASER_SIZES {
            if selection_button(
                ui,
                eraser_size_label(size),
                SelectionVisual::EraserSize(size),
                tools.eraser_size == size,
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
fn section_label(ui: &mut Ui, label: &str) {
    ui.label(
        egui::RichText::new(label)
            .size(tokens::TEXT_SM)
            .color(tokens::COLOR_TEXT_SECONDARY),
    );
}

/// 绘制固定触摸尺寸的色样或数值选择按钮。
fn selection_button(ui: &mut Ui, label: &str, visual: SelectionVisual, selected: bool) -> Response {
    let size = Vec2::splat(tokens::TOUCH_TARGET);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let fill = if selected {
        tokens::COLOR_SELECTED
    } else if response.hovered() {
        tokens::COLOR_HOVER
    } else {
        tokens::COLOR_SURFACE
    };
    let border = if selected {
        tokens::COLOR_PRIMARY
    } else {
        tokens::COLOR_BORDER
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(tokens::BUTTON_RADIUS), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(tokens::BUTTON_RADIUS),
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );

    let center = Pos2::new(
        rect.center().x,
        rect.top() + tokens::SPACE_3 + tokens::ICON_SIZE / 2.0,
    );
    draw_selection_visual(ui, center, visual);
    ui.painter().text(
        Pos2::new(rect.center().x, rect.bottom() - tokens::SPACE_3),
        Align2::CENTER_BOTTOM,
        label,
        FontId::proportional(tokens::TEXT_SM),
        tokens::COLOR_TEXT_PRIMARY,
    );
    response
}

/// 绘制色样、画笔线宽或橡皮擦直径的视觉预览。
fn draw_selection_visual(ui: &Ui, center: Pos2, visual: SelectionVisual) {
    match visual {
        SelectionVisual::Color(color) => {
            ui.painter()
                .circle_filled(center, tokens::ICON_SIZE / 2.0, color);
            ui.painter().circle_stroke(
                center,
                tokens::ICON_SIZE / 2.0,
                Stroke::new(1.0, tokens::COLOR_BORDER),
            );
        }
        SelectionVisual::PenWidth(width) => {
            let visual_width = match width {
                PenWidth::Px4 => 1.0,
                PenWidth::Px8 => 2.0,
                PenWidth::Px16 => 4.0,
                PenWidth::Px24 => 6.0,
            };
            ui.painter().line_segment(
                [
                    center - egui::vec2(tokens::ICON_SIZE / 2.0, 0.0),
                    center + egui::vec2(tokens::ICON_SIZE / 2.0, 0.0),
                ],
                Stroke::new(visual_width, tokens::COLOR_TEXT_SECONDARY),
            );
        }
        SelectionVisual::EraserSize(size) => {
            let radius = match size {
                EraserSize::Px24 => tokens::SPACE_2,
                EraserSize::Px48 => tokens::SPACE_3,
                EraserSize::Px72 => tokens::SPACE_4,
            };
            ui.painter().circle_stroke(
                center,
                radius,
                Stroke::new(2.0, tokens::COLOR_TEXT_SECONDARY),
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
        PenWidth::Px8 => "8px",
        PenWidth::Px16 => "16px",
        PenWidth::Px24 => "24px",
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
