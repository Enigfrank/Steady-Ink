use std::collections::HashMap;

use super::{InkBounds, OperationId};

const MAX_OPERATIONS_PER_NODE: usize = 8;
const MAX_DEPTH: usize = 8;

/// 用于快速查找与指定画布区域相交的墨迹操作。
pub(crate) struct InkSpatialIndex {
    root: QuadTreeNode,
    operation_bounds: HashMap<OperationId, InkBounds>,
    logical_size: [u32; 2],
}

/// 四叉树中一条带有真实边界的操作记录。
#[derive(Debug, Clone, Copy)]
struct IndexedOperation {
    id: OperationId,
    bounds: InkBounds,
}

/// 四叉树节点；跨越多个子象限的操作保留在当前节点。
struct QuadTreeNode {
    bounds: InkBounds,
    operations: Vec<IndexedOperation>,
    children: Option<Box<[QuadTreeNode; 4]>>,
}

impl InkSpatialIndex {
    /// 为指定逻辑画布创建空索引。
    pub(crate) fn new(logical_size: [u32; 2]) -> Self {
        let logical_size = [logical_size[0].max(1), logical_size[1].max(1)];
        Self {
            root: QuadTreeNode::new(Self::root_bounds(logical_size)),
            operation_bounds: HashMap::new(),
            logical_size,
        }
    }

    /// 插入或更新一个操作及其边界。
    pub(crate) fn insert(&mut self, id: OperationId, bounds: InkBounds) {
        if self.operation_bounds.contains_key(&id) {
            self.remove(id);
        }
        self.operation_bounds.insert(id, bounds);
        self.root.insert(IndexedOperation { id, bounds }, 0);
    }

    /// 返回与指定区域相交的操作标识，每个标识最多出现一次。
    pub(crate) fn query(&self, region: InkBounds) -> Vec<OperationId> {
        let mut results = Vec::new();
        self.root.query(region, &mut results);
        results
    }

    /// 从索引中移除指定操作。
    pub(crate) fn remove(&mut self, id: OperationId) {
        if let Some(bounds) = self.operation_bounds.remove(&id) {
            self.root.remove(id, bounds);
        }
    }

    /// 清空索引并按给定操作序列重新构建。
    pub(crate) fn rebuild(
        &mut self,
        operations: impl IntoIterator<Item = (OperationId, InkBounds)>,
    ) {
        self.clear();
        for (id, bounds) in operations {
            self.insert(id, bounds);
        }
    }

    /// 清空全部操作并保留当前逻辑画布尺寸。
    pub(crate) fn clear(&mut self) {
        self.operation_bounds.clear();
        self.root = QuadTreeNode::new(Self::root_bounds(self.logical_size));
    }

    /// 返回索引记录的操作数量。
    #[cfg(test)]
    fn len(&self) -> usize {
        self.operation_bounds.len()
    }

    /// 返回索引是否不含任何操作。
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.operation_bounds.is_empty()
    }

    /// 估算测试场景中索引集合和树节点申请的堆容量。
    #[cfg(test)]
    fn estimated_heap_bytes(&self) -> usize {
        let hash_bucket_bytes = std::mem::size_of::<(OperationId, InkBounds)>() + 1;
        self.operation_bounds.capacity() * hash_bucket_bytes + self.root.estimated_heap_bytes()
    }

    /// 计算逻辑画布对应的根节点边界。
    fn root_bounds(logical_size: [u32; 2]) -> InkBounds {
        InkBounds::from_xywh(0.0, 0.0, logical_size[0] as f32, logical_size[1] as f32)
    }
}

impl QuadTreeNode {
    /// 创建一个没有子节点和操作的四叉树节点。
    fn new(bounds: InkBounds) -> Self {
        Self {
            bounds,
            operations: Vec::new(),
            children: None,
        }
    }

    /// 将操作放入唯一可完整容纳它的节点。
    fn insert(&mut self, operation: IndexedOperation, depth: usize) {
        if !self.bounds.intersects(operation.bounds) {
            return;
        }

        if let Some(child) = self.containing_child_mut(operation.bounds) {
            child.insert(operation, depth + 1);
            return;
        }

        self.operations.push(operation);
        if self.children.is_none()
            && self.operations.len() > MAX_OPERATIONS_PER_NODE
            && depth < MAX_DEPTH
        {
            self.split(depth);
        }
    }

