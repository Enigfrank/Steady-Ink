use super::{CanvasPoint, VariableStrokePoint};

const MIN_ACCEPT_DISTANCE_SQUARED: f32 = 0.25;
const SPEED_TIME_CONSTANT_MS: f32 = 24.0;
const MAX_SPEED_LOGICAL_PIXELS_PER_MS: f32 = 3.0;
const MIN_WIDTH_RATIO: f32 = 0.30;
const SPEED_SCALE: f32 = 0.35;
const SPEED_EXPONENT: f32 = 1.5;
const MIN_TAPER_LENGTH: f32 = 12.0;
const TIP_WIDTH_RATIO: f32 = 0.25;

/// 一个已接受位置及其尚未应用端点渐细的速度基础宽度。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpeedStrokePoint {
    pub point: CanvasPoint,
    pub base_width: f32,
}

/// 以真实采样时间增量计算速度宽度的流式笔锋构建器。
#[derive(Debug, Clone)]
pub(crate) struct SpeedStrokeBuilder {
    samples: Vec<SpeedStrokePoint>,
    filtered_speed: f32,
    last_timestamp_micros: u64,
    total_length: f32,
    max_width: f32,
    dpi_scale: f32,
}

impl SpeedStrokeBuilder {
    /// 从第一条有效物理像素采样创建速度笔锋构建器。
    pub(crate) fn new(
        point: CanvasPoint,
        timestamp_micros: u64,
        max_width: f32,
        dpi_scale: f32,
    ) -> Option<Self> {
        if !finite_point(point) || !max_width.is_finite() || max_width <= 0.0 {
            return None;
        }
        Some(Self {
            samples: vec![SpeedStrokePoint {
                point,
                base_width: max_width,
            }],
            filtered_speed: 0.0,
            last_timestamp_micros: timestamp_micros,
            total_length: 0.0,
            max_width,
            dpi_scale: valid_dpi_scale(dpi_scale),
        })
    }

    /// 以 O(1) 增量接受一个采样并更新滤波速度和基础宽度。
    pub(crate) fn push(&mut self, point: CanvasPoint, timestamp_micros: u64) -> bool {
        if !finite_point(point) {
            return false;
        }
        let Some(previous) = self.samples.last().copied() else {
            return false;
        };
        let distance = distance(previous.point, point);
        if !distance.is_finite() || distance.mul_add(distance, 0.0) < MIN_ACCEPT_DISTANCE_SQUARED {
            return false;
        }

        let timestamp = timestamp_micros.max(self.last_timestamp_micros);
        let delta_micros = timestamp.saturating_sub(self.last_timestamp_micros);
        let delta_ms = delta_micros as f32 / 1_000.0;
        let base_width = if delta_ms.is_finite() && delta_ms > 0.0 {
            let raw_speed =
                (distance / self.dpi_scale / delta_ms).clamp(0.0, MAX_SPEED_LOGICAL_PIXELS_PER_MS);
            let alpha = 1.0 - (-delta_ms / SPEED_TIME_CONSTANT_MS).exp();
            self.filtered_speed += (raw_speed - self.filtered_speed) * alpha.clamp(0.0, 1.0);
            width_for_speed(self.max_width, self.filtered_speed)
        } else {
            width_for_speed(self.max_width, self.filtered_speed)
        };

        self.total_length += distance;
        self.last_timestamp_micros = timestamp;
        self.samples.push(SpeedStrokePoint { point, base_width });
        true
    }

    /// 把当前基础宽度和弧长渐细固化为可变笔迹点集。
    pub(crate) fn finalized_points(&self) -> Vec<VariableStrokePoint> {
        let mut points = Vec::with_capacity(self.samples.len());
        self.finalize_into(&mut points);
        points
    }

    /// 复用调用方缓冲区生成端点渐细后的确定宽度点集。
    pub(crate) fn finalize_into(&self, output: &mut Vec<VariableStrokePoint>) {
        output.clear();
        output.reserve(self.samples.len().saturating_sub(output.capacity()));
        let taper_length = MIN_TAPER_LENGTH.max(self.max_width * 2.0);
        let tip_width = (self.max_width * TIP_WIDTH_RATIO).clamp(1.0, 2.0);
        let mut distance_from_start = 0.0;

        for (index, sample) in self.samples.iter().enumerate() {
            if index > 0 {
                distance_from_start += distance(self.samples[index - 1].point, sample.point);
            }
            let start_factor = smoothstep((distance_from_start / taper_length).clamp(0.0, 1.0));
            let end_factor = smoothstep(
                ((self.total_length - distance_from_start) / taper_length).clamp(0.0, 1.0),
            );
            let start_width = tip_width + (sample.base_width - tip_width) * start_factor;
            let end_width = tip_width + (sample.base_width - tip_width) * end_factor;
            let width = sample
                .base_width
                .min(start_width)
                .min(end_width)
                .clamp(tip_width, self.max_width);
            output.push(VariableStrokePoint {
                point: sample.point,
                width,
            });
        }
    }
}

