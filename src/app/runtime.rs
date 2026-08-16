use std::{
    process::Command,
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

use super::idle_touch_drag::{IdleDragStart, IdleTouchDragState, IdleTouchEnd};
use crate::{
    app::{AppMode, AppState, SlideshowInputMode},
    autostart::{self, MachineAutostartState},
    error::AppError,
    ink::{
        ActiveInkPreview, ActiveStrokeDelta, ActiveStrokeStyle, CanvasPoint, EraseSample,
        InkDocument, InkOperation, InkTool, NaturalStrokeBuilder, OwnedActiveInkPreview,
        VariableStrokePoint,
    },
    input::{
        InputRouter, PointerAction, PointerSample, SharedCanvasPalmCapture, SharedPalmSizePreset,
        WindowsPointerEvent, WindowsPointerTracker,
    },
    logging,
    performance::{PerformanceSnapshot, export_snapshot},
    recovery::{RecoveryEvent, RecoveryManager, RecoveryStartup},
    render::{
        EguiUiState, FrameTerminal, RenderEvent, RenderFrame, RenderPerformanceMetadata,
        RenderThread,
    },
    settings::{SettingsStore, UserSettings},
    slideshow::{
        ComDetector, ComDetectorEvent, ComDiagnostics, PageSwitchOutcome, SlidePage,
        SlideShowControlAction, SlideShowKey, SlideShowSession,
    },
    ui::{self, IdlePanel, ToolState, UiCommand, UiFrameOutput, UiViewState, design_tokens},
    window::{
        D3DWindowContext, IdleWindowView, PhysicalHitRect, SlideshowUiWindow, WindowPlacement,
    },
};

/// egui 请求立即或延迟重绘时发送给 winit 的用户事件。
#[derive(Debug, Clone, Copy)]
enum UserEvent {
    RequestRepaint(Duration),
    ExternalEvent,
    WindowsPointer,
    Render,
    Recovery,
}

/// 区分用户请求的普通退出和完成清理后重启。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplicationExitAction {
    Exit,
    Restart,
}

/// 一条 UI 命令产生的退出动作和可选窗口几何切换结果。
#[derive(Debug, Default)]
struct UiCommandOutcome {
    exit_action: Option<ApplicationExitAction>,
    geometry_placement: Option<WindowPlacement>,
    skip_visual_freeze: bool,
}

/// 一次事件线程绘制结束后需要交给 repaint gate 的提交事实。
#[derive(Debug, Default)]
struct RuntimeRenderOutcome {
    exit_action: Option<ApplicationExitAction>,
    submitted_generation: Option<u64>,
    repaint: bool,
    active_ack: Option<ActiveStrokeAck>,
}

/// 已提交活动增量在 Presented 后推进的事件线程游标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveStrokeAck {
    gesture_id: u64,
    revision: u64,
    sample_count: usize,
    full_resync: bool,
}

/// 合并 winit redraw、渲染 credit 和 egui 延迟重绘的纯状态机。
#[derive(Debug, Default)]
struct RepaintGate {
    dirty: bool,
    os_redraw_requested: bool,
    in_flight: Option<u64>,
    next_repaint: Option<Instant>,
}

impl RepaintGate {
    /// 标记最新可见状态未提交，并返回是否需要请求一次 OS redraw。
    fn invalidate_now(&mut self) -> bool {
        self.dirty = true;
        self.request_redraw_if_ready()
    }

    /// 合并一个 egui 延迟重绘期限。
    fn invalidate_at(&mut self, deadline: Instant) {
        self.next_repaint = Some(
            self.next_repaint
                .map_or(deadline, |current| current.min(deadline)),
        );
    }

    /// 把到期的延迟重绘转换成普通 invalidation。
    fn invalidate_due(&mut self, now: Instant) -> bool {
        if self.next_repaint.is_none_or(|deadline| deadline > now) {
            return false;
        }
        self.next_repaint = None;
        self.invalidate_now()
    }

    /// 消费一个主窗口 redraw；只有 dirty 且没有在途帧时才允许构造画面。
    fn begin_redraw(&mut self) -> bool {
        self.os_redraw_requested = false;
        self.next_repaint = None;
        self.dirty && self.in_flight.is_none()
    }

    /// 记录绘制产生的 accepted generation，并合并绘制期间产生的新状态。
    fn finish_redraw(&mut self, generation: Option<u64>, repaint: bool) -> bool {
        debug_assert!(self.in_flight.is_none());
        self.dirty = repaint;
        self.in_flight = generation;
        self.request_redraw_if_ready()
    }

    /// 消费 Presented、Discarded 或 Fatal 终态；旧 ACK 不返还当前 credit。
    fn acknowledge(&mut self, generation: u64) -> Option<bool> {
        if self.in_flight != Some(generation) {
            return None;
        }
        self.in_flight = None;
        Some(self.request_redraw_if_ready())
    }

    /// 在没有 OS redraw 和在途帧时为 dirty 状态预留一次 redraw 请求。
    fn request_redraw_if_ready(&mut self) -> bool {
        if !self.dirty || self.os_redraw_requested || self.in_flight.is_some() {
            return false;
        }
        self.os_redraw_requested = true;
        true
    }
}

/// 当前尚未提交为墨迹 operation 的单次指针手势。
#[derive(Debug)]
struct ActiveGesture {
    gesture_id: u64,
    revision: u64,
    presented_sample_count: usize,
    resync_required: bool,
    samples: ActiveGestureSamples,
    tool: InkTool,
    tools: ToolState,
    natural_builder: Option<NaturalStrokeBuilder>,
    variable_preview: Vec<VariableStrokePoint>,
}

#[derive(Debug)]
enum ActiveGestureSamples {
    Tool { points: Vec<CanvasPoint> },
    PalmErase(Vec<EraseSample>),
}

impl ActiveGesture {
    /// 使用当前工具选择从第一个物理像素点开始手势。
    fn new(sample: PointerSample, tools: ToolState, gesture_id: u64) -> Self {
        let natural_builder = (tools.tool == InkTool::Pen && tools.natural_taper_enabled)
            .then(|| NaturalStrokeBuilder::new(sample.point, tools.pen_width.pixels()))
            .flatten();
        Self {
            gesture_id,
            revision: 1,
            presented_sample_count: 0,
            resync_required: false,
            samples: ActiveGestureSamples::Tool {
                points: vec![sample.point],
            },
            tool: tools.tool,
            tools,
            natural_builder,
            variable_preview: Vec::new(),
        }
    }

    /// 使用当前工具选择从第一个非空物理像素批次开始手势。
    fn from_points(points: Vec<PointerSample>, tools: ToolState, gesture_id: u64) -> Option<Self> {
        let mut points = points.into_iter();
        let first = points.next()?;
        let mut gesture = Self::new(first, tools, gesture_id);
        gesture.extend(points);
        Some(gesture)
    }

