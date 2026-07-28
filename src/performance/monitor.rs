use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const PERFORMANCE_SAMPLE_CAPACITY: usize = 120;
pub const SLOW_FRAME_THRESHOLD: Duration = Duration::from_millis(33);
const ACTIVE_INTERVAL_LIMIT: Duration = Duration::from_secs(1);
const BYTES_PER_MEBIBYTE: f32 = 1024.0 * 1024.0;

/// 一帧墨迹同步对持久缓存执行的工作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerformanceInkSync {
    Unchanged,
    Incremental,
    RegionRebuild,
    FullRebuild,
}

/// 渲染线程在一个画面成功呈现后提交的完整性能事实。
#[derive(Debug, Clone, Copy)]
pub struct PerformanceFrameSample {
    pub presented_at: Instant,
    pub frame_time: Duration,
    pub render_time: Duration,
    pub input_latency: Option<Duration>,
    pub visible_strokes: Option<usize>,
    pub visible_operations: Option<usize>,
    pub ink_sync: PerformanceInkSync,
    pub managed_gpu_bytes: u64,
}

/// 事件线程和导出器使用的固定大小性能快照。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerformanceSnapshot {
    enabled: bool,
    frame_count: u64,
    input_sample_count: u64,
    fps: f32,
    last_frame_time_ms: f32,
    average_frame_time_ms: f32,
    p95_frame_time_ms: f32,
    max_frame_time_ms: f32,
    average_render_time_ms: f32,
    p95_render_time_ms: f32,
    average_input_latency_ms: f32,
    p95_input_latency_ms: f32,
    visible_strokes: usize,
    visible_operations: usize,
    incremental_sync_count: u64,
    region_rebuild_count: u64,
    full_rebuild_count: u64,
    slow_frame_count: u64,
    managed_gpu_bytes: u64,
    frame_history_ms: [f32; PERFORMANCE_SAMPLE_CAPACITY],
    frame_history_len: usize,
}

impl Default for PerformanceSnapshot {
    /// 创建尚未启用且没有任何样本的快照。
    fn default() -> Self {
        Self {
            enabled: false,
            frame_count: 0,
            input_sample_count: 0,
            fps: 0.0,
            last_frame_time_ms: 0.0,
            average_frame_time_ms: 0.0,
            p95_frame_time_ms: 0.0,
            max_frame_time_ms: 0.0,
            average_render_time_ms: 0.0,
            p95_render_time_ms: 0.0,
            average_input_latency_ms: 0.0,
            p95_input_latency_ms: 0.0,
            visible_strokes: 0,
            visible_operations: 0,
            incremental_sync_count: 0,
            region_rebuild_count: 0,
            full_rebuild_count: 0,
            slow_frame_count: 0,
            managed_gpu_bytes: 0,
            frame_history_ms: [0.0; PERFORMANCE_SAMPLE_CAPACITY],
            frame_history_len: 0,
        }
    }
}

impl PerformanceSnapshot {
    /// 返回采样会话当前是否启用。
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// 返回本次会话成功记录的已呈现画面总数。
    pub const fn frame_count(self) -> u64 {
        self.frame_count
    }

    /// 返回包含真实输入起点的延迟样本总数。
    pub const fn input_sample_count(self) -> u64 {
        self.input_sample_count
    }

    /// 返回活动呈现间隔推导出的每秒帧数。
    pub const fn fps(self) -> f32 {
        self.fps
    }

    /// 返回最近一次提交到呈现耗时，单位为毫秒。
    pub const fn last_frame_time_ms(self) -> f32 {
        self.last_frame_time_ms
    }

    /// 返回有界窗口内的平均提交到呈现耗时，单位为毫秒。
    pub const fn average_frame_time_ms(self) -> f32 {
        self.average_frame_time_ms
    }

    /// 返回有界窗口内的 p95 提交到呈现耗时，单位为毫秒。
    pub const fn p95_frame_time_ms(self) -> f32 {
        self.p95_frame_time_ms
    }

    /// 返回有界窗口内的最大提交到呈现耗时，单位为毫秒。
    pub const fn max_frame_time_ms(self) -> f32 {
        self.max_frame_time_ms
    }

