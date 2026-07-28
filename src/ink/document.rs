use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::{
    CanvasPoint, ClearOperation, EraseSample, EraseStroke, InkBounds, InkColor, InkOperation,
    OperationId, PenWidth, VariableStrokePoint,
};
use crate::ink::DrawStroke;

/// 单个普通批注画布或单个放映位置的结构化墨迹文档。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InkDocument {
    operations: Arc<Vec<InkOperation>>,
    next_operation_id: u64,
}

impl InkDocument {
    /// 创建一个没有任何历史操作的文档。
    pub fn new() -> Self {
        Self {
            operations: Arc::new(Vec::new()),
            next_operation_id: 0,
        }
    }

    /// 追加一条画笔笔画；空点集不会产生操作。
    pub fn append_draw_stroke(
        &mut self,
        points: Vec<CanvasPoint>,
        color: InkColor,
        width: PenWidth,
    ) -> Option<OperationId> {
        if points.is_empty() {
            return None;
        }

        let id = self.allocate_operation_id();
        let stroke = DrawStroke::new(id, points, color, width)?;
        Arc::make_mut(&mut self.operations).push(InkOperation::DrawStroke(stroke));
        Some(id)
    }

    /// 追加一条已经固化逐点宽度的速度笔锋笔画。
    pub fn append_variable_draw_stroke(
        &mut self,
        points: Vec<VariableStrokePoint>,
        color: InkColor,
    ) -> Option<OperationId> {
        if points.is_empty() {
            return None;
        }

        let id = self.allocate_operation_id();
        let stroke = DrawStroke::new_variable(id, points, color)?;
        Arc::make_mut(&mut self.operations).push(InkOperation::DrawStroke(stroke));
        Some(id)
    }

    /// 追加一次普通或手掌区域擦除；空采样不会产生操作。
    pub fn append_erase_stroke(&mut self, samples: Vec<EraseSample>) -> Option<OperationId> {
        if samples.is_empty() {
            return None;
        }

        let id = self.allocate_operation_id();
        let stroke = EraseStroke::new(id, samples)?;
        Arc::make_mut(&mut self.operations).push(InkOperation::EraseStroke(stroke));
        Some(id)
    }

    /// 立即清空当前可见墨迹，并把清屏记为一次可撤销操作。
    pub fn clear(&mut self) -> Option<OperationId> {
        let affected_bounds = self.visible_bounds()?;
        let id = self.allocate_operation_id();
        Arc::make_mut(&mut self.operations).push(InkOperation::Clear(ClearOperation {
            id,
            affected_bounds: Some(affected_bounds),
        }));
        Some(id)
    }

    /// 撤销最近一次画笔、区域擦除或清屏操作。
    pub fn undo(&mut self) -> Option<InkOperation> {
        Arc::make_mut(&mut self.operations).pop()
    }

    /// 返回完整的事实历史，包括已经被后续清屏遮蔽的操作。
    pub fn operations(&self) -> &[InkOperation] {
        &self.operations
    }

    /// 按单调操作标识使用二分查找返回事实历史中的操作。
    pub fn operation(&self, id: OperationId) -> Option<&InkOperation> {
        self.operations
            .binary_search_by_key(&id.get(), |operation| operation.id().get())
            .ok()
            .map(|index| &self.operations[index])
    }

    /// 返回从最近一次清屏之后开始、需要重放到空画布上的操作。
    pub fn replay_operations(&self) -> &[InkOperation] {
        let replay_start = self
            .operations
            .iter()
            .rposition(|operation| matches!(operation, InkOperation::Clear(_)))
            .map_or(0, |clear_index| clear_index + 1);
        &self.operations[replay_start..]
    }

    /// 返回当前可见操作影响范围的保守并集。
    pub fn visible_bounds(&self) -> Option<InkBounds> {
        self.replay_operations()
            .iter()
            .filter_map(InkOperation::bounds)
            .reduce(InkBounds::union)
    }

    /// 返回文档是否没有任何事实历史。
    pub fn has_no_history(&self) -> bool {
        self.operations.is_empty()
    }

    /// 校验恢复数据的 operation id、几何数值和 next id 单调约束。
    pub(crate) fn validate_recovery(&self) -> Result<(), String> {
        if self.next_operation_id == u64::MAX {
            return Err("墨迹 operation id 已耗尽".to_owned());
        }
        let mut previous_id = 0;
        let mut visible_bounds: Option<InkBounds> = None;
        for operation in self.operations.iter() {
            let id = operation.id().get();
            if id == 0 || id <= previous_id || id > self.next_operation_id {
                return Err("墨迹 operation id 不满足严格单调约束".to_owned());
            }
            validate_operation(operation)?;
            match operation {
                InkOperation::Clear(clear) => {
                    if visible_bounds.is_none() || clear.affected_bounds != visible_bounds {
                        return Err("清屏 operation bounds 与此前可见墨迹不一致".to_owned());
                    }
                    visible_bounds = None;
                }
                _ => {
                    if let Some(bounds) = operation.bounds() {
                        visible_bounds =
                            Some(visible_bounds.map_or(bounds, |current| current.union(bounds)));
                    }
                }
            }
            previous_id = id;
        }
        if previous_id > self.next_operation_id {
            return Err("墨迹 next operation id 小于已有 operation".to_owned());
        }
        Ok(())
    }

