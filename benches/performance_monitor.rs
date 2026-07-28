use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use steady_ink::performance::{PerformanceFrameSample, PerformanceInkSync, PerformanceMonitor};

const ITERATIONS: u64 = 100_000;
const MAX_OVERHEAD_NANOS: u128 = 10_000;

/// 运行 release 微基准并在净记账开销达到 1% 预算时失败。
fn main() {
    if cfg!(debug_assertions) {
        println!(r#"{{"skipped":true,"reason":"release benchmark required"}}"#);
        return;
    }
    let baseline = measure_baseline();
    let monitored = measure_monitor();
    let net = monitored.saturating_sub(baseline);
    let nanos_per_frame = net.as_nanos() / u128::from(ITERATIONS);
    println!(
        "{{\"iterations\":{ITERATIONS},\"baseline_nanos\":{},\"monitored_nanos\":{},\"net_nanos_per_frame\":{nanos_per_frame},\"budget_nanos_per_frame\":{MAX_OVERHEAD_NANOS}}}",
        baseline.as_nanos(),
        monitored.as_nanos(),
    );
    assert!(
        nanos_per_frame < MAX_OVERHEAD_NANOS,
        "性能监控净开销 {nanos_per_frame}ns/帧，达到或超过 {MAX_OVERHEAD_NANOS}ns 预算"
    );
}

/// 测量构造相同样本但不进行监控记账的循环成本。
fn measure_baseline() -> Duration {
    let base = Instant::now();
    let started = Instant::now();
    for index in 0..ITERATIONS {
        black_box(sample(base, index));
    }
    started.elapsed()
}

/// 测量包含固定窗口聚合和共享快照发布的完整记账成本。
fn measure_monitor() -> Duration {
    let base = Instant::now();
    let mut monitor = PerformanceMonitor::new();
    monitor.set_enabled(true);
    let started = Instant::now();
    for index in 0..ITERATIONS {
        black_box(monitor.record_frame(sample(base, index)));
    }
    black_box(monitor.snapshot());
    started.elapsed()
}

/// 创建间隔稳定且不触发异常帧日志的基准样本。
fn sample(base: Instant, index: u64) -> PerformanceFrameSample {
    PerformanceFrameSample {
        presented_at: base + Duration::from_micros(index * 16_000),
        frame_time: Duration::from_millis(4),
        render_time: Duration::from_millis(3),
        input_latency: Some(Duration::from_millis(5)),
        visible_strokes: Some(1_000),
        visible_operations: Some(1_024),
        ink_sync: PerformanceInkSync::Incremental,
        managed_gpu_bytes: 64 * 1024 * 1024,
    }
}
