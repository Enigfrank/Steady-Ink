use super::{CanvasPoint, VariableStrokePoint};

const LOCAL_SMOOTHING_MAX_DISTANCE_SQUARED: f32 = 4.0;
const LOCAL_SMOOTHING_NEIGHBOR_WEIGHT: f32 = 0.25;
const LOCAL_SMOOTHING_MIN_DIRECTION_DOT: f32 = 0.5;
const DECELERATION_DISTANCE_RATIO_SQUARED: f32 = 4.0;
const SIGNIFICANT_WIDTH_FEATURE_RATIO: f32 = 0.1;

/// 一个慢速点应保留原位或使用的局部拟合窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalSmoothingWindow {
    Preserve,
    ThreePoint,
    FivePoint,
}

/// 平滑画笔中心线中的一个局部路径段。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum StrokeSegment {
    LineTo(CanvasPoint),
    QuadTo {
        control: CanvasPoint,
        end: CanvasPoint,
    },
}

/// 按输入顺序访问局部二次平滑段，不分配中间段集合。
pub(crate) fn visit_smoothed_segments(
    points: &[CanvasPoint],
    mut visit: impl FnMut(StrokeSegment),
) {
    if points.len() < 2 {
        return;
    }
    if points.len() == 2 {
        visit(StrokeSegment::LineTo(points[1]));
        return;
    }

    let mut control = locally_smoothed_point(points, 1);
    for index in 2..points.len() {
        let next = locally_smoothed_point(points, index);
        visit(StrokeSegment::QuadTo {
            control,
            end: midpoint(control, next),
        });
        control = next;
    }
    visit(StrokeSegment::LineTo(points[points.len() - 1]));
}

/// 抑制慢速像素级阶梯抖动，同时保留端点、转角和减速锚点。
fn locally_smoothed_point(points: &[CanvasPoint], index: usize) -> CanvasPoint {
    let point_at = |point_index| points[point_index];
    let window = local_smoothing_window(points.len(), index, &point_at);
    smoothed_point_at(index, window, &point_at)
}

/// 根据慢速跨度、宏观方向和速度变化选择局部拟合窗口。
fn local_smoothing_window(
    point_count: usize,
    index: usize,
    point_at: &impl Fn(usize) -> CanvasPoint,
) -> LocalSmoothingWindow {
    if index == 0 || index + 1 >= point_count {
        return LocalSmoothingWindow::Preserve;
    }

    let current = point_at(index);
    let incoming_distance = distance_squared(point_at(index - 1), current);
    let outgoing_distance = distance_squared(current, point_at(index + 1));
    if incoming_distance > LOCAL_SMOOTHING_MAX_DISTANCE_SQUARED
        || outgoing_distance > LOCAL_SMOOTHING_MAX_DISTANCE_SQUARED
        || has_abrupt_span_change(incoming_distance, outgoing_distance)
    {
        return LocalSmoothingWindow::Preserve;
    }

    let before = point_at(index.saturating_sub(2));
    let after = point_at((index + 2).min(point_count - 1));
    let Some(incoming_direction) = direction_between(before, current) else {
        return LocalSmoothingWindow::Preserve;
    };
    let Some(outgoing_direction) = direction_between(current, after) else {
        return LocalSmoothingWindow::Preserve;
    };
    let direction_dot = incoming_direction.0.mul_add(
        outgoing_direction.0,
        incoming_direction.1 * outgoing_direction.1,
    );
    if direction_dot < LOCAL_SMOOTHING_MIN_DIRECTION_DOT {
        return LocalSmoothingWindow::Preserve;
    }

    if index >= 2
        && index + 2 < point_count
        && ((index - 2)..(index + 2)).all(|span_index| {
            distance_squared(point_at(span_index), point_at(span_index + 1))
                <= LOCAL_SMOOTHING_MAX_DISTANCE_SQUARED
        })
    {
        LocalSmoothingWindow::FivePoint
    } else {
        LocalSmoothingWindow::ThreePoint
    }
}

/// 按已选三点或五点窗口返回一个拟合后的中心点。
fn smoothed_point_at(
    index: usize,
    window: LocalSmoothingWindow,
    point_at: &impl Fn(usize) -> CanvasPoint,
) -> CanvasPoint {
    CanvasPoint::new(
        smoothed_scalar(index, window, &|point_index| point_at(point_index).x),
        smoothed_scalar(index, window, &|point_index| point_at(point_index).y),
    )
}