    /// 分配文档内单调递增且不会因撤销复用的操作标识。
    fn allocate_operation_id(&mut self) -> OperationId {
        self.next_operation_id += 1;
        OperationId::new(self.next_operation_id)
    }
}

/// 校验一个反序列化墨迹操作中全部可影响 Skia 的缓存几何。
fn validate_operation(operation: &InkOperation) -> Result<(), String> {
    let valid = match operation {
        InkOperation::DrawStroke(stroke) => {
            let expected_bounds = match &stroke.shape {
                crate::ink::DrawStrokeShape::Fixed { points, width } => {
                    let points_valid = !points.is_empty()
                        && points
                            .iter()
                            .all(|point| point.x.is_finite() && point.y.is_finite());
                    points_valid
                        .then(|| InkBounds::from_points(points, width.pixels() / 2.0))
                        .flatten()
                }
                crate::ink::DrawStrokeShape::Variable { points } => {
                    let points_valid = !points.is_empty()
                        && points.iter().all(|sample| {
                            sample.point.x.is_finite()
                                && sample.point.y.is_finite()
                                && sample.width.is_finite()
                                && sample.width >= 0.0
                        });
                    points_valid
                        .then(|| {
                            let centers: Vec<_> =
                                points.iter().map(|sample| sample.point).collect();
                            let max_width = points
                                .iter()
                                .map(|sample| sample.width)
                                .fold(0.0_f32, f32::max);
                            InkBounds::from_points(&centers, max_width / 2.0)
                        })
                        .flatten()
                }
            };
            stroke.bounds.is_valid() && expected_bounds == Some(stroke.bounds)
        }
        InkOperation::EraseStroke(stroke) => {
            let samples_valid = !stroke.samples.is_empty()
                && stroke.samples.iter().all(|sample| {
                    sample.center.x.is_finite()
                        && sample.center.y.is_finite()
                        && sample.radius_x.is_finite()
                        && sample.radius_x >= 0.0
                        && sample.radius_y.is_finite()
                        && sample.radius_y >= 0.0
                        && sample.rotation_radians.is_finite()
                });
            let expected_bounds = samples_valid.then(|| {
                stroke
                    .samples
                    .iter()
                    .copied()
                    .map(EraseSample::bounds)
                    .reduce(InkBounds::union)
            });
            stroke.bounds.is_valid() && expected_bounds.flatten() == Some(stroke.bounds)
        }
        InkOperation::Clear(clear) => clear.affected_bounds.is_some_and(InkBounds::is_valid),
    };
    valid
        .then_some(())
        .ok_or_else(|| "墨迹 operation 含有无效或非有限几何".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证撤销后产生的操作标识间隙不会破坏二分查找。
    #[test]
    fn operation_finds_ids_across_undo_gap() {
        let mut document = InkDocument::new();
        let removed_id = document
            .append_draw_stroke(
                vec![CanvasPoint::new(0.0, 0.0), CanvasPoint::new(4.0, 4.0)],
                InkColor::Red,
                PenWidth::Px4,
            )
            .expect("有效笔画应创建操作");
        document.undo();
        let retained_id = document
            .append_draw_stroke(
                vec![CanvasPoint::new(8.0, 8.0), CanvasPoint::new(12.0, 12.0)],
                InkColor::Blue,
                PenWidth::Px8,
            )
            .expect("撤销后的有效笔画应创建新操作");

        assert!(document.operation(removed_id).is_none());
        assert_eq!(
            document.operation(retained_id).map(InkOperation::id),
            Some(retained_id)
        );
    }

    /// 验证文档 clone 共享历史，任一副本 mutation 时才复制操作向量。
    #[test]
    fn clone_shares_history_until_mutation() {
        let mut original = InkDocument::new();
        original.append_draw_stroke(
            vec![CanvasPoint::new(1.0, 1.0)],
            InkColor::Red,
            PenWidth::Px4,
        );
        let mut snapshot = original.clone();

        assert!(Arc::ptr_eq(&original.operations, &snapshot.operations));
        snapshot.append_draw_stroke(
            vec![CanvasPoint::new(2.0, 2.0)],
            InkColor::Blue,
            PenWidth::Px6,
        );

        assert!(!Arc::ptr_eq(&original.operations, &snapshot.operations));
        assert_eq!(original.operations().len(), 1);
        assert_eq!(snapshot.operations().len(), 2);
    }

    /// 验证恢复校验会重新计算清屏前的可见范围。
    #[test]
    fn recovery_rejects_inconsistent_clear_bounds() {
        let mut document = InkDocument::new();
        document.append_draw_stroke(
            vec![CanvasPoint::new(4.0, 4.0)],
            InkColor::Red,
            PenWidth::Px4,
        );
        document.clear();
        let Some(InkOperation::Clear(clear)) = Arc::make_mut(&mut document.operations).last_mut()
        else {
            panic!("最后一个操作应为清屏");
        };
        clear.affected_bounds = Some(InkBounds {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        });

        assert!(document.validate_recovery().is_err());
    }

    /// 验证恢复数据不能把 operation id 分配器置于溢出边界。
    #[test]
    fn recovery_rejects_exhausted_operation_id() {
        let document = InkDocument {
            operations: Arc::new(Vec::new()),
            next_operation_id: u64::MAX,
        };

        assert!(document.validate_recovery().is_err());
    }
}
