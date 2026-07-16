use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Margin, Popup, PopupCloseBehavior, Pos2, Rect,
    RectAlign, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};

use super::{
    design_tokens as tokens,
    settings_controls::{
        SelectorOrientation, color_selector_width, pen_width_selector_width, render_color_selector,
        render_pen_width_selector,
    },
};
use crate::{
    app::AppMode,
    ink::{EraserSize, InkColor, InkTool, PenWidth},
    slideshow::ComDiagnostics,
    window::{DockSide, GraphicsDiagnostics},
};

/// UI 按钮需要交给应用状态机执行的命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    EnterAnnotation,
    ExitAnnotation,
    SelectPen,
    SelectEraser,
    CycleEraserSize,
    SetColor(InkColor),
    SetPenWidth(PenWidth),
    SetEraserSize(EraserSize),
    Undo,
    Clear,
    OpenSettings,
    CloseSettings,
    ExitApplication,
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
    pub graphics_diagnostics: &'a GraphicsDiagnostics,
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
        self.observe_drag(response);
    }

    /// 合并不直接产生命令的弹层触发按钮拖动状态。
    fn observe_drag(&mut self, response: &Response) {
        if response.dragged() {
            self.drag_delta += response.drag_delta();
        }
        self.drag_stopped |= response.drag_stopped();
    }
}

impl Default for ToolState {
    /// 返回已确认的默认画笔、红色、4pt 和 48px 配置。
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
    render_idle_toolbar_with_surface(ui, false)
}

/// 绘制快捷设置窗口中的不透明悬浮工具栏。
pub(super) fn render_opaque_idle_toolbar(ui: &mut Ui) -> Option<UiCommand> {
    render_idle_toolbar_with_surface(ui, true)
}

/// 根据所在界面选择半透明或不透明表面后绘制非批注悬浮工具栏。
fn render_idle_toolbar_with_surface(ui: &mut Ui, opaque: bool) -> Option<UiCommand> {
    let mut command = None;
    let (background, border) = if opaque {
        (tokens::OPAQUE_COLOR_BACKGROUND, tokens::OPAQUE_COLOR_BORDER)
    } else {
        (tokens::COLOR_BACKGROUND, tokens::COLOR_BORDER)
    };
    Frame::new()
        .fill(background)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(tokens::CARD_RADIUS))
        .inner_margin(Margin::same(tokens::MARGIN_SPACE_2))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                let annotation =
                    icon_button_with_surface(ui, "批注", Icon::Pen, false, None, opaque);
                if annotation.drag_started() {
                    command = Some(UiCommand::BeginIdleToolbarDrag);
                } else if annotation.clicked() {
                    command = Some(UiCommand::EnterAnnotation);
                }
                let settings =
                    icon_button_with_surface(ui, "设置", Icon::Settings, false, None, opaque);
                if settings.drag_started() {
                    command = Some(UiCommand::BeginIdleToolbarDrag);
                } else if settings.clicked() {
                    command = Some(UiCommand::OpenSettings);
                }
                let quick_settings =
                    icon_button_with_surface(ui, "快捷设置", Icon::Sliders, false, None, opaque);
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
    let picker_open = Popup::is_any_open(&context);
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
            let (background, border) = normal_toolbar_surface(picker_open);
            Frame::new()
                .fill(background)
                .stroke(Stroke::new(1.0, border))
                .corner_radius(CornerRadius::same(tokens::CARD_RADIUS))
                .inner_margin(Margin::same(tokens::MARGIN_SPACE_2))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        let mut interaction = render_ink_tool_buttons(
                            ui,
                            tools,
                            normal_picker_placement(dock_side),
                            SelectorOrientation::Vertical,
                            picker_open,
                        );
                        let exit = icon_button_with_surface(
                            ui,
                            "退出批注",
                            Icon::Exit,
                            false,
                            None,
                            picker_open,
                        );
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
        state.position =
            normal_toolbar_docked_position(screen, toolbar_size, state.dock_side, state.position.y);
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