    /// 返回有界窗口内的平均渲染线程耗时，单位为毫秒。
    pub const fn average_render_time_ms(self) -> f32 {
        self.average_render_time_ms
    }

    /// 返回有界窗口内的 p95 渲染线程耗时，单位为毫秒。
    pub const fn p95_render_time_ms(self) -> f32 {
        self.p95_render_time_ms
    }

    /// 返回有界输入样本的平均输入到呈现延迟，单位为毫秒。
    pub const fn average_input_latency_ms(self) -> f32 {
        self.average_input_latency_ms
    }

    /// 返回有界输入样本的 p95 输入到呈现延迟，单位为毫秒。
    pub const fn p95_input_latency_ms(self) -> f32 {
        self.p95_input_latency_ms
    }

    /// 返回最近文档同步后的可见画笔笔画数。
    pub const fn visible_strokes(self) -> usize {
        self.visible_strokes
    }

    /// 返回最近文档同步后的可见操作总数。
    pub const fn visible_operations(self) -> usize {
        self.visible_operations
    }

    /// 返回本次会话的增量墨迹同步次数。
    pub const fn incremental_sync_count(self) -> u64 {
        self.incremental_sync_count
    }

    /// 返回本次会话的局部墨迹重建次数。
    pub const fn region_rebuild_count(self) -> u64 {
        self.region_rebuild_count
    }

    /// 返回本次会话的全量墨迹重建次数。
    pub const fn full_rebuild_count(self) -> u64 {
        self.full_rebuild_count
    }

    /// 返回本次会话超过异常阈值的画面数。
    pub const fn slow_frame_count(self) -> u64 {
        self.slow_frame_count
    }

    /// 返回应用自有 GPU 渲染资源的保守估算字节数。
    pub const fn managed_gpu_bytes(self) -> u64 {
        self.managed_gpu_bytes
    }

    /// 返回应用自有 GPU 渲染资源估算，单位为 MiB。
    pub fn managed_gpu_mebibytes(self) -> f32 {
        self.managed_gpu_bytes as f32 / BYTES_PER_MEBIBYTE
    }

    /// 返回按最旧到最新排列的帧耗时历史。
    pub fn frame_times_ms(&self) -> &[f32] {
        &self.frame_history_ms[..self.frame_history_len]
    }
}

/// 跨线程读取最新固定大小性能快照的轻量句柄。
#[derive(Clone)]
pub struct PerformanceSnapshotReader {
    shared: Arc<Mutex<PerformanceSnapshot>>,
}

impl PerformanceSnapshotReader {
    /// 复制最新快照，不把锁带入 UI 或文件 I/O。
    pub fn snapshot(&self) -> PerformanceSnapshot {
        *self.shared.lock().expect("性能快照互斥量不应中毒")
    }
}

/// 在渲染线程聚合固定容量指标并发布只读快照。
pub struct PerformanceMonitor {
    enabled: bool,
    frame_times: SampleWindow,
    render_times: SampleWindow,
    input_latencies: SampleWindow,
    present_intervals: SampleWindow,
    last_presented_at: Option<Instant>,
    snapshot: PerformanceSnapshot,
    shared: Arc<Mutex<PerformanceSnapshot>>,
}

impl PerformanceMonitor {
    /// 创建默认关闭、没有历史数据的性能监控器。
    pub fn new() -> Self {
        let snapshot = PerformanceSnapshot::default();
        Self {
            enabled: false,
            frame_times: SampleWindow::default(),
            render_times: SampleWindow::default(),
            input_latencies: SampleWindow::default(),
            present_intervals: SampleWindow::default(),
            last_presented_at: None,
            snapshot,
            shared: Arc::new(Mutex::new(snapshot)),
        }
    }

    /// 返回可发送到事件线程的只读快照句柄。
    pub fn snapshot_reader(&self) -> PerformanceSnapshotReader {
        PerformanceSnapshotReader {
            shared: Arc::clone(&self.shared),
        }
    }

