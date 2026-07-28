use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

use winit::{
    application::ApplicationHandler,
    event::{ElementState, StartCause, TouchPhase, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    platform::windows::EventLoopBuilderExtWindows,
    window::WindowId,
};

use crate::{
    app::{AppMode, AppState},
    autostart::{self, MachineAutostartState},
    error::AppError,
    ink::{
        ActiveInkPreview, CanvasPoint, EraseSample, InkDocument, InkOperation, InkTool,
        SpeedStrokeBuilder, VariableStrokePoint,
    },
    input::{
        InputRouter, PointerAction, PointerSample, WindowsPointerEvent, WindowsPointerTracker,
    },
    logging,
    render::Compositor,
    settings::{SettingsStore, UserSettings},
    slideshow::{
        ComDetector, ComDetectorEvent, ComDiagnostics, SlideShowControlAction, SlideShowSession,
    },
    ui::{self, IdlePanel, ToolState, UiCommand, UiViewState, design_tokens},
    window::{D3DWindowContext, IdleWindowView},
};

/// egui 请求立即或延迟重绘时发送给 winit 的用户事件。
#[derive(Debug, Clone, Copy)]
enum UserEvent {
    RequestRepaint(Duration),
    ExternalEvent,
    WindowsPointer,
}

/// 当前尚未提交为墨迹 operation 的单次指针手势。
#[derive(Debug)]
struct ActiveGesture {
    samples: ActiveGestureSamples,
    tool: InkTool,
    tools: ToolState,
    speed_builder: Option<SpeedStrokeBuilder>,
    variable_preview: Vec<VariableStrokePoint>,
}

#[derive(Debug)]
enum ActiveGestureSamples {
    Tool {
        points: Vec<CanvasPoint>,
        timestamps_micros: Option<Vec<u64>>,
    },
    PalmErase(Vec<EraseSample>),
}

impl ActiveGesture {
    /// 使用当前工具选择从第一个物理像素点开始手势。
    fn new(sample: PointerSample, tools: ToolState, dpi_scale: f32) -> Self {
        let speed_builder = (tools.tool == InkTool::Pen && tools.speed_taper_enabled)
            .then(|| {
                SpeedStrokeBuilder::new(
                    sample.point,
                    sample.timestamp_micros,
                    tools.pen_width.pixels(),
                    dpi_scale,
                )
            })
            .flatten();
        Self {
            samples: ActiveGestureSamples::Tool {
                points: vec![sample.point],
                timestamps_micros: speed_builder
                    .as_ref()
                    .map(|_| vec![sample.timestamp_micros]),
            },
            tool: tools.tool,
            tools,
            speed_builder,
            variable_preview: Vec::new(),
        }
    }

    /// 使用当前工具选择从第一个非空物理像素批次开始手势。
    fn from_points(points: Vec<PointerSample>, tools: ToolState, dpi_scale: f32) -> Option<Self> {
        let mut points = points.into_iter();
        let first = points.next()?;
        let mut gesture = Self::new(first, tools, dpi_scale);
        gesture.extend(points);
        Some(gesture)
    }

    /// 使用动态接触椭圆开始一次临时手掌擦除会话。
    fn new_palm_erase(sample: EraseSample, tools: ToolState) -> Self {
        Self {
            samples: ActiveGestureSamples::PalmErase(vec![sample]),
            tool: tools.tool,
            tools,
            speed_builder: None,
            variable_preview: Vec::new(),
        }
    }

    /// 追加一批去重后的物理像素采样。
    fn extend(&mut self, samples: impl IntoIterator<Item = PointerSample>) {
        for sample in samples {
            self.push_point(sample);
        }
    }

    /// 追加一个与上一个点有实际距离的采样，避免驱动重复点膨胀历史。
    fn push_point(&mut self, sample: PointerSample) {
        let ActiveGestureSamples::Tool {
            points,
            timestamps_micros,
        } = &mut self.samples
        else {
            return;
        };
        let should_push = points.last().is_none_or(|last| {
            let delta_x = last.x - sample.point.x;
            let delta_y = last.y - sample.point.y;
            delta_x.mul_add(delta_x, delta_y * delta_y) >= 0.25
        });
        if should_push {
            points.push(sample.point);
            if let Some(timestamps_micros) = timestamps_micros.as_mut() {
                timestamps_micros.push(sample.timestamp_micros);
            }
            if let Some(builder) = self.speed_builder.as_mut() {
                builder.push(sample.point, sample.timestamp_micros);
            }
        }
    }

    /// 追加一个动态手掌接触椭圆采样。
    fn push_palm_erase(&mut self, sample: EraseSample) {
        if let ActiveGestureSamples::PalmErase(samples) = &mut self.samples {
            samples.push(sample);
        }
    }

    /// 将活动手势转换为实时 Skia 预览描述。
    fn preview(&mut self) -> ActiveInkPreview<'_> {
        match &self.samples {
            ActiveGestureSamples::Tool { points, .. } => {
                if let Some(builder) = self.speed_builder.as_ref() {
                    builder.finalize_into(&mut self.variable_preview);
                    ActiveInkPreview::VariableTool {
                        points: &self.variable_preview,
                        color: self.tools.color,
                        eraser_size: self.tools.eraser_size,
                    }
                } else {
                    ActiveInkPreview::Tool {
                        points,
                        tool: self.tool,
                        color: self.tools.color,
                        pen_width: self.tools.pen_width,
                        eraser_size: self.tools.eraser_size,
                    }
                }
            }
            ActiveGestureSamples::PalmErase(samples) => ActiveInkPreview::PalmErase { samples },
        }
    }

    /// 把完整手势提交为一次画笔或区域擦除 operation。
    fn commit(self, document: &mut InkDocument) {
        let speed_builder = self.speed_builder;
        match self.samples {
            ActiveGestureSamples::Tool { points, .. } => match self.tool {
                InkTool::Pen => {
                    if let Some(builder) = speed_builder {
                        document.append_variable_draw_stroke(
                            builder.finalized_points(),
                            self.tools.color,
                        );
                    } else {
                        document.append_draw_stroke(points, self.tools.color, self.tools.pen_width);
                    }
                }
                InkTool::RegionEraser => {
                    let samples = points
                        .into_iter()
                        .map(|point| EraseSample::circle(point, self.tools.eraser_size.pixels()))
                        .collect();
                    document.append_erase_stroke(samples);
                }
            },
            ActiveGestureSamples::PalmErase(samples) => {
                document.append_erase_stroke(samples);
            }
        }
    }
}

