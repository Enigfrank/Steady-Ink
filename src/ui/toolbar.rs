use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Margin, Pos2, Rect, Response, Sense, Shape,
    Stroke, StrokeKind, Ui, Vec2,
};

use super::design_tokens as tokens;
use crate::{
    app::AppMode,
    ink::{EraserSize, InkColor, InkTool, PenWidth},
    slideshow::ComDiagnostics,
    window::{DockSide, GlDiagnostics},
};

/// UI 按钮需要交给应用状态机执行的命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    EnterAnnotation,
    ExitAnnotation,
    SelectPen,
    SelectEraser,
    CycleColor,
    CyclePenWidth,
    CycleEraserSize,
    SetColor(InkColor),
    SetPenWidth(PenWidth),
    SetEraserSize(EraserSize),
    Undo,
    Clear,
    OpenSettings,
    CloseSettings,
    ToggleQuickSettings,
    BeginIdleToolbarDrag,
    SetDockSide(DockSide),
    SetSlideshowIntegrationEnabled(bool),
    ToggleSlideshowToolbar,
    PreviousSlide,
    NextSlide,
    ExitSlideShow,
    RequestDismissSlideshow,
    ConfirmDismissSlideshow,
    CancelDismissSlideshow,
}

/// 非批注窗口当前显示的面板。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdlePanel {
    Toolbar,
    QuickSettings,
    Settings,
}

/// 顶层 UI 一帧渲染所需的只读状态快照。
#[derive(Debug, Clone, Copy)]
pub struct UiViewState<'a> {
    pub mode: AppMode,
    pub idle_panel: IdlePanel,
    pub dock_side: DockSide,
    pub tools: ToolState,
    pub slideshow_integration_enabled: bool,
    pub slide_page_numbers: Option<(u32, u32)>,
    pub slideshow_controls_enabled: bool,
    pub dismiss_slideshow_confirmation: bool,
    pub com_diagnostics: Option<&'a ComDiagnostics>,
    pub slideshow_connection_error: Option<&'a str>,
    pub slideshow_control_error: Option<&'a str>,
    pub settings_error: Option<&'a str>,
    pub settings_path: &'a std::path::Path,
    pub gl_diagnostics: &'a GlDiagnostics,
}

/// 普通批注工具栏当前选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolState {
    pub tool: InkTool,
    pub color: InkColor,
    pub pen_width: PenWidth,
    pub eraser_size: EraserSize,
}

/// 工具按钮组在当前帧产生的命令和拖动状态。
#[derive(Debug, Default)]
pub(super) struct ToolbarInteraction {
    pub command: Option<UiCommand>,
    pub drag_delta: Vec2,
    pub drag_stopped: bool,
}

impl ToolbarInteraction {
    /// 合并一个按钮响应，并让点击命令保持单帧唯一。
    fn observe(&mut self, response: &Response, command: UiCommand) {
        if response.clicked() && self.command.is_none() {
            self.command = Some(command);
        }
        if response.dragged() {
            self.drag_delta += response.drag_delta();
        }
        self.drag_stopped |= response.drag_stopped();
    }
}

impl Default for ToolState {
    /// 返回已确认的默认画笔、红色、8px 和 48px 配置。
    fn default() -> Self {
        Self {
            tool: InkTool::default(),
            color: InkColor::default(),
            pen_width: PenWidth::default(),
            eraser_size: EraserSize::default(),
        }
    }
}

impl ToolState {
    /// 按固定颜色顺序切换快速颜色。
    pub fn cycle_color(&mut self) {
        self.color = match self.color {
            InkColor::Red => InkColor::Yellow,
            InkColor::Yellow => InkColor::Blue,
            InkColor::Blue => InkColor::Green,
            InkColor::Green => InkColor::Black,
            InkColor::Black => InkColor::White,
            InkColor::White => InkColor::Red,
        };
    }

    /// 按 4px、8px、16px、24px 的固定顺序切换画笔粗细。
    pub fn cycle_pen_width(&mut self) {
        self.pen_width = match self.pen_width {
            PenWidth::Px4 => PenWidth::Px8,
            PenWidth::Px8 => PenWidth::Px16,
            PenWidth::Px16 => PenWidth::Px24,
            PenWidth::Px24 => PenWidth::Px4,
        };
    }