/// 返回普通工具栏在弹层关闭和打开状态下使用的表面颜色。
const fn normal_toolbar_surface(picker_open: bool) -> (Color32, Color32) {
    if picker_open {
        (tokens::OPAQUE_COLOR_BACKGROUND, tokens::OPAQUE_COLOR_BORDER)
    } else {
        (tokens::COLOR_BACKGROUND, tokens::COLOR_BORDER)
    }
}

/// 返回八个纵向按钮及卡片内边距形成的固定工具栏尺寸。
fn normal_toolbar_size() -> Vec2 {
    const BUTTON_COUNT: f32 = 8.0;
    Vec2::new(
        tokens::TOUCH_TARGET + tokens::SPACE_2 * 2.0,
        BUTTON_COUNT * tokens::TOUCH_TARGET
            + (BUTTON_COUNT - 1.0) * tokens::SPACE_2
            + tokens::SPACE_2 * 2.0,
    )
}

/// 根据吸附边缘计算普通批注工具栏的稳定屏幕位置。
fn normal_toolbar_position(screen: Rect, size: Vec2, dock_side: DockSide) -> Pos2 {
    normal_toolbar_docked_position(screen, size, dock_side, screen.center().y - size.y / 2.0)
}

/// 吸附普通批注工具栏的横向位置，同时保留并约束用户选择的纵向位置。
fn normal_toolbar_docked_position(
    screen: Rect,
    size: Vec2,
    dock_side: DockSide,
    preferred_y: f32,
) -> Pos2 {
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
            preferred_y,
            screen.top(),
            screen.bottom(),
            size.y,
            tokens::SPACE_6,
        ),
    )
}

/// 根据普通批注工具栏所在侧，把纵向选择栏放到面向屏幕内部的一边。
const fn normal_picker_placement(dock_side: DockSide) -> RectAlign {
    match dock_side {
        DockSide::Left => RectAlign::RIGHT_START,
        DockSide::Right => RectAlign::LEFT_START,
    }
}

static LEFT_PICKER_ALTERNATIVES: [RectAlign; 1] = [RectAlign::LEFT_END];
static RIGHT_PICKER_ALTERNATIVES: [RectAlign; 1] = [RectAlign::RIGHT_END];

/// 返回同一屏幕内侧的备用弹层位置，避免 egui 反向翻转后覆盖普通工具栏。
fn normal_picker_alternatives(placement: RectAlign) -> &'static [RectAlign] {
    if placement == RectAlign::LEFT_START {
        &LEFT_PICKER_ALTERNATIVES
    } else if placement == RectAlign::RIGHT_START {
        &RIGHT_PICKER_ALTERNATIVES
    } else {
        &[]
    }
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
pub(super) fn render_ink_tool_buttons(
    ui: &mut Ui,
    tools: ToolState,
    picker_placement: RectAlign,
    picker_orientation: SelectorOrientation,
    opaque: bool,
) -> ToolbarInteraction {
    let mut interaction = ToolbarInteraction::default();
    let pen = icon_button_with_surface(
        ui,
        "画笔",
        Icon::Pen,
        tools.tool == InkTool::Pen,
        None,
        opaque,
    );
    interaction.observe(&pen, UiCommand::SelectPen);
    let color = icon_button_with_surface(
        ui,
        "颜色",
        Icon::Color,
        false,
        Some(color32(tools.color)),
        opaque,
    );
    interaction.observe_drag(&color);
    keep_picker_command(
        &mut interaction,
        render_color_picker(&color, tools.color, picker_placement, picker_orientation),
    );
    let pen_width = icon_button_with_surface(
        ui,
        pen_width_label(tools.pen_width),
        Icon::PenWidth,
        false,
        None,
        opaque,
    );
    interaction.observe_drag(&pen_width);
    keep_picker_command(
        &mut interaction,
        render_pen_width_picker(
            &pen_width,
            tools.pen_width,
            picker_placement,
            picker_orientation,
        ),
    );
    let eraser = icon_button_with_surface(
        ui,
        "橡皮擦",
        Icon::Eraser,
        tools.tool == InkTool::RegionEraser,
        None,
        opaque,
    );
    interaction.observe(&eraser, UiCommand::SelectEraser);
    let eraser_size = icon_button_with_surface(
        ui,
        eraser_size_label(tools.eraser_size),
        Icon::EraserSize,
        false,
        None,
        opaque,
    );
    interaction.observe(&eraser_size, UiCommand::CycleEraserSize);
    let undo = icon_button_with_surface(ui, "撤销", Icon::Undo, false, None, opaque);
    interaction.observe(&undo, UiCommand::Undo);
    let clear = icon_button_with_surface(ui, "清屏", Icon::Clear, false, None, opaque);
    interaction.observe(&clear, UiCommand::Clear);
    interaction
}