/// 组合窗口、渲染器、状态机和输入路由的单窗口运行时。
struct DesktopRuntime {
    compositor: Compositor,
    window_context: D3DWindowContext,
    state: AppState,
    empty_document: InkDocument,
    tools: ToolState,
    input_router: InputRouter,
    active_gesture: Option<ActiveGesture>,
    windows_pointer_receiver: Receiver<WindowsPointerEvent>,
    pen_contact_active: Arc<AtomicBool>,
    slideshow_detector: ComDetector,
    settings_store: SettingsStore,
    settings: UserSettings,
    settings_error: Option<String>,
    settings_directory_error: Option<String>,
    machine_autostart_state: Option<MachineAutostartState>,
    machine_autostart_error: Option<String>,
    ink_rendering_error: Option<String>,
    idle_panel: IdlePanel,
    com_diagnostics: Option<ComDiagnostics>,
    slideshow_connection_error: Option<String>,
    slideshow_control_error: Option<String>,
    dismiss_slideshow_confirmation: bool,
    idle_window_dragging: bool,
    slideshow_session_generation: u64,
}

impl DesktopRuntime {
    /// 在 winit 恢复阶段创建窗口和全部 GPU 资源。
    fn new(
        event_loop: &ActiveEventLoop,
        event_proxy: EventLoopProxy<UserEvent>,
        windows_pointer_receiver: Receiver<WindowsPointerEvent>,
        pen_contact_active: Arc<AtomicBool>,
    ) -> Result<Self, AppError> {
        let settings_store = SettingsStore::new()?;
        let (mut settings, mut settings_error) = match settings_store.load() {
            Ok(settings) => (settings, None),
            Err(error) => {
                tracing::warn!(%error, "读取设置失败，使用默认值");
                (UserSettings::default(), Some(error.to_string()))
            }
        };
        let (machine_autostart_state, machine_autostart_error) = load_machine_autostart_state();
        let tools = ToolState {
            tool: InkTool::Pen,
            color: settings.tools.color,
            pen_width: settings.tools.pen_width,
            eraser_size: settings.tools.eraser_size,
            speed_taper_enabled: settings.tools.speed_taper_enabled,
        };
        let window_context = D3DWindowContext::new(event_loop)?;
        let (compositor, applied_ink_mode, ink_rendering_error) =
            Compositor::new(event_loop, &window_context, settings.ink_antialiasing)?;
        if applied_ink_mode != settings.ink_antialiasing {
            settings.ink_antialiasing = applied_ink_mode;
            if let Err(error) = settings_store.save(&settings) {
                tracing::warn!(%error, "保存抗锯齿回退设置失败");
                settings_error = Some(error.to_string());
            }
        }
        let wake_proxy = event_proxy;
        let slideshow_detector = ComDetector::spawn(move || {
            let _ = wake_proxy.send_event(UserEvent::ExternalEvent);
        });
        Ok(Self {
            compositor,
            window_context,
            state: AppState::default(),
            empty_document: InkDocument::new(),
            tools,
            input_router: InputRouter::default(),
            active_gesture: None,
            windows_pointer_receiver,
            pen_contact_active,
            slideshow_detector,
            settings_store,
            settings,
            settings_error,
            settings_directory_error: None,
            machine_autostart_state,
            machine_autostart_error,
            ink_rendering_error,
            idle_panel: IdlePanel::Toolbar,
            com_diagnostics: None,
            slideshow_connection_error: None,
            slideshow_control_error: None,
            dismiss_slideshow_confirmation: false,
            idle_window_dragging: false,
            slideshow_session_generation: 0,
        })
    }