    /// 按 24px、48px、72px 的固定顺序切换区域橡皮擦大小。
    pub fn cycle_eraser_size(&mut self) {
        self.eraser_size = match self.eraser_size {
            EraserSize::Px24 => EraserSize::Px48,
            EraserSize::Px48 => EraserSize::Px72,
            EraserSize::Px72 => EraserSize::Px24,
        };
    }
}

/// 根据顶层模式绘制当前工具栏，并返回最多一个离散 UI 命令。
pub fn render(ui: &mut Ui, view: UiViewState<'_>) -> Option<UiCommand> {
    match view.mode {
        AppMode::IdleFloatingToolbar => match view.idle_panel {
            IdlePanel::Toolbar => render_idle_toolbar(ui),
            IdlePanel::QuickSettings => super::quick_settings::render(ui, view),
            IdlePanel::Settings => super::settings_view::render(ui, view),
        },
        AppMode::NormalAnnotating => render_annotation_toolbar(ui, view.tools, view.dock_side),
        AppMode::SlideShowAnnotatingExpanded
        | AppMode::SlideShowAnnotatingCollapsed
        | AppMode::SlideShowConnectionLost => super::slideshow_toolbar::render(ui.ctx(), view),
    }
}

/// 绘制右侧中部非批注悬浮卡片工具栏。
pub(super) fn render_idle_toolbar(ui: &mut Ui) -> Option<UiCommand> {
    let mut command = None;
    Frame::new()
        .fill(tokens::COLOR_BACKGROUND)
        .stroke(Stroke::new(1.0, tokens::COLOR_BORDER))
        .corner_radius(CornerRadius::same(tokens::CARD_RADIUS))
        .inner_margin(Margin::same(tokens::SPACE_2 as i8))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                let annotation = icon_button(ui, "批注", Icon::Pen, false, None);
                if annotation.drag_started() {
                    command = Some(UiCommand::BeginIdleToolbarDrag);
                } else if annotation.clicked() {
                    command = Some(UiCommand::EnterAnnotation);
                }
                let settings = icon_button(ui, "设置", Icon::Settings, false, None);
                if settings.drag_started() {
                    command = Some(UiCommand::BeginIdleToolbarDrag);
                } else if settings.clicked() {
                    command = Some(UiCommand::OpenSettings);
                }
                let quick_settings = icon_button(ui, "快捷设置", Icon::Sliders, false, None);
                if quick_settings.drag_started() {
                    command = Some(UiCommand::BeginIdleToolbarDrag);
                } else if quick_settings.clicked() {
                    command = Some(UiCommand::ToggleQuickSettings);
                }
            });
        });
    command
}

