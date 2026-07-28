use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Instant,
};

use winit::dpi::PhysicalSize;

use super::{Compositor, EguiFrame};
use crate::{
    error::AppError,
    ink::{InkBounds, InkDocument, InkOperation, InkSyncKind, OwnedActiveInkPreview},
    performance::{
        PerformanceFrameSample, PerformanceInkSync, PerformanceMonitor, PerformanceSnapshot,
        PerformanceSnapshotReader,
    },
    window::{D3DRenderContext, D3DRenderTarget, GraphicsDiagnostics},
};

/// 事件线程仅在用户开启监控时附加的一帧时间事实。
#[derive(Debug, Clone, Copy)]
pub struct RenderPerformanceMetadata {
    pub submitted_at: Instant,
    pub input_started_at: Option<Instant>,
}

/// 事件线程提交给渲染线程的完整 owned 画面快照。
pub struct RenderFrame {
    pub generation: u64,
    pub document: InkDocument,
    pub active_preview: Option<OwnedActiveInkPreview>,
    pub egui: EguiFrame,
    pub performance: Option<RenderPerformanceMetadata>,
}

/// 渲染线程异步返回给事件线程的状态变化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderEvent {
    InkRenderingError(Option<String>),
    GraphicsDiagnostics(GraphicsDiagnostics),
    Fatal(String),
}

/// 必须按事件线程提交顺序执行的渲染控制命令。
enum RenderControl {
    Resize(PhysicalSize<u32>),
    SetAnnotationResourcesEnabled(bool),
    InvalidateInk,
    InvalidateInkRegion(InkBounds),
    Shutdown,
}

/// 一个批次中需要先执行的控制命令和可选最新帧。
struct RenderWork {
    controls: VecDeque<RenderControl>,
    frame: Option<RenderFrame>,
}

/// latest-frame 邮箱的互斥状态。
#[derive(Default)]
struct MailboxState {
    controls: VecDeque<RenderControl>,
    frame: Option<RenderFrame>,
    skipped_texture_deltas: Vec<egui::TexturesDelta>,
    closed: bool,
}

/// 以 Condvar 唤醒消费者、以单槽位合并过期画面的 mailbox。
#[derive(Default)]
struct RenderMailbox {
    state: Mutex<MailboxState>,
    ready: Condvar,
}

impl RenderMailbox {
    /// 无阻塞地替换待渲染画面，并保留纹理命令和最早关联输入。
    fn submit_frame(&self, frame: RenderFrame) {
        let mut state = self.state.lock().expect("渲染邮箱互斥量不应中毒");
        if state.closed {
            return;
        }
        if let Some(skipped) = state.frame.replace(frame) {
            let skipped_input = skipped
                .performance
                .and_then(|metadata| metadata.input_started_at);
            state
                .skipped_texture_deltas
                .extend(skipped.egui.texture_deltas);
            if let Some(metadata) = state
                .frame
                .as_mut()
                .and_then(|frame| frame.performance.as_mut())
            {
                metadata.input_started_at = match (metadata.input_started_at, skipped_input) {
                    (Some(current), Some(skipped)) => Some(current.min(skipped)),
                    (current, skipped) => current.or(skipped),
                };
            }
        }
        self.ready.notify_one();
    }

    /// 追加控制命令；连续 resize 和资源驻留切换只保留最后一次目标。
    fn submit_control(&self, control: RenderControl) {
        let mut state = self.state.lock().expect("渲染邮箱互斥量不应中毒");
        if state.closed {
            return;
        }
        let coalesced = match (&mut state.controls.back_mut(), &control) {
            (Some(RenderControl::Resize(current)), RenderControl::Resize(next)) => {
                *current = *next;
                true
            }
            (
                Some(RenderControl::SetAnnotationResourcesEnabled(current)),
                RenderControl::SetAnnotationResourcesEnabled(next),
            ) => {
                *current = *next;
                true
            }
            _ => false,
        };
        if !coalesced {
            state.controls.push_back(control);
        }
        self.ready.notify_one();
    }

    /// 丢弃与新尺寸不匹配的画面并排队最新 resize。
    fn submit_resize(&self, size: PhysicalSize<u32>) {
        let mut state = self.state.lock().expect("渲染邮箱互斥量不应中毒");
        if state.closed {
            return;
        }
        if let Some(stale) = state.frame.take() {
            state
                .skipped_texture_deltas
                .extend(stale.egui.texture_deltas);
        }
        if let Some(RenderControl::Resize(current)) = state.controls.back_mut() {
            *current = size;
        } else {
            state.controls.push_back(RenderControl::Resize(size));
        }
        self.ready.notify_one();
    }

