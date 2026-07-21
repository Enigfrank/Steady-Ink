use super::{CanvasPoint, VariableStrokePoint};

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

    for pair in points[1..].windows(2) {
        let control = pair[0];
        let next = pair[1];
        visit(StrokeSegment::QuadTo {
            control,
            end: midpoint(control, next),
        });
    }
    visit(StrokeSegment::LineTo(points[points.len() - 1]));
}

/// 根据逐点宽度生成左右边界组成的单个闭合轮廓。
pub(crate) fn variable_outline(points: &[VariableStrokePoint]) -> Option<Vec<CanvasPoint>> {
    if points.len() < 2 {
        return None;
    }
    let mut left = Vec::with_capacity(points.len());
    let mut right = Vec::with_capacity(points.len());
    for (index, sample) in points.iter().enumerate() {
        if !sample.point.x.is_finite()
            || !sample.point.y.is_finite()
            || !sample.width.is_finite()
            || sample.width < 0.0
        {
            return None;
        }
        let (tangent_x, tangent_y) = stable_tangent(points, index);
        let half_width = sample.width / 2.0;
        let normal_x = -tangent_y * half_width;
        let normal_y = tangent_x * half_width;
        left.push(CanvasPoint::new(
            sample.point.x + normal_x,
            sample.point.y + normal_y,
        ));
        right.push(CanvasPoint::new(
            sample.point.x - normal_x,
            sample.point.y - normal_y,
        ));
    }
    right.reverse();
    left.extend(right);
    Some(left)
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
}