/// 绘制普通批注模式右侧浅色卡片工具栏。
fn render_annotation_toolbar(
    ui: &mut Ui,
    tools: ToolState,
    dock_side: DockSide,
) -> Option<UiCommand> {
    let context = ui.ctx().clone();
    let area_id = egui::Id::new("normal_annotation_toolbar");
    let state_id = egui::Id::new("normal_annotation_toolbar_state");
    let screen = context.content_rect();
    let toolbar_size = normal_toolbar_size();
    let mut state = context.data_mut(|data| {
        data.get_temp::<NormalToolbarState>(state_id)
            .filter(|state| state.dock_side == dock_side && state.viewport_size == screen.size())
            .unwrap_or_else(|| NormalToolbarState {
                position: normal_toolbar_position(screen, toolbar_size, dock_side),
                dock_side,
                viewport_size: screen.size(),
            })
    });

    let area_response = egui::Area::new(area_id)
        .fixed_pos(state.position)
        .order(egui::Order::Foreground)
        .show(&context, |ui| {
            Frame::new()
                .fill(tokens::COLOR_BACKGROUND)
                .stroke(Stroke::new(1.0, tokens::COLOR_BORDER))
                .corner_radius(CornerRadius::same(tokens::CARD_RADIUS))
                .inner_margin(Margin::same(tokens::SPACE_2 as i8))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        let mut interaction = render_ink_tool_buttons(ui, tools);
                        let exit = icon_button(ui, "退出批注", Icon::Exit, false, None);
                        interaction.observe(&exit, UiCommand::ExitAnnotation);
                        interaction
                    })
                    .inner
                })
                .inner
        });
    let interaction = area_response.inner;
    state.position += interaction.drag_delta;
    state.position.x = constrain_axis_position(
        state.position.x,
        screen.left(),
        screen.right(),
        toolbar_size.x,
        tokens::SPACE_6,
    );
    state.position.y = constrain_axis_position(
        state.position.y,
        screen.top(),
        screen.bottom(),
        toolbar_size.y,
        tokens::SPACE_6,
    );

    let mut command = interaction.command;
    if interaction.drag_stopped {
        state.dock_side = if state.position.x + toolbar_size.x / 2.0 < screen.center().x {
            DockSide::Left
        } else {
            DockSide::Right
        };
        state.position = normal_toolbar_position(screen, toolbar_size, state.dock_side);
        if command.is_none() {
            command = Some(UiCommand::SetDockSide(state.dock_side));
        }
    }
    context.data_mut(|data| data.insert_temp(state_id, state));
    command
}

/// 普通批注工具栏保存在 egui 临时内存中的吸附位置。
#[derive(Debug, Clone, Copy)]
struct NormalToolbarState {
    position: Pos2,
    dock_side: DockSide,
    viewport_size: Vec2,
}

/// 返回八个纵向按钮及卡片内边距形成的固定工具栏尺寸。
fn normal_toolbar_size() -> Vec2 {
    const BUTTON_COUNT: f32 = 8.0;
    Vec2::new(
        tokens::TOOL_BUTTON_WIDTH + tokens::SPACE_2 * 2.0,
        BUTTON_COUNT * tokens::TOUCH_TARGET
            + (BUTTON_COUNT - 1.0) * tokens::SPACE_2
            + tokens::SPACE_2 * 2.0,
    )
}

/// 根据吸附边缘计算普通批注工具栏的稳定屏幕位置。
fn normal_toolbar_position(screen: Rect, size: Vec2, dock_side: DockSide) -> Pos2 {
    let preferred_x = if dock_side == DockSide::Left {
        screen.left() + tokens::SPACE_6
    } else {
        screen.right() - tokens::SPACE_6 - size.x
    };
    Pos2::new(
        constrain_axis_position(
            preferred_x,
            screen.left(),
            screen.right(),
            size.x,
            tokens::SPACE_6,
        ),
        constrain_axis_position(
            screen.center().y - size.y / 2.0,
            screen.top(),
            screen.bottom(),
            size.y,
            tokens::SPACE_6,
        ),
    )
}

/// 将浮动控件约束在单轴可用范围内；空间不足时保持居中并允许对称溢出。
fn constrain_axis_position(
    position: f32,
    viewport_min: f32,
    viewport_max: f32,
    item_extent: f32,
    margin: f32,
) -> f32 {
    let minimum = viewport_min + margin;
    let maximum = viewport_max - margin - item_extent;
    if minimum <= maximum {
        position.clamp(minimum, maximum)
    } else {
        (viewport_min + viewport_max - item_extent) / 2.0
    }
}