/// 在工具栏尚未产生其他命令时保留弹层选择命令。
fn keep_picker_command(interaction: &mut ToolbarInteraction, candidate: Option<UiCommand>) {
    if interaction.command.is_none() {
        interaction.command = candidate;
    }
}

/// 切换颜色弹层，并复用设置页的固定色样选择控件。
fn render_color_picker(
    trigger: &Response,
    selected: InkColor,
    placement: RectAlign,
    orientation: SelectorOrientation,
) -> Option<UiCommand> {
    let selector_width = color_selector_width(orientation);
    let popup_style = trigger.ctx.style_of(egui::Theme::Light);
    let popup = Popup::from_toggle_button_response(trigger)
        .align(placement)
        .width(selector_width)
        .frame(opaque_picker_frame(&popup_style));
    let popup = if orientation == SelectorOrientation::Vertical {
        popup
            .gap(tokens::SPACE_2)
            .align_alternatives(normal_picker_alternatives(placement))
    } else {
        popup
    };
    popup
        .close_behavior(PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.set_min_width(selector_width);
            ui.set_max_width(selector_width);
            render_color_selector(ui, selected, orientation)
        })
        .and_then(|response| response.inner)
}

/// 切换粗细弹层，并复用设置页具有线宽差异预览的选择控件。
fn render_pen_width_picker(
    trigger: &Response,
    selected: PenWidth,
    placement: RectAlign,
    orientation: SelectorOrientation,
) -> Option<UiCommand> {
    let selector_width = pen_width_selector_width(orientation);
    let popup_style = trigger.ctx.style_of(egui::Theme::Light);
    let popup = Popup::from_toggle_button_response(trigger)
        .align(placement)
        .width(selector_width)
        .frame(opaque_picker_frame(&popup_style));
    let popup = if orientation == SelectorOrientation::Vertical {
        popup
            .gap(tokens::SPACE_2)
            .align_alternatives(normal_picker_alternatives(placement))
    } else {
        popup
    };
    popup
        .close_behavior(PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.set_min_width(selector_width);
            ui.set_max_width(selector_width);
            render_pen_width_selector(ui, selected, orientation)
        })
        .and_then(|response| response.inner)
}

/// 返回颜色和粗细选择弹层在打开时使用的不透明边框框架。
fn opaque_picker_frame(style: &egui::Style) -> Frame {
    Frame::popup(style)
        .fill(tokens::OPAQUE_COLOR_BACKGROUND)
        .stroke(Stroke::new(1.0, tokens::OPAQUE_COLOR_BORDER))
}

/// 绘制固定触摸尺寸、图标在上文字在下的功能按钮。
pub(super) fn icon_button(
    ui: &mut Ui,
    label: &str,
    icon: Icon,
    selected: bool,
    swatch: Option<Color32>,
) -> Response {
    icon_button_with_surface(ui, label, icon, selected, swatch, false)
}

/// 绘制设置和快捷设置使用的不透明图标加文字按钮。
pub(super) fn opaque_icon_button(
    ui: &mut Ui,
    label: &str,
    icon: Icon,
    selected: bool,
    swatch: Option<Color32>,
) -> Response {
    icon_button_with_surface(ui, label, icon, selected, swatch, true)
}