    /// 返回当前已发布快照，主要供测试与微基准读取。
    pub fn snapshot(&self) -> PerformanceSnapshot {
        *self.shared.lock().expect("性能快照互斥量不应中毒")
    }

    /// 切换采样会话；重新启用时清空旧窗口并返回 `true`。
    pub fn set_enabled(&mut self, enabled: bool) -> bool {
        if self.enabled == enabled {
            return false;
        }
        self.enabled = enabled;
        if enabled {
            self.frame_times.clear();
            self.render_times.clear();
            self.input_latencies.clear();
            self.present_intervals.clear();
            self.last_presented_at = None;
            self.snapshot = PerformanceSnapshot {
                enabled: true,
                ..PerformanceSnapshot::default()
            };
        } else {
            self.snapshot.enabled = false;
        }
        self.publish();
        enabled
    }

    /// 记录一个实际呈现画面，并返回它是否超过异常帧阈值。
    pub fn record_frame(&mut self, sample: PerformanceFrameSample) -> bool {
        if !self.enabled {
            return false;
        }

        self.record_present_interval(sample.presented_at);
        let frame_time_ms = duration_ms(sample.frame_time);
        self.frame_times.push(frame_time_ms);
        self.render_times.push(duration_ms(sample.render_time));
        if let Some(input_latency) = sample.input_latency {
            self.input_latencies.push(duration_ms(input_latency));
            self.snapshot.input_sample_count = self.snapshot.input_sample_count.saturating_add(1);
        }
        if let Some(visible_strokes) = sample.visible_strokes {
            self.snapshot.visible_strokes = visible_strokes;
        }
        if let Some(visible_operations) = sample.visible_operations {
            self.snapshot.visible_operations = visible_operations;
        }
        match sample.ink_sync {
            PerformanceInkSync::Unchanged => {}
            PerformanceInkSync::Incremental => {
                self.snapshot.incremental_sync_count =
                    self.snapshot.incremental_sync_count.saturating_add(1);
            }
            PerformanceInkSync::RegionRebuild => {
                self.snapshot.region_rebuild_count =
                    self.snapshot.region_rebuild_count.saturating_add(1);
            }
            PerformanceInkSync::FullRebuild => {
                self.snapshot.full_rebuild_count =
                    self.snapshot.full_rebuild_count.saturating_add(1);
            }
        }

        let slow_frame = sample.frame_time >= SLOW_FRAME_THRESHOLD;
        self.snapshot.frame_count = self.snapshot.frame_count.saturating_add(1);
        self.snapshot.slow_frame_count = self
            .snapshot
            .slow_frame_count
            .saturating_add(u64::from(slow_frame));
        self.snapshot.managed_gpu_bytes = sample.managed_gpu_bytes;
        self.refresh_aggregates();
        self.publish();
        slow_frame
    }

    /// 更新呈现间隔；真实空闲不进入活动 FPS 窗口。
    fn record_present_interval(&mut self, presented_at: Instant) {
        if let Some(previous) = self.last_presented_at {
            let interval = presented_at.saturating_duration_since(previous);
            if interval > ACTIVE_INTERVAL_LIMIT {
                self.present_intervals.clear();
            } else {
                self.present_intervals.push(duration_ms(interval));
            }
        }
        self.last_presented_at = Some(presented_at);
    }

    /// 从固定容量窗口刷新所有可展示聚合值。
    fn refresh_aggregates(&mut self) {
        self.snapshot.fps = match self.present_intervals.average() {
            average if average > f32::EPSILON => 1_000.0 / average,
            _ => 0.0,
        };
        self.snapshot.last_frame_time_ms = self.frame_times.last();
        self.snapshot.average_frame_time_ms = self.frame_times.average();
        self.snapshot.p95_frame_time_ms = self.frame_times.percentile_95();
        self.snapshot.max_frame_time_ms = self.frame_times.max();
        self.snapshot.average_render_time_ms = self.render_times.average();
        self.snapshot.p95_render_time_ms = self.render_times.percentile_95();
        self.snapshot.average_input_latency_ms = self.input_latencies.average();
        self.snapshot.p95_input_latency_ms = self.input_latencies.percentile_95();
        self.snapshot.frame_history_len = self
            .frame_times
            .copy_ordered(&mut self.snapshot.frame_history_ms);
    }