    /// 返回当前运行时窗口标识。
    fn window_id(&self) -> WindowId {
        self.window_context.window().id()
    }

    /// 处理非重绘窗口事件，并返回 egui 是否请求重绘。
    fn handle_window_event(&mut self, event: &WindowEvent) -> Result<bool, AppError> {
        let surface_rebuilt = if let WindowEvent::Resized(size) = event {
            self.compositor
                .resize(&mut self.window_context, (*size).into())?;
            self.sync_ink_rendering_state();
            if self.state.mode() == AppMode::IdleFloatingToolbar {
                self.window_context
                    .correct_idle_size(self.current_idle_window_view(), *size);
            }
            true
        } else {
            false
        };
        if self.idle_window_dragging && window_drag_finished(event) {
            self.idle_window_dragging = false;
            self.window_context
                .finish_idle_window_drag(self.current_idle_window_view());
        }

        let event_response = self
            .compositor
            .on_window_event(self.window_context.window(), event);
        if let Some(pointer_action) = self.input_router.route(
            event,
            event_response.consumed,
            self.state.mode().accepts_ink_input(),
            self.pen_contact_active.load(Ordering::Acquire),
        ) {
            self.apply_pointer_action(pointer_action);
            self.request_redraw();
        }
        Ok(surface_rebuilt || event_response.repaint)
    }

    /// 将统一指针动作应用到当前活动手势或普通批注文档。
    fn apply_pointer_action(&mut self, action: PointerAction) {
        let dpi_scale = self.window_context.window().scale_factor() as f32;
        match action {
            PointerAction::Begin(point) => {
                self.active_gesture = Some(ActiveGesture::new(point, self.tools, dpi_scale));
            }
            PointerAction::Move(point) => {
                if let Some(gesture) = self.active_gesture.as_mut() {
                    gesture.push_point(point);
                }
            }
            PointerAction::End(point) => {
                if let Some(mut gesture) = self.active_gesture.take() {
                    gesture.push_point(point);
                    if let Some(document) = self.state.active_document_mut() {
                        gesture.commit(document);
                    }
                }
            }
            PointerAction::BeginBatch(points) => {
                self.active_gesture = ActiveGesture::from_points(points, self.tools, dpi_scale);
            }
            PointerAction::MoveBatch(points) => {
                if let Some(gesture) = self.active_gesture.as_mut() {
                    gesture.extend(points);
                }
            }
            PointerAction::EndBatch(points) => {
                if let Some(mut gesture) = self.active_gesture.take() {
                    gesture.extend(points);
                    if let Some(document) = self.state.active_document_mut() {
                        gesture.commit(document);
                    }
                }
            }
            PointerAction::BeginPalmErase(sample) => {
                self.active_gesture = Some(ActiveGesture::new_palm_erase(sample, self.tools));
            }
            PointerAction::MovePalmErase(sample) => {
                if let Some(gesture) = self.active_gesture.as_mut() {
                    gesture.push_palm_erase(sample);
                }
            }
            PointerAction::EndPalmErase(sample) => {
                if let Some(mut gesture) = self.active_gesture.take() {
                    gesture.push_palm_erase(sample);
                    if let Some(document) = self.state.active_document_mut() {
                        gesture.commit(document);
                    }
                }
            }
            PointerAction::CommitBuffered(points) => {
                if let Some(gesture) = ActiveGesture::from_points(points, self.tools, dpi_scale)
                    && let Some(document) = self.state.active_document_mut()
                {
                    gesture.commit(document);
                }
            }
            PointerAction::Cancel => {
                self.active_gesture = None;
            }
        }
    }

    /// 运行 UI、合成 Skia 与 egui，并返回本帧是否请求退出应用。
    fn render(&mut self) -> Result<bool, AppError> {
        let mode = self.state.mode();
        let tools = self.tools;
        let slideshow_controls_enabled = self.state.slideshow_controls_enabled();
        let slide_page_numbers = slideshow_controls_enabled
            .then(|| {
                self.state
                    .slideshow_session()
                    .and_then(|session| session.current_page().reliable_page_numbers())
            })
            .flatten();
        let view = UiViewState {
            mode,
            idle_panel: self.idle_panel,
            dock_side: self.window_context.dock_side(),
            tools,
            slideshow_integration_enabled: self.settings.slideshow_integration_enabled,
            log_level: self.settings.log_level,
            readable_mode: self.settings.readable_mode,
            ink_antialiasing: self.settings.ink_antialiasing,
            ink_rendering_error: self.ink_rendering_error.as_deref(),
            slideshow_session_generation: self
                .state
                .slideshow_session()
                .map(|_| self.slideshow_session_generation),
            slide_page_numbers,
            slideshow_controls_enabled,
            dismiss_slideshow_confirmation: self.dismiss_slideshow_confirmation,
            com_diagnostics: self.com_diagnostics.as_ref(),
            slideshow_connection_error: self.slideshow_connection_error.as_deref(),
            slideshow_control_error: self.slideshow_control_error.as_deref(),
            settings_error: self.settings_error.as_deref(),
            settings_directory_error: self.settings_directory_error.as_deref(),
            machine_autostart_state: self.machine_autostart_state,
            machine_autostart_error: self.machine_autostart_error.as_deref(),
            settings_path: self.settings_store.path(),
            graphics_diagnostics: self.window_context.diagnostics(),
        };
        let mut ui_command = None;
        self.compositor.run_ui(self.window_context.window(), |ui| {
            ui_command = ui::render(ui, view)
        });

        let document = self.state.active_document().unwrap_or(&self.empty_document);
        let preview = self.active_gesture.as_mut().map(ActiveGesture::preview);
        self.compositor
            .paint(&self.window_context, document, preview)?;
        self.sync_ink_rendering_state();
        self.window_context.present()?;

        Ok(ui_command.is_some_and(|command| self.apply_ui_command(command)))
    }

