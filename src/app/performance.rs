use std::time::{Duration, Instant};

const REPORT_INTERVAL: Duration = Duration::from_secs(5);
const METRICS_ENVIRONMENT_VARIABLE: &str = "STEADY_INK_METRICS";
const PERCENTILE_50: usize = 50;
const PERCENTILE_95: usize = 95;

/// 触发一次窗口重绘请求的运行时来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RedrawReason {
    Startup,
    WindowEvent,
    Egui,
    UiCommand,
    PointerInput,
    SlideShow,
    AnimationTimer,
}

impl RedrawReason {
    const COUNT: usize = 7;

    /// 返回该来源在固定计数数组中的索引。
    const fn index(self) -> usize {
        match self {
            Self::Startup => 0,
            Self::WindowEvent => 1,
            Self::Egui => 2,
            Self::UiCommand => 3,
            Self::PointerInput => 4,
            Self::SlideShow => 5,
            Self::AnimationTimer => 6,
        }
    }
}

/// 可选的低开销运行时性能采样器，用于目标 4K 核显设备验收。
pub(super) struct PerformanceTracker {
    enabled: bool,
    report_started_at: Instant,
    frame_durations: Vec<Duration>,
    input_latencies: Vec<Duration>,
    pending_input_at: Option<Instant>,
    frame_count: u64,
    pointer_batch_count: u64,
    pointer_sample_count: u64,
    surface_rebuild_count: u64,
    redraw_counts: [u64; RedrawReason::COUNT],
}

/// 一次已完成 Present 对应的帧耗时和可选输入到显示延迟。
#[derive(Debug, Clone, Copy)]
pub(super) struct FrameSample {
    pub frame_duration: Duration,
    pub input_to_display: Option<Duration>,
}

impl PerformanceTracker {
    /// 根据环境变量创建默认关闭的性能采样器。
    pub(super) fn new(force_enabled: bool) -> Self {
        let enabled = force_enabled
            || std::env::var(METRICS_ENVIRONMENT_VARIABLE)
                .ok()
                .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true"));
        Self::with_enabled(enabled)
    }

    /// 使用已解析的开关创建采样器，供启动路径和纯逻辑测试复用。
    fn with_enabled(enabled: bool) -> Self {
        if enabled {
            tracing::info!(
                report_interval_seconds = REPORT_INTERVAL.as_secs(),
                "已启用 Steady Ink 性能统计"
            );
        }
        Self {
            enabled,
            report_started_at: Instant::now(),
            frame_durations: Vec::new(),
            input_latencies: Vec::new(),
            pending_input_at: None,
            frame_count: 0,
            pointer_batch_count: 0,
            pointer_sample_count: 0,
            surface_rebuild_count: 0,
            redraw_counts: [0; RedrawReason::COUNT],
        }
    }

    /// 记录一次重绘请求来源。
    pub(super) fn record_redraw(&mut self, reason: RedrawReason) {
        if self.enabled {
            self.redraw_counts[reason.index()] += 1;
        }
    }

    /// 记录一个待显示的指针输入批次及其原始样本数量。
    pub(super) fn record_pointer_batch(&mut self, sample_count: usize) {
        if !self.enabled || sample_count == 0 {
            return;
        }
        self.pending_input_at.get_or_insert_with(Instant::now);
        self.pointer_batch_count += 1;
        self.pointer_sample_count += u64::try_from(sample_count).unwrap_or(u64::MAX);
    }

    /// 记录窗口尺寸变化导致的一次全尺寸 GPU surface 重建。
    pub(super) fn record_surface_rebuild(&mut self) {
        if self.enabled {
            self.surface_rebuild_count += 1;
        }
    }

