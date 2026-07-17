use std::{
    sync::{
        Arc, OnceLock,
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
    app::{
        AppMode, AppState,
        gpu_benchmark::{GpuBenchmark, GpuBenchmarkAction},
        performance::{FrameSample, PerformanceTracker, RedrawReason},
    },
    error::AppError,
    ink::{ActiveInkPreview, CanvasPoint, EraseSample, InkDocument, InkOperation, InkTool},
    input::{InputRouter, PointerAction, WindowsPointerEvent, WindowsPointerTracker},
    render::Compositor,
    settings::{SettingsStore, UserSettings},
    slideshow::{
        ComDetector, ComDetectorEvent, ComDiagnostics, SlideShowControlAction, SlideShowSession,
    },
    ui::{self, IdlePanel, ToolState, UiCommand, UiViewState},
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
}

#[derive(Debug)]
enum ActiveGestureSamples {
    Tool(Vec<CanvasPoint>),
    PalmErase(Vec<EraseSample>),
}

impl ActiveGesture {
    /// 使用当前工具选择从第一个物理像素点开始手势。
    fn new(point: CanvasPoint, tools: ToolState) -> Self {
        Self {
            samples: ActiveGestureSamples::Tool(vec![point]),
            tool: tools.tool,
            tools,
        }
    }

    /// 使用动态接触椭圆开始一次临时手掌擦除会话。
    fn new_palm_erase(sample: EraseSample, tools: ToolState) -> Self {
        Self {
            samples: ActiveGestureSamples::PalmErase(vec![sample]),
            tool: tools.tool,
            tools,
        }
    }

    /// 追加一个与上一个点有实际距离的采样，避免驱动重复点膨胀历史。
    fn push(&mut self, point: CanvasPoint) {
        let ActiveGestureSamples::Tool(points) = &mut self.samples else {
            return;
        };
        let should_push = points.last().is_none_or(|last| {
            let delta_x = last.x - point.x;
            let delta_y = last.y - point.y;
            delta_x.mul_add(delta_x, delta_y * delta_y) >= 0.25
        });
        if should_push {
            points.push(point);
        }
    }

    /// 追加一个动态手掌接触椭圆采样。
    fn push_palm_erase(&mut self, sample: EraseSample) {
        if let ActiveGestureSamples::PalmErase(samples) = &mut self.samples {
            samples.push(sample);
        }
    }

    /// 将活动手势转换为实时 Skia 预览描述。
    fn preview(&self) -> ActiveInkPreview<'_> {
        match &self.samples {
            ActiveGestureSamples::Tool(points) => ActiveInkPreview::Tool {
                points,
                tool: self.tool,
                color: self.tools.color,
                pen_width: self.tools.pen_width,
                eraser_size: self.tools.eraser_size,
            },
            ActiveGestureSamples::PalmErase(samples) => ActiveInkPreview::PalmErase { samples },
        }
    }

    /// 把完整手势提交为一次画笔或区域擦除 operation。
    fn commit(self, document: &mut InkDocument) {
        match self.samples {
            ActiveGestureSamples::Tool(points) => match self.tool {
                InkTool::Pen => {
                    document.append_draw_stroke(points, self.tools.color, self.tools.pen_width);
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
    slideshow_detector: ComDetector,
    settings_store: SettingsStore,
    settings: UserSettings,
    settings_error: Option<String>,
    settings_directory_error: Option<String>,
    idle_panel: IdlePanel,
    com_diagnostics: Option<ComDiagnostics>,
    slideshow_connection_error: Option<String>,
    slideshow_control_error: Option<String>,
    dismiss_slideshow_confirmation: bool,
    idle_window_dragging: bool,
    gpu_benchmark: Option<GpuBenchmark>,
    performance: PerformanceTracker,
}

impl DesktopRuntime {
    /// 在 winit 恢复阶段创建窗口和全部 GPU 资源。
    fn new(
        event_loop: &ActiveEventLoop,
        event_proxy: EventLoopProxy<UserEvent>,
        windows_pointer_receiver: Receiver<WindowsPointerEvent>,
    ) -> Result<Self, AppError> {
        let settings_store = SettingsStore::new()?;
        let (settings, settings_error) = match settings_store.load() {
            Ok(settings) => (settings, None),
            Err(error) => {
                tracing::warn!(%error, "读取设置失败，使用默认值");
                (UserSettings::default(), Some(error.to_string()))
            }
        };
        let tools = ToolState {
            tool: InkTool::Pen,
            color: settings.tools.color,
            pen_width: settings.tools.pen_width,
            eraser_size: settings.tools.eraser_size,
        };
        let window_context = D3DWindowContext::new(event_loop)?;
        let compositor = Compositor::new(event_loop, &window_context)?;
        let gpu_benchmark = GpuBenchmark::from_environment()?;
        let performance = PerformanceTracker::new(gpu_benchmark.is_some());
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
            slideshow_detector,
            settings_store,
            settings,
            settings_error,
            settings_directory_error: None,
            idle_panel: IdlePanel::Toolbar,
            com_diagnostics: None,
            slideshow_connection_error: None,
            slideshow_control_error: None,
            dismiss_slideshow_confirmation: false,
            idle_window_dragging: false,
            gpu_benchmark,
            performance,
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
            self.performance.record_surface_rebuild();
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
        ) {
            self.performance
                .record_pointer_batch(pointer_action_sample_count(&pointer_action));
            self.apply_pointer_action(pointer_action);
            self.request_redraw(RedrawReason::PointerInput);
        }
        Ok(surface_rebuilt || event_response.repaint)
    }

    /// 将统一指针动作应用到当前活动手势或普通批注文档。
    fn apply_pointer_action(&mut self, action: PointerAction) {
        match action {
            PointerAction::Begin(point) if self.active_gesture.is_none() => {
                self.active_gesture = Some(ActiveGesture::new(point, self.tools));
            }
            PointerAction::Move(point) => {
                if let Some(gesture) = self.active_gesture.as_mut() {
                    gesture.push(point);
                }
            }
            PointerAction::End(point) => {
                if let Some(mut gesture) = self.active_gesture.take() {
                    gesture.push(point);
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
                let gesture = ActiveGesture {
                    samples: ActiveGestureSamples::Tool(points),
                    tool: self.tools.tool,
                    tools: self.tools,
                };
                if let Some(document) = self.state.active_document_mut() {
                    gesture.commit(document);
                }
            }
            PointerAction::Cancel => {
                self.active_gesture = None;
            }
            PointerAction::Begin(_) => {}
        }
    }

    /// 运行 UI、合成 Skia 与 egui，并返回本帧是否请求退出应用。
    fn render(&mut self) -> Result<bool, AppError> {
        let frame_started_at = self.performance.begin_frame();
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
            slide_page_numbers,
            slideshow_controls_enabled,
            dismiss_slideshow_confirmation: self.dismiss_slideshow_confirmation,
            com_diagnostics: self.com_diagnostics.as_ref(),
            slideshow_connection_error: self.slideshow_connection_error.as_deref(),
            slideshow_control_error: self.slideshow_control_error.as_deref(),
            settings_error: self.settings_error.as_deref(),
            settings_directory_error: self.settings_directory_error.as_deref(),
            settings_path: self.settings_store.path(),
            graphics_diagnostics: self.window_context.diagnostics(),
        };
        let mut ui_command = None;
        self.compositor.run_ui(self.window_context.window(), |ui| {
            ui_command = ui::render(ui, view)
        });

        let document = self.state.active_document().unwrap_or(&self.empty_document);
        let preview = self.active_gesture.as_ref().map(ActiveGesture::preview);
        self.compositor
            .paint(&self.window_context, document, preview)?;
        self.window_context.present()?;
        let frame_sample = self.performance.finish_frame(frame_started_at);
        if self.advance_gpu_benchmark(frame_sample)? {
            return Ok(true);
        }

        Ok(ui_command.is_some_and(|command| self.apply_ui_command(command)))
    }

    /// 在运行时安装后进入压力场景使用的普通全屏批注模式。
    fn prepare_gpu_benchmark(&mut self) {
        if self.gpu_benchmark.is_some() && self.state.enter_normal_annotation() {
            self.idle_panel = IdlePanel::Toolbar;
            self.window_context.set_annotation_mode(true);
            self.compositor.invalidate_ink_cache();
        }
    }

    /// 在每次 Present 后推进一个压力 operation，并在报告完成后请求退出。
    fn advance_gpu_benchmark(
        &mut self,
        frame_sample: Option<FrameSample>,
    ) -> Result<bool, AppError> {
        if self.gpu_benchmark.is_none()
            || self.window_context.swap_chain_size() != self.window_context.annotation_size()
        {
            return Ok(false);
        }
        let diagnostics = self.window_context.diagnostics().clone();
        let surface_size = self.window_context.swap_chain_size();
        let action = {
            let benchmark = self.gpu_benchmark.as_mut().expect("已检查压力场景存在");
            let document = self
                .state
                .active_document_mut()
                .ok_or_else(|| AppError::Graphics("GPU 压力场景没有活动批注文档".to_owned()))?;
            benchmark.after_present(document, frame_sample, &diagnostics, surface_size)?
        };
        match action {
            GpuBenchmarkAction::RequestNextFrame { sample_count } => {
                self.performance.record_pointer_batch(sample_count);
                self.request_redraw(RedrawReason::PointerInput);
                Ok(false)
            }
            GpuBenchmarkAction::Complete => Ok(true),
        }
    }

    /// 执行工具栏命令，并返回该命令是否要求事件循环退出。
    fn apply_ui_command(&mut self, command: UiCommand) -> bool {
        match command {
            UiCommand::ExitApplication => return true,
            UiCommand::EnterAnnotation => {
                if self.state.enter_normal_annotation() {
                    self.idle_panel = IdlePanel::Toolbar;
                    self.window_context.set_annotation_mode(true);
                    self.compositor.invalidate_ink_cache();
                }
            }
            UiCommand::ExitAnnotation => {
                if self.state.exit_normal_annotation() {
                    self.active_gesture = None;
                    self.input_router.cancel();
                    self.window_context.set_annotation_mode(false);
                    self.compositor.invalidate_ink_cache();
                }
            }
            UiCommand::SelectPen => self.tools.tool = InkTool::Pen,
            UiCommand::SelectEraser => self.tools.tool = InkTool::RegionEraser,
            UiCommand::CycleEraserSize => self.tools.cycle_eraser_size(),
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
                    self.idle_panel = IdlePanel::Settings;
                    self.window_context
                        .set_idle_window_view(IdleWindowView::Settings);
                }
            }
            UiCommand::OpenSettingsDirectory => match self.settings_store.open_directory() {
                Ok(()) => self.settings_directory_error = None,
                Err(error) => {
                    tracing::warn!(%error, "打开配置目录失败");
                    self.settings_directory_error = Some(error.to_string());
                }
            },
            UiCommand::CloseSettings => {
                if self.state.mode() == AppMode::IdleFloatingToolbar {
                    self.idle_panel = IdlePanel::Toolbar;
                    self.window_context
                        .set_idle_window_view(IdleWindowView::Toolbar);
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
        self.request_redraw(RedrawReason::UiCommand);
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

    /// 把当前 idle 面板映射为原生窗口几何类型。
    fn current_idle_window_view(&self) -> IdleWindowView {
        match self.idle_panel {
            IdlePanel::Toolbar => IdleWindowView::Toolbar,
            IdlePanel::QuickSettings => IdleWindowView::QuickSettings,
            IdlePanel::Settings => IdleWindowView::Settings,
        }
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
        let mut sample_count = 0;
        while let Ok(event) = self.windows_pointer_receiver.try_recv() {
            if event.cancels_ui_pointer() {
                self.compositor.cancel_egui_pointer();
            }
            let ui_hit = event.position().is_some_and(|position| {
                let scale_factor = self.window_context.window().scale_factor() as f32;
                let logical_position =
                    egui::pos2(position.x / scale_factor, position.y / scale_factor);
                self.compositor
                    .egui_context()
                    .layer_id_at(logical_position)
                    .is_some()
            });
            if let Some(action) = self.input_router.route_windows_pointer(
                event,
                ui_hit,
                self.state.mode().accepts_ink_input(),
            ) {
                sample_count += pointer_action_sample_count(&action);
                self.apply_pointer_action(action);
                changed = true;
            }
        }
        self.performance.record_pointer_batch(sample_count);
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
                    self.state.start_slideshow(SlideShowSession::new(key, page))
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
        self.window_context.set_annotation_mode(annotation_enabled);
        self.compositor.invalidate_ink_cache();
    }

    /// 记录原因并向唯一窗口请求一次按需重绘。
    fn request_redraw(&mut self, reason: RedrawReason) {
        self.performance.record_redraw(reason);
        self.window_context.window().request_redraw();
    }
}

/// 返回一个统一指针动作包含的原始可见输入样本数量。
fn pointer_action_sample_count(action: &PointerAction) -> usize {
    match action {
        PointerAction::Begin(_)
        | PointerAction::Move(_)
        | PointerAction::End(_)
        | PointerAction::BeginPalmErase(_)
        | PointerAction::MovePalmErase(_)
        | PointerAction::EndPalmErase(_) => 1,
        PointerAction::CommitBuffered(points) => points.len(),
        PointerAction::Cancel => 0,
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

/// winit ApplicationHandler，保证空闲时使用 Wait/WaitUntil 而非持续轮询。
struct DesktopApplication {
    proxy: EventLoopProxy<UserEvent>,
    windows_pointer_receiver: Option<Receiver<WindowsPointerEvent>>,
    runtime: Option<DesktopRuntime>,
    startup_error: Option<AppError>,
    next_repaint: Option<Instant>,
}

impl DesktopApplication {
    /// 创建尚未恢复窗口的应用处理器。
    fn new(
        proxy: EventLoopProxy<UserEvent>,
        windows_pointer_receiver: Receiver<WindowsPointerEvent>,
    ) -> Self {
        Self {
            proxy,
            windows_pointer_receiver: Some(windows_pointer_receiver),
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
        let next_metrics_report = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.performance.next_report_deadline());
        let next_wake = match (self.next_repaint, next_metrics_report) {
            (Some(repaint), Some(metrics)) => Some(repaint.min(metrics)),
            (Some(repaint), None) => Some(repaint),
            (None, Some(metrics)) => Some(metrics),
            (None, None) => None,
        };
        if let Some(next_wake) = next_wake {
            event_loop.set_control_flow(ControlFlow::WaitUntil(next_wake));
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
        match DesktopRuntime::new(event_loop, self.proxy.clone(), windows_pointer_receiver) {
            Ok(runtime) => {
                self.install_repaint_callback(&runtime);
                self.runtime = Some(runtime);
                if let Some(runtime) = self.runtime.as_mut() {
                    if let Err(error) = runtime.window_context.show() {
                        self.startup_error = Some(error);
                        event_loop.exit();
                        return;
                    }
                    runtime.prepare_gpu_benchmark();
                    runtime.request_redraw(RedrawReason::Startup);
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
            Ok(true) => runtime.request_redraw(RedrawReason::WindowEvent),
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
                runtime.request_redraw(RedrawReason::Egui);
            }
            UserEvent::ExternalEvent => {
                if runtime.process_slideshow_events() {
                    runtime.request_redraw(RedrawReason::SlideShow);
                }
                self.update_control_flow(event_loop);
            }
            UserEvent::WindowsPointer => {
                if runtime.process_windows_pointer_events() {
                    runtime.request_redraw(RedrawReason::PointerInput);
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
            }
            if let Some(runtime) = self.runtime.as_mut() {
                runtime.performance.report_if_due(now);
                if repaint_due {
                    runtime.request_redraw(RedrawReason::AnimationTimer);
                }
            }
            self.update_control_flow(event_loop);
        }
    }
}

/// 创建 Windows 用户事件循环并运行单窗口应用。
pub fn run() -> Result<(), AppError> {
    let (windows_pointer_sender, windows_pointer_receiver) = mpsc::channel();
    let proxy_slot = Arc::new(OnceLock::<EventLoopProxy<UserEvent>>::new());
    let hook_proxy_slot = Arc::clone(&proxy_slot);
    let mut pointer_tracker = WindowsPointerTracker::default();
    let mut event_loop_builder = EventLoop::<UserEvent>::with_user_event();
    event_loop_builder.with_msg_hook(move |raw_message| {
        let Some(dispatch) = pointer_tracker.capture_message(raw_message) else {
            return false;
        };
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
    let mut application = DesktopApplication::new(proxy, windows_pointer_receiver);
    event_loop.run_app(&mut application)?;
    application.startup_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证完整手掌手势只提交一个包含全部动态椭圆的擦除 operation。
    #[test]
    fn palm_gesture_commits_one_erase_operation() {
        let first = EraseSample {
            center: CanvasPoint::new(20.0, 30.0),
            radius_x: 40.0,
            radius_y: 24.0,
            rotation_radians: 0.2,
        };
        let second = EraseSample {
            center: CanvasPoint::new(50.0, 60.0),
            radius_x: 48.0,
            radius_y: 28.0,
            rotation_radians: 0.3,
        };
        let mut gesture = ActiveGesture::new_palm_erase(first, ToolState::default());
        gesture.push_palm_erase(second);
        let mut document = InkDocument::new();

        gesture.commit(&mut document);

        assert_eq!(document.operations().len(), 1);
        let InkOperation::EraseStroke(stroke) = &document.operations()[0] else {
            panic!("手掌手势应提交为动态擦除 operation");
        };
        assert_eq!(stroke.samples, vec![first, second]);
    }
}
