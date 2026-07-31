use super::{CanvasPoint, VariableStrokePoint};

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

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证逐点宽度轮廓包含有限的左右边界并保持原始点数。
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