/// 判断相邻跨度是否出现足以作为减速锚点的突变。
fn has_abrupt_span_change(incoming_squared: f32, outgoing_squared: f32) -> bool {
    let shorter = incoming_squared.min(outgoing_squared);
    let longer = incoming_squared.max(outgoing_squared);
    shorter <= f32::EPSILON || longer >= shorter * DECELERATION_DISTANCE_RATIO_SQUARED
}

/// 使用归一化二项式权重拟合五个连续标量样本。
fn five_point_average(first: f32, second: f32, current: f32, fourth: f32, fifth: f32) -> f32 {
    (first + second * 4.0 + current * 6.0 + fourth * 4.0 + fifth) / 16.0
}

/// 按局部窗口拟合一个位置分量或宽度标量。
fn smoothed_scalar(
    index: usize,
    window: LocalSmoothingWindow,
    value_at: &impl Fn(usize) -> f32,
) -> f32 {
    match window {
        LocalSmoothingWindow::Preserve => value_at(index),
        LocalSmoothingWindow::ThreePoint => {
            let current_weight = 1.0 - LOCAL_SMOOTHING_NEIGHBOR_WEIGHT * 2.0;
            value_at(index - 1) * LOCAL_SMOOTHING_NEIGHBOR_WEIGHT
                + value_at(index) * current_weight
                + value_at(index + 1) * LOCAL_SMOOTHING_NEIGHBOR_WEIGHT
        }
        LocalSmoothingWindow::FivePoint => five_point_average(
            value_at(index - 2),
            value_at(index - 1),
            value_at(index),
            value_at(index + 1),
            value_at(index + 2),
        ),
    }
}

/// 返回两个点之间的平方距离，供局部滤波避免不必要的开方。
fn distance_squared(left: CanvasPoint, right: CanvasPoint) -> f32 {
    let delta_x = right.x - left.x;
    let delta_y = right.y - left.y;
    delta_x.mul_add(delta_x, delta_y * delta_y)
}

/// 根据逐点宽度生成左右边界组成的单个闭合轮廓。
pub(crate) fn variable_outline(points: &[VariableStrokePoint]) -> Option<Vec<CanvasPoint>> {
    if points.len() < 2 {
        return None;
    }
    let smoothed = smoothed_variable_points(points)?;
    let mut outline = Vec::with_capacity(smoothed.len() * 2);
    for (index, sample) in smoothed.iter().enumerate() {
        let (tangent_x, tangent_y) = stable_tangent(&smoothed, index);
        let half_width = sample.width / 2.0;
        let normal_x = -tangent_y * half_width;
        let normal_y = tangent_x * half_width;
        outline.push(CanvasPoint::new(
            sample.point.x + normal_x,
            sample.point.y + normal_y,
        ));
    }
    for (index, sample) in smoothed.iter().enumerate().rev() {
        let left = outline[index];
        outline.push(CanvasPoint::new(
            sample.point.x * 2.0 - left.x,
            sample.point.y * 2.0 - left.y,
        ));
    }
    Some(outline)
}

/// 使用同一局部窗口同步拟合动态笔锋的位置和宽度通道。
fn smoothed_variable_points(points: &[VariableStrokePoint]) -> Option<Vec<VariableStrokePoint>> {
    if points.iter().any(|sample| {
        !sample.point.x.is_finite()
            || !sample.point.y.is_finite()
            || !sample.width.is_finite()
            || sample.width < 0.0
    }) {
        return None;
    }

    let mut output = Vec::with_capacity(points.len());
    for index in 0..points.len() {
        let point_at = |point_index: usize| points[point_index].point;
        let window = local_smoothing_window(points.len(), index, &point_at);
        output.push(VariableStrokePoint {
            point: smoothed_point_at(index, window, &point_at),
            width: smoothed_width(points, index, window),
        });
    }
    Some(output)
}

/// 在保留明显局部峰谷的前提下按位置窗口拟合宽度。
fn smoothed_width(
    points: &[VariableStrokePoint],
    index: usize,
    window: LocalSmoothingWindow,
) -> f32 {
    if window == LocalSmoothingWindow::Preserve || is_significant_width_feature(points, index) {
        return points[index].width;
    }
    smoothed_scalar(index, window, &|point_index| points[point_index].width)
}

/// 判断一个宽度峰谷是否足以表示压力或减速特征。
fn is_significant_width_feature(points: &[VariableStrokePoint], index: usize) -> bool {
    if index == 0 || index + 1 >= points.len() {
        return false;
    }
    let previous = points[index - 1].width;
    let current = points[index].width;
    let next = points[index + 1].width;
    let is_peak = current > previous && current >= next || current >= previous && current > next;
    let is_valley = current < previous && current <= next || current <= previous && current < next;
    if !is_peak && !is_valley {
        return false;
    }
    let neighbor_average = (previous + next) / 2.0;
    let local_scale = previous.max(current).max(next).max(f32::EPSILON);
    (current - neighbor_average).abs() >= local_scale * SIGNIFICANT_WIDTH_FEATURE_RATIO
}

