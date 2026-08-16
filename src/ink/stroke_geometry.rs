use skia_safe::PathBuilder;

use super::{CanvasPoint, VariableStrokePoint};

const FILTER_PREVIOUS_WEIGHT: f32 = 0.25;
const FILTER_CURRENT_WEIGHT: f32 = 0.5;
const FILTER_NEXT_WEIGHT: f32 = 0.25;
const CATMULL_ROM_CONTROL_SCALE: f32 = 1.0 / 6.0;

/// 一段由 Catmull-Rom 控制点转换出的三次贝塞尔曲线。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CubicBezierSegment {
    pub(crate) start: CanvasPoint,
    pub(crate) control1: CanvasPoint,
    pub(crate) control2: CanvasPoint,
    pub(crate) end: CanvasPoint,
}

/// 对位置序列执行一次端点保留的轻量三点加权滤波。
pub(crate) fn light_filter_points(points: &[CanvasPoint]) -> Option<Vec<CanvasPoint>> {
    if points.is_empty() || points.iter().any(|point| !finite_point(*point)) {
        return None;
    }
    if points.len() < 3 {
        return Some(points.to_vec());
    }

    let mut filtered = Vec::with_capacity(points.len());
    filtered.push(points[0]);
    for window in points.windows(3) {
        filtered.push(weighted_point(window[0], window[1], window[2]));
    }
    filtered.push(*points.last().expect("至少三个点时必须存在末点"));
    filtered
        .iter()
        .all(|point| finite_point(*point))
        .then_some(filtered)
}

/// 只滤波动态笔锋的位置，保留每个采样点的原始压力宽度。
pub(crate) fn light_filter_variable_points(
    points: &[VariableStrokePoint],
) -> Option<Vec<VariableStrokePoint>> {
    if points.is_empty()
        || points.iter().any(|sample| {
            !finite_point(sample.point) || !sample.width.is_finite() || sample.width < 0.0
        })
    {
        return None;
    }
    let centers: Vec<_> = points.iter().map(|sample| sample.point).collect();
    let filtered_centers = light_filter_points(&centers)?;
    Some(
        points
            .iter()
            .zip(filtered_centers)
            .map(|(sample, point)| VariableStrokePoint {
                point,
                width: sample.width,
            })
            .collect(),
    )
}

/// 把已经滤波的开放点序列转换为连续三次贝塞尔路径。
pub(crate) fn append_open_bezier_path(
    path_builder: &mut PathBuilder,
    filtered_points: &[CanvasPoint],
) -> bool {
    let mut started = false;
    for_each_open_bezier_segment(filtered_points, |segment| {
        if !started {
            path_builder.move_to((segment.start.x, segment.start.y));
            started = true;
        }
        append_cubic_segment(path_builder, segment);
    });
    started
}

/// 把已经滤波的闭合轮廓转换为连续三次贝塞尔路径。
pub(crate) fn append_closed_bezier_path(
    path_builder: &mut PathBuilder,
    filtered_points: &[CanvasPoint],
) -> bool {
    let mut started = false;
    for_each_closed_bezier_segment(filtered_points, |segment| {
        if !started {
            path_builder.move_to((segment.start.x, segment.start.y));
            started = true;
        }
        append_cubic_segment(path_builder, segment);
    });
    if started {
        path_builder.close();
    }
    started
}

