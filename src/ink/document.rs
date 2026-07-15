use super::{
    CanvasPoint, ClearOperation, EraseSample, EraseStroke, InkBounds, InkColor, InkOperation,
    OperationId, PenWidth,
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

    /// 验证清屏只遮蔽既有历史，撤销后可以完整恢复重放序列。
    #[test]
    fn undo_clear_restores_previous_operations() {
        let mut document = InkDocument::new();
        document.append_draw_stroke(
            vec![CanvasPoint::new(10.0, 20.0), CanvasPoint::new(30.0, 40.0)],
            InkColor::Red,
            PenWidth::Px8,
        );

        let clear_id = document.clear().expect("有墨迹时应创建清屏操作");
        assert!(document.replay_operations().is_empty());
        assert_eq!(document.operations().len(), 2);

        let undone = document.undo().expect("清屏操作应可撤销");
        assert_eq!(undone.id(), clear_id);
        assert_eq!(document.replay_operations().len(), 1);
    }

    /// 验证空画布清屏不会制造没有视觉意义的历史节点。
    #[test]
    fn clear_empty_document_is_noop() {
        let mut document = InkDocument::new();
        assert_eq!(document.clear(), None);
        assert!(document.has_no_history());
    }

    /// 验证固定画笔宽度会被计入笔画脏区范围。
    #[test]
    fn draw_bounds_include_half_pen_width() {
        let mut document = InkDocument::new();
        document.append_draw_stroke(
            vec![CanvasPoint::new(10.0, 20.0), CanvasPoint::new(30.0, 40.0)],
            InkColor::Blue,
            PenWidth::Px8,
        );

        assert_eq!(
            document.visible_bounds(),
            Some(InkBounds {
                left: 6.0,
                top: 16.0,
                right: 34.0,
                bottom: 44.0,
            })
        );
    }

    /// 验证 PRD 基线规模的结构化 operation 可以稳定追加且保持完整历史。
    #[test]
    fn baseline_operation_volume_keeps_complete_history() {
        let mut document = InkDocument::new();
        for index in 0..1_000 {
            let offset = index as f32;
            document.append_draw_stroke(
                vec![
                    CanvasPoint::new(offset, offset),
                    CanvasPoint::new(offset + 8.0, offset + 8.0),
                ],
                InkColor::Red,
                PenWidth::Px8,
            );
        }
        for index in 0..200 {
            let offset = index as f32 * 4.0;
            document.append_erase_stroke(vec![EraseSample::circle(
                CanvasPoint::new(offset, offset),
                48.0,
            )]);
        }

        assert_eq!(document.operations().len(), 1_200);
        assert!(document.visible_bounds().is_some());
    }
}
