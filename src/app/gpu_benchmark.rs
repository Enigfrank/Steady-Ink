use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Serialize;
use winit::dpi::PhysicalSize;

use super::performance::{FrameSample, percentile_milliseconds};
use crate::{
    error::AppError,
    ink::{CanvasPoint, EraseSample, InkColor, InkDocument, PenWidth},
    window::GraphicsDiagnostics,
};

const BENCHMARK_ENVIRONMENT_VARIABLE: &str = "STEADY_INK_GPU_BENCHMARK";
const REPORT_ENVIRONMENT_VARIABLE: &str = "STEADY_INK_GPU_BENCHMARK_REPORT";
const DRAW_OPERATION_COUNT: usize = 1_000;
const ERASE_OPERATION_COUNT: usize = 200;
const TOTAL_OPERATION_COUNT: usize = DRAW_OPERATION_COUNT + ERASE_OPERATION_COUNT;
const INPUT_P95_LIMIT_MS: f64 = 33.0;
const DRAW_POINTS_PER_OPERATION: usize = 12;
const ERASE_SAMPLES_PER_OPERATION: usize = 6;

/// 压力场景在一帧 Present 完成后要求运行时执行的下一步。
pub(super) enum GpuBenchmarkAction {
    RequestNextFrame { sample_count: usize },
    Complete,
}

/// 通过真实 DirectComposition/D3D12 呈现路径逐帧驱动固定墨迹负载。
pub(super) struct GpuBenchmark {
    report_path: PathBuf,
    queued_operations: usize,
    measured_operations: usize,
    awaiting_present: bool,
    frame_durations: Vec<Duration>,
    input_latencies: Vec<Duration>,
}