    /// 使用动态接触椭圆开始一次临时手掌擦除会话。
    fn new_palm_erase(sample: EraseSample, tools: ToolState, gesture_id: u64) -> Self {
        Self {
            gesture_id,
            revision: 1,
            presented_sample_count: 0,
            resync_required: false,
            samples: ActiveGestureSamples::PalmErase(vec![sample]),
            tool: tools.tool,
            tools,
            natural_builder: None,
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
        let ActiveGestureSamples::Tool { points } = &mut self.samples else {
            return;
        };
        let should_push = points.last().is_none_or(|last| {
            let delta_x = last.x - sample.point.x;
            let delta_y = last.y - sample.point.y;
            delta_x.mul_add(delta_x, delta_y * delta_y) >= 0.25
        });
        if should_push {
            points.push(sample.point);
            self.revision = self.revision.wrapping_add(1);
            if let Some(builder) = self.natural_builder.as_mut() {
                builder.push(sample.point);
            }
        }
    }

    /// 判断当前手势是否可使用固定宽/自然笔锋的增量预览协议。
    fn uses_incremental_preview(&self) -> bool {
        matches!(&self.samples, ActiveGestureSamples::Tool { .. }) && self.tool == InkTool::Pen
    }

    /// 从上次 Presented 游标提取尚未确认的原始采样尾部。
    fn active_stroke_delta(&self) -> Option<ActiveStrokeDelta> {
        if !self.uses_incremental_preview() {
            return None;
        }
        let ActiveGestureSamples::Tool { points } = &self.samples else {
            return None;
        };
        let style = if self.natural_builder.is_some() {
            ActiveStrokeStyle::Natural {
                color: self.tools.color,
                body_width: self.tools.pen_width,
            }
        } else {
            ActiveStrokeStyle::Fixed {
                color: self.tools.color,
                width: self.tools.pen_width,
            }
        };
        let from_sample = if self.resync_required {
            0
        } else {
            self.presented_sample_count
        };
        Some(ActiveStrokeDelta {
            gesture_id: self.gesture_id,
            revision: self.revision,
            from_sample,
            samples: points[from_sample..].to_vec(),
            style,
            full_resync: self.resync_required,
        })
    }

    /// 只有匹配当前手势和 revision 的 Presented 才推进确认游标。
    fn acknowledge(&mut self, ack: ActiveStrokeAck) {
        if ack.gesture_id == self.gesture_id
            && ack.revision <= self.revision
            && ack.sample_count <= self.sample_len()
            && (!self.resync_required || ack.full_resync)
        {
            self.presented_sample_count = self.presented_sample_count.max(ack.sample_count);
            self.resync_required = false;
        }
    }

    /// 在 render-side cache 丢失后要求下一帧从原始样本零点完整重发。
    fn request_full_resync(&mut self) {
        self.presented_sample_count = 0;
        self.resync_required = true;
    }

    /// 返回当前手势保留的原始工具点或手掌擦除采样数。
    fn sample_len(&self) -> usize {
        match &self.samples {
            ActiveGestureSamples::Tool { points } => points.len(),
            ActiveGestureSamples::PalmErase(samples) => samples.len(),
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
                if let Some(builder) = self.natural_builder.as_ref() {
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
    fn commit(self, document: &mut InkDocument) -> bool {
        let natural_builder = self.natural_builder;
        match self.samples {
            ActiveGestureSamples::Tool { points, .. } => match self.tool {
                InkTool::Pen => {
                    if let Some(builder) = natural_builder {
                        document
                            .append_variable_draw_stroke(
                                builder.finalized_points(),
                                self.tools.color,
                            )
                            .is_some()
                    } else {
                        document
                            .append_draw_stroke(points, self.tools.color, self.tools.pen_width)
                            .is_some()
                    }
                }
                InkTool::RegionEraser => {
                    let samples = points
                        .into_iter()
                        .map(|point| EraseSample::circle(point, self.tools.eraser_size.pixels()))
                        .collect();
                    document.append_erase_stroke(samples).is_some()
                }
            },
            ActiveGestureSamples::PalmErase(samples) => {
                document.append_erase_stroke(samples).is_some()
            }
        }
    }
}

/// 组合主画布、放映控件窗口、渲染器、状态机和输入路由的桌面运行时。
struct DesktopRuntime {
    render_thread: RenderThread,
    recovery: RecoveryManager,
    egui: EguiUiState,
    slideshow_egui: EguiUiState,
    slideshow_ui_window: SlideshowUiWindow,
    window_context: D3DWindowContext,
    state: AppState,
    empty_document: InkDocument,
    tools: ToolState,
    slideshow_input_mode: SlideshowInputMode,
    input_router: InputRouter,
    active_gesture: Option<ActiveGesture>,
    windows_pointer_receiver: Receiver<WindowsPointerEvent>,
    pen_contact_active: Arc<AtomicBool>,
    palm_size_preset: SharedPalmSizePreset,
    canvas_palm_capture: SharedCanvasPalmCapture,
    slideshow_detector: ComDetector,
    settings_store: SettingsStore,
    settings: UserSettings,
    recovery_error: Option<String>,
    performance_export_status: Option<String>,
    performance_export_failed: bool,
    pending_performance_input: Option<Instant>,
    machine_autostart_state: Option<MachineAutostartState>,
    machine_autostart_error: Option<String>,
    ink_rendering_error: Option<String>,
    idle_panel: IdlePanel,
    com_diagnostics: Option<ComDiagnostics>,
    slideshow_connection_error: Option<String>,
    slideshow_control_error: Option<String>,
    dismiss_slideshow_confirmation: bool,
    idle_window_drag: IdleTouchDragState,
    slideshow_session_generation: u64,
    render_generation: u64,
    next_gesture_id: u64,
}

impl DesktopRuntime {
    /// 在 winit 恢复阶段创建窗口和全部 GPU 资源。
    fn new(
        event_loop: &ActiveEventLoop,
        event_proxy: EventLoopProxy<UserEvent>,
        windows_pointer_receiver: Receiver<WindowsPointerEvent>,
        pen_contact_active: Arc<AtomicBool>,
        palm_size_preset: SharedPalmSizePreset,
        canvas_palm_capture: SharedCanvasPalmCapture,
    ) -> Result<Self, AppError> {
        let settings_store = SettingsStore::new()?;
        let settings = match settings_store.load() {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(%error, "读取设置失败，使用默认值");
                UserSettings::default()
            }
        };
        let (machine_autostart_state, machine_autostart_error) = load_machine_autostart_state();
        let recovery_wake_proxy = event_proxy.clone();
        let RecoveryStartup {
            manager: recovery,
            recovered_state,
            diagnostic: recovery_error,
        } = RecoveryManager::start(settings_store.recovery_directory()?, move || {
            let _ = recovery_wake_proxy.send_event(UserEvent::Recovery);
        })?;
        if let Some(detail) = &recovery_error {
            tracing::warn!(%detail, "启动时处理墨迹恢复数据");
        }
        palm_size_preset.store(settings.palm_size_preset);
        let tools = ToolState {
            tool: InkTool::Pen,
            color: settings.tools.color,
            pen_width: settings.tools.pen_width,
            eraser_size: settings.tools.eraser_size,
            natural_taper_enabled: settings.tools.natural_taper_enabled,
        };
        let window_context = D3DWindowContext::new(event_loop)?;
        let slideshow_ui_window = SlideshowUiWindow::new(
            event_loop,
            window_context.render_target().raw_hwnd(),
            window_context.target_annotation_placement(true),
        )?;
        let egui = EguiUiState::new(event_loop, window_context.window());
        let slideshow_egui = EguiUiState::new(event_loop, slideshow_ui_window.window());
        let render_wake_proxy = event_proxy.clone();
        let render_thread = RenderThread::spawn(
            window_context.render_target(),
            egui.context().clone(),
            slideshow_ui_window.render_target(),
            slideshow_egui.context().clone(),
            move || {
                let _ = render_wake_proxy.send_event(UserEvent::Render);
            },
        )?;
        let ink_rendering_error = render_thread.initial_ink_error().map(str::to_owned);
        let wake_proxy = event_proxy;
        let slideshow_detector = ComDetector::spawn(move || {
            let _ = wake_proxy.send_event(UserEvent::ExternalEvent);
        });
        let state = recovered_state.unwrap_or_default();
        let recovered_annotation = state.mode().accepts_ink_input();
        let mut runtime = Self {
            render_thread,
            recovery,
            egui,
            slideshow_egui,
            slideshow_ui_window,
            window_context,
            state,
            empty_document: InkDocument::new(),
            tools,
            slideshow_input_mode: SlideshowInputMode::Ink,
            input_router: InputRouter::default(),
            active_gesture: None,
            windows_pointer_receiver,
            pen_contact_active,
            palm_size_preset,
            canvas_palm_capture,
            slideshow_detector,
            settings_store,
            settings,
            recovery_error,
            performance_export_status: None,
            performance_export_failed: false,
            pending_performance_input: None,
            machine_autostart_state,
            machine_autostart_error,
            ink_rendering_error,
            idle_panel: IdlePanel::Toolbar,
            com_diagnostics: None,
            slideshow_connection_error: None,
            slideshow_control_error: None,
            dismiss_slideshow_confirmation: false,
            idle_window_drag: IdleTouchDragState::default(),
            slideshow_session_generation: 0,
            render_generation: 0,
            next_gesture_id: 0,
        };
        runtime.sync_canvas_palm_capture();
        if recovered_annotation {
            tracing::info!(mode = ?runtime.state.mode(), "已恢复未正常退出的墨迹会话");
            runtime.apply_startup_annotation_transition(true);
        }
        Ok(runtime)
    }

    /// 返回当前运行时窗口标识。
    fn window_id(&self) -> WindowId {
        self.window_context.window().id()
    }

    /// 返回独立放映控件窗口标识。
    fn slideshow_ui_window_id(&self) -> WindowId {
        self.slideshow_ui_window.window_id()
    }

    /// 使用恢复状态对应的稳定几何显示首次创建的窗口。
    fn show(&self) -> Result<(), AppError> {
        let placement = if self.state.mode().accepts_ink_input() {
            self.window_context.target_annotation_placement(true)
        } else {
            self.window_context
                .target_idle_placement(self.current_idle_window_view())
        };
        self.window_context.show(placement)
    }

    /// 处理非重绘窗口事件，并返回 egui 是否请求重绘。
    fn handle_window_event(&mut self, event: &WindowEvent) -> Result<bool, AppError> {
        self.handle_idle_window_drag_event(event);
        let surface_rebuilt = if let WindowEvent::Resized(size) = event {
            self.render_thread.resize(*size);
            if self.state.mode() == AppMode::IdleFloatingToolbar {
                self.window_context
                    .correct_idle_size(self.current_idle_window_view(), *size);
            }
            true
        } else {
            false
        };
        let event_response = self
            .egui
            .on_window_event(self.window_context.window(), event);
        let accepts_canvas_input = self.accepts_canvas_input();
        let pointer_changed = if let Some(pointer_action) = self.input_router.route(
            event,
            event_response.consumed,
            accepts_canvas_input,
            self.pen_contact_active.load(Ordering::Acquire),
        ) {
            if self.apply_pointer_action(pointer_action) {
                self.queue_recovery();
            }
            true
        } else {
            false
        };
        Ok(surface_rebuilt || pointer_changed || event_response.repaint)
    }

    /// 只把独立放映控件窗口事件交给其 egui 状态，不进入墨迹输入路由。
    fn handle_slideshow_ui_window_event(&mut self, event: &WindowEvent) -> bool {
        let surface_rebuilt = if let WindowEvent::Resized(size) = event {
            self.render_thread.resize_slideshow_ui(*size);
            true
        } else {
            false
        };
        let response = self
            .slideshow_egui
            .on_window_event(self.slideshow_ui_window.window(), event);
        surface_rebuilt || response.repaint
    }

    /// 将统一指针动作应用到当前活动手势或普通批注文档。
    fn apply_pointer_action(&mut self, action: PointerAction) -> bool {
        self.note_performance_input();
        match action {
            PointerAction::Begin(point) => {
                self.next_gesture_id = self.next_gesture_id.wrapping_add(1);
                self.active_gesture =
                    Some(ActiveGesture::new(point, self.tools, self.next_gesture_id));
                false
            }
            PointerAction::Move(point) => {
                if let Some(gesture) = self.active_gesture.as_mut() {
                    gesture.push_point(point);
                }
                false
            }
            PointerAction::End(point) => {
                if let Some(mut gesture) = self.active_gesture.take() {
                    gesture.push_point(point);
                    if let Some(document) = self.state.active_document_mut() {
                        return gesture.commit(document);
                    }
                }
                false
            }
            PointerAction::BeginBatch(points) => {
                self.next_gesture_id = self.next_gesture_id.wrapping_add(1);
                self.active_gesture =
                    ActiveGesture::from_points(points, self.tools, self.next_gesture_id);
                false
            }
            PointerAction::MoveBatch(points) => {
                if let Some(gesture) = self.active_gesture.as_mut() {
                    gesture.extend(points);
                }
                false
            }
            PointerAction::EndBatch(points) => {
                if let Some(mut gesture) = self.active_gesture.take() {
                    gesture.extend(points);
                    if let Some(document) = self.state.active_document_mut() {
                        return gesture.commit(document);
                    }
                }
                false
            }
            PointerAction::BeginPalmErase(sample) => {
                self.next_gesture_id = self.next_gesture_id.wrapping_add(1);
                self.active_gesture = Some(ActiveGesture::new_palm_erase(
                    sample,
                    self.tools,
                    self.next_gesture_id,
                ));
                false
            }
            PointerAction::MovePalmErase(sample) => {
                if let Some(gesture) = self.active_gesture.as_mut() {
                    gesture.push_palm_erase(sample);
                }
                false
            }
            PointerAction::EndPalmErase(sample) => {
                if let Some(mut gesture) = self.active_gesture.take() {
                    gesture.push_palm_erase(sample);
                    if let Some(document) = self.state.active_document_mut() {
                        return gesture.commit(document);
                    }
                }
                false
            }
            PointerAction::CommitBuffered(points) => {
                self.next_gesture_id = self.next_gesture_id.wrapping_add(1);
                if let Some(gesture) =
                    ActiveGesture::from_points(points, self.tools, self.next_gesture_id)
                    && let Some(document) = self.state.active_document_mut()
                {
                    return gesture.commit(document);
                }
                false
            }
            PointerAction::Cancel => {
                self.active_gesture = None;
                false
            }
        }
    }

    /// 运行 UI 并返回本次画面是否被渲染线程接受以及后续 invalidation。
    fn render(&mut self) -> Result<RuntimeRenderOutcome, AppError> {
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
        let performance_snapshot = if self.settings.performance_monitoring_enabled
            || self.idle_panel == IdlePanel::Settings
        {
            self.render_thread.performance_snapshot()
        } else {
            PerformanceSnapshot::default()
        };
        let view = UiViewState {
            mode,
            slideshow_input_mode: self.slideshow_input_mode,
            idle_panel: self.idle_panel,
            dock_side: self.window_context.dock_side(),
            tools,
            palm_size_preset: self.settings.palm_size_preset,
            slideshow_integration_enabled: self.settings.slideshow_integration_enabled,
            log_level: self.settings.log_level,
            readable_mode: self.settings.readable_mode,
            performance_monitoring_enabled: self.settings.performance_monitoring_enabled,
            performance_snapshot,
            performance_export_status: self.performance_export_status.as_deref(),
            performance_export_failed: self.performance_export_failed,
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
            machine_autostart_state: self.machine_autostart_state,
            machine_autostart_error: self.machine_autostart_error.as_deref(),
            graphics_diagnostics: self.render_thread.diagnostics(),
        };
        let slideshow_active = mode.is_slideshow();
        let mut ui_output = UiFrameOutput::default();
        let egui_frame = self.egui.run_ui(self.window_context.window(), |ui| {
            if !slideshow_active {
                ui_output = ui::render(ui, view);
            }
        });
        let slideshow_egui_frame =
            self.slideshow_egui
                .run_ui(self.slideshow_ui_window.window(), |ui| {
                    if slideshow_active {
                        ui_output = ui::render(ui, view);
                    }
                });
        if slideshow_active {
            let hit_regions = physical_hit_regions(
                &ui_output.slideshow_hit_regions,
                slideshow_egui_frame.pixels_per_point,
            );
            if let Err(error) = self.slideshow_ui_window.update_regions(&hit_regions) {
                tracing::warn!(%error, "更新放映控件窗口区域失败");
            }
        } else if let Err(error) = self.slideshow_ui_window.hide() {
            tracing::warn!(%error, "隐藏放映控件窗口失败");
        }
        let ui_command = ui_output.command;

        let document = self.state.active_document().unwrap_or(&self.empty_document);
        let active_stroke = self
            .active_gesture
            .as_ref()
            .and_then(ActiveGesture::active_stroke_delta);
        self.render_thread.record_generated(
            active_stroke
                .as_ref()
                .map_or(0, |delta| delta.from_sample + delta.samples.len()),
        );
        let preview = self
            .active_gesture
            .as_mut()
            .filter(|gesture| !gesture.uses_incremental_preview())
            .map(ActiveGesture::preview);
        self.render_generation = self.render_generation.wrapping_add(1);
        let performance =
            self.settings
                .performance_monitoring_enabled
                .then(|| RenderPerformanceMetadata {
                    submitted_at: Instant::now(),
                    input_started_at: self.pending_performance_input.take(),
                });
        if performance.is_none() {
            self.pending_performance_input = None;
        }
        let frame = RenderFrame {
            generation: self.render_generation,
            document: document.clone(),
            active_preview: preview.map(OwnedActiveInkPreview::from),
            active_stroke: active_stroke.clone(),
            egui: egui_frame,
            slideshow_ui: slideshow_egui_frame,
            performance,
        };

        let mut render_outcome = RuntimeRenderOutcome::default();
        let active_ack = active_stroke.as_ref().map(|delta| ActiveStrokeAck {
            gesture_id: delta.gesture_id,
            revision: delta.revision,
            sample_count: delta.from_sample + delta.samples.len(),
            full_resync: delta.full_resync,
        });
        render_outcome.exit_action = if let Some(command) = ui_command {
            if ui_command_changes_window_geometry(command) {
                let previous_state = self.state.clone();
                let outcome = self.apply_ui_command(command);
                if self.state != previous_state {
                    self.queue_recovery();
                }
                if let Some(placement) = outcome.geometry_placement {
                    if outcome.skip_visual_freeze {
                        // 退出批注时不冻结旧帧，直接调整窗口并重绘新UI
                        if let Err(error) = self.window_context.apply_window_placement(placement) {
                            tracing::warn!(?placement, %error, "提交窗口几何失败");
                        } else {
                            self.render_thread.resize(placement.size);
                        }
                        self.render_thread
                            .retain_source_frame_texture_deltas(frame)?;
                    } else {
                        render_outcome.submitted_generation =
                            self.commit_window_geometry(placement, Some(frame))?;
                    }
                    render_outcome.repaint = true;
                } else {
                    render_outcome.submitted_generation = Some(frame.generation);
                    self.render_thread.submit_frame(frame)?;
                    render_outcome.active_ack = active_ack;
                    render_outcome.repaint = true;
                }
                outcome.exit_action
            } else {
                render_outcome.submitted_generation = Some(frame.generation);
                self.render_thread.submit_frame(frame)?;
                render_outcome.active_ack = active_ack;
                let previous_state = self.state.clone();
                let outcome = self.apply_ui_command(command);
                debug_assert!(outcome.geometry_placement.is_none());
                if self.state != previous_state {
                    self.queue_recovery();
                }
                render_outcome.repaint = true;
                outcome.exit_action
            }
        } else {
            render_outcome.submitted_generation = Some(frame.generation);
            self.render_thread.submit_frame(frame)?;
            render_outcome.active_ack = active_ack;
            None
        };
        Ok(render_outcome)
    }

    /// 在匹配的 Presented 终态后确认活动笔画尾部。
    fn acknowledge_active_stroke(&mut self, ack: ActiveStrokeAck) {
        if let Some(gesture) = self.active_gesture.as_mut() {
            gesture.acknowledge(ack);
        }
    }

    /// 将 render-side cache-loss 请求映射为当前活动画笔的 sample-zero 重发。
    fn request_active_stroke_resync(&mut self) {
        if let Some(gesture) = self.active_gesture.as_mut()
            && gesture.uses_incremental_preview()
        {
            gesture.request_full_resync();
        }
    }

    /// 执行工具栏命令，并返回退出动作及可选窗口几何切换结果。
    fn apply_ui_command(&mut self, command: UiCommand) -> UiCommandOutcome {
        let mut outcome = UiCommandOutcome::default();
        if ui_command_changes_window_geometry(command) {
            self.clear_idle_window_drag("UI 窗口几何切换");
        }
        match command {
            UiCommand::ExitApplication => {
                self.reset_slideshow_input_mode();
                self.canvas_palm_capture.store(false);
                outcome.exit_action = Some(ApplicationExitAction::Exit);
                return outcome;
            }
            UiCommand::RestartApplication => {
                self.reset_slideshow_input_mode();
                self.canvas_palm_capture.store(false);
                outcome.exit_action = Some(ApplicationExitAction::Restart);
                return outcome;
            }
            UiCommand::EnterAnnotation => {
                if self.state.enter_normal_annotation() {
                    self.sync_canvas_palm_capture();
                    outcome.geometry_placement = Some(self.prepare_annotation_geometry(true));
                }
            }
            UiCommand::ExitAnnotation => {
                if self.state.exit_normal_annotation() {
                    self.sync_canvas_palm_capture();
                    outcome.geometry_placement = Some(self.prepare_annotation_geometry(false));
                    outcome.skip_visual_freeze = true;
                }
            }
            UiCommand::SelectPen => self.tools.tool = InkTool::Pen,
            UiCommand::ToggleSlideshowPenMode => {
                if self.state.mode().is_slideshow() {
                    let (mode, tool) =
                        slideshow_pen_transition(self.slideshow_input_mode, self.tools.tool);
                    self.set_slideshow_input_mode(mode);
                    self.tools.tool = tool;
                }
            }
            UiCommand::SelectEraser => {
                if self.state.mode().is_slideshow() {
                    self.set_slideshow_input_mode(SlideshowInputMode::Ink);
                }
                self.tools.tool = InkTool::RegionEraser;
            }
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
            UiCommand::SetNaturalTaperEnabled(enabled) => {
                self.tools.natural_taper_enabled = enabled;
                self.settings.tools.natural_taper_enabled = enabled;
                self.save_settings();
            }
            UiCommand::SetPalmSizePreset(preset) => {
                self.settings.palm_size_preset = preset;
                self.palm_size_preset.store(preset);
                self.save_settings();
            }
            UiCommand::Undo => {
                let undone = self.state.active_document_mut().and_then(InkDocument::undo);
                if let Some(operation) = undone {
                    if matches!(operation, InkOperation::Clear(_)) {
                        self.render_thread.invalidate_ink_cache();
                    } else if let Some(bounds) = operation.bounds() {
                        self.render_thread.invalidate_ink_region(bounds);
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
                    outcome.geometry_placement =
                        Some(self.target_idle_window_geometry(IdleWindowView::Settings));
                    self.update_interface_zoom();
                }
            }
            UiCommand::OpenSettingsDirectory => {
                if let Err(error) = self.settings_store.open_directory() {
                    tracing::warn!(%error, "打开配置目录失败");
                }
            }
            UiCommand::SetMachineAutostart(enabled) => {
                self.set_machine_autostart(enabled);
            }
            UiCommand::CloseSettings => {
                if self.state.mode() == AppMode::IdleFloatingToolbar {
                    self.idle_panel = IdlePanel::Toolbar;
                    outcome.geometry_placement =
                        Some(self.target_idle_window_geometry(IdleWindowView::Toolbar));
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
                    outcome.geometry_placement =
                        Some(self.target_idle_window_geometry(window_view));
                    self.update_interface_zoom();
                }
            }
            UiCommand::BeginIdleToolbarDrag => {
                if self.state.mode() == AppMode::IdleFloatingToolbar {
                    match self.idle_window_drag.promote_or_start_mouse() {
                        IdleDragStart::ManualTouch {
                            touch_id,
                            target_outer,
                        } => {
                            tracing::debug!(touch_id, ?target_outer, "egui 已提升工具栏触摸拖动");
                            self.window_context.set_outer_position(target_outer);
                        }
                        IdleDragStart::NativeMouse => {
                            tracing::debug!("egui 已开始工具栏原生鼠标拖动");
                            self.window_context.begin_window_drag();
                        }
                        IdleDragStart::Ignored => {}
                    }
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
            UiCommand::SetPerformanceMonitoringEnabled(enabled) => {
                self.settings.performance_monitoring_enabled = enabled;
                if !enabled {
                    self.pending_performance_input = None;
                }
                self.save_settings();
            }
            UiCommand::ExportPerformanceData => self.export_performance_data(),
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
                    self.reset_slideshow_input_mode();
                    self.dismiss_slideshow_confirmation = false;
                    outcome.geometry_placement = Some(self.prepare_annotation_geometry(false));
                }
            }
            UiCommand::CancelDismissSlideshow => {
                self.dismiss_slideshow_confirmation = false;
            }
        }
        outcome
    }

    /// 保存当前用户偏好，并把失败记录到应用日志。
    fn save_settings(&mut self) {
        if let Err(error) = self.settings_store.save(&self.settings) {
            tracing::warn!(%error, "保存设置失败");
        }
    }

    /// 把最新有界性能快照写入日志目录，并保存非致命 UI 诊断。
    fn export_performance_data(&mut self) {
        let snapshot = self.render_thread.performance_snapshot();
        let result = self
            .settings_store
            .ensure_logs_directory()
            .and_then(|directory| export_snapshot(&directory, snapshot));
        match result {
            Ok(path) => {
                tracing::info!(path = %path.display(), "性能快照已导出");
                self.performance_export_status = Some(format!("已导出至 {}", path.display()));
                self.performance_export_failed = false;
            }
            Err(error) => {
                tracing::warn!(%error, "导出性能快照失败");
                self.performance_export_status = Some(error.to_string());
                self.performance_export_failed = true;
            }
        }
    }

    /// 仅在监控开启时记录自上一帧以来最早的墨迹输入时间。
    fn note_performance_input(&mut self) {
        if self.settings.performance_monitoring_enabled {
            self.pending_performance_input
                .get_or_insert_with(Instant::now);
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

    /// 把当前 idle 面板映射为原生窗口几何类型。
    fn current_idle_window_view(&self) -> IdleWindowView {
        match self.idle_panel {
            IdlePanel::Toolbar => IdleWindowView::Toolbar,
            IdlePanel::QuickSettings => IdleWindowView::QuickSettings,
            IdlePanel::Settings => IdleWindowView::Settings,
        }
    }

    /// 在 egui 路由前选择并驱动非批注工具栏的单根主触摸。
    fn handle_idle_window_drag_event(&mut self, event: &WindowEvent) {
        if self.state.mode() != AppMode::IdleFloatingToolbar {
            self.clear_idle_window_drag("应用模式不再是非批注工具栏");
            return;
        }
        match event {
            WindowEvent::Focused(false) => self.clear_idle_window_drag("主窗口失去焦点"),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.clear_idle_window_drag("主窗口几何已改变");
            }
            WindowEvent::Touch(touch) => match touch.phase {
                TouchPhase::Started if self.idle_window_drag.is_idle() => {
                    let screen_position = match self.window_context.client_to_screen(touch.location)
                    {
                        Ok(position) => position,
                        Err(error) => {
                            tracing::warn!(touch_id = touch.id, %error, "无法选择工具栏主触摸");
                            return;
                        }
                    };
                    let window_outer = match self.window_context.outer_position() {
                        Ok(position) => position,
                        Err(error) => {
                            tracing::warn!(touch_id = touch.id, %error, "无法记录触摸开始时的窗口位置");
                            return;
                        }
                    };
                    if self
                        .idle_window_drag
                        .start_touch(touch.id, screen_position, window_outer)
                    {
                        tracing::debug!(
                            touch_id = touch.id,
                            ?screen_position,
                            ?window_outer,
                            "已选择工具栏主触摸"
                        );
                    }
                }
                TouchPhase::Started => {
                    tracing::debug!(touch_id = touch.id, "忽略额外的工具栏触摸");
                }
                TouchPhase::Moved if self.idle_window_drag.tracks_touch(touch.id) => {
                    match self.window_context.client_to_screen(touch.location) {
                        Ok(screen_position) => {
                            if let Some(target_outer) =
                                self.idle_window_drag.move_touch(touch.id, screen_position)
                            {
                                tracing::trace!(
                                    touch_id = touch.id,
                                    ?screen_position,
                                    ?target_outer,
                                    "按主触摸移动工具栏窗口"
                                );
                                self.window_context.set_outer_position(target_outer);
                            }
                        }
                        Err(error) => tracing::debug!(
                            touch_id = touch.id,
                            %error,
                            "忽略无法转换屏幕坐标的工具栏触摸移动"
                        ),
                    }
                }
                TouchPhase::Ended | TouchPhase::Cancelled => {
                    if self.idle_window_drag.tracks_touch(touch.id)
                        && let Ok(screen_position) =
                            self.window_context.client_to_screen(touch.location)
                        && let Some(target_outer) =
                            self.idle_window_drag.move_touch(touch.id, screen_position)
                    {
                        self.window_context.set_outer_position(target_outer);
                    }
                    match self.idle_window_drag.end_touch(touch.id) {
                        IdleTouchEnd::FinishDrag { last_outer } => {
                            tracing::debug!(
                                touch_id = touch.id,
                                phase = ?touch.phase,
                                ?last_outer,
                                "工具栏主触摸已结束"
                            );
                            self.finish_idle_window_drag(Some(last_outer));
                        }
                        IdleTouchEnd::PendingCleared => tracing::debug!(
                            touch_id = touch.id,
                            phase = ?touch.phase,
                            "工具栏触摸未达到拖动阈值"
                        ),
                        IdleTouchEnd::Ignored => {}
                    }
                }
                TouchPhase::Moved => {}
            },
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } if self.idle_window_drag.end_mouse() => self.finish_idle_window_drag(None),
            _ => {}
        }
    }

    /// 完成一次已确认的窗口拖动，并复用既有左右边缘吸附规则。
    fn finish_idle_window_drag(
        &self,
        manual_touch_position: Option<winit::dpi::PhysicalPosition<i32>>,
    ) {
        let view = self.current_idle_window_view();
        match self
            .window_context
            .finish_idle_window_drag(view, manual_touch_position)
        {
            Ok(side) => tracing::debug!(?view, ?side, "悬浮工具栏拖动已完成并吸附"),
            Err(error) => tracing::warn!(?view, %error, "完成悬浮工具栏拖动后的窗口几何更新失败"),
        }
    }

    /// 在非指针生命周期边界无吸附地清理 pending 或 active 拖动。
    fn clear_idle_window_drag(&mut self, reason: &'static str) {
        if self.idle_window_drag.clear() {
            tracing::debug!(reason, "已清理工具栏窗口拖动状态");
        }
    }

    /// 返回非批注视图对应的稳定窗口目标几何，不在命令处理阶段移动 HWND。
    fn target_idle_window_geometry(&self, view: IdleWindowView) -> WindowPlacement {
        self.window_context.target_idle_placement(view)
    }

    /// 在工具界面与完整设置页之间切换 egui 的全局显示缩放。
    fn update_interface_zoom(&self) {
        let zoom = if self.idle_panel == IdlePanel::Settings {
            design_tokens::SETTINGS_ZOOM_FACTOR
        } else {
            design_tokens::TOOLBAR_ZOOM_FACTOR
        };
        self.egui.context().set_zoom_factor(zoom);
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

    /// 返回当前顶层模式和放映瞬时模式是否共同允许软件画布接收墨迹。
    fn accepts_canvas_input(&self) -> bool {
        canvas_palm_capture_enabled(self.state.mode(), self.slideshow_input_mode)
    }

    /// 将运行时画布输入能力同步到原生消息 hook 的启发式手掌门。
    fn sync_canvas_palm_capture(&self) {
        let enabled = self.accepts_canvas_input();
        if self.canvas_palm_capture.store(enabled) {
            tracing::debug!(
                enabled,
                mode = ?self.state.mode(),
                slideshow_input_mode = ?self.slideshow_input_mode,
                "原生启发式手掌接管门已切换"
            );
        }
    }

    /// 切换放映输入模式，并在路由边界处终止尚未完成的墨迹手势。
    fn set_slideshow_input_mode(&mut self, mode: SlideshowInputMode) {
        if self.slideshow_input_mode != mode {
            self.active_gesture = None;
            self.input_router.cancel();
        }
        if let Err(error) = self
            .window_context
            .set_slideshow_mouse_mode(slideshow_mouse_hit_test_enabled(self.state.mode(), mode))
        {
            tracing::warn!(%error, ?mode, "切换放映画布整体穿透失败");
            return;
        }
        self.slideshow_input_mode = mode;
        self.sync_canvas_palm_capture();
    }

    /// 退出或新建放映会话时恢复默认画笔输入并关闭原生穿透。
    fn reset_slideshow_input_mode(&mut self) {
        self.set_slideshow_input_mode(SlideshowInputMode::Ink);
    }

    /// 排空 COM detector 事件，并把统一事件应用到状态机和窗口生命周期。
    fn process_slideshow_events(&mut self) -> bool {
        let previous_state = self.state.clone();
        let mut changed = false;
        while let Ok(event) = self.slideshow_detector.try_recv() {
            changed |= self.apply_slideshow_event(event);
        }
        if self.state != previous_state {
            self.queue_recovery();
        }
        changed
    }

    /// 排空原生 Pointer Input 语义，并用上一帧 egui 区域做 UI 命中判断。
    fn process_windows_pointer_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.windows_pointer_receiver.try_recv() {
            if event.cancels_ui_pointer() {
                if self.state.mode().is_slideshow() {
                    self.slideshow_egui.cancel_pointer();
                } else {
                    self.egui.cancel_pointer();
                }
            }
            let ui_hit = {
                let egui_context = if self.state.mode().is_slideshow() {
                    self.slideshow_egui.context()
                } else {
                    self.egui.context()
                };
                event
                    .position()
                    .and_then(|position| {
                        egui_position_from_physical(position, egui_context.pixels_per_point())
                    })
                    .is_some_and(|position| egui_context.layer_id_at(position).is_some())
            };
            let accepts_canvas_input = self.accepts_canvas_input();
            if let Some(action) =
                self.input_router
                    .route_windows_pointer(event, ui_hit, accepts_canvas_input)
            {
                if self.apply_pointer_action(action) {
                    self.queue_recovery();
                }
                changed = true;
            }
        }
        changed
    }

    /// 应用精确页面切换结果，仅在真实换页时终止输入并重建活动墨迹缓存。
    fn apply_page_switch_outcome(
        &mut self,
        key: &SlideShowKey,
        page: SlidePage,
        outcome: PageSwitchOutcome,
    ) -> bool {
        match outcome {
            PageSwitchOutcome::Unchanged => false,
            PageSwitchOutcome::MetadataUpdated => {
                tracing::info!(
                    application = ?key.application,
                    window_id = key.window_id,
                    page = page.key.show_position(),
                    total_pages = ?page.total_pages,
                    "放映当前页元数据已更新"
                );
                true
            }
            PageSwitchOutcome::PageChanged => {
                self.active_gesture = None;
                self.input_router.cancel();
                self.render_thread.invalidate_ink_cache();
                tracing::info!(
                    application = ?key.application,
                    window_id = key.window_id,
                    page = page.key.show_position(),
                    total_pages = ?page.total_pages,
                    "放映页面已切换并恢复逐页墨迹"
                );
                true
            }
        }
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
                if same_session {
                    if self.state.mode() == AppMode::SlideShowConnectionLost {
                        let Some(outcome) = self.state.restore_slideshow_connection(&key, page)
                        else {
                            return false;
                        };
                        self.apply_page_switch_outcome(&key, page, outcome);
                        self.slideshow_connection_error = None;
                        self.dismiss_slideshow_confirmation = false;
                        tracing::info!(
                            application = ?key.application,
                            window_id = key.window_id,
                            page = page.key.show_position(),
                            "放映连接已恢复"
                        );
                        true
                    } else {
                        let Some(outcome) = self.state.change_slide(&key, page) else {
                            return false;
                        };
                        self.apply_page_switch_outcome(&key, page, outcome)
                    }
                } else {
                    let changed = self.state.start_slideshow(SlideShowSession::new(key, page));
                    if changed {
                        self.reset_slideshow_input_mode();
                        self.tools.tool = InkTool::Pen;
                        self.slideshow_session_generation =
                            self.slideshow_session_generation.wrapping_add(1);
                        self.apply_annotation_transition(true);
                        self.slideshow_connection_error = None;
                        self.dismiss_slideshow_confirmation = false;
                        if let Some(session) = self.state.slideshow_session() {
                            tracing::info!(
                                application = ?session.key().application,
                                window_id = session.key().window_id,
                                page = page.key.show_position(),
                                "放映批注会话已开始"
                            );
                        }
                    }
                    changed
                }
            }
            ComDetectorEvent::SlideChanged { key, page } => {
                let Some(outcome) = self.state.change_slide(&key, page) else {
                    return false;
                };
                self.apply_page_switch_outcome(&key, page, outcome)
            }
            ComDetectorEvent::SlideShowEnded { key } => {
                let changed = self.state.end_slideshow(&key);
                if changed {
                    self.reset_slideshow_input_mode();
                    self.apply_annotation_transition(false);
                    self.slideshow_connection_error = None;
                    self.dismiss_slideshow_confirmation = false;
                    tracing::info!(
                        application = ?key.application,
                        window_id = key.window_id,
                        "放映批注会话已确认结束"
                    );
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
                    self.render_thread.invalidate_ink_cache();
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

    /// 清理活动输入并恢复普通工具栏视觉状态，不在这里改变窗口或 GPU 资源。
    fn prepare_annotation_state(&mut self) {
        self.clear_idle_window_drag("批注模式切换");
        self.active_gesture = None;
        self.input_router.cancel();
        self.idle_panel = IdlePanel::Toolbar;
        self.update_interface_zoom();
    }

    /// 清理批注切换状态、更新 GPU 驻留策略并返回稳定目标几何。
    fn prepare_annotation_geometry(&mut self, annotation_enabled: bool) -> WindowPlacement {
        self.prepare_annotation_state();
        self.render_thread
            .set_annotation_resources_enabled(annotation_enabled);
        self.render_thread.invalidate_ink_cache();
        self.window_context
            .target_annotation_placement(annotation_enabled)
    }

    /// 在窗口尚未显示的恢复阶段直接提交批注几何，无需冻结不可见的旧 visual。
    fn apply_startup_annotation_transition(&mut self, annotation_enabled: bool) {
        let placement = self.prepare_annotation_geometry(annotation_enabled);
        match self.window_context.apply_window_placement(placement) {
            Ok(()) => self.render_thread.resize(placement.size),
            Err(error) => {
                tracing::warn!(
                    annotation_enabled,
                    mode = ?self.state.mode(),
                    ?placement,
                    %error,
                    "启动恢复时批注窗口几何切换失败"
                );
            }
        }
    }

    /// 为非 UI 事件使用与 UI 命令相同的无动画 visual 冻结协议。
    fn apply_annotation_transition(&mut self, annotation_enabled: bool) {
        let placement = self.prepare_annotation_geometry(annotation_enabled);
        if let Err(error) = self.commit_window_geometry(placement, None) {
            tracing::warn!(%error, "提交批注窗口切换失败");
        }
    }

    /// 冻结旧 visual，并返回失败回退时真正提交的 source generation。
    fn commit_window_geometry(
        &mut self,
        target: WindowPlacement,
        source_frame: Option<RenderFrame>,
    ) -> Result<Option<u64>, AppError> {
        self.clear_idle_window_drag("窗口几何提交");
        let source = match self.window_context.current_placement() {
            Ok(placement) => placement,
            Err(error) => {
                tracing::warn!(?target, %error, "读取窗口源几何失败，已取消窗口切换");
                if let Some(frame) = source_frame {
                    let generation = frame.generation;
                    self.render_thread.submit_frame(frame)?;
                    return Ok(Some(generation));
                }
                return Ok(None);
            }
        };
        let visual_offset = source.visual_offset_to(target);
        // 先把 HWND 移动到目标几何，再设置反向 visual 偏移把画面冻结回原屏幕位置。
        // 窗口移动与偏移提交落在同一 DWM 合成周期，避免偏移先生效而窗口未动产生的闪现帧。
        if let Err(error) = self.window_context.apply_window_placement(target) {
            tracing::warn!(?source, ?target, %error, "提交最终窗口几何失败");
            if let Some(frame) = source_frame {
                let generation = frame.generation;
                self.render_thread.submit_frame(frame)?;
                return Ok(Some(generation));
            }
            return Ok(None);
        }
        if let Err(error) = self
            .render_thread
            .hold_window_visual(visual_offset, source_frame)
        {
            tracing::warn!(?source, ?target, %error, "冻结旧窗口画面失败");
            return Ok(None);
        }

        self.render_thread.arm_window_visual_reset();
        self.render_thread.resize(target.size);
        Ok(None)
    }

    /// 排空渲染线程结果，并在 fatal error 时恢复统一 AppError 传播。
    fn process_render_events(
        &mut self,
        mut observe_terminal: impl FnMut(u64, &FrameTerminal),
        mut request_active_resync: impl FnMut(),
    ) -> Result<bool, AppError> {
        let mut changed = false;
        while let Some(event) = self.render_thread.try_recv_event() {
            match event {
                RenderEvent::FrameTerminal {
                    generation,
                    outcome,
                } => {
                    observe_terminal(generation, &outcome);
                    if let FrameTerminal::Fatal(detail) = outcome {
                        return Err(AppError::Graphics(detail));
                    }
                }
                RenderEvent::InkRenderingError(error) => {
                    changed |= self.ink_rendering_error != error;
                    self.ink_rendering_error = error;
                }
                RenderEvent::GraphicsDiagnostics(_) => changed = true,
                RenderEvent::ActiveStrokeResyncRequested => request_active_resync(),
                RenderEvent::Fatal(detail) => return Err(AppError::Graphics(detail)),
            }
        }
        Ok(changed)
    }

    /// 排空恢复 worker 错误并更新设置诊断。
    fn process_recovery_events(&mut self) -> bool {
        let mut changed = false;
        while let Some(RecoveryEvent::Error(detail)) = self.recovery.try_recv_event() {
            tracing::warn!(%detail, "墨迹后台保存失败");
            changed |= self.recovery_error.as_deref() != Some(detail.as_str());
            self.recovery_error = Some(detail);
        }
        changed
    }

    /// 把当前完整状态 cheap-clone 提交给 latest-state 恢复邮箱。
    fn queue_recovery(&mut self) {
        if !self.recovery.submit(self.state.clone()) && self.recovery_error.is_none() {
            self.recovery_error = Some("墨迹恢复线程已停止，无法继续自动保存".to_owned());
        }
    }

    /// 按退出原因清理或保留恢复文件，并始终停止渲染线程。
    fn shutdown(&mut self, clean_exit: bool) -> Result<(), AppError> {
        self.clear_idle_window_drag("应用关闭");
        self.reset_slideshow_input_mode();
        self.canvas_palm_capture.store(false);
        if let Err(error) = self.slideshow_ui_window.hide() {
            tracing::warn!(%error, "关闭应用时隐藏放映控件窗口失败");
        }
        let recovery_result = if clean_exit {
            self.recovery.shutdown_clean()
        } else {
            self.recovery.shutdown_preserve()
        };
        let render_result = self.render_thread.shutdown();
        recovery_result.and(render_result)
    }

    /// 由主窗口驱动一次同时包含画布和放映控件表面的按需重绘。
    fn request_redraw(&self) {
        self.window_context.window().request_redraw();
    }
}

/// 返回一条 UI 命令是否会改变原生窗口和交换链尺寸。
const fn ui_command_changes_window_geometry(command: UiCommand) -> bool {
    matches!(
        command,
        UiCommand::EnterAnnotation
            | UiCommand::ExitAnnotation
            | UiCommand::OpenSettings
            | UiCommand::CloseSettings
            | UiCommand::ToggleQuickSettings
            | UiCommand::ConfirmDismissSlideshow
    )
}

/// 把原生物理像素坐标换算为包含 egui zoom 的点坐标。
fn egui_position_from_physical(position: CanvasPoint, pixels_per_point: f32) -> Option<egui::Pos2> {
    (pixels_per_point.is_finite() && pixels_per_point > 0.0)
        .then(|| egui::pos2(position.x / pixels_per_point, position.y / pixels_per_point))
}

/// 把本帧 egui 点坐标区域向外取整为客户区物理像素矩形。
fn physical_hit_regions(regions: &[egui::Rect], pixels_per_point: f32) -> Vec<PhysicalHitRect> {
    if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
        return Vec::new();
    }
    regions
        .iter()
        .filter(|region| region.is_finite())
        .filter_map(|region| {
            let physical = PhysicalHitRect {
                min_x: (region.min.x * pixels_per_point).floor() as i32,
                min_y: (region.min.y * pixels_per_point).floor() as i32,
                max_x: (region.max.x * pixels_per_point).ceil() as i32,
                max_y: (region.max.y * pixels_per_point).ceil() as i32,
            };
            (physical.min_x < physical.max_x && physical.min_y < physical.max_y).then_some(physical)
        })
        .collect()
}

/// 返回顶层应用模式与放映输入模式是否共同允许启发式手掌接管。
const fn canvas_palm_capture_enabled(app_mode: AppMode, input_mode: SlideshowInputMode) -> bool {
    app_mode.accepts_ink_input() && (!app_mode.is_slideshow() || input_mode.accepts_ink_input())
}

/// 根据放映输入模式和当前工具计算画笔按钮的完整状态转换。
const fn slideshow_pen_transition(
    input_mode: SlideshowInputMode,
    tool: InkTool,
) -> (SlideshowInputMode, InkTool) {
    match (input_mode, tool) {
        (SlideshowInputMode::Ink, InkTool::Pen) => (SlideshowInputMode::Mouse, InkTool::Pen),
        (SlideshowInputMode::Ink, InkTool::RegionEraser) | (SlideshowInputMode::Mouse, _) => {
            (SlideshowInputMode::Ink, InkTool::Pen)
        }
    }
}

/// 返回当前顶层状态是否允许原生放映画布选择性穿透。
const fn slideshow_mouse_hit_test_enabled(
    app_mode: AppMode,
    input_mode: SlideshowInputMode,
) -> bool {
    app_mode.is_slideshow() && matches!(input_mode, SlideshowInputMode::Mouse)
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
    palm_size_preset: SharedPalmSizePreset,
    canvas_palm_capture: SharedCanvasPalmCapture,
    runtime: Option<DesktopRuntime>,
    startup_error: Option<AppError>,
    repaint_gate: RepaintGate,
    in_flight_active: Option<ActiveStrokeAck>,
    exit_action: Option<ApplicationExitAction>,
}

impl DesktopApplication {
    /// 创建尚未恢复窗口的应用处理器。
    fn new(
        proxy: EventLoopProxy<UserEvent>,
        windows_pointer_receiver: Receiver<WindowsPointerEvent>,
        pen_contact_active: Arc<AtomicBool>,
        palm_size_preset: SharedPalmSizePreset,
        canvas_palm_capture: SharedCanvasPalmCapture,
    ) -> Self {
        Self {
            proxy,
            windows_pointer_receiver: Some(windows_pointer_receiver),
            pen_contact_active,
            palm_size_preset,
            canvas_palm_capture,
            runtime: None,
            startup_error: None,
            repaint_gate: RepaintGate::default(),
            in_flight_active: None,
            exit_action: None,
        }
    }

    /// 安装 egui 的重绘回调，使动画按需唤醒等待型事件循环。
    fn install_repaint_callback(&self, runtime: &DesktopRuntime) {
        let proxy = self.proxy.clone();
        runtime
            .egui
            .context()
            .set_request_repaint_callback(move |info| {
                let _ = proxy.send_event(UserEvent::RequestRepaint(info.delay));
            });
        let proxy = self.proxy.clone();
        runtime
            .slideshow_egui
            .context()
            .set_request_repaint_callback(move |info| {
                let _ = proxy.send_event(UserEvent::RequestRepaint(info.delay));
            });
    }

    /// 根据下一个延迟重绘时间更新 winit 控制流。
    fn update_control_flow(&self, event_loop: &ActiveEventLoop) {
        if let Some(next_repaint) = self.repaint_gate.next_repaint {
            event_loop.set_control_flow(ControlFlow::WaitUntil(next_repaint));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

impl ApplicationHandler<UserEvent> for DesktopApplication {
    /// 在 Windows 恢复应用时创建主画布、放映控件窗口和 GPU 资源。
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
            self.palm_size_preset.clone(),
            self.canvas_palm_capture.clone(),
        ) {
            Ok(runtime) => {
                self.install_repaint_callback(&runtime);
                self.runtime = Some(runtime);
                if let Some(runtime) = self.runtime.as_mut() {
                    if let Err(error) = runtime.show() {
                        self.startup_error = Some(error);
                        event_loop.exit();
                        return;
                    }
                    if self.repaint_gate.invalidate_now() {
                        runtime.request_redraw();
                    }
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
        let is_main_window = window_id == runtime.window_id();
        let is_slideshow_ui_window = window_id == runtime.slideshow_ui_window_id();
        if !is_main_window && !is_slideshow_ui_window {
            return;
        }
        if is_main_window && matches!(event, WindowEvent::CloseRequested) {
            self.exit_action = Some(ApplicationExitAction::Exit);
            event_loop.exit();
            return;
        }
        if matches!(event, WindowEvent::Destroyed) {
            event_loop.exit();
            return;
        }

        if is_main_window && matches!(event, WindowEvent::RedrawRequested) {
            if !self.repaint_gate.begin_redraw() {
                self.update_control_flow(event_loop);
                return;
            }
            match runtime.render() {
                Ok(outcome) => {
                    let request_redraw = self
                        .repaint_gate
                        .finish_redraw(outcome.submitted_generation, outcome.repaint);
                    self.in_flight_active = outcome.active_ack;
                    if request_redraw {
                        runtime.request_redraw();
                    }
                    if let Some(exit_action) = outcome.exit_action {
                        self.exit_action = Some(exit_action);
                        event_loop.exit();
                    } else {
                        self.update_control_flow(event_loop);
                    }
                }
                Err(error) => {
                    self.startup_error = Some(error);
                    event_loop.exit();
                }
            }
            return;
        }

        if is_slideshow_ui_window {
            if !matches!(event, WindowEvent::RedrawRequested)
                && runtime.handle_slideshow_ui_window_event(&event)
                && self.repaint_gate.invalidate_now()
            {
                runtime.request_redraw();
            }
            return;
        }

        match runtime.handle_window_event(&event) {
            Ok(true) if self.repaint_gate.invalidate_now() => runtime.request_redraw(),
            Ok(true) => {}
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
                self.repaint_gate.next_repaint = None;
                if self.repaint_gate.invalidate_now() {
                    runtime.request_redraw();
                }
            }
            UserEvent::ExternalEvent => {
                if runtime.process_slideshow_events() && self.repaint_gate.invalidate_now() {
                    runtime.request_redraw();
                }
                self.update_control_flow(event_loop);
            }
            UserEvent::WindowsPointer => {
                if runtime.process_windows_pointer_events() && self.repaint_gate.invalidate_now() {
                    runtime.request_redraw();
                }
                self.update_control_flow(event_loop);
            }
            UserEvent::Render => {
                let mut request_redraw = false;
                let mut terminal_generation = None;
                let mut presented_generation = None;
                let mut active_resync_requested = false;
                let repaint_gate = &mut self.repaint_gate;
                match runtime.process_render_events(
                    |generation, outcome| match repaint_gate.acknowledge(generation) {
                        Some(request) => {
                            request_redraw |= request;
                            terminal_generation = Some(generation);
                            if matches!(outcome, FrameTerminal::Presented) {
                                presented_generation = Some(generation);
                            }
                        }
                        None => tracing::warn!(
                            generation,
                            ?outcome,
                            in_flight = ?repaint_gate.in_flight,
                            "忽略不匹配的渲染终态 ACK"
                        ),
                    },
                    || active_resync_requested = true,
                ) {
                    Ok(changed) => {
                        if terminal_generation.is_some() {
                            let active_ack = self.in_flight_active.take();
                            if !active_resync_requested
                                && presented_generation.is_some()
                                && let Some(ack) = active_ack
                            {
                                runtime.acknowledge_active_stroke(ack);
                            }
                        }
                        if active_resync_requested {
                            runtime.request_active_stroke_resync();
                        }
                        if changed || active_resync_requested {
                            request_redraw |= self.repaint_gate.invalidate_now();
                        }
                        if request_redraw {
                            runtime.request_redraw();
                        }
                    }
                    Err(error) => {
                        self.startup_error = Some(error);
                        event_loop.exit();
                    }
                }
                self.update_control_flow(event_loop);
            }
            UserEvent::Recovery => {
                if runtime.process_recovery_events() && self.repaint_gate.invalidate_now() {
                    runtime.request_redraw();
                }
                self.update_control_flow(event_loop);
            }
            UserEvent::RequestRepaint(delay) => {
                let requested_time = Instant::now() + delay;
                self.repaint_gate.invalidate_at(requested_time);
                self.update_control_flow(event_loop);
            }
        }
    }

    /// 在 WaitUntil 到期时请求一次重绘，不进入持续轮询。
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            let now = Instant::now();
            if self.repaint_gate.invalidate_due(now)
                && let Some(runtime) = self.runtime.as_ref()
            {
                runtime.request_redraw();
            }
            self.update_control_flow(event_loop);
        }
    }

    /// 退出事件循环时按退出原因处理恢复文件并停止渲染线程。
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.shutdown(self.exit_action.is_some())
        {
            self.startup_error = Some(error);
        }
    }
}

/// 启动当前可执行文件的新实例，并转发原始命令行参数。
fn restart_current_executable() -> Result<(), AppError> {
    let executable = std::env::current_exe().map_err(|error| {
        AppError::Application(format!("无法定位当前可执行文件，不能重启: {error}"))
    })?;
    let child = Command::new(&executable)
        .args(std::env::args_os().skip(1))
        .spawn()
        .map_err(|error| {
            AppError::Application(format!("启动 {} 失败: {error}", executable.display()))
        })?;
    tracing::info!(
        path = %executable.display(),
        process_id = child.id(),
        "已启动 Steady Ink 重启实例"
    );
    Ok(())
}

/// 在运行或关闭无错误时判断是否需要启动重启实例。
fn restart_required_after_run(
    exit_action: Option<ApplicationExitAction>,
    runtime_error: Option<AppError>,
) -> Result<bool, AppError> {
    if let Some(error) = runtime_error {
        return Err(error);
    }
    Ok(exit_action == Some(ApplicationExitAction::Restart))
}

/// 创建 Windows 用户事件循环并运行单窗口应用。
pub fn run() -> Result<(), AppError> {
    let (windows_pointer_sender, windows_pointer_receiver) = mpsc::channel();
    let pen_contact_active = Arc::new(AtomicBool::new(false));
    let hook_pen_contact_active = Arc::clone(&pen_contact_active);
    let proxy_slot = Arc::new(OnceLock::<EventLoopProxy<UserEvent>>::new());
    let hook_proxy_slot = Arc::clone(&proxy_slot);
    let palm_size_preset = SharedPalmSizePreset::default();
    let canvas_palm_capture = SharedCanvasPalmCapture::default();
    let mut pointer_tracker = WindowsPointerTracker::with_palm_configuration(
        palm_size_preset.clone(),
        canvas_palm_capture.clone(),
    );
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
    let mut application = DesktopApplication::new(
        proxy,
        windows_pointer_receiver,
        pen_contact_active,
        palm_size_preset,
        canvas_palm_capture,
    );
    event_loop.run_app(&mut application)?;
    let exit_action = application.exit_action;
    let runtime_error = application.startup_error.take();
    drop(application);
    if restart_required_after_run(exit_action, runtime_error)? {
        restart_current_executable()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证繁忙期任意数量即时 invalidation 只保留一个 dirty 后继帧。
    #[test]
    fn repaint_gate_keeps_exactly_one_frame_in_flight() {
        let mut gate = RepaintGate::default();
        assert!(gate.invalidate_now());
        for _ in 0..1_000 {
            assert!(!gate.invalidate_now());
        }
        assert!(gate.begin_redraw());
        assert!(!gate.finish_redraw(Some(41), false));

        for _ in 0..1_000 {
            assert!(!gate.invalidate_now());
        }
        assert_eq!(gate.in_flight, Some(41));
        assert_eq!(gate.acknowledge(40), None);
        assert_eq!(gate.in_flight, Some(41));
        assert_eq!(gate.acknowledge(41), Some(true));
        assert_eq!(gate.acknowledge(41), None);

        assert!(gate.begin_redraw());
        assert!(!gate.finish_redraw(Some(42), false));
        assert_eq!(gate.acknowledge(41), None);
        assert_eq!(gate.in_flight, Some(42));
        assert_eq!(gate.acknowledge(42), Some(false));
        assert!(!gate.dirty);
    }

    /// 验证延迟、输入和诊断 invalidation 到期后合并为一个 redraw 请求。
    #[test]
    fn repaint_gate_coalesces_delayed_and_immediate_invalidations() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(20);
        let mut gate = RepaintGate::default();
        gate.invalidate_at(deadline + Duration::from_millis(10));
        gate.invalidate_at(deadline);

        assert!(!gate.invalidate_due(now));
        assert!(gate.invalidate_due(deadline));
        assert!(!gate.invalidate_now());
        assert!(!gate.invalidate_now());
        assert!(gate.begin_redraw());
        assert!(!gate.finish_redraw(Some(7), false));
        assert_eq!(gate.acknowledge(7), Some(false));
    }

    /// 验证未提交的几何 source frame 不占 credit，目标状态可立即请求重绘。
    #[test]
    fn repaint_gate_does_not_credit_geometry_source_frame() {
        let mut gate = RepaintGate::default();
        assert!(gate.invalidate_now());
        assert!(gate.begin_redraw());

        assert!(gate.finish_redraw(None, true));
        assert!(gate.in_flight.is_none());
        assert!(gate.begin_redraw());
        assert!(!gate.finish_redraw(Some(9), false));
        assert_eq!(gate.acknowledge(9), Some(false));
    }

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

    /// 验证 egui 区域按像素比例向外取整，完整覆盖边缘物理像素。
    #[test]
    fn hit_regions_expand_to_physical_pixel_boundaries() {
        let regions = physical_hit_regions(
            &[egui::Rect::from_min_max(
                egui::pos2(10.25, 20.5),
                egui::pos2(30.25, 40.5),
            )],
            1.5,
        );

        assert_eq!(
            regions,
            vec![PhysicalHitRect {
                min_x: 15,
                min_y: 30,
                max_x: 46,
                max_y: 61,
            }]
        );
    }

    /// 验证放映画笔按钮严格执行画笔、橡皮擦和触摸三种入口的状态表。
    #[test]
    fn slideshow_pen_button_follows_mode_transition_table() {
        assert_eq!(
            slideshow_pen_transition(SlideshowInputMode::Ink, InkTool::Pen),
            (SlideshowInputMode::Mouse, InkTool::Pen)
        );
        assert_eq!(
            slideshow_pen_transition(SlideshowInputMode::Ink, InkTool::RegionEraser),
            (SlideshowInputMode::Ink, InkTool::Pen)
        );
        assert_eq!(
            slideshow_pen_transition(SlideshowInputMode::Mouse, InkTool::RegionEraser),
            (SlideshowInputMode::Ink, InkTool::Pen)
        );
    }

    /// 验证只有活动放映的 Mouse 模式启用原生命中穿透。
    #[test]
    fn native_pass_through_requires_slideshow_mouse_mode() {
        assert!(slideshow_mouse_hit_test_enabled(
            AppMode::SlideShowAnnotatingExpanded,
            SlideshowInputMode::Mouse
        ));
        assert!(!slideshow_mouse_hit_test_enabled(
            AppMode::SlideShowAnnotatingExpanded,
            SlideshowInputMode::Ink
        ));
        assert!(!slideshow_mouse_hit_test_enabled(
            AppMode::IdleFloatingToolbar,
            SlideshowInputMode::Mouse
        ));
    }

    /// 验证启发式手掌门只在可接收墨迹的普通或放映画布模式开启。
    #[test]
    fn canvas_palm_capture_gate_matches_runtime_input_mode() {
        assert!(!canvas_palm_capture_enabled(
            AppMode::IdleFloatingToolbar,
            SlideshowInputMode::Ink
        ));
        assert!(canvas_palm_capture_enabled(
            AppMode::NormalAnnotating,
            SlideshowInputMode::Ink
        ));
        assert!(canvas_palm_capture_enabled(
            AppMode::SlideShowAnnotatingExpanded,
            SlideshowInputMode::Ink
        ));
        assert!(!canvas_palm_capture_enabled(
            AppMode::SlideShowAnnotatingCollapsed,
            SlideshowInputMode::Mouse
        ));
        assert!(canvas_palm_capture_enabled(
            AppMode::SlideShowConnectionLost,
            SlideshowInputMode::Ink
        ));
    }

    /// 验证只有会改变原生窗口尺寸的命令走目标几何重绘路径。
    #[test]
    fn geometry_commands_are_classified_for_target_redraw() {
        for command in [
            UiCommand::EnterAnnotation,
            UiCommand::ExitAnnotation,
            UiCommand::OpenSettings,
            UiCommand::CloseSettings,
            UiCommand::ToggleQuickSettings,
            UiCommand::ConfirmDismissSlideshow,
        ] {
            assert!(ui_command_changes_window_geometry(command));
        }
        for command in [
            UiCommand::SelectPen,
            UiCommand::Undo,
            UiCommand::ToggleSlideshowToolbar,
            UiCommand::ExitApplication,
        ] {
            assert!(!ui_command_changes_window_geometry(command));
        }
    }

    /// 验证只有显式重启动作会在成功关闭后请求拉起新实例。
    #[test]
    fn restart_requires_explicit_restart_action() {
        assert!(
            restart_required_after_run(Some(ApplicationExitAction::Restart), None)
                .expect("成功关闭后应接受重启动作")
        );
        assert!(
            !restart_required_after_run(Some(ApplicationExitAction::Exit), None)
                .expect("普通退出应保持成功")
        );
        assert!(!restart_required_after_run(None, None).expect("异常退出路径不应伪造重启"));
    }

    /// 验证运行或关闭错误优先返回并阻止已经记录的重启动作。
    #[test]
    fn runtime_error_prevents_restart() {
        let error = restart_required_after_run(
            Some(ApplicationExitAction::Restart),
            Some(AppError::Application("关闭失败".to_owned())),
        )
        .expect_err("关闭失败时不得启动新实例");

        assert_eq!(error.to_string(), "应用进程操作失败: 关闭失败");
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
            1,
        )
        .expect("non-empty batch must start a gesture");
        let ActiveGestureSamples::Tool { points } = gesture.samples else {
            panic!("tool gesture must retain point samples");
        };

        assert_eq!(
            points,
            vec![CanvasPoint::new(0.0, 0.0), CanvasPoint::new(1.0, 0.0)]
        );
    }

    /// 验证活动增量从未确认游标开始，并且 Presented 后才前移游标。
    #[test]
    fn active_stroke_cursor_advances_only_after_matching_presented_ack() {
        let mut gesture = ActiveGesture::new(
            PointerSample::new(CanvasPoint::new(0.0, 0.0), 0),
            ToolState::default(),
            17,
        );
        let first = gesture
            .active_stroke_delta()
            .expect("Begin 应携带首个活动采样");
        assert_eq!(first.from_sample, 0);
        assert_eq!(first.samples.len(), 1);

        gesture.acknowledge(ActiveStrokeAck {
            gesture_id: 99,
            revision: first.revision,
            sample_count: 1,
            full_resync: false,
        });
        assert_eq!(gesture.active_stroke_delta().unwrap().from_sample, 0);

        gesture.acknowledge(ActiveStrokeAck {
            gesture_id: 17,
            revision: first.revision,
            sample_count: 1,
            full_resync: false,
        });
        let repeated = gesture
            .active_stroke_delta()
            .expect("没有新采样时仍需携带空增量保持活动 surface");
        assert_eq!(repeated.from_sample, 1);
        assert!(repeated.samples.is_empty());

        gesture.push_point(PointerSample::new(CanvasPoint::new(4.0, 0.0), 1));
        let next = gesture.active_stroke_delta().expect("新增采样应出现在尾部");
        assert_eq!(next.from_sample, 1);
        assert_eq!(next.samples, vec![CanvasPoint::new(4.0, 0.0)]);
    }

    /// 验证 resize/cache-loss 后持续从 sample zero 重发，直到 full resync 被 Presented。
    #[test]
    fn active_stroke_resize_resync_repeats_sample_zero_until_presented() {
        let mut gesture = ActiveGesture::new(
            PointerSample::new(CanvasPoint::new(0.0, 0.0), 0),
            ToolState::default(),
            23,
        );
        gesture.push_point(PointerSample::new(CanvasPoint::new(4.0, 0.0), 1));
        gesture.acknowledge(ActiveStrokeAck {
            gesture_id: 23,
            revision: gesture.revision,
            sample_count: 2,
            full_resync: false,
        });
        assert_eq!(gesture.active_stroke_delta().unwrap().from_sample, 2);

        gesture.request_full_resync();
        let first_resync = gesture.active_stroke_delta().unwrap();
        assert!(first_resync.full_resync);
        assert_eq!(first_resync.from_sample, 0);
        assert_eq!(first_resync.samples.len(), 2);

        let discarded = ActiveStrokeAck {
            gesture_id: 23,
            revision: first_resync.revision,
            sample_count: 2,
            full_resync: true,
        };
        let repeated = gesture.active_stroke_delta().unwrap();
        assert!(repeated.full_resync);
        assert_eq!(repeated.from_sample, 0);

        gesture.acknowledge(discarded);
        let after_presented = gesture.active_stroke_delta().unwrap();
        assert!(!after_presented.full_resync);
        assert_eq!(after_presented.from_sample, 2);
    }

    /// 验证合成慢于输入时普通画面始终保持单 credit，dirty 只生成一个后继帧。
    #[test]
    fn synthetic_slow_renderer_never_exceeds_single_flight_credit() {
        let mut gate = RepaintGate::default();
        let mut latest_state = 0_u64;
        let mut generated = 0_u64;
        let mut presented = 0_u64;
        let mut max_in_flight = 0_usize;

        assert!(gate.invalidate_now());
        assert!(gate.begin_redraw());
        generated += 1;
        assert!(!gate.finish_redraw(Some(generated), false));
        max_in_flight = max_in_flight.max(usize::from(gate.in_flight.is_some()));

        // 代表渲染线程在一次 10ms paint/present 期间收到的高频最新状态。
        for state in 1..=1_000 {
            latest_state = state;
            assert!(!gate.invalidate_now());
            max_in_flight = max_in_flight.max(usize::from(gate.in_flight.is_some()));
        }
        presented += 1;
        assert_eq!(gate.acknowledge(generated), Some(true));
        assert!(gate.begin_redraw());
        generated += 1;
        assert!(!gate.finish_redraw(Some(generated), false));

        assert_eq!(latest_state, 1_000);
        assert_eq!(generated, presented + 1);
        assert_eq!(max_in_flight, 1);
    }

    /// 验证固定宽度预览在急转弯时严格结束于最后一个真实采样。
    #[test]
    fn fixed_preview_has_no_extrapolated_tail() {
        let mut gesture = ActiveGesture::from_points(
            vec![
                PointerSample::new(CanvasPoint::new(0.0, 0.0), 0),
                PointerSample::new(CanvasPoint::new(8.0, 0.0), 8_000),
                PointerSample::new(CanvasPoint::new(8.0, 8.0), 16_000),
            ],
            ToolState::default(),
            2,
        )
        .expect("有效批次应创建活动手势");

        let ActiveInkPreview::Tool { points, .. } = gesture.preview() else {
            panic!("固定宽度画笔应生成固定预览");
        };
        assert_eq!(points.len(), 3);
        assert_eq!(points.last(), Some(&CanvasPoint::new(8.0, 8.0)));

        let mut document = InkDocument::new();
        gesture.commit(&mut document);
        let [InkOperation::DrawStroke(stroke)] = document.operations() else {
            panic!("提交应生成一条画笔操作");
        };
        let crate::ink::DrawStrokeShape::Fixed { points, .. } = &stroke.shape else {
            panic!("默认画笔应提交固定宽度几何");
        };
        assert_eq!(points.len(), 3);
        assert_eq!(points.last(), Some(&CanvasPoint::new(8.0, 8.0)));
    }

    /// 验证自然笔锋预览不外推尾段，且提交使用完全相同的变量点。
    #[test]
    fn natural_taper_preview_matches_committed_points() {
        let tools = ToolState {
            natural_taper_enabled: true,
            ..ToolState::default()
        };
        let mut gesture = ActiveGesture::from_points(
            vec![
                PointerSample::new(CanvasPoint::new(0.0, 0.0), 0),
                PointerSample::new(CanvasPoint::new(8.0, 0.0), 8_000),
                PointerSample::new(CanvasPoint::new(8.0, 8.0), 16_000),
            ],
            tools,
            3,
        )
        .expect("有效批次应创建活动手势");
        let ActiveInkPreview::VariableTool { points, .. } = gesture.preview() else {
            panic!("自然笔锋应生成可变宽度预览");
        };
        let preview_points = points.to_vec();
        assert_eq!(
            preview_points.last().map(|point| point.point),
            Some(CanvasPoint::new(8.0, 8.0))
        );

        let mut document = InkDocument::new();
        assert!(gesture.commit(&mut document));
        let [InkOperation::DrawStroke(stroke)] = document.operations() else {
            panic!("提交应生成一条画笔操作");
        };
        let crate::ink::DrawStrokeShape::Variable { points } = &stroke.shape else {
            panic!("自然笔锋应提交可变宽度几何");
        };
        assert_eq!(points, &preview_points);
    }

    /// 验证相同几何路径的自然笔锋不受采样时间戳变化影响。
    #[test]
    fn natural_taper_ignores_pointer_timestamps() {
        let tools = ToolState {
            natural_taper_enabled: true,
            ..ToolState::default()
        };
        let positions = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(8.0, 0.0),
            CanvasPoint::new(16.0, 8.0),
            CanvasPoint::new(32.0, 8.0),
        ];
        let mut first = ActiveGesture::from_points(
            positions
                .into_iter()
                .enumerate()
                .map(|(index, point)| PointerSample::new(point, index as u64 * 1_000))
                .collect(),
            tools,
            4,
        )
        .expect("第一组采样应创建活动手势");
        let mut second = ActiveGesture::from_points(
            positions
                .into_iter()
                .enumerate()
                .map(|(index, point)| PointerSample::new(point, index as u64 * 100_000 + 7))
                .collect(),
            tools,
            5,
        )
        .expect("第二组采样应创建活动手势");

        let ActiveInkPreview::VariableTool {
            points: first_points,
            ..
        } = first.preview()
        else {
            panic!("第一组采样应生成自然笔锋");
        };
        let ActiveInkPreview::VariableTool {
            points: second_points,
            ..
        } = second.preview()
        else {
            panic!("第二组采样应生成自然笔锋");
        };
        assert_eq!(first_points, second_points);
    }
}