    /// 把当前节点分成四个象限，并重新分配可完整下沉的操作。
    fn split(&mut self, depth: usize) {
        let half_width = self.bounds.width() / 2.0;
        let half_height = self.bounds.height() / 2.0;
        let center_x = self.bounds.left + half_width;
        let center_y = self.bounds.top + half_height;
        self.children = Some(Box::new([
            Self::new(InkBounds::from_xywh(
                self.bounds.left,
                self.bounds.top,
                half_width,
                half_height,
            )),
            Self::new(InkBounds::from_xywh(
                center_x,
                self.bounds.top,
                half_width,
                half_height,
            )),
            Self::new(InkBounds::from_xywh(
                self.bounds.left,
                center_y,
                half_width,
                half_height,
            )),
            Self::new(InkBounds::from_xywh(
                center_x,
                center_y,
                half_width,
                half_height,
            )),
        ]));

        let operations = std::mem::take(&mut self.operations);
        for operation in operations {
            self.insert(operation, depth);
        }
    }

    /// 收集当前节点及相交子节点中的真实相交操作。
    fn query(&self, region: InkBounds, results: &mut Vec<OperationId>) {
        if !self.bounds.intersects(region) {
            return;
        }

        results.extend(
            self.operations
                .iter()
                .filter(|operation| operation.bounds.intersects(region))
                .map(|operation| operation.id),
        );
        if let Some(children) = &self.children {
            for child in children.iter() {
                child.query(region, results);
            }
        }
    }

    /// 从操作所在的唯一节点移除它。
    fn remove(&mut self, id: OperationId, bounds: InkBounds) {
        if !self.bounds.intersects(bounds) {
            return;
        }
        if let Some(child) = self.containing_child_mut(bounds) {
            child.remove(id, bounds);
            return;
        }
        self.operations.retain(|operation| operation.id != id);
    }

    /// 返回唯一完整包含目标边界的可变子节点。
    fn containing_child_mut(&mut self, bounds: InkBounds) -> Option<&mut QuadTreeNode> {
        self.children.as_mut()?.iter_mut().find(|child| {
            child.bounds.left <= bounds.left
                && child.bounds.top <= bounds.top
                && child.bounds.right >= bounds.right
                && child.bounds.bottom >= bounds.bottom
        })
    }