impl GpuBenchmark {
    /// 根据显式环境变量创建压力场景；正常运行默认返回 `None`。
    pub(super) fn from_environment() -> Result<Option<Self>, AppError> {
        let enabled = std::env::var(BENCHMARK_ENVIRONMENT_VARIABLE)
            .ok()
            .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true"));
        if !enabled {
            return Ok(None);
        }
        let report_path = std::env::var_os(REPORT_ENVIRONMENT_VARIABLE)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                AppError::Settings(format!(
                    "启用 GPU 压力场景时必须设置 {REPORT_ENVIRONMENT_VARIABLE}"
                ))
            })?;
        tracing::info!(
            draw_operations = DRAW_OPERATION_COUNT,
            erase_operations = ERASE_OPERATION_COUNT,
            report_path = %report_path.display(),
            "已启用 Intel GPU 墨迹压力场景"
        );
        Ok(Some(Self {
            report_path,
            queued_operations: 0,
            measured_operations: 0,
            awaiting_present: false,
            frame_durations: Vec::with_capacity(TOTAL_OPERATION_COUNT),
            input_latencies: Vec::with_capacity(TOTAL_OPERATION_COUNT),
        }))
    }

    /// 消费刚完成的帧样本，追加下一项负载或写入最终报告。
    pub(super) fn after_present(
        &mut self,
        document: &mut InkDocument,
        frame_sample: Option<FrameSample>,
        diagnostics: &GraphicsDiagnostics,
        surface_size: PhysicalSize<u32>,
    ) -> Result<GpuBenchmarkAction, AppError> {
        if self.awaiting_present {
            let sample = frame_sample.ok_or_else(|| {
                AppError::Graphics("GPU 压力场景未收到已启用的帧性能样本".to_owned())
            })?;
            self.frame_durations.push(sample.frame_duration);
            let input_latency = sample.input_to_display.ok_or_else(|| {
                AppError::Graphics("GPU 压力场景帧缺少输入到显示延迟样本".to_owned())
            })?;
            self.input_latencies.push(input_latency);
            self.measured_operations += 1;
            self.awaiting_present = false;
        }

        if self.measured_operations == TOTAL_OPERATION_COUNT {
            self.write_report(document, diagnostics, surface_size)?;
            return Ok(GpuBenchmarkAction::Complete);
        }

        let sample_count = append_benchmark_operation(
            document,
            self.queued_operations,
            surface_size.width,
            surface_size.height,
        )?;
        self.queued_operations += 1;
        self.awaiting_present = true;
        Ok(GpuBenchmarkAction::RequestNextFrame { sample_count })
    }

    /// 生成机器可读报告，并在环境或 p95 不符合验收条件时返回失败。
    fn write_report(
        &mut self,
        document: &InkDocument,
        diagnostics: &GraphicsDiagnostics,
        surface_size: PhysicalSize<u32>,
    ) -> Result<(), AppError> {
        let frame_p50_ms = percentile_milliseconds(&mut self.frame_durations, 50);
        let frame_p95_ms = percentile_milliseconds(&mut self.frame_durations, 95);
        let input_p50_ms = percentile_milliseconds(&mut self.input_latencies, 50);
        let input_p95_ms = percentile_milliseconds(&mut self.input_latencies, 95);
        let mut failures = Vec::new();
        if !diagnostics.vendor.contains("0x8086") {
            failures.push(format!("adapter 不是 Intel: {}", diagnostics.vendor));
        }
        if diagnostics.software_fallback {
            failures.push("使用了 WARP 软件回退".to_owned());
        }
        if self.queued_operations != TOTAL_OPERATION_COUNT
            || self.measured_operations != TOTAL_OPERATION_COUNT
            || document.operations().len() != TOTAL_OPERATION_COUNT
        {
            failures.push(format!(
                "operation 计数不匹配: queued={}, measured={}, document={}",
                self.queued_operations,
                self.measured_operations,
                document.operations().len()
            ));
        }
        if self.input_latencies.len() != TOTAL_OPERATION_COUNT {
            failures.push(format!(
                "输入延迟样本数量不匹配: {}",
                self.input_latencies.len()
            ));
        }
        match input_p95_ms {
            Some(value) if value <= INPUT_P95_LIMIT_MS => {}
            Some(value) => failures.push(format!(
                "input-to-display p95 {value:.3} ms 超过 {INPUT_P95_LIMIT_MS:.3} ms"
            )),
            None => failures.push("缺少 input-to-display p95".to_owned()),
        }

        let report = GpuBenchmarkReport {
            benchmark: "steady-ink-gpu-baseline-v1",
            renderer: &diagnostics.renderer,
            vendor: &diagnostics.vendor,
            graphics_version: &diagnostics.version,
            software_fallback: diagnostics.software_fallback,
            surface_width: surface_size.width,
            surface_height: surface_size.height,
            draw_operations: DRAW_OPERATION_COUNT,
            erase_operations: ERASE_OPERATION_COUNT,
            total_operations: document.operations().len(),
            frame_samples: self.frame_durations.len(),
            input_samples: self.input_latencies.len(),
            frame_p50_ms,
            frame_p95_ms,
            input_to_display_p50_ms: input_p50_ms,
            input_to_display_p95_ms: input_p95_ms,
            input_to_display_limit_ms: INPUT_P95_LIMIT_MS,
            passed: failures.is_empty(),
            failure_reason: (!failures.is_empty()).then(|| failures.join("; ")),
        };
        write_report(&self.report_path, &report)?;
        tracing::info!(
            passed = report.passed,
            input_to_display_p95_ms = ?report.input_to_display_p95_ms,
            report_path = %self.report_path.display(),
            "Intel GPU 墨迹压力场景完成"
        );
        if let Some(reason) = report.failure_reason {
            return Err(AppError::Graphics(format!(
                "GPU 压力场景验收失败: {reason}; 报告: {}",
                self.report_path.display()
            )));
        }
        Ok(())
    }
}

/// 压力场景写出的 TOML 结果结构。
#[derive(Serialize)]
struct GpuBenchmarkReport<'a> {
    benchmark: &'a str,
    renderer: &'a str,
    vendor: &'a str,
    graphics_version: &'a str,
    software_fallback: bool,
    surface_width: u32,
    surface_height: u32,
    draw_operations: usize,
    erase_operations: usize,
    total_operations: usize,
    frame_samples: usize,
    input_samples: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_p50_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_p95_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_to_display_p50_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_to_display_p95_ms: Option<f64>,
    input_to_display_limit_ms: f64,
    passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_reason: Option<String>,
}

/// 写入压力结果，并创建尚不存在的父目录。
fn write_report(path: &Path, report: &GpuBenchmarkReport<'_>) -> Result<(), AppError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            AppError::Settings(format!(
                "无法创建 GPU 压力报告目录 {}: {error}",
                parent.display()
            ))
        })?;
    }
    let content = toml::to_string_pretty(report)
        .map_err(|error| AppError::Settings(format!("无法序列化 GPU 压力报告: {error}")))?;
    fs::write(path, content).map_err(|error| {
        AppError::Settings(format!("无法写入 GPU 压力报告 {}: {error}", path.display()))
    })
}

