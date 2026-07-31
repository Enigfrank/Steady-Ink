use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Margin, Popup, PopupCloseBehavior, Pos2, Rect,
    RectAlign, Response, Sense, Shape, Stroke, Ui, Vec2,
};

use super::{
    design_tokens as tokens, pixel_snap,
    settings_controls::{
        SelectorOrientation, color_selector_width, pen_width_selector_width, render_color_selector,
        render_pen_width_selector,
    },
};
use crate::{
    app::{AppMode, SlideshowInputMode},
    autostart::MachineAutostartState,
    ink::{EraserSize, InkColor, InkTool, PenWidth},
    performance::PerformanceSnapshot,
    settings::{LogLevel, PalmSizePreset},
    slideshow::ComDiagnostics,
    window::{DockSide, GraphicsDiagnostics},
};

/// UI 按钮需要交给应用状态机执行的命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    EnterAnnotation,
    ExitAnnotation,
    SelectPen,
    ToggleSlideshowPenMode,
    SelectEraser,
    CycleEraserSize,
    SetColor(InkColor),
    SetPenWidth(PenWidth),
    SetEraserSize(EraserSize),
    SetNaturalTaperEnabled(bool),
    SetPalmSizePreset(PalmSizePreset),
    Undo,
    Clear,
    OpenSettings,
    OpenSettingsDirectory,
    RestartApplication,
    CloseSettings,
    ExitApplication,
    ToggleQuickSettings,
    BeginIdleToolbarDrag,
    SetDockSide(DockSide),
    SetSlideshowIntegrationEnabled(bool),
    SetLogLevel(LogLevel),
    SetReadableMode(bool),
    SetPerformanceMonitoringEnabled(bool),
    ExportPerformanceData,
    SetMachineAutostart(bool),
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
    pub slideshow_input_mode: SlideshowInputMode,
    pub idle_panel: IdlePanel,
    pub dock_side: DockSide,
    pub tools: ToolState,
    pub palm_size_preset: PalmSizePreset,
    pub slideshow_integration_enabled: bool,
    pub log_level: LogLevel,
    pub readable_mode: bool,
    pub performance_monitoring_enabled: bool,
    pub performance_snapshot: PerformanceSnapshot,
    pub performance_export_status: Option<&'a str>,
    pub performance_export_failed: bool,
    pub ink_rendering_error: Option<&'a str>,
    pub slideshow_session_generation: Option<u64>,
    pub slide_page_numbers: Option<(u32, u32)>,
    pub slideshow_controls_enabled: bool,
    pub dismiss_slideshow_confirmation: bool,
    pub com_diagnostics: Option<&'a ComDiagnostics>,
    pub slideshow_connection_error: Option<&'a str>,
    pub slideshow_control_error: Option<&'a str>,
    pub machine_autostart_state: Option<MachineAutostartState>,
    pub machine_autostart_error: Option<&'a str>,
    pub graphics_diagnostics: &'a GraphicsDiagnostics,
}

/// 顶层 UI 一帧产生的离散命令和放映原生命中区域。
#[derive(Debug, Default)]
pub struct UiFrameOutput {
    pub command: Option<UiCommand>,
    pub slideshow_hit_regions: Vec<Rect>,
}

/// 普通批注工具栏当前选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolState {
    pub tool: InkTool,
    pub color: InkColor,
    pub pen_width: PenWidth,
    pub eraser_size: EraserSize,
    pub natural_taper_enabled: bool,
}

/// 工具按钮组在当前帧产生的命令、拖动状态和打开中的弹层区域。
#[derive(Debug, Default)]
pub(super) struct ToolbarInteraction {
    pub command: Option<UiCommand>,
    pub drag_delta: Vec2,
    pub drag_stopped: bool,
    /// 本帧打开的弹层区域，供放映控件窗口扩展命中区域。
    pub popup_rects: Vec<Rect>,
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
    /// 返回已确认的默认画笔、红色、4px 和 72px 配置。
    fn default() -> Self {
        Self {
            tool: InkTool::default(),
            color: InkColor::default(),
            pen_width: PenWidth::default(),
            eraser_size: EraserSize::default(),
            natural_taper_enabled: false,
        }
    }
}