    /// 执行工具栏命令，并返回该命令是否要求事件循环退出。
    fn apply_ui_command(&mut self, command: UiCommand) -> bool {
        match command {
            UiCommand::ExitApplication => return true,
            UiCommand::EnterAnnotation => {
                if self.state.enter_normal_annotation() {
                    self.prepare_annotation_transition(true);
                }
            }
            UiCommand::ExitAnnotation => {
                if self.state.exit_normal_annotation() {
                    self.prepare_annotation_transition(false);
                }
            }
            UiCommand::SelectPen => self.tools.tool = InkTool::Pen,
            UiCommand::SelectEraser => self.tools.tool = InkTool::RegionEraser,
            UiCommand::CycleEraserSize => {
                self.tools.cycle_eraser_size();
                self.settings.tools.eraser_size = self.tools.eraser_size;
                self.save_settings();
            }
            UiCommand::SetColor(color) => {
                self.tools.color = color;
                self.settings.tools.color = color;
                self.save_settings();
            }
            UiCommand::SetPenWidth(width) => {
                self.tools.pen_width = width;
                self.settings.tools.pen_width = width;
                self.save_settings();
            }
            UiCommand::SetEraserSize(size) => {
                self.tools.eraser_size = size;
                self.settings.tools.eraser_size = size;
                self.save_settings();
            }
            UiCommand::SetSpeedTaperEnabled(enabled) => {
                self.tools.speed_taper_enabled = enabled;
                self.settings.tools.speed_taper_enabled = enabled;
                self.save_settings();
            }
            UiCommand::SetInkAntialiasing(mode) => {
                match self
                    .compositor
                    .set_ink_antialiasing(&self.window_context, mode)
                {
                    Ok(()) => self.sync_ink_rendering_state(),
                    Err(error) => {
                        tracing::warn!(%error, "切换墨迹抗锯齿失败");
                        self.ink_rendering_error = Some(error.to_string());
                    }
                }
            }
            UiCommand::Undo => {
                let undone = self.state.active_document_mut().and_then(InkDocument::undo);
                if let Some(operation) = undone {
                    if matches!(operation, InkOperation::Clear(_)) {
                        self.compositor.invalidate_ink_cache();
                    } else if let Some(bounds) = operation.bounds() {
                        self.compositor.invalidate_ink_region(bounds);
                    }
                }
            }
            UiCommand::Clear => {
                if let Some(document) = self.state.active_document_mut() {
                    document.clear();
                }
            }
            UiCommand::OpenSettings => {
                if self.state.mode() == AppMode::IdleFloatingToolbar {
                    self.refresh_machine_autostart();
                    self.idle_panel = IdlePanel::Settings;
                    self.window_context
                        .set_idle_window_view(IdleWindowView::Settings);
                    self.update_interface_zoom();
                }
            }
            UiCommand::OpenSettingsDirectory => match self.settings_store.open_directory() {
                Ok(()) => self.settings_directory_error = None,
                Err(error) => {
                    tracing::warn!(%error, "打开配置目录失败");
                    self.settings_directory_error = Some(error.to_string());
                }
            },
            UiCommand::SetMachineAutostart(enabled) => {
                self.set_machine_autostart(enabled);
            }
            UiCommand::CloseSettings => {
                if self.state.mode() == AppMode::IdleFloatingToolbar {
                    self.idle_panel = IdlePanel::Toolbar;
                    self.window_context
                        .set_idle_window_view(IdleWindowView::Toolbar);
                    self.update_interface_zoom();
                }
            }
            UiCommand::ToggleQuickSettings => {
                if self.state.mode() == AppMode::IdleFloatingToolbar {
                    self.idle_panel = if self.idle_panel == IdlePanel::QuickSettings {
                        IdlePanel::Toolbar
                    } else {
                        IdlePanel::QuickSettings
                    };
                    let window_view = if self.idle_panel == IdlePanel::QuickSettings {
                        IdleWindowView::QuickSettings
                    } else {
                        IdleWindowView::Toolbar
                    };
                    self.window_context.set_idle_window_view(window_view);
                    self.update_interface_zoom();
                }
            }
            UiCommand::BeginIdleToolbarDrag => {
                if self.state.mode() == AppMode::IdleFloatingToolbar {
                    self.idle_window_dragging = true;
                    self.window_context.begin_window_drag();
                }
            }
            UiCommand::SetDockSide(side) => {
                self.window_context.set_dock_side(side);
            }
            UiCommand::SetSlideshowIntegrationEnabled(enabled) => {
                self.settings.slideshow_integration_enabled = enabled;
                self.slideshow_control_error = None;
                self.save_settings();
                if enabled {
                    let _ = self.slideshow_detector.request_resync();
                }
            }
            UiCommand::SetLogLevel(level) => {
                self.settings.log_level = level;
                logging::set_level(level);
                self.save_settings();
            }
            UiCommand::SetReadableMode(enabled) => {
                self.settings.readable_mode = enabled;
                self.save_settings();
            }
            UiCommand::ToggleSlideshowToolbar => match self.state.mode() {
                AppMode::SlideShowAnnotatingExpanded => {
                    self.state.collapse_slideshow_toolbar();
                }
                AppMode::SlideShowAnnotatingCollapsed => {
                    self.state.expand_slideshow_toolbar();
                }
                _ => {}
            },
            UiCommand::PreviousSlide => {
                self.request_slideshow_control(SlideShowControlAction::Previous);
            }
            UiCommand::NextSlide => {
                self.request_slideshow_control(SlideShowControlAction::Next);
            }
            UiCommand::ExitSlideShow => {
                self.request_slideshow_control(SlideShowControlAction::Exit);
            }
            UiCommand::RequestDismissSlideshow => {
                if self.state.mode() == AppMode::SlideShowConnectionLost {
                    self.dismiss_slideshow_confirmation = true;
                }
            }
            UiCommand::ConfirmDismissSlideshow => {
                if self.dismiss_slideshow_confirmation
                    && self.state.dismiss_disconnected_slideshow()
                {
                    self.dismiss_slideshow_confirmation = false;
                    self.prepare_annotation_transition(false);
                }
            }
            UiCommand::CancelDismissSlideshow => {
                self.dismiss_slideshow_confirmation = false;
            }
        }
        self.request_redraw();
        false
    }

