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
    const fn popup_width(self, option_count: usize) -> f32 {
        match self {
            Self::Horizontal => {
                option_count as f32 * tokens::TOUCH_TARGET
                    + (option_count - 1) as f32 * tokens::SPACE_2
            }
            Self::Vertical => tokens::TOUCH_TARGET,
        }
    }
}

/// 返回颜色选择器在指定排列方向下需要的内容宽度。
pub(super) const fn color_selector_width(orientation: SelectorOrientation) -> f32 {
    orientation.popup_width(COLORS.len())
}

/// 返回画笔粗细选择器在指定排列方向下需要的内容宽度。
pub(super) const fn pen_width_selector_width(orientation: SelectorOrientation) -> f32 {
    orientation.popup_width(PEN_WIDTHS.len())
}

/// 绘制颜色、画笔粗细和区域橡皮擦大小三组共用选择控件。
pub fn render_tool_preferences(ui: &mut Ui, tools: ToolState) -> Option<UiCommand> {
    let color_command = render_color_selector(ui, tools.color, SelectorOrientation::Horizontal);
    let pen_width_command =
        render_pen_width_selector(ui, tools.pen_width, SelectorOrientation::Horizontal);
    let eraser_size_command = render_eraser_size_selector(ui, tools.eraser_size);
    color_command.or(pen_width_command).or(eraser_size_command)
}

/// 绘制固定快速颜色选择器，并返回用户选中的颜色命令。
pub(super) fn render_color_selector(
    ui: &mut Ui,
    selected: InkColor,
    orientation: SelectorOrientation,
) -> Option<UiCommand> {
    let mut command = None;
    section_label(ui, "画笔颜色");
    orientation.show(ui, |ui| {
        for color in COLORS {
            if selection_button(
                ui,
                color_label(color),
                SelectionVisual::Color(color32(color)),
                selected == color,
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
) -> Option<UiCommand> {
    let mut command = None;
    section_label(ui, "画笔粗细");
    orientation.show(ui, |ui| {
        for width in PEN_WIDTHS {
            if selection_button(
                ui,
                pen_width_label(width),
                SelectionVisual::PenWidth(width),
                selected == width,
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
fn render_eraser_size_selector(ui: &mut Ui, selected: EraserSize) -> Option<UiCommand> {
    let mut command = None;
    section_label(ui, "橡皮擦大小");
    ui.horizontal(|ui| {
        for size in ERASER_SIZES {
            if selection_button(
                ui,
                eraser_size_label(size),
                SelectionVisual::EraserSize(size),
                selected == size,
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
        tokens::OPAQUE_COLOR_SELECTED
    } else if response.hovered() {
        tokens::OPAQUE_COLOR_HOVER
    } else {
        tokens::OPAQUE_COLOR_SURFACE
    };
    let border = if selected {
        tokens::OPAQUE_COLOR_PRIMARY_SURFACE
    } else {
        tokens::OPAQUE_COLOR_BORDER
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
                Stroke::new(1.0, tokens::OPAQUE_COLOR_BORDER),
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
                Stroke::new(tokens::scale_points(2.0), tokens::COLOR_TEXT_SECONDARY),
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
        PenWidth::Px4 => "4pt",
        PenWidth::Px8 => "8pt",
        PenWidth::Px16 => "16pt",
        PenWidth::Px24 => "24pt",
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

#[cfg(test)]
mod tests {
    use egui::{Context, RawInput};

    use super::*;

    /// 收集指定方向下三个固定触摸项的实际布局矩形。
    fn selector_option_rects(orientation: SelectorOrientation) -> Vec<egui::Rect> {
        let context = Context::default();
        let mut rects = Vec::new();
        let _ = context.run_ui(RawInput::default(), |ui| {
            orientation.show(ui, |ui| {
                for _ in 0..3 {
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::splat(tokens::TOUCH_TARGET), Sense::hover());
                    rects.push(rect);
                }
            });
        });
        rects
    }

    /// 验证普通批注使用的一列选择项保持同一横坐标并按纵向递增。
    #[test]
    fn vertical_selector_orientation_stacks_options_in_one_column() {
        let rects = selector_option_rects(SelectorOrientation::Vertical);

        assert!(rects.windows(2).all(|pair| {
            (pair[0].center().x - pair[1].center().x).abs() < f32::EPSILON
                && pair[1].top() > pair[0].bottom()
        }));
        assert_eq!(
            color_selector_width(SelectorOrientation::Vertical),
            tokens::TOUCH_TARGET
        );
        assert_eq!(
            pen_width_selector_width(SelectorOrientation::Vertical),
            tokens::TOUCH_TARGET
        );
    }

    /// 验证设置和放映模式保留的一排选择项按横向递增。
    #[test]
    fn horizontal_selector_orientation_keeps_options_in_one_row() {
        let rects = selector_option_rects(SelectorOrientation::Horizontal);

        assert!(rects.windows(2).all(|pair| {
            (pair[0].center().y - pair[1].center().y).abs() < f32::EPSILON
                && pair[1].left() > pair[0].right()
        }));
        assert!(color_selector_width(SelectorOrientation::Horizontal) > tokens::TOUCH_TARGET);
        assert!(pen_width_selector_width(SelectorOrientation::Horizontal) > tokens::TOUCH_TARGET);
    }
}