/// 根据所在界面选择表面透明度后绘制统一的图标加文字按钮。
fn icon_button_with_surface(
    ui: &mut Ui,
    label: &str,
    icon: Icon,
    selected: bool,
    swatch: Option<Color32>,
    opaque: bool,
) -> Response {
    let desired_size = Vec2::splat(tokens::TOUCH_TARGET);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());
    if ui.is_rect_visible(rect) {
        let enabled = ui.is_enabled();
        let (surface, hover, selected_surface, border) = if opaque {
            (
                tokens::OPAQUE_COLOR_SURFACE,
                tokens::OPAQUE_COLOR_HOVER,
                tokens::OPAQUE_COLOR_SELECTED,
                tokens::OPAQUE_COLOR_BORDER,
            )
        } else {
            (
                tokens::COLOR_SURFACE,
                tokens::COLOR_HOVER,
                tokens::COLOR_SELECTED,
                tokens::COLOR_BORDER,
            )
        };
        let fill = if !enabled {
            hover
        } else if selected {
            selected_surface
        } else if response.hovered() {
            hover
        } else {
            surface
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(tokens::BUTTON_RADIUS), fill);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(tokens::BUTTON_RADIUS),
            Stroke::new(1.0, border),
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
    let stroke = Stroke::new(tokens::scale_points(2.0), foreground);
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
            for (offset, knob) in [
                (-half * 0.6, -half * 0.3),
                (0.0, half * 0.4),
                (half * 0.6, -half * 0.1),
            ] {
                let y = center.y + offset;
                painter.line_segment(
                    [Pos2::new(center.x - half, y), Pos2::new(center.x + half, y)],
                    stroke,
                );
                painter.circle_filled(Pos2::new(center.x + knob, y), half * 0.25, foreground);
            }
        }
        Icon::Color => {
            painter.circle_filled(center, half * 0.65, swatch.unwrap_or(tokens::COLOR_PRIMARY));
            painter.circle_stroke(center, half * 0.65, stroke);
        }
        Icon::PenWidth => {
            for (offset, width) in [
                (-half * 0.5, tokens::scale_points(1.0)),
                (0.0, tokens::scale_points(2.0)),
                (half * 0.5, tokens::scale_points(3.0)),
            ] {
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
            painter.circle_stroke(
                center + egui::vec2(tokens::scale_points(2.0), tokens::scale_points(1.0)),
                half * 0.65,
                stroke,
            );
            painter.add(Shape::convex_polygon(
                vec![
                    center + egui::vec2(-half, -tokens::scale_points(2.0)),
                    center + egui::vec2(-half * 0.35, -half * 0.7),
                    center + egui::vec2(-half * 0.35, half * 0.35),
                ],
                foreground,
                Stroke::NONE,
            ));
        }
        Icon::Clear => {
            let rect = Rect::from_center_size(
                center,
                egui::vec2(tokens::scale_points(16.0), tokens::scale_points(13.0)),
            );
            painter.rect_stroke(rect, tokens::scale_points(2.0), stroke, StrokeKind::Inside);
            painter.line_segment(
                [rect.left_top(), rect.right_bottom()],
                Stroke::new(tokens::scale_points(2.0), accent),
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
                Stroke::new(tokens::scale_points(2.0), accent),
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
                Stroke::new(tokens::scale_points(2.0), foreground),
            );
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.2, half * 0.55),
                    center + egui::vec2(half * 0.75, -half * 0.55),
                ],
                Stroke::new(tokens::scale_points(2.0), foreground),
            );
        }
        Icon::Cancel => {
            painter.line_segment(
                [
                    center + egui::vec2(-half * 0.65, -half * 0.65),
                    center + egui::vec2(half * 0.65, half * 0.65),
                ],
                Stroke::new(tokens::scale_points(2.0), foreground),
            );
            painter.line_segment(
                [
                    center + egui::vec2(half * 0.65, -half * 0.65),
                    center + egui::vec2(-half * 0.65, half * 0.65),
                ],
                Stroke::new(tokens::scale_points(2.0), foreground),
            );
        }
        Icon::Power => {
            painter.circle_stroke(center, half * 0.72, stroke);
            painter.line_segment(
                [
                    center + egui::vec2(0.0, -half),
                    center + egui::vec2(0.0, -half * 0.15),
                ],
                Stroke::new(tokens::scale_points(2.0), accent),
            );
        }
    }
}