    /// 保存当前用户偏好，并把失败信息留给设置诊断界面。
    fn save_settings(&mut self) {
        match self.settings_store.save(&self.settings) {
            Ok(()) => self.settings_error = None,
            Err(error) => {
                tracing::warn!(%error, "保存设置失败");
                self.settings_error = Some(error.to_string());
            }
        }
    }

    /// 查询 HKLM Run 中的实际自启动状态，并把路径异常转换为非阻塞诊断。
    fn refresh_machine_autostart(&mut self) {
        match autostart::query_machine_autostart() {
            Ok(state) => {
                self.machine_autostart_state = Some(state);
                self.machine_autostart_error =
                    matches!(state, MachineAutostartState::EnabledPathMismatch).then(|| {
                        "系统级自启动已存在，但路径不是当前程序；重新启用可修复。".to_owned()
                    });
            }
            Err(error) => {
                tracing::warn!(%error, "查询系统级自启动失败");
                self.machine_autostart_error = Some(error.to_string());
            }
        }
    }

    /// 请求一次 UAC 提权变更，并只在复查成功后更新设置页状态。
    fn set_machine_autostart(&mut self, enabled: bool) {
        let previous_state = self.machine_autostart_state;
        match autostart::request_machine_autostart_change(enabled) {
            Ok(()) => self.refresh_machine_autostart(),
            Err(error) => {
                tracing::warn!(%error, enabled, "修改系统级自启动失败");
                self.machine_autostart_state = previous_state;
                self.machine_autostart_error = Some(error.to_string());
            }
        }
    }

    /// 同步渲染器实际生效模式、错误诊断和持久化设置。
    fn sync_ink_rendering_state(&mut self) {
        let applied_mode = self.compositor.ink_antialiasing_mode();
        let mode_changed = self.settings.ink_antialiasing != applied_mode;
        self.ink_rendering_error = self.compositor.ink_rendering_error().map(str::to_owned);
        if mode_changed {
            self.settings.ink_antialiasing = applied_mode;
            self.save_settings();
        }
    }

    /// 把当前 idle 面板映射为原生窗口几何类型。
    fn current_idle_window_view(&self) -> IdleWindowView {
        match self.idle_panel {
            IdlePanel::Toolbar => IdleWindowView::Toolbar,
            IdlePanel::QuickSettings => IdleWindowView::QuickSettings,
            IdlePanel::Settings => IdleWindowView::Settings,
        }
    }

    /// 在工具界面与完整设置页之间切换 egui 的全局显示缩放。
    fn update_interface_zoom(&self) {
        let zoom = if self.idle_panel == IdlePanel::Settings {
            design_tokens::SETTINGS_ZOOM_FACTOR
        } else {
            design_tokens::TOOLBAR_ZOOM_FACTOR
        };
        self.compositor.egui_context().set_zoom_factor(zoom);
    }

