use super::{CanvasPoint, VariableStrokePoint};

const MIN_ACCEPT_DISTANCE_SQUARED: f32 = 0.25;
const SHORT_STROKE_BODY_WIDTHS: f32 = 3.0;
const START_TAPER_BODY_WIDTHS: f32 = 1.5;
const START_TAPER_LENGTH_RATIO: f32 = 0.20;
const END_TAPER_BODY_WIDTHS: f32 = 3.0;
const END_TAPER_LENGTH_RATIO: f32 = 0.35;
const START_TIP_WIDTH_RATIO: f32 = 0.70;
const END_TIP_WIDTH_RATIO: f32 = 0.25;

/// 一个已接受的位置及其从笔画起点累计的物理像素弧长。
#[derive(Debug, Clone, Copy, PartialEq)]
struct NaturalStrokePoint {
    point: CanvasPoint,
    distance_from_start: f32,
}

/// 按几何弧长生成轻起笔和明显收笔的流式笔锋构建器。
#[derive(Debug, Clone)]
pub(crate) struct NaturalStrokeBuilder {
    samples: Vec<NaturalStrokePoint>,
    total_length: f32,
    body_width: f32,
}

impl NaturalStrokeBuilder {
    /// 从第一条有效物理像素采样创建自然笔锋构建器。
    pub(crate) fn new(point: CanvasPoint, body_width: f32) -> Option<Self> {
        if !finite_point(point) || !body_width.is_finite() || body_width <= 0.0 {
            return None;
        }
        Some(Self {
            samples: vec![NaturalStrokePoint {
                point,
                distance_from_start: 0.0,
            }],
            total_length: 0.0,
            body_width,
        })
    }

    /// 以 O(1) 增量接受一个有实际位移的有限位置采样。
    pub(crate) fn push(&mut self, point: CanvasPoint) -> bool {
        if !finite_point(point) {
            return false;
        }
        let Some(previous) = self.samples.last().copied() else {
            return false;
        };
        let segment_length = distance(previous.point, point);
        if !segment_length.is_finite()
            || segment_length.mul_add(segment_length, 0.0) < MIN_ACCEPT_DISTANCE_SQUARED
        {
            return false;
        }

        self.total_length += segment_length;
        self.samples.push(NaturalStrokePoint {
            point,
            distance_from_start: self.total_length,
        });
        true
    }

    /// 生成当前几何路径对应的确定性可变宽度点集。
    pub(crate) fn finalized_points(&self) -> Vec<VariableStrokePoint> {
        let mut points = Vec::with_capacity(self.samples.len());
        self.finalize_into(&mut points);
        points
    }

    /// 复用调用方缓冲区生成与提交阶段完全相同的自然笔锋点集。
    pub(crate) fn finalize_into(&self, output: &mut Vec<VariableStrokePoint>) {
        output.clear();
        output.reserve(self.samples.len().saturating_sub(output.capacity()));

        for sample in &self.samples {
            output.push(VariableStrokePoint {
                point: sample.point,
                width: width_at_distance(
                    sample.distance_from_start,
                    self.total_length,
                    self.body_width,
                ),
            });
        }
    }
}

/// 判断坐标是否可安全参与几何计算。
fn finite_point(point: CanvasPoint) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

/// 返回两个物理像素点之间的欧氏距离。
fn distance(left: CanvasPoint, right: CanvasPoint) -> f32 {
    let delta_x = right.x - left.x;
    let delta_y = right.y - left.y;
    delta_x.mul_add(delta_x, delta_y * delta_y).sqrt()
}

/// 根据当前弧长位置生成短笔画保护或非对称起收笔宽度。
fn width_at_distance(distance: f32, total_length: f32, body_width: f32) -> f32 {
    // 单点笔画（落笔未移动）使用起笔宽度，避免出现大圆点
    if total_length == 0.0 {
        return body_width * START_TIP_WIDTH_RATIO;
    }

    if total_length <= body_width * SHORT_STROKE_BODY_WIDTHS {
        return body_width;
    }

    let start_taper_length =
        (body_width * START_TAPER_BODY_WIDTHS).min(total_length * START_TAPER_LENGTH_RATIO);
    let end_taper_length =
        (body_width * END_TAPER_BODY_WIDTHS).min(total_length * END_TAPER_LENGTH_RATIO);
    let start_progress = smoothstep((distance / start_taper_length).clamp(0.0, 1.0));
    let end_progress = smoothstep(((total_length - distance) / end_taper_length).clamp(0.0, 1.0));
    let start_width =
        body_width * (START_TIP_WIDTH_RATIO + (1.0 - START_TIP_WIDTH_RATIO) * start_progress);
    let end_width = body_width * (END_TIP_WIDTH_RATIO + (1.0 - END_TIP_WIDTH_RATIO) * end_progress);
    start_width.min(end_width).clamp(0.0, body_width)
}

