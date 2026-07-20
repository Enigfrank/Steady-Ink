use super::CanvasPoint;

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
}