    /// 仅在状态机仍有可控放映会话时向 COM STA 发送带会话标识的动作。
    fn request_slideshow_control(&self, action: SlideShowControlAction) {
        if !self.settings.slideshow_integration_enabled || !self.state.slideshow_controls_enabled()
        {
            return;
        }
        let Some(show_key) = self
            .state
            .slideshow_session()
            .map(|session| session.key().clone())
        else {
            return;
        };
        if !self.slideshow_detector.request_control(show_key, action) {
            tracing::warn!(?action, "COM detector 已退出，无法发送放映控制");
        }
    }

    /// 排空 COM detector 事件，并把统一事件应用到状态机和窗口生命周期。
    fn process_slideshow_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.slideshow_detector.try_recv() {
            changed |= self.apply_slideshow_event(event);
        }
        changed
    }

    /// 排空原生 Pointer Input 语义，并用上一帧 egui 区域做 UI 命中判断。
    fn process_windows_pointer_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.windows_pointer_receiver.try_recv() {
            if event.cancels_ui_pointer() {
                self.compositor.cancel_egui_pointer();
            }
            let ui_hit = {
                let egui_context = self.compositor.egui_context();
                event
                    .position()
                    .and_then(|position| {
                        egui_position_from_physical(position, egui_context.pixels_per_point())
                    })
                    .is_some_and(|position| egui_context.layer_id_at(position).is_some())
            };
            if let Some(action) = self.input_router.route_windows_pointer(
                event,
                ui_hit,
                self.state.mode().accepts_ink_input(),
            ) {
                self.apply_pointer_action(action);
                changed = true;
            }
        }
        changed
    }

    /// 应用一个 detector 事件；只有实际改变可见状态时返回 true。
    fn apply_slideshow_event(&mut self, event: ComDetectorEvent) -> bool {
        match event {
            ComDetectorEvent::Diagnostics(diagnostics) => {
                let changed = self.com_diagnostics.as_ref() != Some(&diagnostics);
                self.com_diagnostics = Some(diagnostics);
                changed
            }
            _ if !self.settings.slideshow_integration_enabled => false,
            ComDetectorEvent::SlideShowStarted { key, page } => {
                let same_session = self
                    .state
                    .slideshow_session()
                    .is_some_and(|session| session.key() == &key);
                let changed = if same_session {
                    if self.state.mode() == AppMode::SlideShowConnectionLost {
                        self.state.restore_slideshow_connection(&key, page)
                    } else {
                        self.state.change_slide(&key, page)
                    }
                } else {
                    let changed = self.state.start_slideshow(SlideShowSession::new(key, page));
                    if changed {
                        self.slideshow_session_generation =
                            self.slideshow_session_generation.wrapping_add(1);
                    }
                    changed
                };
                if changed {
                    self.prepare_annotation_transition(true);
                    self.slideshow_connection_error = None;
                    self.dismiss_slideshow_confirmation = false;
                }
                changed
            }
            ComDetectorEvent::SlideChanged { key, page } => {
                let changed = self.state.change_slide(&key, page);
                if changed {
                    self.active_gesture = None;
                    self.input_router.cancel();
                    self.compositor.invalidate_ink_cache();
                }
                changed
            }
            ComDetectorEvent::SlideShowEnded { key } => {
                let changed = self.state.end_slideshow(&key);
                if changed {
                    self.prepare_annotation_transition(false);
                    self.slideshow_connection_error = None;
                    self.dismiss_slideshow_confirmation = false;
                }
                changed
            }
            ComDetectorEvent::ConnectionLost { key, detail } => {
                let matches_session = self.state.slideshow_session().is_some_and(|session| {
                    key.as_ref()
                        .is_none_or(|event_key| session.key() == event_key)
                });
                let changed = matches_session && self.state.lose_slideshow_connection();
                if changed {
                    self.active_gesture = None;
                    self.input_router.cancel();
                    self.slideshow_connection_error = Some(detail);
                    self.dismiss_slideshow_confirmation = false;
                    self.compositor.invalidate_ink_cache();
                }
                changed
            }
            ComDetectorEvent::ControlSucceeded { action, backend } => {
                tracing::info!(?action, ?backend, "放映控制已执行");
                self.slideshow_control_error.take().is_some()
            }
            ComDetectorEvent::ControlFailed { action, detail } => {
                tracing::warn!(?action, %detail, "放映控制失败");
                let changed = self.slideshow_control_error.as_deref() != Some(detail.as_str());
                self.slideshow_control_error = Some(detail);
                changed
            }
        }
    }

    /// 清理活动输入并同步全屏或悬浮窗口几何及墨迹缓存。
    fn prepare_annotation_transition(&mut self, annotation_enabled: bool) {
        self.active_gesture = None;
        self.input_router.cancel();
        self.idle_panel = IdlePanel::Toolbar;
        self.update_interface_zoom();
        if let Err(error) = self
            .compositor
            .set_annotation_resources_enabled(annotation_enabled)
        {
            tracing::warn!(%error, annotation_enabled, "切换墨迹资源驻留模式失败");
            self.ink_rendering_error = Some(error.to_string());
        }
        self.window_context.set_annotation_mode(annotation_enabled);
        self.compositor.invalidate_ink_cache();
    }

    /// 向唯一窗口请求一次按需重绘。
    fn request_redraw(&self) {
        self.window_context.window().request_redraw();
    }
}