/// 返回指定中心点的稳定单位切向，退化时搜索最近的有效方向。
fn stable_tangent(points: &[VariableStrokePoint], index: usize) -> (f32, f32) {
    let current = points[index].point;
    let mut direction = if index == 0 {
        direction_between(current, points[1].point)
    } else if index + 1 == points.len() {
        direction_between(points[index - 1].point, current)
    } else {
        direction_between(points[index - 1].point, points[index + 1].point)
    };
    if direction.is_none() {
        for offset in 1..points.len() {
            if index >= offset {
                direction = direction_between(points[index - offset].point, current);
                if direction.is_some() {
                    break;
                }
            }
            if index + offset < points.len() {
                direction = direction_between(current, points[index + offset].point);
                if direction.is_some() {
                    break;
                }
            }
        }
    }
    direction.unwrap_or((1.0, 0.0))
}

/// 返回两点之间的有限单位方向，零长度时返回 `None`。
fn direction_between(left: CanvasPoint, right: CanvasPoint) -> Option<(f32, f32)> {
    let delta_x = right.x - left.x;
    let delta_y = right.y - left.y;
    let length = delta_x.mul_add(delta_x, delta_y * delta_y).sqrt();
    (length.is_finite() && length > f32::EPSILON).then_some((delta_x / length, delta_y / length))
}