/// 判断坐标是否可安全参与速度和几何计算。
fn finite_point(point: CanvasPoint) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

/// 把无效 DPI 值退化为 1，避免宽度因除零或 NaN 崩溃。
fn valid_dpi_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// 返回两个物理像素点之间的有限距离。
fn distance(left: CanvasPoint, right: CanvasPoint) -> f32 {
    let delta_x = right.x - left.x;
    let delta_y = right.y - left.y;
    delta_x.mul_add(delta_x, delta_y * delta_y).sqrt()
}

/// 用平滑单调函数把滤波速度映射为不超过最大宽度的宽度。
fn width_for_speed(max_width: f32, speed: f32) -> f32 {
    let normalized_speed = (speed / SPEED_SCALE).clamp(0.0, MAX_SPEED_LOGICAL_PIXELS_PER_MS);
    let ratio =
        MIN_WIDTH_RATIO + (1.0 - MIN_WIDTH_RATIO) / (1.0 + normalized_speed.powf(SPEED_EXPONENT));
    (max_width * ratio).clamp(max_width * MIN_WIDTH_RATIO, max_width)
}

/// 返回三次平滑插值，避免端点宽度出现分段跳变。
fn smoothstep(value: f32) -> f32 {
    value * value * (3.0 - 2.0 * value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建一条便于测试的水平速度笔迹。
    fn builder(dpi_scale: f32) -> SpeedStrokeBuilder {
        SpeedStrokeBuilder::new(CanvasPoint::new(0.0, 0.0), 0, 16.0, dpi_scale)
            .expect("测试起点应有效")
    }

    /// 验证匀速运动在不同 DPI 下归一化为相同宽度响应。
    #[test]
    fn dpi_normalization_keeps_logical_speed_equivalent() {
        let mut at_100 = builder(1.0);
        let mut at_150 = builder(1.5);
        at_100.push(CanvasPoint::new(30.0, 0.0), 10_000);
        at_150.push(CanvasPoint::new(45.0, 0.0), 10_000);

        assert_eq!(at_100.samples[1].base_width, at_150.samples[1].base_width);
    }

    /// 验证快速运动只会减小宽度且始终保持在最小比例以上。
    #[test]
    fn speed_mapping_is_monotonic_and_bounded() {
        let mut builder = builder(1.0);
        builder.push(CanvasPoint::new(1.0, 0.0), 10_000);
        let slow_width = builder.samples[1].base_width;
        builder.push(CanvasPoint::new(101.0, 0.0), 10_001);
        let fast_width = builder.samples[2].base_width;

        assert!(slow_width <= 16.0);
        assert!(fast_width < slow_width);
        assert!(fast_width >= 16.0 * MIN_WIDTH_RATIO);
    }

    /// 验证重复或倒退时间不会让宽度产生非有限值或异常尖刺。
    #[test]
    fn invalid_time_reuses_a_finite_filtered_width() {
        let mut builder = builder(1.0);
        builder.push(CanvasPoint::new(4.0, 0.0), 10_000);
        let before = builder.samples[1].base_width;
        builder.push(CanvasPoint::new(8.0, 0.0), 9_000);
        let after = builder.samples[2].base_width;

        assert!(after.is_finite());
        assert_eq!(after, before);
    }

    /// 验证端点渐细按弧长生成可见尖端并保持中段不超过最大宽度。
    #[test]
    fn finalized_points_have_tapered_ends() {
        let mut builder = builder(1.0);
        for index in 1..=8 {
            builder.push(CanvasPoint::new(index as f32 * 8.0, 0.0), index * 10_000);
        }
        let points = builder.finalized_points();

        assert!(points.first().is_some_and(|point| point.width < 16.0));
        assert!(points.last().is_some_and(|point| point.width < 16.0));
        assert!(points.iter().all(|point| point.width.is_finite()));
        assert!(points.iter().all(|point| point.width <= 16.0));
    }
}