impl ToolState {
    /// 按 36px、72px、144px 的固定顺序切换区域橡皮擦大小。
    pub fn cycle_eraser_size(&mut self) {
        self.eraser_size = match self.eraser_size {
            EraserSize::Px36 => EraserSize::Px72,
            EraserSize::Px72 => EraserSize::Px144,
            EraserSize::Px144 => EraserSize::Px36,
        };
    }
}

/// 根据顶层模式绘制当前工具栏，并返回命令及实际放映 UI 命中区域。
pub fn render(ui: &mut Ui, view: UiViewState<'_>) -> UiFrameOutput {
    let output = match view.mode {
        AppMode::IdleFloatingToolbar => match view.idle_panel {
            IdlePanel::Toolbar => UiFrameOutput {
                command: render_idle_toolbar(ui, view.readable_mode),
                ..UiFrameOutput::default()
            },
            IdlePanel::QuickSettings => UiFrameOutput {
                command: super::quick_settings::render(ui, view),
                ..UiFrameOutput::default()
            },
            IdlePanel::Settings => UiFrameOutput {
                command: super::settings_view::render(ui, view),
                ..UiFrameOutput::default()
            },
        },
        AppMode::NormalAnnotating => UiFrameOutput {
            command: render_annotation_toolbar(ui, view.tools, view.dock_side, view.readable_mode),
            ..UiFrameOutput::default()
        },
        AppMode::SlideShowAnnotatingExpanded
        | AppMode::SlideShowAnnotatingCollapsed
        | AppMode::SlideShowConnectionLost => super::slideshow_toolbar::render(ui.ctx(), view),
    };
    if view.performance_monitoring_enabled && view.mode.accepts_ink_input() {
        super::performance_overlay::render(ui.ctx(), view.performance_snapshot, view.readable_mode);
    }
    output
}

/// 绘制右侧中部非批注悬浮卡片工具栏。
pub(super) fn render_idle_toolbar(ui: &mut Ui, readable_mode: bool) -> Option<UiCommand> {
    render_idle_toolbar_with_surface(ui, readable_mode)
}

/// 绘制未批注状态的主操作优先悬浮工具栏。
fn render_idle_toolbar_with_surface(ui: &mut Ui, readable_mode: bool) -> Option<UiCommand> {
    let mut command = None;
    pixel_snap::show_pixel_aligned_frame(
        ui,
        tokens::material_frame(
            readable_mode,
            tokens::MaterialRole::Floating,
            CornerRadius::same(tokens::CARD_RADIUS),
            Margin::same(tokens::MARGIN_SPACE_2),
        ),
        |ui| {
            ui.vertical_centered(|ui| {
                let annotation =
                    icon_button_with_surface(ui, "开始批注", Icon::Pen, true, None, readable_mode);
                if annotation.drag_started() {
                    command = Some(UiCommand::BeginIdleToolbarDrag);
                } else if annotation.clicked() {
                    command = Some(UiCommand::EnterAnnotation);
                }
                ui.add_space(tokens::SPACE_2);
                let quick_settings = icon_button_with_surface(
                    ui,
                    "快捷设置",
                    Icon::Sliders,
                    false,
                    None,
                    readable_mode,
                );
                if quick_settings.drag_started() {
                    command = Some(UiCommand::BeginIdleToolbarDrag);
                } else if quick_settings.clicked() {
                    command = Some(UiCommand::ToggleQuickSettings);
                }
                ui.add_space(tokens::SPACE_2);
                let settings = icon_button_with_surface(
                    ui,
                    "设置",
                    Icon::Settings,
                    false,
                    None,
                    readable_mode,
                );
                if settings.drag_started() {
                    command = Some(UiCommand::BeginIdleToolbarDrag);
                } else if settings.clicked() {
                    command = Some(UiCommand::OpenSettings);
                }
            });
        },
    );
    command
}