/// 绘制普通和放映工具栏共用的七个墨迹工具按钮。
pub(super) fn render_ink_tool_buttons(ui: &mut Ui, tools: ToolState) -> ToolbarInteraction {
    let mut interaction = ToolbarInteraction::default();
    let pen = icon_button(ui, "画笔", Icon::Pen, tools.tool == InkTool::Pen, None);
    interaction.observe(&pen, UiCommand::SelectPen);
    let color = icon_button(ui, "颜色", Icon::Color, false, Some(color32(tools.color)));
    interaction.observe(&color, UiCommand::CycleColor);
    let pen_width = icon_button(
        ui,
        pen_width_label(tools.pen_width),
        Icon::PenWidth,
        false,
        None,
    );
    interaction.observe(&pen_width, UiCommand::CyclePenWidth);
    let eraser = icon_button(
        ui,
        "橡皮擦",
        Icon::Eraser,
        tools.tool == InkTool::RegionEraser,
        None,
    );
    interaction.observe(&eraser, UiCommand::SelectEraser);
    let eraser_size = icon_button(
        ui,
        eraser_size_label(tools.eraser_size),
        Icon::EraserSize,
        false,
        None,
    );
    interaction.observe(&eraser_size, UiCommand::CycleEraserSize);
    let undo = icon_button(ui, "撤销", Icon::Undo, false, None);
    interaction.observe(&undo, UiCommand::Undo);
    let clear = icon_button(ui, "清屏", Icon::Clear, false, None);
    interaction.observe(&clear, UiCommand::Clear);
    interaction
}

/// 绘制固定触摸尺寸、图标在上文字在下的功能按钮。
pub(super) fn icon_button(
    ui: &mut Ui,
    label: &str,
    icon: Icon,
    selected: bool,
    swatch: Option<Color32>,
) -> Response {
    let desired_size = Vec2::new(tokens::TOOL_BUTTON_WIDTH, tokens::TOUCH_TARGET);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());
    if ui.is_rect_visible(rect) {
        let enabled = ui.is_enabled();
        let fill = if !enabled {
            tokens::COLOR_HOVER
        } else if selected {
            tokens::COLOR_SELECTED
        } else if response.hovered() {
            tokens::COLOR_HOVER
        } else {
            tokens::COLOR_SURFACE
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(tokens::BUTTON_RADIUS), fill);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(tokens::BUTTON_RADIUS),
            Stroke::new(1.0, tokens::COLOR_BORDER),
            StrokeKind::Inside,
        );

        let icon_center = Pos2::new(
            rect.center().x,
            rect.top() + tokens::SPACE_3 + tokens::ICON_SIZE / 2.0,
        );
        let foreground = if enabled {
            tokens::COLOR_TEXT_SECONDARY
        } else {
            tokens::COLOR_TEXT_TERTIARY
        };
        let accent = if enabled {
            tokens::COLOR_ERROR
        } else {
            tokens::COLOR_TEXT_TERTIARY
        };
        draw_icon(ui, icon_center, icon, swatch, foreground, accent);
        ui.painter().text(
            Pos2::new(rect.center().x, rect.bottom() - tokens::SPACE_3),
            Align2::CENTER_BOTTOM,
            label,
            FontId::proportional(tokens::TEXT_SM),
            if enabled {
                tokens::COLOR_TEXT_PRIMARY
            } else {
                tokens::COLOR_TEXT_TERTIARY
            },
        );
    }
    response
}