    /// 阻塞到至少一个控制命令或画面可用，并一次取走当前工作批次。
    fn wait_for_work(&self) -> RenderWork {
        let mut state = self.state.lock().expect("渲染邮箱互斥量不应中毒");
        while state.controls.is_empty() && state.frame.is_none() && !state.closed {
            state = self.ready.wait(state).expect("渲染邮箱互斥量不应中毒");
        }
        let mut frame = state.frame.take();
        if let Some(frame) = frame.as_mut()
            && !state.skipped_texture_deltas.is_empty()
        {
            let mut deltas = std::mem::take(&mut state.skipped_texture_deltas);
            deltas.append(&mut frame.egui.texture_deltas);
            frame.egui.texture_deltas = deltas;
        }
        RenderWork {
            controls: std::mem::take(&mut state.controls),
            frame,
        }
    }

    /// 令后续生产者立即返回，并唤醒等待中的消费者。
    fn close(&self) {
        let mut state = self.state.lock().expect("渲染邮箱互斥量不应中毒");
        state.closed = true;
        self.ready.notify_all();
    }
}

/// 管理渲染线程、latest-frame mailbox 和结果通道的事件线程句柄。
pub struct RenderThread {
    mailbox: Arc<RenderMailbox>,
    events: mpsc::Receiver<RenderEvent>,
    join: Option<JoinHandle<()>>,
    diagnostics: GraphicsDiagnostics,
    initial_ink_error: Option<String>,
    performance: PerformanceSnapshotReader,
}

impl RenderThread {
    /// 启动渲染线程并同步等待 D3D12、DirectComposition 与 Skia 初始化结果。
    pub fn spawn(
        target: D3DRenderTarget,
        egui_context: egui::Context,
        wake_event_loop: impl Fn() + Send + 'static,
    ) -> Result<Self, AppError> {
        let mailbox = Arc::new(RenderMailbox::default());
        let worker_mailbox = Arc::clone(&mailbox);
        let performance_monitor = PerformanceMonitor::new();
        let performance = performance_monitor.snapshot_reader();
        let (events_tx, events) = mpsc::channel();
        let (initialized_tx, initialized_rx) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name("steady-ink-render".to_owned())
            .spawn(move || {
                run_render_thread(
                    target,
                    egui_context,
                    worker_mailbox,
                    events_tx,
                    initialized_tx,
                    performance_monitor,
                    &wake_event_loop,
                );
            })
            .map_err(|error| AppError::Graphics(format!("无法启动渲染线程: {error}")))?;
        let (diagnostics, initial_ink_error) = initialized_rx
            .recv()
            .map_err(|_| AppError::Graphics("渲染线程在初始化完成前退出".to_owned()))?
            .map_err(AppError::Graphics)?;
        Ok(Self {
            mailbox,
            events,
            join: Some(join),
            diagnostics,
            initial_ink_error,
            performance,
        })
    }

    /// 返回渲染线程初始化时采集的图形设备诊断。
    pub const fn diagnostics(&self) -> &GraphicsDiagnostics {
        &self.diagnostics
    }

    /// 返回 compositor 初始化时发生的非致命墨迹增强降级错误。
    pub fn initial_ink_error(&self) -> Option<&str> {
        self.initial_ink_error.as_deref()
    }

    /// 复制渲染线程最新发布的固定大小性能快照。
    pub fn performance_snapshot(&self) -> PerformanceSnapshot {
        self.performance.snapshot()
    }

    /// 提交最新 owned frame；若旧帧尚未消费则只保留其纹理命令。
    pub fn submit_frame(&self, frame: RenderFrame) {
        self.mailbox.submit_frame(frame);
    }

    /// 请求在下一帧前调整 swap chain 和全部 Skia surface。
    pub fn resize(&self, size: PhysicalSize<u32>) {
        self.mailbox.submit_resize(size);
    }

    /// 请求切换批注模式下的大型 GPU 资源驻留策略。
    pub fn set_annotation_resources_enabled(&self, enabled: bool) {
        self.mailbox
            .submit_control(RenderControl::SetAnnotationResourcesEnabled(enabled));
    }

    /// 请求从权威文档历史完整重建墨迹缓存。
    pub fn invalidate_ink_cache(&self) {
        self.mailbox.submit_control(RenderControl::InvalidateInk);
    }

    /// 请求只重建指定墨迹区域。
    pub fn invalidate_ink_region(&self, bounds: InkBounds) {
        self.mailbox
            .submit_control(RenderControl::InvalidateInkRegion(bounds));
    }