/// 返回原生窗口拖动循环是否已经通过鼠标或触摸抬起结束。
fn window_drag_finished(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::MouseInput {
            state: ElementState::Released,
            ..
        } | WindowEvent::Touch(winit::event::Touch {
            phase: TouchPhase::Ended | TouchPhase::Cancelled,
            ..
        })
    )
}

/// 把原生物理像素坐标换算为包含 egui zoom 的点坐标。
fn egui_position_from_physical(position: CanvasPoint, pixels_per_point: f32) -> Option<egui::Pos2> {
    (pixels_per_point.is_finite() && pixels_per_point > 0.0)
        .then(|| egui::pos2(position.x / pixels_per_point, position.y / pixels_per_point))
}

/// 启动时读取一次系统级自启动状态，不让注册表访问进入每帧渲染路径。
fn load_machine_autostart_state() -> (Option<MachineAutostartState>, Option<String>) {
    match autostart::query_machine_autostart() {
        Ok(state) => {
            let error = matches!(state, MachineAutostartState::EnabledPathMismatch)
                .then(|| "系统级自启动已存在，但路径不是当前程序；重新启用可修复。".to_owned());
            (Some(state), error)
        }
        Err(error) => {
            tracing::warn!(%error, "启动时查询系统级自启动失败");
            (None, Some(error.to_string()))
        }
    }
}

/// winit ApplicationHandler，保证空闲时使用 Wait/WaitUntil 而非持续轮询。
struct DesktopApplication {
    proxy: EventLoopProxy<UserEvent>,
    windows_pointer_receiver: Option<Receiver<WindowsPointerEvent>>,
    pen_contact_active: Arc<AtomicBool>,
    runtime: Option<DesktopRuntime>,
    startup_error: Option<AppError>,
    next_repaint: Option<Instant>,
}

impl DesktopApplication {
    /// 创建尚未恢复窗口的应用处理器。
    fn new(
        proxy: EventLoopProxy<UserEvent>,
        windows_pointer_receiver: Receiver<WindowsPointerEvent>,
        pen_contact_active: Arc<AtomicBool>,
    ) -> Self {
        Self {
            proxy,
            windows_pointer_receiver: Some(windows_pointer_receiver),
            pen_contact_active,
            runtime: None,
            startup_error: None,
            next_repaint: None,
        }
    }

    /// 安装 egui 的重绘回调，使动画按需唤醒等待型事件循环。
    fn install_repaint_callback(&self, runtime: &DesktopRuntime) {
        let proxy = self.proxy.clone();
        runtime
            .compositor
            .egui_context()
            .set_request_repaint_callback(move |info| {
                let _ = proxy.send_event(UserEvent::RequestRepaint(info.delay));
            });
    }

    /// 根据下一个延迟重绘时间更新 winit 控制流。
    fn update_control_flow(&self, event_loop: &ActiveEventLoop) {
        if let Some(next_repaint) = self.next_repaint {
            event_loop.set_control_flow(ControlFlow::WaitUntil(next_repaint));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

impl ApplicationHandler<UserEvent> for DesktopApplication {
    /// 在 Windows 恢复应用时创建唯一窗口和 GPU 资源。
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.runtime.is_some() || self.startup_error.is_some() {
            return;
        }
        let Some(windows_pointer_receiver) = self.windows_pointer_receiver.take() else {
            self.startup_error = Some(AppError::Graphics(
                "Windows Pointer Input 接收器已经被占用".to_owned(),
            ));
            event_loop.exit();
            return;
        };
        match DesktopRuntime::new(
            event_loop,
            self.proxy.clone(),
            windows_pointer_receiver,
            Arc::clone(&self.pen_contact_active),
        ) {
            Ok(runtime) => {
                self.install_repaint_callback(&runtime);
                self.runtime = Some(runtime);
                if let Some(runtime) = self.runtime.as_mut() {
                    if let Err(error) = runtime.window_context.show() {
                        self.startup_error = Some(error);
                        event_loop.exit();
                        return;
                    }
                    runtime.request_redraw();
                }
                self.update_control_flow(event_loop);
            }
            Err(error) => {
                self.startup_error = Some(error);
                event_loop.exit();
            }
        }
    }

    /// 处理关闭、重绘、尺寸和输入事件。
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        if window_id != runtime.window_id() {
            return;
        }
        if matches!(event, WindowEvent::CloseRequested | WindowEvent::Destroyed) {
            event_loop.exit();
            return;
        }

        if matches!(event, WindowEvent::RedrawRequested) {
            self.next_repaint = None;
            match runtime.render() {
                Ok(true) => event_loop.exit(),
                Ok(false) => self.update_control_flow(event_loop),
                Err(error) => {
                    self.startup_error = Some(error);
                    event_loop.exit();
                }
            }
            return;
        }

        match runtime.handle_window_event(&event) {
            Ok(true) => runtime.request_redraw(),
            Ok(false) => {}
            Err(error) => {
                self.startup_error = Some(error);
                event_loop.exit();
            }
        }
    }

    /// 记录 egui 请求的立即或延迟重绘时间。
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        match event {
            UserEvent::RequestRepaint(delay) if delay.is_zero() => {
                self.next_repaint = None;
                runtime.request_redraw();
            }
            UserEvent::ExternalEvent => {
                if runtime.process_slideshow_events() {
                    runtime.request_redraw();
                }
                self.update_control_flow(event_loop);
            }
            UserEvent::WindowsPointer => {
                if runtime.process_windows_pointer_events() {
                    runtime.request_redraw();
                }
                self.update_control_flow(event_loop);
            }
            UserEvent::RequestRepaint(delay) => {
                let requested_time = Instant::now() + delay;
                self.next_repaint = Some(
                    self.next_repaint
                        .map_or(requested_time, |current| current.min(requested_time)),
                );
                self.update_control_flow(event_loop);
            }
        }
    }

