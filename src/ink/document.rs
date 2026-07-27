use super::{
    CanvasPoint, ClearOperation, EraseSample, EraseStroke, InkBounds, InkColor, InkOperation,
    OperationId, PenWidth, VariableStrokePoint,
};
use crate::ink::DrawStroke;

/// 单个普通批注画布或单个放映位置的结构化墨迹文档。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InkDocument {
    operations: Vec<InkOperation>,
    next_operation_id: u64,
}

impl InkDocument {
    /// 创建一个没有任何历史操作的文档。
    pub const fn new() -> Self {
        Self {
            operations: Vec::new(),
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
        self.operations.push(InkOperation::DrawStroke(stroke));
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
        self.operations.push(InkOperation::DrawStroke(stroke));
        Some(id)
    }

    /// 追加一次普通或手掌区域擦除；空采样不会产生操作。
    pub fn append_erase_stroke(&mut self, samples: Vec<EraseSample>) -> Option<OperationId> {
        if samples.is_empty() {
            return None;
        }

        let id = self.allocate_operation_id();
        let stroke = EraseStroke::new(id, samples)?;
        self.operations.push(InkOperation::EraseStroke(stroke));
        Some(id)
    }

    /// 立即清空当前可见墨迹，并把清屏记为一次可撤销操作。
    pub fn clear(&mut self) -> Option<OperationId> {
        let affected_bounds = self.visible_bounds()?;
        let id = self.allocate_operation_id();
        self.operations.push(InkOperation::Clear(ClearOperation {
            id,
            affected_bounds: Some(affected_bounds),
        }));
        Some(id)
    }

    /// 撤销最近一次画笔、区域擦除或清屏操作。
    pub fn undo(&mut self) -> Option<InkOperation> {
        self.operations.pop()
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

    /// 分配文档内单调递增且不会因撤销复用的操作标识。
    fn allocate_operation_id(&mut self) -> OperationId {
        self.next_operation_id += 1;
        OperationId::new(self.next_operation_id)
    }
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
}