/// 绘制普通批注模式右侧浅色卡片工具栏。
fn render_annotation_toolbar(
    ui: &mut Ui,
    tools: ToolState,
    dock_side: DockSide,
    readable_mode: bool,
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
            pixel_snap::show_pixel_aligned_frame(
                ui,
                tokens::material_frame(
                    readable_mode,
                    tokens::MaterialRole::Floating,
                    CornerRadius::same(tokens::CARD_RADIUS),
                    Margin::same(tokens::MARGIN_SPACE_2),
                ),
                |ui| {
                    ui.vertical_centered(|ui| {
                        let mut interaction = render_ink_tool_buttons(
                            ui,
                            tools,
                            None,
                            normal_picker_placement(dock_side),
                            SelectorOrientation::Vertical,
                            readable_mode,
                        );
                        ui.add_space(tokens::SPACE_2);
                        let exit = icon_button_with_surface(
                            ui,
                            "退出批注",
                            Icon::Exit,
                            false,
                            None,
                            readable_mode,
                        );
                        interaction.observe(&exit, UiCommand::ExitAnnotation);
                        interaction
                    })
                    .inner
                },
            )
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

/// 返回八个纵向按钮及卡片内边距形成的固定工具栏尺寸。
fn normal_toolbar_size() -> Vec2 {
    const BUTTON_COUNT: f32 = 8.0;
    const GROUP_GAP_COUNT: f32 = 3.0;
    Vec2::new(
        tokens::TOUCH_TARGET + tokens::SPACE_2 * 2.0,
        BUTTON_COUNT * tokens::TOUCH_TARGET
            + (BUTTON_COUNT - 1.0 + GROUP_GAP_COUNT) * tokens::SPACE_2
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
    slideshow_input_mode: Option<SlideshowInputMode>,
    picker_placement: RectAlign,
    picker_orientation: SelectorOrientation,
    readable_mode: bool,
) -> ToolbarInteraction {
    let mut interaction = ToolbarInteraction::default();
    let mouse_mode = slideshow_input_mode == Some(SlideshowInputMode::Mouse);
    let (pen_label, pen_icon, pen_command) = if mouse_mode {
        ("触摸", Icon::Cursor, UiCommand::ToggleSlideshowPenMode)
    } else if slideshow_input_mode.is_some() {
        ("画笔", Icon::Pen, UiCommand::ToggleSlideshowPenMode)
    } else {
        ("画笔", Icon::Pen, UiCommand::SelectPen)
    };
    let pen = icon_button_with_surface(
        ui,
        pen_label,
        pen_icon,
        mouse_mode || tools.tool == InkTool::Pen,
        None,
        readable_mode,
    );
    interaction.observe(&pen, pen_command);
    let color = ui
        .add_enabled_ui(!mouse_mode, |ui| {
            icon_button_with_surface(
                ui,
                "颜色",
                Icon::Color,
                tools.tool == InkTool::Pen,
                Some(color32(tools.color)),
                readable_mode,
            )
        })
        .inner;
    interaction.observe_drag(&color);
    let (color_command, color_rect) = render_color_picker(
        &color,
        tools.color,
        picker_placement,
        picker_orientation,
        readable_mode,
    );
    keep_picker_command(&mut interaction, color_command);
    if let Some(rect) = color_rect {
        interaction.popup_rects.push(rect);
    }
    let pen_width = ui
        .add_enabled_ui(!mouse_mode, |ui| {
            icon_button_with_surface(
                ui,
                pen_width_label(tools.pen_width),
                Icon::PenWidth,
                tools.tool == InkTool::Pen,
                None,
                readable_mode,
            )
        })
        .inner;
    interaction.observe_drag(&pen_width);
    let (width_command, width_rect) = render_pen_width_picker(
        &pen_width,
        tools.pen_width,
        picker_placement,
        picker_orientation,
        readable_mode,
    );
    keep_picker_command(&mut interaction, width_command);
    if let Some(rect) = width_rect {
        interaction.popup_rects.push(rect);
    }
    ui.add_space(tokens::SPACE_2);
    let eraser = icon_button_with_surface(
        ui,
        "橡皮擦",
        Icon::Eraser,
        tools.tool == InkTool::RegionEraser,
        None,
        readable_mode,
    );
    interaction.observe(&eraser, UiCommand::SelectEraser);
    let eraser_size = icon_button_with_surface(
        ui,
        eraser_size_label(tools.eraser_size),
        Icon::EraserSize,
        tools.tool == InkTool::RegionEraser,
        None,
        readable_mode,
    );
    interaction.observe(&eraser_size, UiCommand::CycleEraserSize);
    ui.add_space(tokens::SPACE_2);
    let undo = icon_button_with_surface(ui, "撤销", Icon::Undo, false, None, readable_mode);
    interaction.observe(&undo, UiCommand::Undo);
    let clear = icon_button_with_surface(ui, "清屏", Icon::Clear, false, None, readable_mode);
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
    readable_mode: bool,
) -> (Option<UiCommand>, Option<Rect>) {
    let selector_width = color_selector_width(orientation, tokens::TOOL_METRICS);
    let popup_style = trigger.ctx.style_of(egui::Theme::Light);
    let picker_frame = picker_frame(&popup_style, readable_mode);
    let popup = Popup::from_toggle_button_response(trigger)
        .align(placement)
        .width(selector_width)
        .frame(Frame::NONE);
    let popup = if orientation == SelectorOrientation::Vertical {
        popup
            .gap(tokens::SPACE_2)
            .align_alternatives(normal_picker_alternatives(placement))
    } else {
        popup
    };
    let popup_id = popup.get_id();
    let context = popup.ctx().clone();
    match popup
        .close_behavior(PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            pixel_snap::show_pixel_aligned_frame(ui, picker_frame, |ui| {
                ui.set_min_width(selector_width);
                ui.set_max_width(selector_width);
                render_color_selector(
                    ui,
                    selected,
                    orientation,
                    tokens::TOOL_METRICS,
                    readable_mode,
                )
            })
            .inner
        }) {
        Some(response) => {
            let rect = Popup::is_id_open(&context, popup_id).then_some(response.response.rect);
            (response.inner, rect)
        }
        None => (None, None),
    }
}

/// 切换粗细弹层，并复用设置页具有线宽差异预览的选择控件。
fn render_pen_width_picker(
    trigger: &Response,
    selected: PenWidth,
    placement: RectAlign,
    orientation: SelectorOrientation,
    readable_mode: bool,
) -> (Option<UiCommand>, Option<Rect>) {
    let selector_width = pen_width_selector_width(orientation, tokens::TOOL_METRICS);
    let popup_style = trigger.ctx.style_of(egui::Theme::Light);
    let picker_frame = picker_frame(&popup_style, readable_mode);
    let popup = Popup::from_toggle_button_response(trigger)
        .align(placement)
        .width(selector_width)
        .frame(Frame::NONE);
    let popup = if orientation == SelectorOrientation::Vertical {
        popup
            .gap(tokens::SPACE_2)
            .align_alternatives(normal_picker_alternatives(placement))
    } else {
        popup
    };
    let popup_id = popup.get_id();
    let context = popup.ctx().clone();
    match popup
        .close_behavior(PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            pixel_snap::show_pixel_aligned_frame(ui, picker_frame, |ui| {
                ui.set_min_width(selector_width);
                ui.set_max_width(selector_width);
                render_pen_width_selector(
                    ui,
                    selected,
                    orientation,
                    tokens::TOOL_METRICS,
                    readable_mode,
                )
            })
            .inner
        }) {
        Some(response) => {
            let rect = Popup::is_id_open(&context, popup_id).then_some(response.response.rect);
            (response.inner, rect)
        }
        None => (None, None),
    }
}