    /// 在 WaitUntil 到期时请求一次重绘，不进入持续轮询。
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            let now = Instant::now();
            let repaint_due = self.next_repaint.is_some_and(|deadline| deadline <= now);
            if repaint_due {
                self.next_repaint = None;
                if let Some(runtime) = self.runtime.as_ref() {
                    runtime.request_redraw();
                }
            }
            self.update_control_flow(event_loop);
        }
    }
}

/// 创建 Windows 用户事件循环并运行单窗口应用。
pub fn run() -> Result<(), AppError> {
    let (windows_pointer_sender, windows_pointer_receiver) = mpsc::channel();
    let pen_contact_active = Arc::new(AtomicBool::new(false));
    let hook_pen_contact_active = Arc::clone(&pen_contact_active);
    let proxy_slot = Arc::new(OnceLock::<EventLoopProxy<UserEvent>>::new());
    let hook_proxy_slot = Arc::clone(&proxy_slot);
    let mut pointer_tracker = WindowsPointerTracker::default();
    let mut event_loop_builder = EventLoop::<UserEvent>::with_user_event();
    event_loop_builder.with_msg_hook(move |raw_message| {
        let Some(dispatch) = pointer_tracker.capture_message(raw_message) else {
            return false;
        };
        hook_pen_contact_active.store(pointer_tracker.pen_contact_active(), Ordering::Release);
        if let Some(event) = dispatch.event
            && windows_pointer_sender.send(event).is_ok()
            && let Some(proxy) = hook_proxy_slot.get()
        {
            let _ = proxy.send_event(UserEvent::WindowsPointer);
        }
        dispatch.swallow_winit
    });
    let event_loop = event_loop_builder.build()?;
    let proxy = event_loop.create_proxy();
    let _ = proxy_slot.set(proxy.clone());
    let mut application =
        DesktopApplication::new(proxy, windows_pointer_receiver, pen_contact_active);
    event_loop.run_app(&mut application)?;
    application.startup_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证原生物理坐标使用 egui 的完整像素比例换算。
    #[test]
    fn physical_position_uses_egui_pixels_per_point() {
        let position = egui_position_from_physical(CanvasPoint::new(1_600.0, 800.0), 1.6)
            .expect("valid pixels-per-point must convert the position");

        assert_eq!(position, egui::pos2(1_000.0, 500.0));
    }

    /// 验证无效 egui 像素比例不会产生错误的 UI 命中坐标。
    #[test]
    fn invalid_pixels_per_point_has_no_egui_position() {
        assert!(egui_position_from_physical(CanvasPoint::new(1.0, 1.0), 0.0).is_none());
        assert!(egui_position_from_physical(CanvasPoint::new(1.0, 1.0), f32::NAN).is_none());
    }

    /// 验证批量追加复用最小距离去重，不保留驱动重叠点。
    #[test]
    fn active_gesture_deduplicates_batched_points() {
        let gesture = ActiveGesture::from_points(
            vec![
                PointerSample::new(CanvasPoint::new(0.0, 0.0), 100),
                PointerSample::new(CanvasPoint::new(0.25, 0.25), 200),
                PointerSample::new(CanvasPoint::new(1.0, 0.0), 300),
            ],
            ToolState::default(),
            1.0,
        )
        .expect("non-empty batch must start a gesture");
        let ActiveGestureSamples::Tool {
            points,
            timestamps_micros,
        } = gesture.samples
        else {
            panic!("tool gesture must retain point samples");
        };

        assert_eq!(
            points,
            vec![CanvasPoint::new(0.0, 0.0), CanvasPoint::new(1.0, 0.0)]
        );
        assert!(timestamps_micros.is_none());
    }
}