/// 返回两个画布点的中点。
const fn midpoint(left: CanvasPoint, right: CanvasPoint) -> CanvasPoint {
    CanvasPoint::new((left.x + right.x) / 2.0, (left.y + right.y) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 收集平滑段，供纯几何断言复用。
    fn segments(points: &[CanvasPoint]) -> Vec<StrokeSegment> {
        let mut segments = Vec::new();
        visit_smoothed_segments(points, |segment| segments.push(segment));
        segments
    }

    /// 验证不足两个点时不产生路径段。
    #[test]
    fn empty_and_single_point_paths_have_no_segments() {
        assert!(segments(&[]).is_empty());
        assert!(segments(&[CanvasPoint::new(4.0, 8.0)]).is_empty());
    }

    /// 验证两个点保留一条直线，避免短笔画被过度拟合。
    #[test]
    fn two_points_keep_a_line_segment() {
        let points = [CanvasPoint::new(0.0, 0.0), CanvasPoint::new(8.0, 4.0)];

        assert_eq!(
            segments(&points),
            vec![StrokeSegment::LineTo(CanvasPoint::new(8.0, 4.0))]
        );
    }

    /// 验证三点路径使用局部控制点并保留真实终点。
    #[test]
    fn three_points_use_a_quadratic_midpoint_and_real_tail() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(4.0, 8.0),
            CanvasPoint::new(12.0, 4.0),
        ];

        assert_eq!(
            segments(&points),
            vec![
                StrokeSegment::QuadTo {
                    control: CanvasPoint::new(4.0, 8.0),
                    end: CanvasPoint::new(8.0, 6.0),
                },
                StrokeSegment::LineTo(CanvasPoint::new(12.0, 4.0)),
            ]
        );
    }

    /// 验证缓慢移动形成的 1px 阶梯点列被压向连续对角线，同时保留真实终点。
    #[test]
    fn slow_pixel_staircase_is_locally_smoothed() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(1.0, 0.0),
            CanvasPoint::new(1.0, 1.0),
            CanvasPoint::new(2.0, 1.0),
            CanvasPoint::new(2.0, 2.0),
        ];

        assert_eq!(
            segments(&points),
            vec![
                StrokeSegment::QuadTo {
                    control: CanvasPoint::new(0.75, 0.25),
                    end: CanvasPoint::new(1.0, 0.5),
                },
                StrokeSegment::QuadTo {
                    control: CanvasPoint::new(1.25, 0.75),
                    end: CanvasPoint::new(1.5, 1.0),
                },
                StrokeSegment::QuadTo {
                    control: CanvasPoint::new(1.75, 1.25),
                    end: CanvasPoint::new(1.875, 1.625),
                },
                StrokeSegment::LineTo(CanvasPoint::new(2.0, 2.0)),
            ]
        );
    }

    /// 验证方向连续的慢速段使用五点窗口吸收更宽范围的像素抖动。
    #[test]
    fn continuous_slow_path_uses_five_point_context() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(1.0, 0.0),
            CanvasPoint::new(1.0, 1.0),
            CanvasPoint::new(2.0, 1.0),
            CanvasPoint::new(3.0, 1.0),
        ];

        assert_eq!(
            locally_smoothed_point(&points, 2),
            CanvasPoint::new(1.3125, 0.6875)
        );
    }

    /// 验证连续小跨度形成的真实直角不会被当成像素阶梯拉圆。
    #[test]
    fn deliberate_small_corner_keeps_its_anchor() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(1.0, 0.0),
            CanvasPoint::new(2.0, 0.0),
            CanvasPoint::new(2.0, 1.0),
            CanvasPoint::new(2.0, 2.0),
        ];

        assert_eq!(locally_smoothed_point(&points, 2), points[2]);
    }

    /// 验证相邻跨度变化达到两倍时保留减速锚点。
    #[test]
    fn abrupt_slowdown_keeps_its_anchor() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(2.0, 0.0),
            CanvasPoint::new(2.5, 0.0),
            CanvasPoint::new(3.0, 0.0),
        ];

        assert_eq!(locally_smoothed_point(&points, 1), points[1]);
    }

    /// 验证长路径每个内部点只生成一个局部二次段。
    #[test]
    fn long_path_has_linear_segment_count() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(2.0, 4.0),
            CanvasPoint::new(6.0, 8.0),
            CanvasPoint::new(12.0, 4.0),
            CanvasPoint::new(20.0, 0.0),
        ];
        let segments = segments(&points);

        assert_eq!(segments.len(), points.len() - 1);
        assert_eq!(
            segments.last(),
            Some(&StrokeSegment::LineTo(CanvasPoint::new(20.0, 0.0)))
        );
    }

    /// 验证逐点宽度轮廓包含有限的左右边界并保持线性点数。
    #[test]
    fn variable_outline_is_finite_and_linear() {
        let points = [
            VariableStrokePoint {
                point: CanvasPoint::new(0.0, 0.0),
                width: 4.0,
            },
            VariableStrokePoint {
                point: CanvasPoint::new(8.0, 0.0),
                width: 8.0,
            },
            VariableStrokePoint {
                point: CanvasPoint::new(16.0, 4.0),
                width: 4.0,
            },
        ];
        let outline = variable_outline(&points).expect("多点轮廓应生成");

        assert_eq!(outline.len(), points.len() * 2);
        assert!(
            outline
                .iter()
                .all(|point| point.x.is_finite() && point.y.is_finite())
        );
    }

    /// 验证完全重合中心点不会产生 NaN，并使用稳定退化方向。
    #[test]
    fn variable_outline_handles_repeated_centers() {
        let points = [
            VariableStrokePoint {
                point: CanvasPoint::new(4.0, 4.0),
                width: 2.0,
            },
            VariableStrokePoint {
                point: CanvasPoint::new(4.0, 4.0),
                width: 2.0,
            },
        ];

        assert!(
            variable_outline(&points)
                .expect("退化多点轮廓仍应生成")
                .iter()
                .all(|point| point.x.is_finite() && point.y.is_finite())
        );
    }

    /// 验证动态笔锋的位置和普通宽度变化共享同一个五点窗口。
    #[test]
    fn variable_position_and_width_share_the_fitting_window() {
        let points = [
            variable_point(0.0, 0.0, 4.0),
            variable_point(1.0, 0.0, 6.0),
            variable_point(1.0, 1.0, 7.0),
            variable_point(2.0, 1.0, 10.0),
            variable_point(3.0, 1.0, 12.0),
        ];
        let smoothed = smoothed_variable_points(&points).expect("动态笔锋应可拟合");

        assert_eq!(smoothed[2].point, CanvasPoint::new(1.3125, 0.6875));
        assert_eq!(smoothed[2].width, 7.625);
    }

    /// 验证明显的局部宽度峰作为压力或减速特征保持原值。
    #[test]
    fn significant_width_peak_is_preserved() {
        let points = [
            variable_point(0.0, 0.0, 4.0),
            variable_point(1.0, 0.0, 6.0),
            variable_point(1.0, 1.0, 12.0),
            variable_point(2.0, 1.0, 6.0),
            variable_point(3.0, 1.0, 4.0),
        ];
        let smoothed = smoothed_variable_points(&points).expect("动态笔锋应可拟合");

        assert_eq!(smoothed[2].width, 12.0);
    }

    /// 创建动态笔锋测试点并保持用例只表达几何意图。
    fn variable_point(x: f32, y: f32, width: f32) -> VariableStrokePoint {
        VariableStrokePoint {
            point: CanvasPoint::new(x, y),
            width,
        }
    }
}