/// 返回端点一阶导数为零的三次平滑插值。
fn smoothstep(value: f32) -> f32 {
    value * value * (3.0 - 2.0 * value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建指定主体宽度和水平采样间距的自然笔锋。
    fn horizontal_builder(body_width: f32, length: f32, step: f32) -> NaturalStrokeBuilder {
        let mut builder = NaturalStrokeBuilder::new(CanvasPoint::new(0.0, 0.0), body_width)
            .expect("测试参数应创建有效构建器");
        let mut x = step;
        while x < length {
            assert!(builder.push(CanvasPoint::new(x, 0.0)));
            x += step;
        }
        assert!(builder.push(CanvasPoint::new(length, 0.0)));
        builder
    }

    /// 验证四档长笔画都保留所选主体宽度和预定端点比例。
    #[test]
    fn supported_widths_keep_full_width_bodies_and_scaled_tips() {
        for body_width in [4.0, 6.0, 8.0, 16.0] {
            let points = horizontal_builder(body_width, body_width * 10.0, body_width * 0.25)
                .finalized_points();
            let first = points.first().expect("长笔画应有起点");
            let last = points.last().expect("长笔画应有终点");

            assert!((first.width - body_width * START_TIP_WIDTH_RATIO).abs() < 0.001);
            assert!((last.width - body_width * END_TIP_WIDTH_RATIO).abs() < 0.001);
            assert!(points.iter().any(|point| point.width == body_width));
            assert!(points.iter().all(|point| point.width <= body_width));
        }
    }

    /// 验证起笔单调增宽、主体全宽且收笔单调变窄。
    #[test]
    fn taper_regions_are_monotonic_around_a_full_width_body() {
        let body_width = 8.0;
        let builder = horizontal_builder(body_width, 80.0, 1.0);
        let points = builder.finalized_points();
        let start_length = body_width * START_TAPER_BODY_WIDTHS;
        let end_start = builder.total_length - body_width * END_TAPER_BODY_WIDTHS;

        let start_widths: Vec<_> = points
            .iter()
            .filter(|point| point.point.x <= start_length)
            .map(|point| point.width)
            .collect();
        let end_widths: Vec<_> = points
            .iter()
            .filter(|point| point.point.x >= end_start)
            .map(|point| point.width)
            .collect();

        assert!(start_widths.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(end_widths.windows(2).all(|pair| pair[0] >= pair[1]));
        assert!(points.iter().any(|point| point.width == body_width));
    }

    /// 验证单点使用起笔宽度避免大圆点，短笔画保持主体宽度。
    #[test]
    fn short_strokes_keep_the_body_width() {
        let body_width = 8.0;
        let point = NaturalStrokeBuilder::new(CanvasPoint::new(0.0, 0.0), body_width)
            .expect("单点应创建构建器")
            .finalized_points();
        let short = horizontal_builder(body_width, body_width * 2.5, 1.0).finalized_points();
        let boundary = horizontal_builder(body_width, body_width * 3.0, 1.0).finalized_points();

        assert_eq!(point[0].width, body_width * START_TIP_WIDTH_RATIO);
        assert!(short.iter().all(|point| point.width == body_width));
        assert!(boundary.iter().all(|point| point.width == body_width));
    }

    /// 验证刚超过短笔画阈值时两端开始渐细但仍保留全宽主体。
    #[test]
    fn stroke_just_above_short_threshold_keeps_a_body_section() {
        let body_width = 8.0;
        let points = horizontal_builder(body_width, body_width * 3.0 + 1.0, 0.5).finalized_points();

        assert!(points.first().is_some_and(|point| point.width < body_width));
        assert!(points.last().is_some_and(|point| point.width < body_width));
        assert!(points.iter().any(|point| point.width == body_width));
    }

    /// 验证相同几何位置的宽度不受中间采样密度影响。
    #[test]
    fn sampling_density_does_not_change_width_at_shared_positions() {
        let sparse = horizontal_builder(8.0, 100.0, 20.0).finalized_points();
        let dense = horizontal_builder(8.0, 100.0, 5.0).finalized_points();

        for sparse_point in sparse {
            let dense_point = dense
                .iter()
                .find(|point| point.point == sparse_point.point)
                .expect("密集采样应包含稀疏采样位置");
            assert!((dense_point.width - sparse_point.width).abs() < 0.001);
        }
    }

    /// 验证非有限点和不足最小位移的重复点不会进入笔画。
    #[test]
    fn invalid_and_overlapping_points_are_rejected() {
        let mut builder = NaturalStrokeBuilder::new(CanvasPoint::new(0.0, 0.0), 8.0)
            .expect("有效起点应创建构建器");

        assert!(!builder.push(CanvasPoint::new(f32::NAN, 0.0)));
        assert!(!builder.push(CanvasPoint::new(0.25, 0.25)));
        assert!(builder.push(CanvasPoint::new(1.0, 0.0)));
        assert_eq!(builder.finalized_points().len(), 2);
    }
}