/// 根据逐点宽度生成左右边界组成的单个闭合轮廓。
pub(crate) fn variable_outline(points: &[VariableStrokePoint]) -> Option<Vec<CanvasPoint>> {
    if points.len() < 2
        || points.iter().any(|sample| {
            !sample.point.x.is_finite()
                || !sample.point.y.is_finite()
                || !sample.width.is_finite()
                || sample.width < 0.0
        })
    {
        return None;
    }

    let mut outline = Vec::with_capacity(points.len() * 2);
    for (index, sample) in points.iter().enumerate() {
        let (tangent_x, tangent_y) = stable_tangent(points, index);
        let half_width = sample.width / 2.0;
        let normal_x = -tangent_y * half_width;
        let normal_y = tangent_x * half_width;
        outline.push(CanvasPoint::new(
            sample.point.x + normal_x,
            sample.point.y + normal_y,
        ));
    }
    for (index, sample) in points.iter().enumerate().rev() {
        let left = outline[index];
        outline.push(CanvasPoint::new(
            sample.point.x * 2.0 - left.x,
            sample.point.y * 2.0 - left.y,
        ));
    }
    Some(outline)
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

/// 判断点坐标是否可安全参与滤波和曲线计算。
fn finite_point(point: CanvasPoint) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

/// 使用固定三点权重计算一个内部滤波点。
fn weighted_point(previous: CanvasPoint, current: CanvasPoint, next: CanvasPoint) -> CanvasPoint {
    CanvasPoint::new(
        previous.x * FILTER_PREVIOUS_WEIGHT
            + current.x * FILTER_CURRENT_WEIGHT
            + next.x * FILTER_NEXT_WEIGHT,
        previous.y * FILTER_PREVIOUS_WEIGHT
            + current.y * FILTER_CURRENT_WEIGHT
            + next.y * FILTER_NEXT_WEIGHT,
    )
}

/// 遍历已经滤波的开放序列对应的 Catmull-Rom 贝塞尔段。
pub(crate) fn for_each_open_bezier_segment(
    filtered_points: &[CanvasPoint],
    mut visit: impl FnMut(CubicBezierSegment),
) -> bool {
    if filtered_points.len() < 2 {
        return false;
    }
    let bounds =
        PointBounds::from_points(filtered_points).expect("开放曲线的有效点序列必须拥有包围盒");
    for index in 0..filtered_points.len() - 1 {
        let previous = if index == 0 {
            filtered_points[index]
        } else {
            filtered_points[index - 1]
        };
        let start = filtered_points[index];
        let end = filtered_points[index + 1];
        let next = filtered_points.get(index + 2).copied().unwrap_or(end);
        visit(catmull_rom_segment(previous, start, end, next, bounds));
    }
    true
}

/// 返回开放曲线指定索引尚未应用全局 bounds clamp 的 Catmull-Rom 段。
pub(crate) fn open_bezier_segment_unclamped_at(
    filtered_points: &[CanvasPoint],
    index: usize,
) -> Option<CubicBezierSegment> {
    if index + 1 >= filtered_points.len() {
        return None;
    }
    let previous = if index == 0 {
        filtered_points[index]
    } else {
        filtered_points[index - 1]
    };
    let start = filtered_points[index];
    let end = filtered_points[index + 1];
    let next = filtered_points.get(index + 2).copied().unwrap_or(end);
    Some(catmull_rom_segment_unclamped(previous, start, end, next))
}

/// 遍历已经滤波的闭合轮廓对应的 Catmull-Rom 贝塞尔段。
pub(crate) fn for_each_closed_bezier_segment(
    filtered_points: &[CanvasPoint],
    mut visit: impl FnMut(CubicBezierSegment),
) -> bool {
    if filtered_points.len() < 3 {
        return false;
    }
    let bounds =
        PointBounds::from_points(filtered_points).expect("闭合曲线的有效点序列必须拥有包围盒");
    let point_count = filtered_points.len();
    for index in 0..point_count {
        let previous = filtered_points[(index + point_count - 1) % point_count];
        let start = filtered_points[index];
        let end = filtered_points[(index + 1) % point_count];
        let next = filtered_points[(index + 2) % point_count];
        visit(catmull_rom_segment(previous, start, end, next, bounds));
    }
    true
}

/// 把一段 Catmull-Rom 相邻点转换为带边界保护的三次贝塞尔段。
fn catmull_rom_segment(
    previous: CanvasPoint,
    start: CanvasPoint,
    end: CanvasPoint,
    next: CanvasPoint,
    bounds: PointBounds,
) -> CubicBezierSegment {
    clamp_cubic_segment(
        catmull_rom_segment_unclamped(previous, start, end, next),
        bounds,
    )
}

/// 把一段 Catmull-Rom 相邻点转换为尚未应用全局 bounds 的三次贝塞尔段。
fn catmull_rom_segment_unclamped(
    previous: CanvasPoint,
    start: CanvasPoint,
    end: CanvasPoint,
    next: CanvasPoint,
) -> CubicBezierSegment {
    let control1 = CanvasPoint::new(
        start.x + (end.x - previous.x) * CATMULL_ROM_CONTROL_SCALE,
        start.y + (end.y - previous.y) * CATMULL_ROM_CONTROL_SCALE,
    );
    let control2 = CanvasPoint::new(
        end.x - (next.x - start.x) * CATMULL_ROM_CONTROL_SCALE,
        end.y - (next.y - start.y) * CATMULL_ROM_CONTROL_SCALE,
    );
    CubicBezierSegment {
        start,
        control1,
        control2,
        end,
    }
}

/// 仅限制一段曲线的控制点，端点已经来自全局 bounds 内的滤波序列。
fn clamp_cubic_segment(mut segment: CubicBezierSegment, bounds: PointBounds) -> CubicBezierSegment {
    segment.control1 = bounds.clamp(segment.control1);
    segment.control2 = bounds.clamp(segment.control2);
    segment
}

/// 把一个贝塞尔段追加到 Skia 路径。
fn append_cubic_segment(path_builder: &mut PathBuilder, segment: CubicBezierSegment) {
    path_builder.cubic_to(
        (segment.control1.x, segment.control1.y),
        (segment.control2.x, segment.control2.y),
        (segment.end.x, segment.end.y),
    );
}

/// 用于约束曲线控制点、避免平滑路径超出原始几何包围盒的范围。
#[derive(Clone, Copy)]
struct PointBounds {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl PointBounds {
    /// 从有限点序列计算坐标包围盒。
    fn from_points(points: &[CanvasPoint]) -> Option<Self> {
        let first = points.first().copied()?;
        if !finite_point(first) {
            return None;
        }
        let mut bounds = Self {
            left: first.x,
            top: first.y,
            right: first.x,
            bottom: first.y,
        };
        for point in &points[1..] {
            if !finite_point(*point) {
                return None;
            }
            bounds.left = bounds.left.min(point.x);
            bounds.top = bounds.top.min(point.y);
            bounds.right = bounds.right.max(point.x);
            bounds.bottom = bounds.bottom.max(point.y);
        }
        Some(bounds)
    }

    /// 将一个控制点限制在路径点的坐标包围盒内。
    const fn clamp(self, point: CanvasPoint) -> CanvasPoint {
        CanvasPoint::new(
            point.x.clamp(self.left, self.right),
            point.y.clamp(self.top, self.bottom),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证轻量滤波保持端点并衰减中间点的瞬时偏移。
    #[test]
    fn light_filter_preserves_endpoints_and_damps_middle() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(4.0, 8.0),
            CanvasPoint::new(8.0, 0.0),
        ];
        let filtered = light_filter_points(&points).expect("有效点序列应完成滤波");

        assert_eq!(filtered[0], points[0]);
        assert_eq!(filtered[1], CanvasPoint::new(4.0, 4.0));
        assert_eq!(filtered[2], points[2]);
    }

    /// 验证开放 Catmull-Rom 路径为每个相邻点对生成一个贝塞尔段。
    #[test]
    fn open_curve_generates_cubic_segments() {
        let points = light_filter_points(&[
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(4.0, 4.0),
            CanvasPoint::new(8.0, 0.0),
        ])
        .expect("有效点序列应完成滤波");
        let mut segments = Vec::new();
        assert!(for_each_open_bezier_segment(&points, |segment| segments.push(segment)));

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start, points[0]);
        assert_eq!(segments[0].end, points[1]);
        assert_eq!(segments[1].start, points[1]);
        assert_eq!(segments[1].end, points[2]);
        assert!(
            segments
                .iter()
                .flat_map(|segment| [segment.control1, segment.control2])
                .all(finite_point)
        );
    }

    /// 验证闭合 Catmull-Rom 路径在末段回到首点。
    #[test]
    fn closed_curve_returns_to_first_point() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(8.0, 0.0),
            CanvasPoint::new(8.0, 8.0),
            CanvasPoint::new(0.0, 8.0),
        ];
        let mut segments = Vec::new();
        assert!(for_each_closed_bezier_segment(&points, |segment| segments.push(segment)));

        assert_eq!(segments.len(), points.len());
        assert_eq!(segments.last().expect("闭合路径应有末段").end, points[0]);
    }

    /// 验证动态笔锋滤波只改变位置而保留原始宽度。
    #[test]
    fn variable_filter_preserves_widths() {
        let points = [
            VariableStrokePoint {
                point: CanvasPoint::new(0.0, 0.0),
                width: 2.0,
            },
            VariableStrokePoint {
                point: CanvasPoint::new(4.0, 8.0),
                width: 6.0,
            },
            VariableStrokePoint {
                point: CanvasPoint::new(8.0, 0.0),
                width: 4.0,
            },
        ];
        let filtered = light_filter_variable_points(&points).expect("有效动态点应完成滤波");

        assert_eq!(
            filtered
                .iter()
                .map(|sample| sample.width)
                .collect::<Vec<_>>(),
            points.iter().map(|sample| sample.width).collect::<Vec<_>>()
        );
        assert_eq!(filtered[1].point, CanvasPoint::new(4.0, 4.0));
    }

    /// 验证非有限位置和非法宽度不会进入滤波或曲线阶段。
    #[test]
    fn filters_reject_invalid_geometry() {
        assert!(light_filter_points(&[CanvasPoint::new(f32::NAN, 0.0)]).is_none());
        assert!(
            light_filter_variable_points(&[VariableStrokePoint {
                point: CanvasPoint::new(0.0, 0.0),
                width: -1.0,
            }])
            .is_none()
        );
    }

    /// 验证逐点宽度轮廓包含有限的左右边界并保持原始点数。
    #[test]
    fn variable_outline_is_finite() {
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

    /// 验证非法压力宽度不会生成不可渲染的轮廓。
    #[test]
    fn variable_outline_rejects_invalid_width() {
        let points = [
            VariableStrokePoint {
                point: CanvasPoint::new(0.0, 0.0),
                width: 4.0,
            },
            VariableStrokePoint {
                point: CanvasPoint::new(1.0, 0.0),
                width: f32::NAN,
            },
        ];

        assert!(variable_outline(&points).is_none());
    }
}