/// 向文档追加一个确定性的画笔或动态擦除 operation，并返回原始样本数量。
fn append_benchmark_operation(
    document: &mut InkDocument,
    operation_index: usize,
    surface_width: u32,
    surface_height: u32,
) -> Result<usize, AppError> {
    let inserted = if operation_index < DRAW_OPERATION_COUNT {
        let points = benchmark_draw_points(operation_index, surface_width, surface_height);
        let sample_count = points.len();
        let colors = [
            InkColor::Red,
            InkColor::Yellow,
            InkColor::Blue,
            InkColor::Green,
            InkColor::Black,
            InkColor::White,
        ];
        let widths = [PenWidth::Px4, PenWidth::Px8, PenWidth::Px16, PenWidth::Px24];
        document
            .append_draw_stroke(
                points,
                colors[operation_index % colors.len()],
                widths[operation_index % widths.len()],
            )
            .map(|_| sample_count)
    } else if operation_index < TOTAL_OPERATION_COUNT {
        let erase_index = operation_index - DRAW_OPERATION_COUNT;
        let samples = benchmark_erase_samples(erase_index, surface_width, surface_height);
        let sample_count = samples.len();
        document.append_erase_stroke(samples).map(|_| sample_count)
    } else {
        return Err(AppError::Graphics(format!(
            "GPU 压力场景收到越界 operation 索引 {operation_index}"
        )));
    };
    inserted.ok_or_else(|| {
        AppError::Graphics(format!("GPU 压力场景无法创建 operation {operation_index}"))
    })
}

/// 生成覆盖 40 x 25 网格的十二点短笔画。
fn benchmark_draw_points(index: usize, width: u32, height: u32) -> Vec<CanvasPoint> {
    const COLUMNS: usize = 40;
    const ROWS: usize = DRAW_OPERATION_COUNT / COLUMNS;
    let column = index % COLUMNS;
    let row = index / COLUMNS;
    let cell_width = width.max(1) as f32 / COLUMNS as f32;
    let cell_height = height.max(1) as f32 / ROWS as f32;
    (0..DRAW_POINTS_PER_OPERATION)
        .map(|sample_index| {
            let progress = sample_index as f32 / (DRAW_POINTS_PER_OPERATION - 1) as f32;
            let x = (column as f32 + 0.1 + progress * 0.8) * cell_width;
            let wave = (index as f32 * 0.17 + sample_index as f32 * 0.55).sin() * 0.25;
            let y = (row as f32 + 0.5 + wave) * cell_height;
            CanvasPoint::new(x, y.clamp(0.0, height.max(1) as f32 - 1.0))
        })
        .collect()
}

/// 生成覆盖 20 x 10 网格的六采样动态椭圆擦除路径。
fn benchmark_erase_samples(index: usize, width: u32, height: u32) -> Vec<EraseSample> {
    const COLUMNS: usize = 20;
    const ROWS: usize = ERASE_OPERATION_COUNT / COLUMNS;
    let column = index % COLUMNS;
    let row = index / COLUMNS;
    let cell_width = width.max(1) as f32 / COLUMNS as f32;
    let cell_height = height.max(1) as f32 / ROWS as f32;
    (0..ERASE_SAMPLES_PER_OPERATION)
        .map(|sample_index| {
            let progress = sample_index as f32 / (ERASE_SAMPLES_PER_OPERATION - 1) as f32;
            EraseSample {
                center: CanvasPoint::new(
                    (column as f32 + 0.2 + progress * 0.6) * cell_width,
                    (row as f32 + 0.35 + progress * 0.3) * cell_height,
                ),
                radius_x: (cell_width * (0.28 + progress * 0.08)).clamp(12.0, 72.0),
                radius_y: (cell_height * (0.16 + progress * 0.06)).clamp(8.0, 48.0),
                rotation_radians: (index % 8) as f32 * std::f32::consts::PI / 8.0,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::InkOperation;

    /// 验证压力数据生成器精确创建 1000 条画笔和 200 次擦除。
    #[test]
    fn workload_generator_creates_required_operation_mix() {
        let mut document = InkDocument::new();
        for index in 0..TOTAL_OPERATION_COUNT {
            append_benchmark_operation(&mut document, index, 1_920, 1_080)
                .expect("基线 operation 应可创建");
        }

        assert_eq!(document.operations().len(), TOTAL_OPERATION_COUNT);
        assert_eq!(
            document
                .operations()
                .iter()
                .filter(|operation| matches!(operation, InkOperation::DrawStroke(_)))
                .count(),
            DRAW_OPERATION_COUNT
        );
        assert_eq!(
            document
                .operations()
                .iter()
                .filter(|operation| matches!(operation, InkOperation::EraseStroke(_)))
                .count(),
            ERASE_OPERATION_COUNT
        );
    }
}