    /// 估算当前节点以下的向量和子节点堆容量。
    #[cfg(test)]
    fn estimated_heap_bytes(&self) -> usize {
        let operation_bytes = self.operations.capacity() * std::mem::size_of::<IndexedOperation>();
        let child_bytes = self.children.as_ref().map_or(0, |children| {
            std::mem::size_of::<[QuadTreeNode; 4]>()
                + children
                    .iter()
                    .map(QuadTreeNode::estimated_heap_bytes)
                    .sum::<usize>()
        });
        operation_bytes + child_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建测试使用的操作标识。
    const fn operation_id(value: u64) -> OperationId {
        OperationId::new(value)
    }

    /// 验证基础插入只返回真实相交的操作。
    #[test]
    fn insert_and_query_filter_non_intersecting_operations() {
        let mut index = InkSpatialIndex::new([1000, 1000]);
        index.insert(
            operation_id(1),
            InkBounds::from_xywh(10.0, 10.0, 50.0, 50.0),
        );
        index.insert(
            operation_id(2),
            InkBounds::from_xywh(110.0, 110.0, 50.0, 50.0),
        );

        let results = index.query(InkBounds::from_xywh(0.0, 0.0, 100.0, 100.0));

        assert_eq!(results, vec![operation_id(1)]);
    }

    /// 验证节点分裂后不会把操作复制到无关象限或返回重复标识。
    #[test]
    fn split_keeps_query_results_unique_and_spatially_precise() {
        let mut index = InkSpatialIndex::new([1000, 1000]);
        for value in 1..=9 {
            let offset = value as f32 * 10.0;
            index.insert(
                operation_id(value),
                InkBounds::from_xywh(offset, offset, 4.0, 4.0),
            );
        }

        let results = index.query(InkBounds::from_xywh(0.0, 0.0, 30.0, 30.0));

        assert_eq!(results.len(), 3);
        assert!(results.contains(&operation_id(1)));
        assert!(results.contains(&operation_id(2)));
        assert!(results.contains(&operation_id(3)));
    }

    /// 验证跨越多个象限的操作保留在父节点且只返回一次。
    #[test]
    fn cross_quadrant_operation_is_returned_once() {
        let mut index = InkSpatialIndex::new([1000, 1000]);
        for value in 1..=8 {
            let offset = value as f32 * 10.0;
            index.insert(
                operation_id(value),
                InkBounds::from_xywh(offset, offset, 4.0, 4.0),
            );
        }
        index.insert(
            operation_id(9),
            InkBounds::from_xywh(490.0, 490.0, 20.0, 20.0),
        );

        let results = index.query(InkBounds::from_xywh(495.0, 495.0, 2.0, 2.0));

        assert_eq!(results, vec![operation_id(9)]);
    }

    /// 验证重复插入同一标识会更新位置而不保留旧记录。
    #[test]
    fn inserting_existing_id_replaces_previous_bounds() {
        let mut index = InkSpatialIndex::new([1000, 1000]);
        index.insert(
            operation_id(1),
            InkBounds::from_xywh(10.0, 10.0, 20.0, 20.0),
        );
        index.insert(
            operation_id(1),
            InkBounds::from_xywh(500.0, 500.0, 20.0, 20.0),
        );

        assert!(
            index
                .query(InkBounds::from_xywh(0.0, 0.0, 100.0, 100.0))
                .is_empty()
        );
        assert_eq!(
            index.query(InkBounds::from_xywh(490.0, 490.0, 50.0, 50.0)),
            vec![operation_id(1)]
        );
        assert_eq!(index.len(), 1);
    }

    /// 验证删除操作会同时移除边界缓存和树节点记录。
    #[test]
    fn remove_deletes_operation() {
        let mut index = InkSpatialIndex::new([1000, 1000]);
        index.insert(
            operation_id(1),
            InkBounds::from_xywh(10.0, 10.0, 50.0, 50.0),
        );

        index.remove(operation_id(1));

        assert!(
            index
                .query(InkBounds::from_xywh(0.0, 0.0, 100.0, 100.0))
                .is_empty()
        );
        assert!(index.is_empty());
    }

    /// 验证批量重建会清除旧操作并建立新索引。
    #[test]
    fn rebuild_replaces_existing_operations() {
        let mut index = InkSpatialIndex::new([1000, 1000]);
        index.insert(
            operation_id(1),
            InkBounds::from_xywh(10.0, 10.0, 50.0, 50.0),
        );

        index.rebuild([
            (
                operation_id(2),
                InkBounds::from_xywh(100.0, 100.0, 50.0, 50.0),
            ),
            (
                operation_id(3),
                InkBounds::from_xywh(200.0, 200.0, 50.0, 50.0),
            ),
        ]);

        assert_eq!(index.len(), 2);
        assert!(
            index
                .query(InkBounds::from_xywh(0.0, 0.0, 50.0, 50.0))
                .is_empty()
        );
        assert_eq!(
            index.query(InkBounds::from_xywh(90.0, 90.0, 70.0, 70.0)),
            vec![operation_id(2)]
        );
    }

    /// 验证典型 1000 操作索引的已申请容量保持在 100KB 预算内。
    #[test]
    fn thousand_uniform_operations_stay_within_memory_budget() {
        let mut index = InkSpatialIndex::new([1000, 1000]);
        let mut id = 1;
        for row in 0..25 {
            for column in 0..40 {
                index.insert(
                    operation_id(id),
                    InkBounds::from_xywh(
                        column as f32 * 25.0 + 4.0,
                        row as f32 * 40.0 + 4.0,
                        8.0,
                        8.0,
                    ),
                );
                id += 1;
            }
        }

        assert_eq!(index.len(), 1000);
        assert!(index.estimated_heap_bytes() <= 100 * 1024);
    }
}