    /// 用一次短锁覆盖共享快照，不在锁内执行其他工作。
    fn publish(&self) {
        *self.shared.lock().expect("性能快照互斥量不应中毒") = self.snapshot;
    }
}

impl Default for PerformanceMonitor {
    /// 创建默认关闭的性能监控器。
    fn default() -> Self {
        Self::new()
    }
}

/// 不分配内存的固定容量浮点样本环。
#[derive(Debug, Clone, Copy)]
struct SampleWindow {
    values: [f32; PERFORMANCE_SAMPLE_CAPACITY],
    len: usize,
    next: usize,
}

impl Default for SampleWindow {
    /// 创建空的固定容量窗口。
    fn default() -> Self {
        Self {
            values: [0.0; PERFORMANCE_SAMPLE_CAPACITY],
            len: 0,
            next: 0,
        }
    }
}

impl SampleWindow {
    /// 追加一个样本并覆盖最旧项。
    fn push(&mut self, value: f32) {
        self.values[self.next] = value;
        self.next = (self.next + 1) % PERFORMANCE_SAMPLE_CAPACITY;
        self.len = self.len.saturating_add(1).min(PERFORMANCE_SAMPLE_CAPACITY);
    }

    /// 清空逻辑窗口并保留内联存储。
    fn clear(&mut self) {
        self.len = 0;
        self.next = 0;
    }

    /// 返回最近追加的样本，空窗口返回零。
    fn last(&self) -> f32 {
        if self.len == 0 {
            0.0
        } else {
            self.values[(self.next + PERFORMANCE_SAMPLE_CAPACITY - 1) % PERFORMANCE_SAMPLE_CAPACITY]
        }
    }

    /// 返回窗口算术平均值，空窗口返回零。
    fn average(&self) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        self.iter().sum::<f32>() / self.len as f32
    }

    /// 返回窗口最大值，空窗口返回零。
    fn max(&self) -> f32 {
        self.iter().fold(0.0, f32::max)
    }

    /// 按最近秩定义返回窗口 p95，空窗口返回零。
    fn percentile_95(&self) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        let mut sorted = [0.0; PERFORMANCE_SAMPLE_CAPACITY];
        let copied = self.copy_ordered(&mut sorted);
        sorted[..copied].sort_by(f32::total_cmp);
        let rank = copied.saturating_mul(95).div_ceil(100).saturating_sub(1);
        sorted[rank]
    }

    /// 把当前窗口按最旧到最新复制到目标数组并返回项数。
    fn copy_ordered(&self, target: &mut [f32; PERFORMANCE_SAMPLE_CAPACITY]) -> usize {
        for (index, value) in self.iter().enumerate() {
            target[index] = value;
        }
        self.len
    }

    /// 按最旧到最新遍历当前逻辑窗口。
    fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        let start = if self.len == PERFORMANCE_SAMPLE_CAPACITY {
            self.next
        } else {
            0
        };
        (0..self.len).map(move |offset| self.values[(start + offset) % PERFORMANCE_SAMPLE_CAPACITY])
    }
}

