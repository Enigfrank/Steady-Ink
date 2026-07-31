use skia_safe::{Canvas, Color, Paint, PaintCap, PaintJoin, PaintStyle, PathBuilder};

use super::{
    CanvasPoint, DrawStrokeShape, InkColor, InkOperation, PenWidth,
    stroke_geometry::{append_open_bezier_path, light_filter_points},
};

const MAX_OPERATIONS_PER_BATCH: usize = 100;

/// 将连续同属性的固定宽度笔画合并为较少的 Skia 路径绘制。
pub(crate) struct BatchDrawer {
    current: Option<StrokeBatch>,
}

/// 一个使用相同颜色和宽度绘制的固定宽度笔画批次。
struct StrokeBatch {
    color: InkColor,
    width: PenWidth,
    path_builder: PathBuilder,
    operation_count: usize,
}

impl BatchDrawer {
    /// 创建没有待提交笔画的批处理器。
    pub(crate) const fn new() -> Self {
        Self { current: None }
    }

    /// 尝试把一个操作加入当前批次；属性不匹配或不可合并时返回 `false`。
    pub(crate) fn try_add(&mut self, operation: &InkOperation) -> bool {
        let InkOperation::DrawStroke(stroke) = operation else {
            return false;
        };
        let DrawStrokeShape::Fixed { points, width } = &stroke.shape else {
            return false;
        };
        if points.len() < 2 {
            return false;
        }

        match self.current.as_mut() {
            Some(batch)
                if batch.color == stroke.color
                    && batch.width == *width
                    && batch.operation_count < MAX_OPERATIONS_PER_BATCH =>
            {
                batch.append(points);
                true
            }
            Some(_) => false,
            None => {
                let Some(batch) = StrokeBatch::new(stroke.color, *width, points) else {
                    return false;
                };
                self.current = Some(batch);
                true
            }
        }
    }

    /// 将当前批次作为一次路径绘制提交，并返回产生的绘制调用数。
    pub(crate) fn flush(&mut self, canvas: &Canvas) -> usize {
        let Some(mut batch) = self.current.take() else {
            return 0;
        };
        let rgba = batch.color.rgba();
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_color(Color::from_argb(rgba[3], rgba[0], rgba[1], rgba[2]));
        paint.set_style(PaintStyle::Stroke);
        paint.set_stroke_width(batch.width.pixels());
        paint.set_stroke_cap(PaintCap::Round);
        paint.set_stroke_join(PaintJoin::Round);
        canvas.draw_path(&batch.path_builder.detach(), &paint);
        1
    }

    /// 返回当前尚未提交的操作数量。
    #[cfg(test)]
    fn pending_operation_count(&self) -> usize {
        self.current
            .as_ref()
            .map_or(0, |batch| batch.operation_count)
    }
}

impl Default for BatchDrawer {
    /// 创建默认的空批处理器。
    fn default() -> Self {
        Self::new()
    }
}

impl StrokeBatch {
    /// 用第一条固定宽度笔画创建批次。
    fn new(color: InkColor, width: PenWidth, points: &[CanvasPoint]) -> Option<Self> {
        let mut batch = Self {
            color,
            width,
            path_builder: PathBuilder::new(),
            operation_count: 0,
        };
        batch.append(points).then_some(batch)
    }

    /// 将一条固定宽度笔画滤波后作为独立贝塞尔子路径追加到批次。
    fn append(&mut self, points: &[CanvasPoint]) -> bool {
        let Some(filtered_points) = light_filter_points(points) else {
            return false;
        };
        if !append_open_bezier_path(&mut self.path_builder, &filtered_points) {
            return false;
        }
        self.operation_count += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::{DrawStroke, OperationId, VariableStrokePoint};

    /// 创建可参与批处理的固定宽度笔画。
    fn fixed_stroke(id: u64, color: InkColor, width: PenWidth) -> InkOperation {
        InkOperation::DrawStroke(
            DrawStroke::new(
                OperationId::new(id),
                vec![
                    CanvasPoint::new(id as f32, 0.0),
                    CanvasPoint::new(id as f32, 8.0),
                ],
                color,
                width,
            )
            .expect("两点固定宽度笔画应有效"),
        )
    }

    /// 验证连续同色同宽笔画会进入同一批次。
    #[test]
    fn matching_fixed_strokes_share_batch() {
        let mut drawer = BatchDrawer::new();

        assert!(drawer.try_add(&fixed_stroke(1, InkColor::Red, PenWidth::Px4)));
        assert!(drawer.try_add(&fixed_stroke(2, InkColor::Red, PenWidth::Px4)));

        assert_eq!(drawer.pending_operation_count(), 2);
    }

    /// 验证颜色或宽度变化要求先提交当前批次。
    #[test]
    fn changed_style_requires_flush() {
        let mut drawer = BatchDrawer::new();
        assert!(drawer.try_add(&fixed_stroke(1, InkColor::Red, PenWidth::Px4)));

        assert!(!drawer.try_add(&fixed_stroke(2, InkColor::Blue, PenWidth::Px4)));
        assert!(!drawer.try_add(&fixed_stroke(3, InkColor::Red, PenWidth::Px8)));

        assert_eq!(drawer.pending_operation_count(), 1);
    }

    /// 验证单点固定笔画不会因合并而丢失圆点语义。
    #[test]
    fn single_point_stroke_is_not_batched() {
        let stroke = InkOperation::DrawStroke(
            DrawStroke::new(
                OperationId::new(1),
                vec![CanvasPoint::new(4.0, 4.0)],
                InkColor::Red,
                PenWidth::Px4,
            )
            .expect("单点固定宽度笔画应有效"),
        );

        assert!(!BatchDrawer::new().try_add(&stroke));
    }

    /// 验证逐点宽度笔画不会进入固定宽度批次。
    #[test]
    fn variable_stroke_is_not_batched() {
        let stroke = InkOperation::DrawStroke(
            DrawStroke::new_variable(
                OperationId::new(1),
                vec![
                    VariableStrokePoint {
                        point: CanvasPoint::new(0.0, 0.0),
                        width: 4.0,
                    },
                    VariableStrokePoint {
                        point: CanvasPoint::new(8.0, 8.0),
                        width: 6.0,
                    },
                ],
                InkColor::Red,
            )
            .expect("逐点宽度笔画应有效"),
        );

        assert!(!BatchDrawer::new().try_add(&stroke));
    }

    /// 验证单批次达到上限后要求提交，避免路径无限增长。
    #[test]
    fn batch_size_is_bounded() {
        let mut drawer = BatchDrawer::new();
        for id in 1..=MAX_OPERATIONS_PER_BATCH as u64 {
            assert!(drawer.try_add(&fixed_stroke(id, InkColor::Red, PenWidth::Px4)));
        }

        assert!(!drawer.try_add(&fixed_stroke(101, InkColor::Red, PenWidth::Px4)));
        assert_eq!(drawer.pending_operation_count(), MAX_OPERATIONS_PER_BATCH);
    }
}