/// 返回颜色和粗细选择弹层的材质框架，并沿用 egui 的弹层内边距。
fn picker_frame(style: &egui::Style, readable_mode: bool) -> Frame {
    let popup = Frame::popup(style);
    tokens::material_frame(
        readable_mode,
        tokens::MaterialRole::Popover,
        popup.corner_radius,
        popup.inner_margin,
    )
}

/// 绘制固定触摸尺寸、图标在上文字在下的功能按钮。
pub(super) fn icon_button(
    ui: &mut Ui,
    label: &str,
    icon: Icon,
    selected: bool,
    swatch: Option<Color32>,
    readable_mode: bool,
) -> Response {
    icon_button_with_surface(ui, label, icon, selected, swatch, readable_mode)
}

/// 根据当前外观模式绘制统一的图标加文字按钮。
fn icon_button_with_surface(
    ui: &mut Ui,
    label: &str,
    icon: Icon,
    selected: bool,
    swatch: Option<Color32>,
    readable_mode: bool,
) -> Response {
    let desired_size = Vec2::splat(tokens::TOUCH_TARGET);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());
    if ui.is_rect_visible(rect) {
        let enabled = ui.is_enabled();
        let (fill, border) = tokens::button_colors(readable_mode, selected, &response, enabled);
        pixel_snap::paint_pixel_aligned_rect(
            ui,
            rect,
            CornerRadius::same(tokens::BUTTON_RADIUS),
            fill,
            Stroke::new(1.0, border),
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
    response.on_hover_text(label)
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
    paint_icon(
        ui,
        center,
        icon,
        swatch,
        foreground,
        accent,
        tokens::TOOL_METRICS,
    );
}

/// 返回标准顺时针重启图标使用的开放圆弧采样点。
fn restart_icon_arc(center: Pos2, half: f32) -> Vec<Pos2> {
    const SEGMENT_COUNT: usize = 20;
    let radius = half * 0.72;
    let start_angle = 0.5_f32;
    let sweep = std::f32::consts::TAU - 1.0;
    (0..=SEGMENT_COUNT)
        .map(|step| {
            let progress = step as f32 / SEGMENT_COUNT as f32;
            let angle = start_angle + sweep * progress;
            center + egui::vec2(angle.cos() * radius, angle.sin() * radius)
        })
        .collect()
}

/// 按指定页面尺寸 profile 绘制共享线性图标。
pub(super) fn paint_icon(
    ui: &Ui,
    center: Pos2,
    icon: Icon,
    swatch: Option<Color32>,
    foreground: Color32,
    accent: Color32,
    metrics: tokens::InterfaceMetrics,
) {
    let painter = ui.painter();
    let half = metrics.icon_size / 2.0;
    let stroke = Stroke::new(metrics.points(2.0), foreground);
    let paint_line = |points, stroke| {
        pixel_snap::paint_pixel_aligned_line(painter, points, stroke);
    };
    match icon {
        Icon::Pen => {
            paint_line(
                [
                    center + egui::vec2(-half * 0.6, half * 0.6),
                    center + egui::vec2(half * 0.6, -half * 0.6),
                ],
                stroke,
            );
            paint_line(
                [
                    center + egui::vec2(-half * 0.7, half * 0.7),
                    center + egui::vec2(-half * 0.2, half * 0.55),
                ],
                stroke,
            );
        }
        Icon::Cursor => {
            let points = vec![
                center + egui::vec2(-half * 0.65, -half * 0.85),
                center + egui::vec2(half * 0.65, half * 0.15),
                center + egui::vec2(half * 0.1, half * 0.25),
                center + egui::vec2(half * 0.45, half * 0.8),
                center + egui::vec2(half * 0.1, half),
                center + egui::vec2(-half * 0.2, half * 0.45),
                center + egui::vec2(-half * 0.65, half * 0.75),
            ];
            painter.add(Shape::convex_polygon(points, foreground, stroke));
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
                paint_line(
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
                paint_line(
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
                (-half * 0.5, metrics.points(1.0)),
                (0.0, metrics.points(2.0)),
                (half * 0.5, metrics.points(3.0)),
            ] {
                paint_line(
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
                center + egui::vec2(metrics.points(2.0), metrics.points(1.0)),
                half * 0.65,
                stroke,
            );
            painter.add(Shape::convex_polygon(
                vec![
                    center + egui::vec2(-half, -metrics.points(2.0)),
                    center + egui::vec2(-half * 0.35, -half * 0.7),
                    center + egui::vec2(-half * 0.35, half * 0.35),
                ],
                foreground,
                Stroke::NONE,
            ));
        }
        Icon::Restart => {
            let arc = restart_icon_arc(center, half);
            let arrow_corner = *arc.last().expect("重启图标圆弧至少包含一个点");
            painter.add(Shape::line(arc, stroke));
            paint_line(
                [arrow_corner + egui::vec2(0.0, -half * 0.45), arrow_corner],
                stroke,
            );
            paint_line(
                [arrow_corner, arrow_corner + egui::vec2(-half * 0.45, 0.0)],
                stroke,
            );
        }
        Icon::Clear => {
            let rect = Rect::from_center_size(
                center,
                egui::vec2(metrics.points(16.0), metrics.points(13.0)),
            );
            pixel_snap::paint_pixel_aligned_rect(
                ui,
                rect,
                metrics.points(2.0),
                Color32::TRANSPARENT,
                stroke,
            );
            paint_line(
                [rect.left_top(), rect.right_bottom()],
                Stroke::new(metrics.points(2.0), accent),
            );
        }
        Icon::Exit => {
            paint_line(
                [
                    center + egui::vec2(-half * 0.8, -half * 0.7),
                    center + egui::vec2(-half * 0.8, half * 0.7),
                ],
                stroke,
            );
            paint_line(
                [
                    center + egui::vec2(-half * 0.8, -half * 0.7),
                    center + egui::vec2(-half * 0.2, -half * 0.7),
                ],
                stroke,
            );
            paint_line(
                [
                    center + egui::vec2(-half * 0.8, half * 0.7),
                    center + egui::vec2(-half * 0.2, half * 0.7),
                ],
                stroke,
            );
            paint_line(
                [
                    center + egui::vec2(-half * 0.15, 0.0),
                    center + egui::vec2(half * 0.75, 0.0),
                ],
                Stroke::new(metrics.points(2.0), accent),
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
            paint_line(
                [
                    center + egui::vec2(half * 0.55, -half * 0.7),
                    center + egui::vec2(-half * 0.45, 0.0),
                ],
                stroke,
            );
            paint_line(
                [
                    center + egui::vec2(-half * 0.45, 0.0),
                    center + egui::vec2(half * 0.55, half * 0.7),
                ],
                stroke,
            );
        }
        Icon::Next => {
            paint_line(
                [
                    center + egui::vec2(-half * 0.55, -half * 0.7),
                    center + egui::vec2(half * 0.45, 0.0),
                ],
                stroke,
            );
            paint_line(
                [
                    center + egui::vec2(half * 0.45, 0.0),
                    center + egui::vec2(-half * 0.55, half * 0.7),
                ],
                stroke,
            );
        }
        Icon::Collapse => {
            paint_line(
                [
                    center + egui::vec2(-half * 0.65, 0.0),
                    center + egui::vec2(half * 0.2, 0.0),
                ],
                stroke,
            );
            paint_line(
                [
                    center + egui::vec2(-half * 0.2, -half * 0.45),
                    center + egui::vec2(-half * 0.65, 0.0),
                ],
                stroke,
            );
            paint_line(
                [
                    center + egui::vec2(-half * 0.65, 0.0),
                    center + egui::vec2(-half * 0.2, half * 0.45),
                ],
                stroke,
            );
        }
        Icon::Expand => {
            paint_line(
                [
                    center + egui::vec2(-half * 0.2, 0.0),
                    center + egui::vec2(half * 0.65, 0.0),
                ],
                stroke,
            );
            paint_line(
                [
                    center + egui::vec2(half * 0.2, -half * 0.45),
                    center + egui::vec2(half * 0.65, 0.0),
                ],
                stroke,
            );
            paint_line(
                [
                    center + egui::vec2(half * 0.65, 0.0),
                    center + egui::vec2(half * 0.2, half * 0.45),
                ],
                stroke,
            );
        }
        Icon::Confirm => {
            paint_line(
                [
                    center + egui::vec2(-half * 0.7, 0.0),
                    center + egui::vec2(-half * 0.2, half * 0.55),
                ],
                Stroke::new(metrics.points(2.0), foreground),
            );
            paint_line(
                [
                    center + egui::vec2(-half * 0.2, half * 0.55),
                    center + egui::vec2(half * 0.75, -half * 0.55),
                ],
                Stroke::new(metrics.points(2.0), foreground),
            );
        }
        Icon::Cancel => {
            paint_line(
                [
                    center + egui::vec2(-half * 0.65, -half * 0.65),
                    center + egui::vec2(half * 0.65, half * 0.65),
                ],
                Stroke::new(metrics.points(2.0), foreground),
            );
            paint_line(
                [
                    center + egui::vec2(half * 0.65, -half * 0.65),
                    center + egui::vec2(-half * 0.65, half * 0.65),
                ],
                Stroke::new(metrics.points(2.0), foreground),
            );
        }
        Icon::Folder => {
            let left = center.x - half * 0.85;
            let right = center.x + half * 0.85;
            let tab_right = center.x - half * 0.05;
            let tab_top = center.y - half * 0.7;
            let top = center.y - half * 0.35;
            let bottom = center.y + half * 0.7;
            for points in [
                [Pos2::new(left, top), Pos2::new(left, bottom)],
                [Pos2::new(left, bottom), Pos2::new(right, bottom)],
                [Pos2::new(right, bottom), Pos2::new(right, top)],
                [Pos2::new(right, top), Pos2::new(tab_right, top)],
                [Pos2::new(tab_right, top), Pos2::new(tab_right, tab_top)],
                [Pos2::new(tab_right, tab_top), Pos2::new(left, tab_top)],
                [Pos2::new(left, tab_top), Pos2::new(left, top)],
            ] {
                paint_line(points, stroke);
            }
        }
        Icon::Download => {
            paint_line(
                [
                    center + egui::vec2(0.0, -half * 0.8),
                    center + egui::vec2(0.0, half * 0.35),
                ],
                stroke,
            );
            for points in [
                [
                    center + egui::vec2(-half * 0.4, 0.0),
                    center + egui::vec2(0.0, half * 0.4),
                ],
                [
                    center + egui::vec2(0.0, half * 0.4),
                    center + egui::vec2(half * 0.4, 0.0),
                ],
                [
                    center + egui::vec2(-half * 0.7, half * 0.75),
                    center + egui::vec2(half * 0.7, half * 0.75),
                ],
            ] {
                paint_line(points, stroke);
            }
        }
        Icon::Power => {
            painter.circle_stroke(center, half * 0.72, stroke);
            paint_line(
                [
                    center + egui::vec2(0.0, -half),
                    center + egui::vec2(0.0, -half * 0.15),
                ],
                Stroke::new(metrics.points(2.0), accent),
            );
        }
    }
}

/// 返回画笔粗细按钮使用的紧凑标签。
pub(super) const fn pen_width_label(width: PenWidth) -> &'static str {
    match width {
        PenWidth::Px4 => "4px",
        PenWidth::Px6 => "6px",
        PenWidth::Px8 => "8px",
        PenWidth::Px16 => "16px",
    }
}

/// 返回橡皮擦大小按钮使用的紧凑标签。
pub(super) const fn eraser_size_label(size: EraserSize) -> &'static str {
    match size {
        EraserSize::Px36 => "36px",
        EraserSize::Px72 => "72px",
        EraserSize::Px144 => "144px",
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
    Cursor,
    Settings,
    Sliders,
    Color,
    PenWidth,
    Eraser,
    EraserSize,
    Undo,
    Restart,
    Clear,
    Exit,
    Previous,
    Next,
    Collapse,
    Expand,
    Confirm,
    Cancel,
    Folder,
    Download,
    Power,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证颜色弹层关闭时返回空区域，打开后返回实际显示区域。
    #[test]
    fn color_picker_reports_popup_rect_only_when_open() {
        let context = egui::Context::default();
        crate::ui::configure_context(&context);
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));

        // 第一帧：渲染颜色按钮并记录其区域。
        let mut button_rect = egui::Rect::NOTHING;
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..egui::RawInput::default()
            },
            |ui| {
                button_rect =
                    icon_button_with_surface(ui, "颜色", Icon::Color, false, None, false).rect;
            },
        );
        assert!(button_rect.width() > 0.0 && button_rect.height() > 0.0);

        let pointer_event = |pressed: bool| egui::Event::PointerButton {
            pos: button_rect.center(),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let render_with_picker =
            |context: &egui::Context| -> Option<(Option<UiCommand>, Option<Rect>)> {
                let mut result = None;
                let _ = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..egui::RawInput::default()
                    },
                    |ui| {
                        let button =
                            icon_button_with_surface(ui, "颜色", Icon::Color, false, None, false);
                        result = Some(render_color_picker(
                            &button,
                            InkColor::Red,
                            egui::RectAlign::TOP_START,
                            SelectorOrientation::Horizontal,
                            false,
                        ));
                    },
                );
                result
            };

        // 未点击时弹层关闭，不返回显示区域。
        let (_, rect) = render_with_picker(&context).expect("未点击帧也应返回 picker 结果");
        assert!(rect.is_none(), "弹层关闭时不应返回显示区域");

        // 同帧按下并释放颜色按钮，触发弹层切换；点击帧同步渲染弹层使命令生效。
        let mut click = egui::RawInput {
            screen_rect: Some(screen),
            ..egui::RawInput::default()
        };
        click
            .events
            .push(egui::Event::PointerMoved(button_rect.center()));
        click.events.push(pointer_event(true));
        click.events.push(pointer_event(false));
        let mut clicked = false;
        let mut open_result = None;
        let _ = context.run_ui(click, |ui| {
            let button = icon_button_with_surface(ui, "颜色", Icon::Color, false, None, false);
            clicked = button.clicked();
            open_result = Some(render_color_picker(
                &button,
                InkColor::Red,
                egui::RectAlign::TOP_START,
                SelectorOrientation::Horizontal,
                false,
            ));
        });
        assert!(clicked, "颜色按钮应响应点击并切换弹层");
        let (_, rect) = open_result.expect("点击帧也应返回 picker 结果");
        let popup_rect = rect.expect("弹层打开时应返回显示区域");
        assert!(popup_rect.width() > 0.0 && popup_rect.height() > 0.0);
        assert!(
            !popup_rect.contains(button_rect.center()),
            "弹层应避开触发按钮，实际位置 {popup_rect:?}"
        );
    }

    /// 验证重启图标保留明显缺口，不会退化为封闭圆圈。
    #[test]
    fn restart_icon_arc_remains_open() {
        let arc = restart_icon_arc(Pos2::ZERO, 10.0);
        let gap = arc
            .first()
            .expect("圆弧应有起点")
            .distance(*arc.last().expect("圆弧应有终点"));

        assert_eq!(arc.len(), 21);
        assert!(gap > 5.0);
        assert!(
            arc.iter()
                .all(|point| point.x.is_finite() && point.y.is_finite())
        );
    }
}