    /// 非阻塞读取一个渲染诊断或 fatal error。
    pub fn try_recv_event(&mut self) -> Option<RenderEvent> {
        let event = self.events.try_recv().ok()?;
        if let RenderEvent::GraphicsDiagnostics(diagnostics) = &event {
            self.diagnostics.clone_from(diagnostics);
        }
        Some(event)
    }

    /// 排队 shutdown 并等待 GPU 对象在所属线程内销毁。
    pub fn shutdown(&mut self) -> Result<(), AppError> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        self.mailbox.submit_control(RenderControl::Shutdown);
        let result = join
            .join()
            .map_err(|_| AppError::Graphics("渲染线程异常终止".to_owned()));
        self.mailbox.close();
        result
    }
}

impl Drop for RenderThread {
    /// 兜底停止并回收渲染线程，避免窗口销毁后仍访问 HWND。
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// 在线程内创建全部 GPU 状态并持续处理控制命令和最新帧。
fn run_render_thread(
    target: D3DRenderTarget,
    egui_context: egui::Context,
    mailbox: Arc<RenderMailbox>,
    events: mpsc::Sender<RenderEvent>,
    initialized: mpsc::SyncSender<Result<(GraphicsDiagnostics, Option<String>), String>>,
    mut performance_monitor: PerformanceMonitor,
    wake_event_loop: &impl Fn(),
) {
    let initialization = D3DRenderContext::new(target).and_then(|context| {
        let (compositor, initial_ink_error) = Compositor::new(&context, egui_context)?;
        let diagnostics = context.diagnostics_snapshot();
        Ok((context, compositor, diagnostics, initial_ink_error))
    });
    let (mut window_context, mut compositor, diagnostics, mut last_ink_error) = match initialization
    {
        Ok(initialized_state) => initialized_state,
        Err(error) => {
            let _ = initialized.send(Err(error.to_string()));
            mailbox.close();
            return;
        }
    };
    let mut last_diagnostics = diagnostics.clone();
    if initialized
        .send(Ok((diagnostics, last_ink_error.clone())))
        .is_err()
    {
        mailbox.close();
        return;
    }

    loop {
        let work = mailbox.wait_for_work();
        let mut shutdown = false;
        for control in work.controls {
            let result = match control {
                RenderControl::Resize(size) => compositor.resize(&mut window_context, size.into()),
                RenderControl::SetAnnotationResourcesEnabled(enabled) => {
                    compositor.set_annotation_resources_enabled(enabled)
                }
                RenderControl::InvalidateInk => {
                    compositor.invalidate_ink_cache();
                    Ok(())
                }
                RenderControl::InvalidateInkRegion(bounds) => {
                    compositor.invalidate_ink_region(bounds);
                    Ok(())
                }
                RenderControl::Shutdown => {
                    shutdown = true;
                    Ok(())
                }
            };
            if let Err(error) = result {
                send_render_event(
                    &events,
                    RenderEvent::Fatal(error.to_string()),
                    wake_event_loop,
                );
                mailbox.close();
                return;
            }
            let diagnostics = window_context.diagnostics_snapshot();
            if diagnostics != last_diagnostics {
                last_diagnostics.clone_from(&diagnostics);
                send_render_event(
                    &events,
                    RenderEvent::GraphicsDiagnostics(diagnostics),
                    wake_event_loop,
                );
            }
            if shutdown {
                mailbox.close();
                return;
            }
        }

        if let Some(frame) = work.frame {
            let performance_metadata = frame.performance;
            let monitoring_started =
                performance_monitor.set_enabled(performance_metadata.is_some());
            let render_started_at = performance_metadata.map(|_| Instant::now());
            let generation = frame.generation;
            let preview = frame
                .active_preview
                .as_ref()
                .map(OwnedActiveInkPreview::as_borrowed);
            let result = compositor
                .paint(&window_context, &frame.document, preview, frame.egui)
                .and_then(|ink_sync| window_context.present().map(|()| ink_sync));
            let ink_sync = match result {
                Ok(ink_sync) => ink_sync,
                Err(error) => {
                    send_render_event(
                        &events,
                        RenderEvent::Fatal(error.to_string()),
                        wake_event_loop,
                    );
                    mailbox.close();
                    return;
                }
            };
            if let (Some(metadata), Some(render_started_at)) =
                (performance_metadata, render_started_at)
            {
                let presented_at = Instant::now();
                let count_document = monitoring_started || ink_sync != InkSyncKind::Unchanged;
                let (visible_strokes, visible_operations) = count_document
                    .then(|| visible_ink_counts(&frame.document))
                    .map_or((None, None), |(strokes, operations)| {
                        (Some(strokes), Some(operations))
                    });
                let frame_time = presented_at.saturating_duration_since(metadata.submitted_at);
                let render_time = presented_at.saturating_duration_since(render_started_at);
                let input_latency = metadata
                    .input_started_at
                    .map(|started_at| presented_at.saturating_duration_since(started_at));
                let managed_gpu_bytes = compositor.estimated_managed_gpu_bytes(&window_context);
                let performance_ink_sync = performance_ink_sync(ink_sync);
                let slow_frame = performance_monitor.record_frame(PerformanceFrameSample {
                    presented_at,
                    frame_time,
                    render_time,
                    input_latency,
                    visible_strokes,
                    visible_operations,
                    ink_sync: performance_ink_sync,
                    managed_gpu_bytes,
                });
                if slow_frame {
                    let snapshot = performance_monitor.snapshot();
                    tracing::warn!(
                        generation,
                        frame_time_micros = frame_time.as_micros(),
                        render_time_micros = render_time.as_micros(),
                        input_latency_micros = ?input_latency.map(|duration| duration.as_micros()),
                        visible_strokes = snapshot.visible_strokes(),
                        visible_operations = snapshot.visible_operations(),
                        ink_sync = ?performance_ink_sync,
                        managed_gpu_bytes,
                        "检测到异常渲染帧"
                    );
                }
            }
            let ink_error = compositor.ink_rendering_error().map(str::to_owned);
            if ink_error != last_ink_error {
                last_ink_error.clone_from(&ink_error);
                send_render_event(
                    &events,
                    RenderEvent::InkRenderingError(ink_error),
                    wake_event_loop,
                );
            }
        }
    }
}

/// 统计最近一次清屏后的可见画笔数和全部操作数。
fn visible_ink_counts(document: &InkDocument) -> (usize, usize) {
    let operations = document.replay_operations();
    let strokes = operations
        .iter()
        .filter(|operation| matches!(operation, InkOperation::DrawStroke(_)))
        .count();
    (strokes, operations.len())
}

/// 把墨迹域同步事实映射为稳定的性能域分类。
const fn performance_ink_sync(sync: InkSyncKind) -> PerformanceInkSync {
    match sync {
        InkSyncKind::Unchanged => PerformanceInkSync::Unchanged,
        InkSyncKind::Incremental => PerformanceInkSync::Incremental,
        InkSyncKind::RegionRebuild => PerformanceInkSync::RegionRebuild,
        InkSyncKind::FullRebuild => PerformanceInkSync::FullRebuild,
    }
}

/// 发送渲染结果后唤醒等待型 winit 事件循环。
fn send_render_event(
    events: &mpsc::Sender<RenderEvent>,
    event: RenderEvent,
    wake_event_loop: &impl Fn(),
) {
    if events.send(event).is_ok() {
        wake_event_loop();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use egui::{TextureId, TexturesDelta};

    use super::*;
    use crate::ink::{CanvasPoint, InkColor, PenWidth};

    /// 创建只携带 generation 和纹理释放标识的测试帧。
    fn test_frame(generation: u64, texture: u64) -> RenderFrame {
        RenderFrame {
            generation,
            document: InkDocument::new(),
            active_preview: None,
            egui: EguiFrame {
                shapes: Vec::new(),
                pixels_per_point: 1.0,
                texture_deltas: vec![TexturesDelta {
                    set: Vec::new(),
                    free: vec![TextureId::Managed(texture)],
                }],
            },
            performance: None,
        }
    }

    /// 编译期锁定所有跨线程帧 payload 的 Send 契约。
    #[test]
    fn render_frame_payload_is_send() {
        fn assert_send<T: Send>() {}

        assert_send::<D3DRenderTarget>();
        assert_send::<egui::Context>();
        assert_send::<EguiFrame>();
        assert_send::<RenderFrame>();
    }

    /// 验证只保留最新画面，同时保留纹理命令和最早关联输入。
    #[test]
    fn latest_frame_replaces_stale_frames_without_losing_texture_deltas() {
        let mailbox = RenderMailbox::default();
        let submitted_at = Instant::now();
        let earliest_input = submitted_at - Duration::from_millis(6);
        let mut first = test_frame(1, 11);
        first.performance = Some(RenderPerformanceMetadata {
            submitted_at: submitted_at - Duration::from_millis(4),
            input_started_at: Some(earliest_input),
        });
        mailbox.submit_frame(first);
        let mut second = test_frame(2, 22);
        second.performance = Some(RenderPerformanceMetadata {
            submitted_at: submitted_at - Duration::from_millis(2),
            input_started_at: None,
        });
        mailbox.submit_frame(second);
        let mut latest = test_frame(3, 33);
        latest.performance = Some(RenderPerformanceMetadata {
            submitted_at,
            input_started_at: Some(submitted_at - Duration::from_millis(2)),
        });
        mailbox.submit_frame(latest);

        let work = mailbox.wait_for_work();
        let frame = work.frame.expect("最新画面应可消费");
        let textures: Vec<_> = frame
            .egui
            .texture_deltas
            .iter()
            .flat_map(|delta| delta.free.iter())
            .copied()
            .collect();

        assert_eq!(frame.generation, 3);
        assert_eq!(
            frame
                .performance
                .expect("最新画面的性能 metadata 应保留")
                .submitted_at,
            submitted_at
        );
        assert_eq!(
            frame
                .performance
                .expect("被覆盖画面的输入起点应合并到最终画面")
                .input_started_at,
            Some(earliest_input)
        );
        assert_eq!(
            textures,
            vec![
                TextureId::Managed(11),
                TextureId::Managed(22),
                TextureId::Managed(33)
            ]
        );
    }

    /// 验证 resize 丢弃旧尺寸画面、合并连续尺寸并保留纹理 delta。
    #[test]
    fn resize_discards_stale_frame_and_coalesces_size() {
        let mailbox = RenderMailbox::default();
        mailbox.submit_frame(test_frame(1, 11));
        mailbox.submit_resize(PhysicalSize::new(800, 600));
        mailbox.submit_resize(PhysicalSize::new(1280, 720));
        mailbox.submit_frame(test_frame(2, 22));

        let work = mailbox.wait_for_work();
        let controls: Vec<_> = work.controls.into_iter().collect();
        assert!(matches!(
            controls.as_slice(),
            [RenderControl::Resize(size)] if *size == PhysicalSize::new(1280, 720)
        ));
        let frame = work.frame.expect("resize 后的新画面应保留");
        assert_eq!(frame.egui.texture_deltas.len(), 2);
    }

    /// 验证慢合成负载下事件线程提交 p95 至少比同步执行低 50%。
    #[test]
    fn latest_frame_submission_beats_synchronous_rendering_by_half() {
        const SAMPLE_COUNT: usize = 24;
        const SYNTHETIC_RENDER_TIME: Duration = Duration::from_millis(10);

        let synchronous: Vec<_> = (0..SAMPLE_COUNT)
            .map(|_| {
                let started = Instant::now();
                thread::sleep(SYNTHETIC_RENDER_TIME);
                started.elapsed()
            })
            .collect();
        let mailbox = Arc::new(RenderMailbox::default());
        let consumer_mailbox = Arc::clone(&mailbox);
        let consumer = thread::spawn(move || {
            loop {
                let work = consumer_mailbox.wait_for_work();
                if work
                    .controls
                    .iter()
                    .any(|control| matches!(control, RenderControl::Shutdown))
                {
                    return;
                }
                if work.frame.is_some() {
                    thread::sleep(SYNTHETIC_RENDER_TIME);
                }
            }
        });
        let asynchronous: Vec<_> = (0..SAMPLE_COUNT)
            .map(|generation| {
                let started = Instant::now();
                mailbox.submit_frame(test_frame(generation as u64, generation as u64));
                started.elapsed()
            })
            .collect();
        mailbox.submit_control(RenderControl::Shutdown);
        consumer.join().expect("慢消费者线程应正常退出");

        assert!(percentile_95(&asynchronous) * 2 < percentile_95(&synchronous));
    }

    /// 验证性能统计只计算最近一次清屏后的可见画笔和操作。
    #[test]
    fn visible_counts_follow_replay_operations() {
        let mut document = InkDocument::new();
        document.append_draw_stroke(
            vec![CanvasPoint::new(0.0, 0.0), CanvasPoint::new(4.0, 4.0)],
            InkColor::Red,
            PenWidth::Px4,
        );
        document.append_draw_stroke(
            vec![CanvasPoint::new(8.0, 8.0), CanvasPoint::new(12.0, 12.0)],
            InkColor::Blue,
            PenWidth::Px6,
        );
        document.clear();
        document.append_draw_stroke(
            vec![CanvasPoint::new(16.0, 16.0), CanvasPoint::new(20.0, 20.0)],
            InkColor::Black,
            PenWidth::Px8,
        );

        assert_eq!(visible_ink_counts(&document), (1, 1));
    }

    /// 返回一组测试耗时的最近秩 p95。
    fn percentile_95(samples: &[Duration]) -> Duration {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let index = ((sorted.len() * 95).div_ceil(100)).saturating_sub(1);
        sorted[index]
    }
}