    /// 返回当前帧起点；统计关闭时避免额外读取高精度时钟。
    pub(super) fn begin_frame(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    /// 在交换缓冲完成后记录帧耗时和最近输入到显示的近似延迟。
    pub(super) fn finish_frame(
        &mut self,
        frame_started_at: Option<Instant>,
    ) -> Option<FrameSample> {
        if !self.enabled {
            return None;
        }
        let now = Instant::now();
        let frame_duration = now.duration_since(frame_started_at?);
        let input_to_display = self
            .pending_input_at
            .take()
            .map(|input_at| now.duration_since(input_at));
        self.frame_durations.push(frame_duration);
        self.input_latencies.extend(input_to_display);
        self.frame_count += 1;

        self.report_if_due(now);
        Some(FrameSample {
            frame_duration,
            input_to_display,
        })
    }

    /// 返回统计模式下一次只读汇总需要唤醒事件循环的时间。
    pub(super) fn next_report_deadline(&self) -> Option<Instant> {
        self.enabled
            .then_some(self.report_started_at + REPORT_INTERVAL)
    }

    /// 到达统计周期时直接输出汇总，不为统计本身请求新帧。
    pub(super) fn report_if_due(&mut self, now: Instant) {
        if self.enabled && now.duration_since(self.report_started_at) >= REPORT_INTERVAL {
            self.report(now);
        }
    }

    /// 输出并清空当前统计窗口，下一窗口重新累计。
    fn report(&mut self, now: Instant) {
        let frame_p50_ms = percentile_milliseconds(&mut self.frame_durations, PERCENTILE_50);
        let frame_p95_ms = percentile_milliseconds(&mut self.frame_durations, PERCENTILE_95);
        let input_p50_ms = percentile_milliseconds(&mut self.input_latencies, PERCENTILE_50);
        let input_p95_ms = percentile_milliseconds(&mut self.input_latencies, PERCENTILE_95);
        tracing::info!(
            target: "steady_ink::performance",
            frames = self.frame_count,
            pointer_batches = self.pointer_batch_count,
            pointer_samples = self.pointer_sample_count,
            frame_p50_ms = ?frame_p50_ms,
            frame_p95_ms = ?frame_p95_ms,
            input_to_display_p50_ms = ?input_p50_ms,
            input_to_display_p95_ms = ?input_p95_ms,
            surface_rebuilds = self.surface_rebuild_count,
            startup_redraws = self.redraw_counts[RedrawReason::Startup.index()],
            window_event_redraws = self.redraw_counts[RedrawReason::WindowEvent.index()],
            egui_redraws = self.redraw_counts[RedrawReason::Egui.index()],
            ui_command_redraws = self.redraw_counts[RedrawReason::UiCommand.index()],
            pointer_redraws = self.redraw_counts[RedrawReason::PointerInput.index()],
            slideshow_redraws = self.redraw_counts[RedrawReason::SlideShow.index()],
            animation_redraws = self.redraw_counts[RedrawReason::AnimationTimer.index()],
            "Steady Ink 性能统计窗口"
        );

        self.report_started_at = now;
        self.frame_durations.clear();
        self.input_latencies.clear();
        self.frame_count = 0;
        self.pointer_batch_count = 0;
        self.pointer_sample_count = 0;
        self.surface_rebuild_count = 0;
        self.redraw_counts.fill(0);
    }
}

impl Drop for PerformanceTracker {
    /// 应用正常退出时输出最后一个不足完整周期的统计窗口。
    fn drop(&mut self) {
        if self.enabled && (self.frame_count > 0 || self.pointer_sample_count > 0) {
            self.report(Instant::now());
        }
    }
}

/// 返回已排序耗时样本的指定百分位毫秒值。
pub(super) fn percentile_milliseconds(samples: &mut [Duration], percentile: usize) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable();
    let rank = (samples.len() * percentile)
        .div_ceil(100)
        .clamp(1, samples.len());
    let index = rank - 1;
    Some(samples[index].as_secs_f64() * 1_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证百分位计算对稳定有序样本返回预期位置。
    #[test]
    fn percentile_uses_nearest_rank_within_sample_range() {
        let mut samples: Vec<_> = (1..=100).map(Duration::from_millis).collect();

        assert_eq!(
            percentile_milliseconds(&mut samples, PERCENTILE_50),
            Some(50.0)
        );
        assert_eq!(
            percentile_milliseconds(&mut samples, PERCENTILE_95),
            Some(95.0)
        );

        let mut short_window = vec![Duration::from_millis(10), Duration::from_millis(20)];
        assert_eq!(
            percentile_milliseconds(&mut short_window, PERCENTILE_95),
            Some(20.0)
        );
    }

    /// 验证关闭统计时不读取帧时钟也不生成样本。
    #[test]
    fn disabled_tracker_returns_no_frame_sample() {
        let mut tracker = PerformanceTracker::with_enabled(false);

        assert_eq!(tracker.begin_frame(), None);
        assert!(tracker.finish_frame(None).is_none());
        assert_eq!(tracker.next_report_deadline(), None);
    }

    /// 验证启用统计时待处理输入只对应下一次 Present 的一个延迟样本。
    #[test]
    fn enabled_tracker_consumes_pending_input_once() {
        let mut tracker = PerformanceTracker::with_enabled(true);
        tracker.record_pointer_batch(3);

        let first = tracker
            .finish_frame(tracker.begin_frame())
            .expect("启用统计时应生成帧样本");
        let second = tracker
            .finish_frame(tracker.begin_frame())
            .expect("后续 Present 仍应生成帧样本");

        assert!(first.input_to_display.is_some());
        assert_eq!(second.input_to_display, None);
        assert_eq!(tracker.pointer_batch_count, 1);
        assert_eq!(tracker.pointer_sample_count, 3);
    }
}