/// 使用统一 20pt 线性风格绘制内置功能图标。
fn draw_icon(
    ui: &Ui,
    center: Pos2,
    icon: Icon,
    swatch: Option<Color32>,
    foreground: Color32,
    accent: Color32,
) {
    let painter = ui.painter();
    let half = tokens::ICON_SIZE / 2.0;
    let stroke = Stroke::new(2.0, foreground);
    match icon {
        Icon::Pen => {
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.6, half * 0.6),
                    center + egui::vec2(half * 0.6, -half * 0.6),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.7, half * 0.7),
                    center + egui::vec2(-half * 0.2, half * 0.55),
                ],
                stroke,
            );
        }
        Icon::Settings => {
            painter.circle_stroke(center, half * 0.45, stroke);
            painter.circle_filled(center, half * 0.15, foreground);
            for direction in [
                egui::vec2(0.0, -1.0),
                egui::vec2(1.0, 0.0),
                egui::vec2(0.0, 1.0),
                egui::vec2(-1.0, 0.0),
            ] {
                painter.line_segment(
                    [
                        center + direction * half * 0.6,
                        center + direction * half * 0.9,
                    ],
                    stroke,
                );
            }
        }
        Icon::Sliders => {
            for (offset, knob) in [(-6.0, -3.0), (0.0, 4.0), (6.0, -1.0)] {
                let y = center.y + offset;
                painter.line_segment(
                    [Pos2::new(center.x - half, y), Pos2::new(center.x + half, y)],
                    stroke,
                );
                painter.circle_filled(Pos2::new(center.x + knob, y), 2.5, foreground);
            }
        }
        Icon::Color => {
            painter.circle_filled(center, half * 0.65, swatch.unwrap_or(tokens::COLOR_PRIMARY));
            painter.circle_stroke(center, half * 0.65, stroke);
        }
        Icon::PenWidth => {
            for (offset, width) in [(-5.0, 1.0), (0.0, 2.0), (5.0, 3.0)] {
                painter.line_segment(
                    [
                        Pos2::new(center.x - half * 0.75, center.y + offset),
                        Pos2::new(center.x + half * 0.75, center.y + offset),
                    ],
                    Stroke::new(width, foreground),
                );
            }
        }
        Icon::Eraser => {
            let points = vec![
                center + egui::vec2(-half * 0.7, half * 0.2),
                center + egui::vec2(half * 0.1, -half * 0.7),
                center + egui::vec2(half * 0.7, -half * 0.1),
                center + egui::vec2(-half * 0.1, half * 0.7),
            ];
            painter.add(Shape::closed_line(points, stroke));
        }
        Icon::EraserSize => {
            painter.circle_stroke(center, half * 0.7, stroke);
            painter.circle_stroke(center, half * 0.35, stroke);
        }
        Icon::Undo => {
            painter.circle_stroke(center + egui::vec2(2.0, 1.0), half * 0.65, stroke);
            painter.add(Shape::convex_polygon(
                vec![
                    center + egui::vec2(-half, -2.0),
                    center + egui::vec2(-half * 0.35, -half * 0.7),
                    center + egui::vec2(-half * 0.35, half * 0.35),
                ],
                foreground,
                Stroke::NONE,
            ));
        }
        Icon::Clear => {
            let rect = Rect::from_center_size(center, egui::vec2(16.0, 13.0));
            painter.rect_stroke(rect, 2.0, stroke, StrokeKind::Inside);
            painter.line_segment(
                [rect.left_top(), rect.right_bottom()],
                Stroke::new(2.0, accent),
            );
        }
        Icon::Exit => {
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.8, -half * 0.7),
                    center + egui::vec2(-half * 0.8, half * 0.7),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.8, -half * 0.7),
                    center + egui::vec2(-half * 0.2, -half * 0.7),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.8, half * 0.7),
                    center + egui::vec2(-half * 0.2, half * 0.7),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.15, 0.0),
                    center + egui::vec2(half * 0.75, 0.0),
                ],
                Stroke::new(2.0, accent),
            );
            painter.add(Shape::convex_polygon(
                vec![
                    center + egui::vec2(half * 0.75, 0.0),
                    center + egui::vec2(half * 0.35, -half * 0.35),
                    center + egui::vec2(half * 0.35, half * 0.35),
                ],
                accent,
                Stroke::NONE,
            ));
        }
        Icon::Previous => {
            painter.line_segment(
                [
                    center + egui::vec2(half * 0.55, -half * 0.7),
                    center + egui::vec2(-half * 0.45, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.45, 0.0),
                    center + egui::vec2(half * 0.55, half * 0.7),
                ],
                stroke,
            );
        }
        Icon::Next => {
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.55, -half * 0.7),
                    center + egui::vec2(half * 0.45, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(half * 0.45, 0.0),
                    center + egui::vec2(-half * 0.55, half * 0.7),
                ],
                stroke,
            );
        }
        Icon::Collapse => {
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.65, 0.0),
                    center + egui::vec2(half * 0.2, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.2, -half * 0.45),
                    center + egui::vec2(-half * 0.65, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.65, 0.0),
                    center + egui::vec2(-half * 0.2, half * 0.45),
                ],
                stroke,
            );
        }
        Icon::Expand => {
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.2, 0.0),
                    center + egui::vec2(half * 0.65, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(half * 0.2, -half * 0.45),
                    center + egui::vec2(half * 0.65, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(half * 0.65, 0.0),
                    center + egui::vec2(half * 0.2, half * 0.45),
                ],
                stroke,
            );
        }
        Icon::Confirm => {
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.7, 0.0),
                    center + egui::vec2(-half * 0.2, half * 0.55),
                ],
                Stroke::new(2.0, foreground),
            );
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.2, half * 0.55),
                    center + egui::vec2(half * 0.75, -half * 0.55),
                ],
                Stroke::new(2.0, foreground),
            );
        }
        Icon::Cancel => {
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.65, -half * 0.65),
                    center + egui::vec2(half * 0.65, half * 0.65),
                ],
                Stroke::new(2.0, foreground),
            );
            painter.line_segment(
                [
                    center + egui::vec2(half * 0.65, -half * 0.65),
                    center + egui::vec2(-half * 0.65, half * 0.65),
                ],
                Stroke::new(2.0, foreground),
            );
        }
    }
}

