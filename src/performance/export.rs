use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::PerformanceSnapshot;
use crate::error::AppError;

const PERFORMANCE_SCHEMA_VERSION: u32 = 1;
const MEMORY_SCOPE: &str = "managed_render_resources_estimate";

/// 写入一次有界性能快照并返回创建的 JSON 文件路径。
pub fn export_snapshot(
    directory: &Path,
    snapshot: PerformanceSnapshot,
) -> Result<PathBuf, AppError> {
    fs::create_dir_all(directory).map_err(|error| {
        AppError::Settings(format!(
            "创建性能导出目录 {} 失败: {error}",
            directory.display()
        ))
    })?;
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let captured_at = now
        .format(&Rfc3339)
        .map_err(|error| AppError::Settings(format!("格式化性能导出时间失败: {error}")))?;
    let path = directory.join(export_file_name(now));
    let document = PerformanceExport::from_snapshot(snapshot, captured_at);
    let json = serde_json::to_vec_pretty(&document)
        .map_err(|error| AppError::Settings(format!("序列化性能快照失败: {error}")))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            AppError::Settings(format!("创建性能快照 {} 失败: {error}", path.display()))
        })?;
    file.write_all(&json).map_err(|error| {
        AppError::Settings(format!("写入性能快照 {} 失败: {error}", path.display()))
    })?;
    Ok(path)
}

/// 生成同时可读且具有纳秒冲突保护的导出文件名。
fn export_file_name(now: OffsetDateTime) -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos());
    format!(
        "steady-ink-performance-{:04}{:02}{:02}-{:02}{:02}{:02}-{unique:09}.json",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
    )
}

/// JSON 文件的稳定顶层 schema。
#[derive(Serialize)]
struct PerformanceExport {
    schema_version: u32,
    captured_at: String,
    memory_scope: &'static str,
    metrics: ExportMetrics,
    counters: ExportCounters,
    frame_times_ms: Vec<f32>,
}

impl PerformanceExport {
    /// 从固定大小运行时快照创建只在导出时分配的文档。
    fn from_snapshot(snapshot: PerformanceSnapshot, captured_at: String) -> Self {
        Self {
            schema_version: PERFORMANCE_SCHEMA_VERSION,
            captured_at,
            memory_scope: MEMORY_SCOPE,
            metrics: ExportMetrics {
                fps: snapshot.fps(),
                last_frame_time_ms: snapshot.last_frame_time_ms(),
                average_frame_time_ms: snapshot.average_frame_time_ms(),
                p95_frame_time_ms: snapshot.p95_frame_time_ms(),
                max_frame_time_ms: snapshot.max_frame_time_ms(),
                average_render_time_ms: snapshot.average_render_time_ms(),
                p95_render_time_ms: snapshot.p95_render_time_ms(),
                average_input_latency_ms: snapshot.average_input_latency_ms(),
                p95_input_latency_ms: snapshot.p95_input_latency_ms(),
                managed_gpu_bytes: snapshot.managed_gpu_bytes(),
            },
            counters: ExportCounters {
                frame_count: snapshot.frame_count(),
                input_sample_count: snapshot.input_sample_count(),
                visible_strokes: snapshot.visible_strokes(),
                visible_operations: snapshot.visible_operations(),
                incremental_sync_count: snapshot.incremental_sync_count(),
                region_rebuild_count: snapshot.region_rebuild_count(),
                full_rebuild_count: snapshot.full_rebuild_count(),
                slow_frame_count: snapshot.slow_frame_count(),
            },
            frame_times_ms: snapshot.frame_times_ms().to_vec(),
        }
    }
}

/// JSON 中按物理量分组的浮点和内存指标。
#[derive(Serialize)]
struct ExportMetrics {
    fps: f32,
    last_frame_time_ms: f32,
    average_frame_time_ms: f32,
    p95_frame_time_ms: f32,
    max_frame_time_ms: f32,
    average_render_time_ms: f32,
    p95_render_time_ms: f32,
    average_input_latency_ms: f32,
    p95_input_latency_ms: f32,
    managed_gpu_bytes: u64,
}

/// JSON 中按离散事件分组的累计计数。
#[derive(Serialize)]
struct ExportCounters {
    frame_count: u64,
    input_sample_count: u64,
    visible_strokes: usize,
    visible_operations: usize,
    incremental_sync_count: u64,
    region_rebuild_count: u64,
    full_rebuild_count: u64,
    slow_frame_count: u64,
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::performance::{
        PERFORMANCE_SAMPLE_CAPACITY, PerformanceFrameSample, PerformanceInkSync, PerformanceMonitor,
    };

    /// 验证导出 JSON 的 schema、内存语义和有界历史可被结构化解析。
    #[test]
    fn exported_snapshot_has_versioned_bounded_json() {
        let mut monitor = PerformanceMonitor::new();
        monitor.set_enabled(true);
        let base = Instant::now();
        for index in 0..(PERFORMANCE_SAMPLE_CAPACITY + 4) {
            monitor.record_frame(PerformanceFrameSample {
                presented_at: base + Duration::from_millis(index as u64 * 16),
                frame_time: Duration::from_millis(8),
                render_time: Duration::from_millis(7),
                input_latency: Some(Duration::from_millis(9)),
                visible_strokes: Some(3),
                visible_operations: Some(4),
                ink_sync: PerformanceInkSync::Incremental,
                managed_gpu_bytes: 1_024,
            });
        }
        let directory = std::env::temp_dir().join(format!(
            "steady-ink-performance-export-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("测试系统时间应晚于 Unix epoch")
                .as_nanos()
        ));

        let path = export_snapshot(&directory, monitor.snapshot()).expect("性能快照应能导出");
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("导出文件应能读取"))
                .expect("导出文件应是有效 JSON");

        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["memory_scope"], MEMORY_SCOPE);
        assert_eq!(json["metrics"]["managed_gpu_bytes"], 1_024);
        assert_eq!(
            json["frame_times_ms"]
                .as_array()
                .expect("帧历史应是数组")
                .len(),
            PERFORMANCE_SAMPLE_CAPACITY
        );

        fs::remove_file(&path).expect("测试导出文件应能删除");
        fs::remove_dir(&directory).expect("测试导出目录应能删除");
    }
}