/// 把标准库时长转换为 overlay 和导出使用的毫秒数。
fn duration_ms(duration: Duration) -> f32 {
    duration.as_secs_f32() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建具有可控时间和默认计数的测试样本。
    fn sample(base: Instant, index: u64, frame_ms: u64) -> PerformanceFrameSample {
        PerformanceFrameSample {
            presented_at: base + Duration::from_millis(index * 16),
            frame_time: Duration::from_millis(frame_ms),
            render_time: Duration::from_millis(frame_ms.saturating_sub(1)),
            input_latency: None,
            visible_strokes: Some(4),
            visible_operations: Some(5),
            ink_sync: PerformanceInkSync::Unchanged,
            managed_gpu_bytes: 8 * 1024 * 1024,
        }
    }

    /// 验证禁用监控不会记录或发布任何样本。
    #[test]
    fn disabled_monitor_does_not_record() {
        let mut monitor = PerformanceMonitor::new();
        let reader = monitor.snapshot_reader();

        assert!(!monitor.record_frame(sample(Instant::now(), 0, 12)));
        assert_eq!(reader.snapshot(), PerformanceSnapshot::default());
    }

    /// 验证聚合值、输入延迟和同步计数来自实际样本。
    #[test]
    fn frame_samples_update_aggregates_and_counts() {
        let base = Instant::now();
        let mut monitor = PerformanceMonitor::new();
        assert!(monitor.set_enabled(true));
        monitor.record_frame(sample(base, 0, 10));
        let mut second = sample(base, 1, 20);
        second.input_latency = Some(Duration::from_millis(24));
        second.ink_sync = PerformanceInkSync::RegionRebuild;
        monitor.record_frame(second);
        let mut third = sample(base, 2, 30);
        third.ink_sync = PerformanceInkSync::FullRebuild;
        monitor.record_frame(third);

        let snapshot = monitor.snapshot();
        assert_eq!(snapshot.frame_count(), 3);
        assert!((snapshot.fps() - 62.5).abs() < 0.01);
        assert_eq!(snapshot.average_frame_time_ms(), 20.0);
        assert_eq!(snapshot.p95_frame_time_ms(), 30.0);
        assert_eq!(snapshot.max_frame_time_ms(), 30.0);
        assert_eq!(snapshot.input_sample_count(), 1);
        assert_eq!(snapshot.average_input_latency_ms(), 24.0);
        assert_eq!(snapshot.region_rebuild_count(), 1);
        assert_eq!(snapshot.full_rebuild_count(), 1);
        assert_eq!(snapshot.visible_strokes(), 4);
        assert_eq!(snapshot.visible_operations(), 5);
    }

    /// 验证历史达到容量后只保留最新 120 项且顺序稳定。
    #[test]
    fn frame_history_is_bounded_and_chronological() {
        let base = Instant::now();
        let mut monitor = PerformanceMonitor::new();
        monitor.set_enabled(true);
        for index in 0..130 {
            monitor.record_frame(sample(base, index, index));
        }

        let snapshot = monitor.snapshot();
        assert_eq!(snapshot.frame_times_ms().len(), PERFORMANCE_SAMPLE_CAPACITY);
        assert_eq!(snapshot.frame_times_ms().first(), Some(&10.0));
        assert_eq!(snapshot.frame_times_ms().last(), Some(&129.0));
    }

    /// 验证长时间无画面后不会把空闲间隔计入 FPS。
    #[test]
    fn idle_gap_resets_active_fps_window() {
        let base = Instant::now();
        let mut monitor = PerformanceMonitor::new();
        monitor.set_enabled(true);
        monitor.record_frame(sample(base, 0, 8));
        monitor.record_frame(sample(base, 1, 8));
        assert!(monitor.snapshot().fps() > 0.0);

        let mut after_idle = sample(base, 2, 8);
        after_idle.presented_at = base + Duration::from_secs(2);
        monitor.record_frame(after_idle);

        assert_eq!(monitor.snapshot().fps(), 0.0);
    }

    /// 验证关闭保留最后数据而重新开启开始空会话。
    #[test]
    fn reenabling_starts_a_fresh_session() {
        let mut monitor = PerformanceMonitor::new();
        monitor.set_enabled(true);
        monitor.record_frame(sample(Instant::now(), 0, 12));
        monitor.set_enabled(false);
        assert_eq!(monitor.snapshot().frame_count(), 1);
        assert!(!monitor.snapshot().enabled());

        assert!(monitor.set_enabled(true));
        assert_eq!(monitor.snapshot().frame_count(), 0);
        assert!(monitor.snapshot().enabled());
    }

    /// 验证 33ms 边界被计入异常帧。
    #[test]
    fn slow_frame_threshold_is_inclusive() {
        let mut monitor = PerformanceMonitor::new();
        monitor.set_enabled(true);

        assert!(monitor.record_frame(sample(Instant::now(), 0, 33)));
        assert_eq!(monitor.snapshot().slow_frame_count(), 1);
    }
}