/// 返回画笔粗细按钮使用的紧凑标签。
pub(super) const fn pen_width_label(width: PenWidth) -> &'static str {
    match width {
        PenWidth::Px4 => "4px",
        PenWidth::Px8 => "8px",
        PenWidth::Px16 => "16px",
        PenWidth::Px24 => "24px",
    }
}

/// 返回橡皮擦大小按钮使用的紧凑标签。
pub(super) const fn eraser_size_label(size: EraserSize) -> &'static str {
    match size {
        EraserSize::Px24 => "24px",
        EraserSize::Px48 => "48px",
        EraserSize::Px72 => "72px",
    }
}

/// 把墨迹颜色转换为 egui 色值，保持 Skia 与 UI 色样一致。
pub(super) fn color32(color: InkColor) -> Color32 {
    let [red, green, blue, alpha] = color.rgba();
    Color32::from_rgba_unmultiplied(red, green, blue, alpha)
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Icon {
    Pen,
    Settings,
    Sliders,
    Color,
    PenWidth,
    Eraser,
    EraserSize,
    Undo,
    Clear,
    Exit,
    Previous,
    Next,
    Collapse,
    Expand,
    Confirm,
    Cancel,
}

#[cfg(test)]
mod tests {
    use egui::{Context, RawInput};

    use super::*;
    use crate::window::{IDLE_HEIGHT_POINTS, IDLE_WIDTH_POINTS};

    /// 验证扩窗过渡帧不会因 viewport 小于普通批注工具栏而崩溃，并会在扩窗后重新定位。
    #[test]
    fn normal_toolbar_survives_small_viewport_and_recenters_after_resize() {
        let context = Context::default();
        let state_id = egui::Id::new("normal_annotation_toolbar_state");
        let compact_viewport = Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(IDLE_WIDTH_POINTS as f32, IDLE_HEIGHT_POINTS as f32),
        );

        let _ = context.run_ui(
            RawInput {
                screen_rect: Some(compact_viewport),
                ..Default::default()
            },
            |ui| {
                let _ = render_annotation_toolbar(ui, ToolState::default(), DockSide::Right);
            },
        );
        let compact_state = context
            .data_mut(|data| data.get_temp::<NormalToolbarState>(state_id))
            .expect("普通批注工具栏应保存临时布局状态");
        assert_eq!(compact_state.viewport_size, compact_viewport.size());
        assert_eq!(
            compact_state.position,
            normal_toolbar_position(compact_viewport, normal_toolbar_size(), DockSide::Right,)
        );

        let expanded_viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, 1080.0));
        let _ = context.run_ui(
            RawInput {
                screen_rect: Some(expanded_viewport),
                ..Default::default()
            },
            |ui| {
                let _ = render_annotation_toolbar(ui, ToolState::default(), DockSide::Right);
            },
        );
        let expanded_state = context
            .data_mut(|data| data.get_temp::<NormalToolbarState>(state_id))
            .expect("扩窗后普通批注工具栏应刷新临时布局状态");
        assert_eq!(expanded_state.viewport_size, expanded_viewport.size());
        assert_eq!(
            expanded_state.position,
            normal_toolbar_position(expanded_viewport, normal_toolbar_size(), DockSide::Right,)
        );
    }
}