/// 返回画笔粗细按钮使用的紧凑标签。
pub(super) const fn pen_width_label(width: PenWidth) -> &'static str {
    match width {
        PenWidth::Px4 => "4pt",
        PenWidth::Px8 => "8pt",
        PenWidth::Px16 => "16pt",
        PenWidth::Px24 => "24pt",
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
    Power,
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
            Vec2::new(
                IDLE_WIDTH_POINTS.round() as f32,
                IDLE_HEIGHT_POINTS.round() as f32,
            ),
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
        assert!(
            (compact_state.viewport_size - compact_viewport.size()).length() < 0.01,
            "紧凑 viewport 应保持缩放后的实际尺寸"
        );
        let expected_compact_position =
            normal_toolbar_position(compact_viewport, normal_toolbar_size(), DockSide::Right);
        assert!(
            (compact_state.position - expected_compact_position).length() < 0.01,
            "紧凑 viewport 中的工具栏位置应稳定"
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

    /// 验证普通工具栏吸附左右侧时只改变横坐标，并保留可见的纵向位置。
    #[test]
    fn normal_toolbar_side_snap_preserves_vertical_position() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_920.0, 1_080.0));
        let size = normal_toolbar_size();
        let preferred_y = 120.0;
        let left = normal_toolbar_docked_position(screen, size, DockSide::Left, preferred_y);
        let right = normal_toolbar_docked_position(screen, size, DockSide::Right, preferred_y);

        assert_eq!(left.y, preferred_y);
        assert_eq!(right.y, preferred_y);
        assert_eq!(left.x, tokens::SPACE_6);
        assert_eq!(right.x, screen.right() - tokens::SPACE_6 - size.x);
    }

    /// 验证普通工具栏两侧的纵向选择栏始终向屏幕内部展开。
    #[test]
    fn normal_picker_opens_toward_screen_interior() {
        let trigger =
            Rect::from_min_size(Pos2::new(800.0, 300.0), Vec2::splat(tokens::TOUCH_TARGET));
        let popup_size = Vec2::new(tokens::TOUCH_TARGET, tokens::TOUCH_TARGET * 6.0);

        for (dock_side, expected) in [
            (DockSide::Left, RectAlign::RIGHT_START),
            (DockSide::Right, RectAlign::LEFT_START),
        ] {
            let placement = normal_picker_placement(dock_side);
            assert_eq!(placement, expected);

            for alignment in std::iter::once(placement)
                .chain(normal_picker_alternatives(placement).iter().copied())
            {
                let popup = alignment.align_rect(&trigger, popup_size, tokens::SPACE_2);
                match dock_side {
                    DockSide::Left => assert!(popup.left() >= trigger.right()),
                    DockSide::Right => assert!(popup.right() <= trigger.left()),
                }
            }
        }
    }

    /// 验证普通工具栏仅在颜色或粗细弹层打开时切换为不透明表面。
    #[test]
    fn normal_toolbar_surface_tracks_picker_open_state() {
        let (closed_background, closed_border) = normal_toolbar_surface(false);
        assert_eq!(closed_background.a(), tokens::INTERFACE_ALPHA);
        assert_eq!(closed_border.a(), tokens::INTERFACE_ALPHA);

        let (open_background, open_border) = normal_toolbar_surface(true);
        assert_eq!(open_background.a(), tokens::OPAQUE_INTERFACE_ALPHA);
        assert_eq!(open_border.a(), tokens::OPAQUE_INTERFACE_ALPHA);
    }
}
