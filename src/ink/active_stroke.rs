use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    ops::Range,
};

use super::{
    CanvasPoint, InkBounds, InkColor, PenWidth, VariableStrokePoint,
    stroke_geometry::{CubicBezierSegment, open_bezier_segment_unclamped_at},
};

/// 活动画笔在 Begin 时锁定的几何样式。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ActiveStrokeStyle {
    Fixed {
        color: InkColor,
        width: PenWidth,
    },
    Natural {
        color: InkColor,
        body_width: PenWidth,
    },
}

/// 从上一次成功呈现的采样游标开始传输的一段原始点。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActiveStrokeDelta {
    pub(crate) gesture_id: u64,
    pub(crate) revision: u64,
    pub(crate) from_sample: usize,
    pub(crate) samples: Vec<CanvasPoint>,
    pub(crate) style: ActiveStrokeStyle,
    pub(crate) full_resync: bool,
}

/// 活动笔画更新的可观测工作量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ActiveStrokeWork {
    pub(crate) appended_samples: usize,
    pub(crate) recomputed_primitives: usize,
    pub(crate) full_redraw: bool,
    pub(crate) full_fallback: bool,
}

/// 一块需要清除并从邻接 halo 连续重放的活动笔画区域。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ActiveStrokeReplay {
    Fixed {
        bounds: InkBounds,
        segment_range: Range<usize>,
    },
    Natural {
        bounds: InkBounds,
        point_range: Range<usize>,
    },
}

impl ActiveStrokeReplay {
    /// 返回需要在 retained surface 上清除的旧/新像素并集。
    pub(crate) const fn bounds(&self) -> InkBounds {
        match self {
            Self::Fixed { bounds, .. } | Self::Natural { bounds, .. } => *bounds,
        }
    }
}

/// 以固定像素网格索引 retained primitive，避免每帧扫描整条长笔画。
#[derive(Debug, Default)]
struct ActiveGeometrySpatialIndex {
    buckets: HashMap<(i32, i32), HashSet<usize>>,
    global_entries: HashSet<usize>,
    bounds: Vec<Option<InkBounds>>,
}

impl ActiveGeometrySpatialIndex {
    /// 插入或更新一个 primitive 的保守 AA 像素范围。
    fn insert(&mut self, index: usize, bounds: InkBounds) {
        self.remove(index);
        if self.bounds.len() <= index {
            self.bounds.resize(index + 1, None);
        }
        if let Some((left, top, right, bottom)) = spatial_cell_range(bounds) {
            for y in top..=bottom {
                for x in left..=right {
                    self.buckets.entry((x, y)).or_default().insert(index);
                }
            }
        } else {
            self.global_entries.insert(index);
        }
        self.bounds[index] = Some(bounds);
    }

    /// 移除一个 primitive 之前登记的全部网格单元。
    fn remove(&mut self, index: usize) {
        let Some(bounds) = self.bounds.get(index).copied().flatten() else {
            return;
        };
        if let Some((left, top, right, bottom)) = spatial_cell_range(bounds) {
            for y in top..=bottom {
                for x in left..=right {
                    let cell = (x, y);
                    let remove_bucket = self.buckets.get_mut(&cell).is_some_and(|entries| {
                        entries.remove(&index);
                        entries.is_empty()
                    });
                    if remove_bucket {
                        self.buckets.remove(&cell);
                    }
                }
            }
        } else {
            self.global_entries.remove(&index);
        }
        self.bounds[index] = None;
    }

    /// 删除给定长度以后的旧 primitive 条目。
    fn truncate(&mut self, len: usize) {
        for index in len..self.bounds.len() {
            self.remove(index);
        }
        self.bounds.truncate(len);
    }

    /// 返回区域是否命中局部 replay 范围之外的稳定 primitive。
    fn intersects_outside(&self, region: InkBounds, inside_replay: impl Fn(usize) -> bool) -> bool {
        if self.global_entries.iter().copied().any(|index| {
            !inside_replay(index)
                && self.bounds[index].is_some_and(|bounds| bounds.intersects(region))
        }) {
            return true;
        }
        let Some((left, top, right, bottom)) = spatial_cell_range(region) else {
            return self.bounds.iter().enumerate().any(|(index, bounds)| {
                !inside_replay(index) && bounds.is_some_and(|bounds| bounds.intersects(region))
            });
        };
        for y in top..=bottom {
            for x in left..=right {
                if self.buckets.get(&(x, y)).is_some_and(|entries| {
                    entries.iter().copied().any(|index| {
                        !inside_replay(index)
                            && self.bounds[index].is_some_and(|bounds| bounds.intersects(region))
                    })
                }) {
                    return true;
                }
            }
        }
        false
    }

    /// 清空一次手势留下的空间条目并保留集合容量。
    fn clear(&mut self) {
        self.buckets.clear();
        self.global_entries.clear();
        self.bounds.clear();
    }
}

/// 渲染线程保留的活动笔画几何；它不持有 Skia 或 GPU 对象。
#[derive(Debug, Default)]
pub(crate) struct ActiveStrokeRenderCache {
    gesture_id: Option<u64>,
    revision: u64,
    style: Option<ActiveStrokeStyle>,
    raw_points: Vec<CanvasPoint>,
    filtered_points: Vec<CanvasPoint>,
    filtered_bounds: FilteredPointBounds,
    filtered_variable_points: Vec<VariableStrokePoint>,
    raw_distances: Vec<f32>,
    unclamped_primitives: Vec<CubicBezierSegment>,
    primitives: Vec<CubicBezierSegment>,
    fixed_spatial_index: ActiveGeometrySpatialIndex,
    natural_spatial_index: ActiveGeometrySpatialIndex,
    clamp_sensitive: [HashSet<usize>; CLAMP_SIDE_COUNT],
    bounds: Option<Bounds>,
    replay_regions: Vec<ActiveStrokeReplay>,
    last_work: ActiveStrokeWork,
}

impl ActiveStrokeRenderCache {
    /// 应用一个连续增量；序列不连续时明确报错，调用方可要求 full resync。
    pub(crate) fn apply_delta(
        &mut self,
        delta: &ActiveStrokeDelta,
    ) -> Result<ActiveStrokeWork, ActiveStrokeSequenceError> {
        self.replay_regions.clear();
        self.last_work = ActiveStrokeWork::default();
        if delta.samples.iter().any(|point| !finite_point(*point)) {
            return Err(ActiveStrokeSequenceError::NonFiniteSample);
        }
        if delta.full_resync {
            if delta.from_sample != 0 {
                return Err(ActiveStrokeSequenceError::Discontinuous);
            }
            return self.apply_full(
                delta.gesture_id,
                delta.revision,
                delta.style,
                &delta.samples,
            );
        }
        let sequence_matches = self.gesture_id == Some(delta.gesture_id)
            && self.style == Some(delta.style)
            && delta.from_sample == self.raw_points.len()
            && delta.revision > self.revision;
        if self.gesture_id == Some(delta.gesture_id)
            && self.style == Some(delta.style)
            && delta.from_sample <= self.raw_points.len()
            && delta
                .samples
                .iter()
                .enumerate()
                .take(self.raw_points.len().saturating_sub(delta.from_sample))
                .all(|(index, point)| self.raw_points[delta.from_sample + index] == *point)
        {
            let already_present = self.raw_points.len().saturating_sub(delta.from_sample);
            if already_present >= delta.samples.len() {
                self.revision = self.revision.max(delta.revision);
                self.last_work = ActiveStrokeWork::default();
                return Ok(self.last_work);
            }
            let previous_bounds = self.bounds;
            let appended = &delta.samples[already_present..];
            self.raw_points.extend_from_slice(appended);
            self.revision = delta.revision;
            let mut work = ActiveStrokeWork {
                appended_samples: appended.len(),
                ..ActiveStrokeWork::default()
            };
            match delta.style {
                ActiveStrokeStyle::Fixed { .. } => {
                    self.rebuild_fixed_tail(previous_bounds, &mut work)
                }
                ActiveStrokeStyle::Natural { body_width, .. } => {
                    self.rebuild_natural_points(body_width, &mut work)
                }
            }
            self.last_work = work;
            return Ok(work);
        }
        let reset_surface = !sequence_matches && self.gesture_id.is_some();
        if !sequence_matches {
            if delta.from_sample != 0 {
                return Err(ActiveStrokeSequenceError::Discontinuous);
            }
            self.reset_for(delta.gesture_id, delta.style);
        }

        let previous_bounds = self.bounds;
        self.raw_points.extend_from_slice(&delta.samples);
        self.gesture_id = Some(delta.gesture_id);
        self.style = Some(delta.style);
        self.revision = delta.revision;
        let mut work = ActiveStrokeWork {
            appended_samples: delta.samples.len(),
            full_redraw: reset_surface,
            ..ActiveStrokeWork::default()
        };
        match delta.style {
            ActiveStrokeStyle::Fixed { .. } => {
                self.rebuild_fixed_tail(previous_bounds, &mut work);
            }
            ActiveStrokeStyle::Natural { body_width, .. } => {
                self.rebuild_natural_points(body_width, &mut work);
            }
        }
        self.last_work = work;
        Ok(work)
    }

    /// 在序列断裂后使用完整原始点重建一次活动几何。
    pub(crate) fn apply_full(
        &mut self,
        gesture_id: u64,
        revision: u64,
        style: ActiveStrokeStyle,
        points: &[CanvasPoint],
    ) -> Result<ActiveStrokeWork, ActiveStrokeSequenceError> {
        self.replay_regions.clear();
        if points.iter().any(|point| !finite_point(*point)) {
            return Err(ActiveStrokeSequenceError::NonFiniteSample);
        }
        self.reset_for(gesture_id, style);
        self.raw_points.extend_from_slice(points);
        self.revision = revision;
        let mut work = ActiveStrokeWork {
            appended_samples: points.len(),
            full_redraw: true,
            full_fallback: true,
            ..ActiveStrokeWork::default()
        };
        match style {
            ActiveStrokeStyle::Fixed { .. } => self.rebuild_fixed_tail(None, &mut work),
            ActiveStrokeStyle::Natural { body_width, .. } => {
                self.rebuild_natural_points(body_width, &mut work)
            }
        }
        self.last_work = work;
        Ok(work)
    }

    /// 返回当前更新的工作量，供性能诊断读取。
    pub(crate) const fn last_work(&self) -> ActiveStrokeWork {
        self.last_work
    }

    /// 记录渲染层局部 replay 无法执行而实际采用完整活动层重画。
    pub(crate) fn record_render_full_fallback(&mut self) {
        self.last_work.full_redraw = true;
        self.last_work.full_fallback = true;
    }

    /// 返回当前保留的原始采样数。
    pub(crate) fn sample_count(&self) -> usize {
        self.raw_points.len()
    }

    /// 返回固定宽度的已滤波点和自然笔锋的已滤波宽度点。
    pub(crate) fn geometry(&self) -> (&[CanvasPoint], &[VariableStrokePoint]) {
        (&self.filtered_points, &self.filtered_variable_points)
    }

    /// 返回本次增量需要清除并局部重放的区域。
    pub(crate) fn replay_regions(&self) -> &[ActiveStrokeReplay] {
        &self.replay_regions
    }

    /// 返回固定宽笔画保留的全局 clamp 后 cubic primitives。
    pub(crate) fn fixed_primitives(&self) -> &[CubicBezierSegment] {
        &self.primitives
    }

    /// 返回 Begin 时锁定的样式，供 compositor 选择同一套 Skia 路径绘制。
    pub(crate) const fn style(&self) -> Option<ActiveStrokeStyle> {
        self.style
    }

    /// 清理已结束或被取消的活动手势。
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// 为新的手势标识和 Begin 样式清空全部 retained CPU 几何。
    fn reset_for(&mut self, gesture_id: u64, style: ActiveStrokeStyle) {
        self.gesture_id = Some(gesture_id);
        self.revision = 0;
        self.style = Some(style);
        self.raw_points.clear();
        self.filtered_points.clear();
        self.filtered_bounds.clear();
        self.filtered_variable_points.clear();
        self.raw_distances.clear();
        self.unclamped_primitives.clear();
        self.primitives.clear();
        self.fixed_spatial_index.clear();
        self.natural_spatial_index.clear();
        self.clamp_sensitive = Default::default();
        self.bounds = None;
        self.replay_regions.clear();
    }

    /// 更新固定宽笔画的滤波尾部和受变化边界实际影响的 cubic primitive。
    fn rebuild_fixed_tail(&mut self, previous_bounds: Option<Bounds>, work: &mut ActiveStrokeWork) {
        if self.raw_points.is_empty() {
            self.filtered_points.clear();
            self.unclamped_primitives.clear();
            self.primitives.clear();
            self.fixed_spatial_index.clear();
            self.clamp_sensitive = Default::default();
            self.bounds = None;
            return;
        }
        let ActiveStrokeStyle::Fixed { width, .. } = self.style.expect("固定宽重建必须有样式")
        else {
            return;
        };
        let old_len = self.filtered_points.len();
        let old_segment_count = self.primitives.len();
        let old_single_dirty = (old_segment_count == 0)
            .then(|| fixed_ink_bounds(&self.filtered_points, &[], width.pixels()))
            .flatten();
        self.filtered_points
            .resize(self.raw_points.len(), self.raw_points[0]);
        let filter_start = old_len.saturating_sub(1).min(self.raw_points.len());
        for index in filter_start..self.raw_points.len() {
            if index < old_len {
                self.filtered_bounds.remove(self.filtered_points[index]);
            }
            let point = filtered_point_at(&self.raw_points, index);
            self.filtered_points[index] = point;
            self.filtered_bounds.insert(point);
        }
        let new_bounds = self
            .filtered_bounds
            .bounds()
            .expect("固定宽有效滤波点必须拥有 bounds");
        self.bounds = Some(new_bounds);
        let segment_count = self.filtered_points.len().saturating_sub(1);
        let changed_start = filter_start.saturating_sub(2).min(segment_count);

        let Some(previous_bounds) = previous_bounds else {
            self.rebuild_all_fixed_primitives(new_bounds);
            work.recomputed_primitives = segment_count;
            if let Some(dirty) =
                fixed_ink_bounds(&self.filtered_points, &self.primitives, width.pixels())
            {
                if segment_count <= MAX_LOCAL_REPLAY_POINTS {
                    self.replay_regions.push(ActiveStrokeReplay::Fixed {
                        bounds: dirty,
                        segment_range: 0..segment_count,
                    });
                } else {
                    work.full_redraw = true;
                }
            }
            return;
        };

        if self.unclamped_primitives.len() != old_segment_count
            || !new_bounds.is_expansion_of(previous_bounds)
        {
            self.rebuild_all_fixed_primitives(new_bounds);
            work.recomputed_primitives = segment_count;
            work.full_fallback = true;
            work.full_redraw = true;
            return;
        }

        let mut affected = HashSet::new();
        affected.extend(changed_start..segment_count);
        for side in new_bounds.changed_expansion_sides(previous_bounds) {
            affected.extend(
                self.clamp_sensitive[side]
                    .iter()
                    .copied()
                    .filter(|index| *index < segment_count),
            );
        }
        let affected_start = affected.iter().copied().min().unwrap_or(changed_start);
        let affected_end = affected
            .iter()
            .copied()
            .max()
            .map_or(changed_start, |index| index + 1);
        if affected.len() > MAX_LOCAL_REPLAY_POINTS
            || affected_end.saturating_sub(affected_start) > MAX_LOCAL_REPLAY_POINTS
        {
            self.rebuild_all_fixed_primitives(new_bounds);
            work.recomputed_primitives = segment_count;
            work.full_fallback = true;
            work.full_redraw = true;
            return;
        }

        let old_dirty = union_optional_bounds(
            old_single_dirty,
            fixed_ink_bounds(
                &self.filtered_points,
                &self.primitives
                    [affected_start.min(old_segment_count)..affected_end.min(old_segment_count)],
                width.pixels(),
            ),
        );
        for index in changed_start..old_segment_count {
            self.remove_clamp_sensitive(index);
        }
        self.unclamped_primitives.truncate(changed_start);
        self.primitives.truncate(changed_start);
        for index in changed_start..segment_count {
            let segment = open_bezier_segment_unclamped_at(&self.filtered_points, index)
                .expect("有效固定宽尾部索引必须生成 cubic");
            self.unclamped_primitives.push(segment);
            self.primitives.push(new_bounds.clamp_segment(segment));
            self.index_clamp_sensitive(index, segment, new_bounds);
        }
        for index in affected
            .iter()
            .copied()
            .filter(|index| *index < changed_start)
        {
            let segment = self.unclamped_primitives[index];
            self.primitives[index] = new_bounds.clamp_segment(segment);
            self.remove_clamp_sensitive(index);
            self.index_clamp_sensitive(index, segment, new_bounds);
        }
        self.fixed_spatial_index.truncate(segment_count);
        for index in changed_start..segment_count {
            let bounds = fixed_ink_bounds(
                &[],
                std::slice::from_ref(&self.primitives[index]),
                width.pixels(),
            )
            .expect("固定宽 cubic 必须拥有 AA bounds");
            self.fixed_spatial_index.insert(index, bounds);
        }
        for index in affected
            .iter()
            .copied()
            .filter(|index| *index < changed_start)
        {
            let bounds = fixed_ink_bounds(
                &[],
                std::slice::from_ref(&self.primitives[index]),
                width.pixels(),
            )
            .expect("固定宽 cubic 必须拥有 AA bounds");
            self.fixed_spatial_index.insert(index, bounds);
        }
        work.recomputed_primitives = affected.len();

        let new_dirty = fixed_ink_bounds(
            &self.filtered_points,
            &self.primitives[affected_start..affected_end],
            width.pixels(),
        );
        let Some(dirty) = union_optional_bounds(old_dirty, new_dirty) else {
            return;
        };
        let Some(segment_range) = bounded_fixed_replay_range(
            &self.primitives,
            affected_start..affected_end,
            dirty,
            width.pixels(),
        ) else {
            work.full_fallback = true;
            work.full_redraw = true;
            return;
        };
        if self
            .fixed_spatial_index
            .intersects_outside(dirty, |index| segment_range.contains(&index))
        {
            work.full_fallback = true;
            work.full_redraw = true;
            return;
        }
        let replay_bounds =
            fixed_ink_bounds(&[], &self.primitives[segment_range.clone()], width.pixels())
                .map_or(dirty, |bounds| dirty.union(bounds));
        self.replay_regions.push(ActiveStrokeReplay::Fixed {
            bounds: replay_bounds,
            segment_range,
        });
    }

    /// 完整重建固定宽的未 clamp/clamped 段及四侧敏感索引。
    fn rebuild_all_fixed_primitives(&mut self, bounds: Bounds) {
        let segment_count = self.filtered_points.len().saturating_sub(1);
        self.unclamped_primitives.clear();
        self.primitives.clear();
        self.clamp_sensitive = Default::default();
        self.unclamped_primitives.reserve(segment_count);
        self.primitives.reserve(segment_count);
        for index in 0..segment_count {
            let segment = open_bezier_segment_unclamped_at(&self.filtered_points, index)
                .expect("有效固定宽索引必须生成 cubic");
            self.unclamped_primitives.push(segment);
            self.primitives.push(bounds.clamp_segment(segment));
            self.index_clamp_sensitive(index, segment, bounds);
        }
        let ActiveStrokeStyle::Fixed { width, .. } = self.style.expect("固定宽索引必须有样式")
        else {
            return;
        };
        self.fixed_spatial_index.clear();
        for (index, segment) in self.primitives.iter().enumerate() {
            let bounds = fixed_ink_bounds(&[], std::slice::from_ref(segment), width.pixels())
                .expect("固定宽 cubic 必须拥有 AA bounds");
            self.fixed_spatial_index.insert(index, bounds);
        }
    }

    /// 记录一段未 clamp 控制点依赖的具体全局边界。
    fn index_clamp_sensitive(&mut self, index: usize, segment: CubicBezierSegment, bounds: Bounds) {
        for side in bounds.sensitive_sides(segment) {
            self.clamp_sensitive[side].insert(index);
        }
    }

    /// 从全部边界集合移除即将被重算的段索引。
    fn remove_clamp_sensitive(&mut self, index: usize) {
        for sensitive in &mut self.clamp_sensitive {
            sensitive.remove(&index);
        }
    }

    /// 更新自然笔锋的累计弧长、滤波位置和动态起收笔宽度。
    fn rebuild_natural_points(&mut self, body_width: PenWidth, work: &mut ActiveStrokeWork) {
        if self.raw_points.is_empty() {
            self.filtered_variable_points.clear();
            self.natural_spatial_index.clear();
            return;
        }
        let old_len = self.filtered_variable_points.len();
        let old_total = self.raw_distances.last().copied().unwrap_or(0.0);
        if self.raw_distances.is_empty() {
            self.raw_distances.push(0.0);
        }
        for pair in self.raw_points[self.raw_distances.len().saturating_sub(1)..].windows(2) {
            let dx = pair[1].x - pair[0].x;
            let dy = pair[1].y - pair[0].y;
            let segment = dx.mul_add(dx, dy * dy).sqrt();
            self.raw_distances
                .push(self.raw_distances.last().copied().unwrap_or(0.0) + segment);
        }
        let total = self.raw_distances.last().copied().unwrap_or(0.0);
        self.filtered_variable_points.resize(
            self.raw_points.len(),
            VariableStrokePoint {
                point: self.raw_points[0],
                width: body_width.pixels(),
            },
        );
        let body = body_width.pixels();
        let mut affected =
            natural_affected_ranges(&self.raw_distances, old_len, old_total, total, body);
        let old_dirty: Vec<_> = affected
            .iter()
            .map(|range| variable_ink_bounds(&self.filtered_variable_points[..old_len], range))
            .collect();

        for range in &affected {
            for index in range.clone() {
                self.filtered_variable_points[index] = VariableStrokePoint {
                    point: filtered_point_at(&self.raw_points, index),
                    width: natural_width(self.raw_distances[index], total, body),
                };
                work.recomputed_primitives = work.recomputed_primitives.saturating_add(1);
            }
        }
        let edge_count = self.filtered_variable_points.len().saturating_sub(1);
        self.natural_spatial_index.truncate(edge_count);
        let mut changed_edges = HashSet::new();
        for range in &affected {
            changed_edges.extend(range.start.saturating_sub(1)..range.end.min(edge_count));
        }
        for edge in changed_edges {
            let bounds = variable_ink_bounds(
                &self.filtered_variable_points,
                &(edge..edge.saturating_add(2)),
            )
            .expect("自然笔锋相邻点必须拥有 AA bounds");
            self.natural_spatial_index.insert(edge, bounds);
        }

        for (range, old_bounds) in affected.drain(..).zip(old_dirty) {
            let new_bounds = variable_ink_bounds(&self.filtered_variable_points, &range);
            let Some(dirty) = union_optional_bounds(old_bounds, new_bounds) else {
                continue;
            };
            let Some(point_range) =
                bounded_natural_replay_range(&self.filtered_variable_points, range, dirty)
            else {
                work.full_fallback = true;
                work.full_redraw = true;
                self.replay_regions.clear();
                return;
            };
            let replay_bounds = variable_ink_bounds(&self.filtered_variable_points, &point_range)
                .map_or(dirty, |bounds| dirty.union(bounds));
            self.replay_regions.push(ActiveStrokeReplay::Natural {
                bounds: replay_bounds,
                point_range,
            });
        }
        merge_overlapping_natural_regions(&mut self.replay_regions, work);
        let crosses_stable_geometry = self.replay_regions.iter().any(|replay| {
            let ActiveStrokeReplay::Natural {
                bounds,
                point_range,
            } = replay
            else {
                return false;
            };
            self.natural_spatial_index
                .intersects_outside(*bounds, |edge| {
                    edge >= point_range.start && edge + 1 < point_range.end
                })
        });
        if crosses_stable_geometry {
            self.replay_regions.clear();
            work.full_fallback = true;
            work.full_redraw = true;
        }
    }
}

/// 活动增量序列错误；调用方必须发送 from_sample=0 的 full resync。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveStrokeSequenceError {
    Discontinuous,
    NonFiniteSample,
}

/// 支持尾部点替换的精确坐标多重集合，避免每次追加扫描完整滤波序列。
#[derive(Debug, Default)]
struct FilteredPointBounds {
    x: BTreeMap<OrderedCoordinate, usize>,
    y: BTreeMap<OrderedCoordinate, usize>,
}

impl FilteredPointBounds {
    /// 增加一个有限滤波点的两个坐标计数。
    fn insert(&mut self, point: CanvasPoint) {
        *self.x.entry(OrderedCoordinate(point.x)).or_default() += 1;
        *self.y.entry(OrderedCoordinate(point.y)).or_default() += 1;
    }

    /// 移除即将被尾部滤波重算替换的旧点。
    fn remove(&mut self, point: CanvasPoint) {
        Self::remove_coordinate(&mut self.x, OrderedCoordinate(point.x));
        Self::remove_coordinate(&mut self.y, OrderedCoordinate(point.y));
    }

    /// 返回当前坐标集合的精确轴对齐边界。
    fn bounds(&self) -> Option<Bounds> {
        Some(Bounds {
            left: self.x.first_key_value()?.0.0,
            top: self.y.first_key_value()?.0.0,
            right: self.x.last_key_value()?.0.0,
            bottom: self.y.last_key_value()?.0.0,
        })
    }

    /// 清空一次结束、取消或替换手势留下的坐标计数。
    fn clear(&mut self) {
        self.x.clear();
        self.y.clear();
    }

    /// 递减一个坐标的多重计数，并在最后一个点移除后删除键。
    fn remove_coordinate(map: &mut BTreeMap<OrderedCoordinate, usize>, value: OrderedCoordinate) {
        let remove_key = {
            let count = map.get_mut(&value).expect("滤波 bounds 必须包含待替换坐标");
            *count -= 1;
            *count == 0
        };
        if remove_key {
            map.remove(&value);
        }
    }
}

/// 用 `f32::total_cmp` 为有限物理坐标提供与相等关系一致的稳定顺序。
#[derive(Debug, Clone, Copy)]
struct OrderedCoordinate(f32);

impl PartialEq for OrderedCoordinate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for OrderedCoordinate {}

impl PartialOrd for OrderedCoordinate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedCoordinate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
struct Bounds {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

pub(crate) const ACTIVE_AA_PAD: f32 = 2.0;
const ACTIVE_SPATIAL_CELL_SIZE: f32 = 128.0;
const MAX_SPATIAL_CELLS_PER_PRIMITIVE: i64 = 4096;
const MAX_REPLAY_HALO_SCAN: usize = 64;
const MAX_LOCAL_REPLAY_POINTS: usize = 256;
const CLAMP_LEFT: usize = 0;
const CLAMP_TOP: usize = 1;
const CLAMP_RIGHT: usize = 2;
const CLAMP_BOTTOM: usize = 3;
const CLAMP_SIDE_COUNT: usize = 4;

/// 返回一个有限像素范围覆盖的空间网格；超大 primitive 留在全局集合中。
fn spatial_cell_range(bounds: InkBounds) -> Option<(i32, i32, i32, i32)> {
    let left = (bounds.left / ACTIVE_SPATIAL_CELL_SIZE).floor() as i32;
    let top = (bounds.top / ACTIVE_SPATIAL_CELL_SIZE).floor() as i32;
    let right = (bounds.right / ACTIVE_SPATIAL_CELL_SIZE).floor() as i32;
    let bottom = (bounds.bottom / ACTIVE_SPATIAL_CELL_SIZE).floor() as i32;
    let columns = i64::from(right).saturating_sub(i64::from(left)) + 1;
    let rows = i64::from(bottom).saturating_sub(i64::from(top)) + 1;
    (columns > 0 && rows > 0 && columns.saturating_mul(rows) <= MAX_SPATIAL_CELLS_PER_PRIMITIVE)
        .then_some((left, top, right, bottom))
}

impl Bounds {
    /// 判断新 bounds 是否只向外扩张；收缩需要重新发现新受限控制点。
    const fn is_expansion_of(self, previous: Self) -> bool {
        self.left <= previous.left
            && self.top <= previous.top
            && self.right >= previous.right
            && self.bottom >= previous.bottom
    }

    /// 返回本次向外变化的具体边界，用于定位已登记的敏感段。
    fn changed_expansion_sides(self, previous: Self) -> impl Iterator<Item = usize> {
        [
            (self.left < previous.left).then_some(CLAMP_LEFT),
            (self.top < previous.top).then_some(CLAMP_TOP),
            (self.right > previous.right).then_some(CLAMP_RIGHT),
            (self.bottom > previous.bottom).then_some(CLAMP_BOTTOM),
        ]
        .into_iter()
        .flatten()
    }

    /// 使用当前全局 filtered bounds 限制一段的两个控制点。
    fn clamp_segment(self, mut segment: CubicBezierSegment) -> CubicBezierSegment {
        for control in [&mut segment.control1, &mut segment.control2] {
            control.x = control.x.clamp(self.left, self.right);
            control.y = control.y.clamp(self.top, self.bottom);
        }
        segment
    }

    /// 返回未 clamp 段依赖的四侧边界索引。
    fn sensitive_sides(self, segment: CubicBezierSegment) -> impl Iterator<Item = usize> {
        let controls = [segment.control1, segment.control2];
        [
            controls
                .iter()
                .any(|control| control.x < self.left)
                .then_some(CLAMP_LEFT),
            controls
                .iter()
                .any(|control| control.y < self.top)
                .then_some(CLAMP_TOP),
            controls
                .iter()
                .any(|control| control.x > self.right)
                .then_some(CLAMP_RIGHT),
            controls
                .iter()
                .any(|control| control.y > self.bottom)
                .then_some(CLAMP_BOTTOM),
        ]
        .into_iter()
        .flatten()
    }
}

/// 返回固定宽尾部的保守像素范围，包括描边半径和 analytic AA 边缘。
pub(crate) fn fixed_ink_bounds(
    filtered_points: &[CanvasPoint],
    segments: &[CubicBezierSegment],
    width: f32,
) -> Option<InkBounds> {
    let radius = if segments.is_empty() && filtered_points.len() == 1 {
        width + ACTIVE_AA_PAD
    } else {
        width / 2.0 + ACTIVE_AA_PAD
    };
    if segments.is_empty() {
        return InkBounds::from_points(filtered_points, radius);
    }
    let first = segments.first()?;
    let mut bounds = InkBounds::from_points(&[first.start], 0.0)?;
    for segment in segments {
        for point in [
            segment.start,
            segment.control1,
            segment.control2,
            segment.end,
        ] {
            bounds.left = bounds.left.min(point.x);
            bounds.top = bounds.top.min(point.y);
            bounds.right = bounds.right.max(point.x);
            bounds.bottom = bounds.bottom.max(point.y);
        }
    }
    Some(bounds.expanded(radius))
}

/// 选择一个使两端人工 Round cap 都落在 dirty clip 外的有界固定宽重放区间。
fn bounded_fixed_replay_range(
    segments: &[CubicBezierSegment],
    affected: Range<usize>,
    dirty: InkBounds,
    width: f32,
) -> Option<Range<usize>> {
    let mut start = affected.start.saturating_sub(2);
    let mut end = affected.end.saturating_add(2).min(segments.len());
    let mut scanned = 0;
    while start > 0
        && circle_intersects_bounds(segments[start].start, width / 2.0 + ACTIVE_AA_PAD, dirty)
    {
        start -= 1;
        scanned += 1;
        if scanned > MAX_REPLAY_HALO_SCAN {
            return None;
        }
    }
    while end < segments.len()
        && circle_intersects_bounds(segments[end - 1].end, width / 2.0 + ACTIVE_AA_PAD, dirty)
    {
        end += 1;
        scanned += 1;
        if scanned > MAX_REPLAY_HALO_SCAN {
            return None;
        }
    }
    (end.saturating_sub(start) <= MAX_LOCAL_REPLAY_POINTS).then_some(start..end)
}

/// 返回旧/新可选范围的并集。
fn union_optional_bounds(left: Option<InkBounds>, right: Option<InkBounds>) -> Option<InkBounds> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.union(right)),
        (left, right) => left.or(right),
    }
}

/// 计算自然笔锋旧/新起收笔动态区、滤波邻接点及 outline halo 的并集。
fn natural_affected_ranges(
    distances: &[f32],
    old_len: usize,
    old_total: f32,
    new_total: f32,
    body: f32,
) -> Vec<Range<usize>> {
    let new_len = distances.len();
    if old_len == 0 || old_total <= body * 3.0 || new_total <= body * 3.0 {
        return std::iter::once(0..new_len).collect();
    }

    let old_start = (body * 1.5).min(old_total * 0.20);
    let new_start = (body * 1.5).min(new_total * 0.20);
    let head_end = distances
        .partition_point(|distance| *distance <= old_start.max(new_start))
        .saturating_add(2)
        .min(new_len);

    let old_end = (body * 3.0).min(old_total * 0.35);
    let new_end = (body * 3.0).min(new_total * 0.35);
    let old_tail_start = distances.partition_point(|distance| *distance < old_total - old_end);
    let new_tail_start = distances.partition_point(|distance| *distance < new_total - new_end);
    let tail_start = old_tail_start
        .min(new_tail_start)
        .min(old_len.saturating_sub(2))
        .saturating_sub(2);

    if head_end >= tail_start {
        std::iter::once(0..new_len).collect()
    } else {
        vec![0..head_end, tail_start..new_len]
    }
}

/// 返回自然笔锋一段受影响中心线及半宽的保守像素范围。
pub(crate) fn variable_ink_bounds(
    points: &[VariableStrokePoint],
    range: &Range<usize>,
) -> Option<InkBounds> {
    let start = range.start.min(points.len());
    let end = range.end.min(points.len());
    let selected = points.get(start..end)?;
    let first = selected.first()?;
    let mut bounds = InkBounds::from_points(&[first.point], 0.0)?;
    let mut max_radius = first.width / 2.0;
    for sample in &selected[1..] {
        bounds.left = bounds.left.min(sample.point.x);
        bounds.top = bounds.top.min(sample.point.y);
        bounds.right = bounds.right.max(sample.point.x);
        bounds.bottom = bounds.bottom.max(sample.point.y);
        max_radius = max_radius.max(sample.width / 2.0);
    }
    Some(bounds.expanded(max_radius + ACTIVE_AA_PAD))
}

/// 扩展自然笔锋重放 halo，确保局部闭合端落在 dirty clip 外。
fn bounded_natural_replay_range(
    points: &[VariableStrokePoint],
    affected: Range<usize>,
    dirty: InkBounds,
) -> Option<Range<usize>> {
    let mut start = affected.start.saturating_sub(4);
    let mut end = affected.end.saturating_add(4).min(points.len());
    let mut scanned = 0;
    while start > 0
        && circle_intersects_bounds(
            points[start].point,
            points[start].width / 2.0 + ACTIVE_AA_PAD,
            dirty,
        )
    {
        start -= 1;
        scanned += 1;
        if scanned > MAX_REPLAY_HALO_SCAN {
            return None;
        }
    }
    while end < points.len()
        && circle_intersects_bounds(
            points[end - 1].point,
            points[end - 1].width / 2.0 + ACTIVE_AA_PAD,
            dirty,
        )
    {
        end += 1;
        scanned += 1;
        if scanned > MAX_REPLAY_HALO_SCAN {
            return None;
        }
    }
    (end.saturating_sub(start) <= MAX_LOCAL_REPLAY_POINTS).then_some(start..end)
}

/// 合并几何上重叠的头尾 clip，避免同一 AA 像素被 source-over 重放两次。
fn merge_overlapping_natural_regions(
    regions: &mut Vec<ActiveStrokeReplay>,
    work: &mut ActiveStrokeWork,
) {
    if regions.len() != 2 {
        return;
    }
    let (
        ActiveStrokeReplay::Natural {
            bounds: first_bounds,
            point_range: first_range,
        },
        ActiveStrokeReplay::Natural {
            bounds: second_bounds,
            point_range: second_range,
        },
    ) = (&regions[0], &regions[1])
    else {
        return;
    };
    if !bounds_intersect(*first_bounds, *second_bounds) {
        return;
    }
    let bounds = first_bounds.union(*second_bounds);
    let point_range =
        first_range.start.min(second_range.start)..first_range.end.max(second_range.end);
    if point_range.len() > MAX_LOCAL_REPLAY_POINTS {
        regions.clear();
        work.full_fallback = true;
        work.full_redraw = true;
        return;
    }
    regions.clear();
    regions.push(ActiveStrokeReplay::Natural {
        bounds,
        point_range,
    });
}

/// 判断一个圆形 halo 是否可能影响指定 dirty clip。
fn circle_intersects_bounds(center: CanvasPoint, radius: f32, bounds: InkBounds) -> bool {
    center.x + radius >= bounds.left
        && center.x - radius <= bounds.right
        && center.y + radius >= bounds.top
        && center.y - radius <= bounds.bottom
}

/// 判断两个闭区间像素范围是否相交。
fn bounds_intersect(left: InkBounds, right: InkBounds) -> bool {
    left.left <= right.right
        && left.right >= right.left
        && left.top <= right.bottom
        && left.bottom >= right.top
}

/// 判断物理像素点是否可安全参与增量几何计算。
fn finite_point(point: CanvasPoint) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

/// 复现持久自然笔锋在指定弧长位置的宽度公式。
fn natural_width(distance: f32, total: f32, body: f32) -> f32 {
    if total == 0.0 {
        return body * 0.70;
    }
    if total <= body * 3.0 {
        return body;
    }
    let start = (body * 1.5).min(total * 0.20);
    let end = (body * 3.0).min(total * 0.35);
    let start_progress = (distance / start).clamp(0.0, 1.0);
    let end_progress = ((total - distance) / end).clamp(0.0, 1.0);
    let smooth = |value: f32| value * value * (3.0 - 2.0 * value);
    let start_width = body * (0.70 + 0.30 * smooth(start_progress));
    let end_width = body * (0.25 + 0.75 * smooth(end_progress));
    start_width.min(end_width).clamp(0.0, body)
}

/// 使用既有 0.25/0.5/0.25 公式计算指定索引的滤波位置。
fn filtered_point_at(points: &[CanvasPoint], index: usize) -> CanvasPoint {
    if points.len() < 3 || index == 0 || index + 1 == points.len() {
        return points[index];
    }
    CanvasPoint::new(
        points[index - 1].x * 0.25 + points[index].x * 0.5 + points[index + 1].x * 0.25,
        points[index - 1].y * 0.25 + points[index].y * 0.5 + points[index + 1].y * 0.25,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::{NaturalStrokeBuilder, stroke_geometry::light_filter_variable_points};

    /// 创建测试使用的固定宽活动增量。
    fn fixed_delta(
        id: u64,
        revision: u64,
        from: usize,
        points: &[CanvasPoint],
    ) -> ActiveStrokeDelta {
        ActiveStrokeDelta {
            gesture_id: id,
            revision,
            from_sample: from,
            samples: points.to_vec(),
            style: ActiveStrokeStyle::Fixed {
                color: InkColor::Red,
                width: PenWidth::Px4,
            },
            full_resync: false,
        }
    }

    /// 构造尾部停在早期水平中段上方的长路径，下一点向上移动时会穿过稳定前缀。
    fn long_path_before_stable_prefix_crossing() -> Vec<CanvasPoint> {
        let mut points = Vec::new();
        points.extend((0..=100).map(|step| CanvasPoint::new(20.0 + step as f32 * 2.0, 20.0)));
        points.extend((1..=100).map(|step| CanvasPoint::new(220.0, 20.0 + step as f32 * 2.0)));
        points.extend((1..=60).map(|step| CanvasPoint::new(220.0 - step as f32 * 2.0, 220.0)));
        points.extend((1..=96).map(|step| CanvasPoint::new(100.0, 220.0 - step as f32 * 2.0)));
        points
    }

    /// 将维护中的滤波坐标多重集合与测试侧按完整点集重建的参考结果比较。
    fn assert_filtered_bounds_match_points(cache: &ActiveStrokeRenderCache) {
        let mut expected = FilteredPointBounds::default();
        for point in &cache.filtered_points {
            expected.insert(*point);
        }
        assert_eq!(cache.filtered_bounds.x, expected.x);
        assert_eq!(cache.filtered_bounds.y, expected.y);
        assert_eq!(cache.filtered_bounds.bounds(), expected.bounds());
        assert_eq!(
            cache.filtered_bounds.x.values().sum::<usize>(),
            cache.filtered_points.len()
        );
        assert_eq!(
            cache.filtered_bounds.y.values().sum::<usize>(),
            cache.filtered_points.len()
        );
    }

    /// 验证独立坐标重数只在最后一个副本移除后删键，且有限坐标使用 total_cmp 顺序。
    #[test]
    fn filtered_point_bounds_preserve_duplicate_counts_and_total_order() {
        let mut bounds = FilteredPointBounds::default();
        let first = CanvasPoint::new(1.0, 2.0);
        let second = CanvasPoint::new(1.0, 3.0);
        let third = CanvasPoint::new(4.0, 2.0);
        for point in [first, second, third] {
            bounds.insert(point);
        }

        assert_eq!(bounds.x.get(&OrderedCoordinate(1.0)), Some(&2));
        assert_eq!(bounds.y.get(&OrderedCoordinate(2.0)), Some(&2));
        assert_eq!(
            bounds.bounds(),
            Some(Bounds {
                left: 1.0,
                top: 2.0,
                right: 4.0,
                bottom: 3.0,
            })
        );

        bounds.remove(first);
        assert_eq!(bounds.x.get(&OrderedCoordinate(1.0)), Some(&1));
        assert_eq!(bounds.y.get(&OrderedCoordinate(2.0)), Some(&1));
        bounds.remove(second);
        assert!(!bounds.x.contains_key(&OrderedCoordinate(1.0)));
        assert!(!bounds.y.contains_key(&OrderedCoordinate(3.0)));
        assert_eq!(
            bounds.bounds(),
            Some(Bounds {
                left: 4.0,
                top: 2.0,
                right: 4.0,
                bottom: 2.0,
            })
        );
        assert_eq!(
            OrderedCoordinate(-0.0).cmp(&OrderedCoordinate(0.0)),
            Ordering::Less
        );
        assert_eq!(
            OrderedCoordinate(-10.0).cmp(&OrderedCoordinate(10.0)),
            Ordering::Less
        );
    }

    /// 验证尾部替换、full resync、新手势和 clear 都保持或重置精确坐标多重集合。
    #[test]
    fn filtered_point_bounds_follow_tail_and_gesture_lifecycle() {
        let initial = [
            CanvasPoint::new(5.0, 0.0),
            CanvasPoint::new(5.0, 10.0),
            CanvasPoint::new(5.0, 0.0),
        ];
        let mut cache = ActiveStrokeRenderCache::default();
        cache.apply_delta(&fixed_delta(40, 1, 0, &initial)).unwrap();
        assert_filtered_bounds_match_points(&cache);
        assert_eq!(
            cache.filtered_bounds.x.get(&OrderedCoordinate(5.0)),
            Some(&3)
        );
        assert_eq!(
            cache.filtered_bounds.y.get(&OrderedCoordinate(0.0)),
            Some(&2)
        );

        let appended = [CanvasPoint::new(5.0, 20.0), CanvasPoint::new(5.0, 20.0)];
        cache
            .apply_delta(&fixed_delta(40, 2, initial.len(), &appended))
            .unwrap();
        assert_filtered_bounds_match_points(&cache);
        assert_eq!(
            cache.filtered_bounds.x.get(&OrderedCoordinate(5.0)),
            Some(&5)
        );
        assert_eq!(
            cache.filtered_bounds.y.get(&OrderedCoordinate(0.0)),
            Some(&1)
        );

        let resync = [
            CanvasPoint::new(30.0, 30.0),
            CanvasPoint::new(40.0, 35.0),
            CanvasPoint::new(50.0, 30.0),
        ];
        cache
            .apply_delta(&ActiveStrokeDelta {
                gesture_id: 40,
                revision: 3,
                from_sample: 0,
                samples: resync.to_vec(),
                style: ActiveStrokeStyle::Fixed {
                    color: InkColor::Red,
                    width: PenWidth::Px4,
                },
                full_resync: true,
            })
            .unwrap();
        assert_filtered_bounds_match_points(&cache);
        assert!(
            !cache
                .filtered_bounds
                .x
                .contains_key(&OrderedCoordinate(5.0))
        );

        cache
            .apply_delta(&ActiveStrokeDelta {
                gesture_id: 41,
                revision: 1,
                from_sample: 0,
                samples: vec![CanvasPoint::new(60.0, 60.0)],
                style: ActiveStrokeStyle::Natural {
                    color: InkColor::Red,
                    body_width: PenWidth::Px4,
                },
                full_resync: false,
            })
            .unwrap();
        assert!(cache.filtered_points.is_empty());
        assert!(cache.filtered_bounds.bounds().is_none());

        cache.clear();
        assert!(cache.filtered_bounds.x.is_empty());
        assert!(cache.filtered_bounds.y.is_empty());
        assert!(cache.filtered_bounds.bounds().is_none());
    }

    /// 验证逐点固定宽更新与一次性完整构建拥有相同滤波点和 cubic。
    #[test]
    fn incremental_fixed_geometry_matches_full_geometry() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(10.0, 10.0),
            CanvasPoint::new(5.0, 5.0),
            CanvasPoint::new(6.0, 6.0),
            CanvasPoint::new(7.0, 7.0),
        ];
        let mut incremental = ActiveStrokeRenderCache::default();
        incremental
            .apply_delta(&fixed_delta(1, 1, 0, &points[..1]))
            .unwrap();
        for (index, point) in points.iter().enumerate().skip(1) {
            incremental
                .apply_delta(&fixed_delta(1, index as u64 + 1, index, &[*point]))
                .unwrap();
        }
        let mut full = ActiveStrokeRenderCache::default();
        full.apply_full(
            1,
            points.len() as u64,
            ActiveStrokeStyle::Fixed {
                color: InkColor::Red,
                width: PenWidth::Px4,
            },
            &points,
        )
        .unwrap();
        assert_eq!(incremental.geometry(), full.geometry());
        assert_eq!(incremental.primitives, full.primitives);
    }

    /// 验证固定宽混合批次追加与一次性完整构建拥有完全相同的滤波点和 cubic。
    #[test]
    fn batched_fixed_geometry_matches_full_geometry() {
        let points = [
            CanvasPoint::new(0.0, 20.0),
            CanvasPoint::new(12.0, 4.0),
            CanvasPoint::new(28.0, 36.0),
            CanvasPoint::new(48.0, 8.0),
            CanvasPoint::new(72.0, 40.0),
            CanvasPoint::new(96.0, 12.0),
            CanvasPoint::new(124.0, 28.0),
            CanvasPoint::new(152.0, 16.0),
        ];
        let mut incremental = ActiveStrokeRenderCache::default();
        let mut from = 0;
        for (revision, batch_len) in [2, 3, 1, 2].into_iter().enumerate() {
            let end = from + batch_len;
            incremental
                .apply_delta(&fixed_delta(
                    17,
                    revision as u64 + 1,
                    from,
                    &points[from..end],
                ))
                .unwrap();
            from = end;
        }
        let mut full = ActiveStrokeRenderCache::default();
        full.apply_full(
            17,
            4,
            ActiveStrokeStyle::Fixed {
                color: InkColor::Red,
                width: PenWidth::Px4,
            },
            &points,
        )
        .unwrap();

        assert_eq!(incremental.geometry(), full.geometry());
        assert_eq!(incremental.primitives, full.primitives);
    }

    /// 验证全局 clamp bounds 稳定后，固定宽追加只重算有界 cubic 尾部。
    #[test]
    fn fixed_tail_work_is_bounded_when_global_bounds_stay_stable() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(100.0, 0.0),
            CanvasPoint::new(100.0, 100.0),
            CanvasPoint::new(0.0, 100.0),
            CanvasPoint::new(50.0, 50.0),
            CanvasPoint::new(60.0, 60.0),
        ];
        let mut cache = ActiveStrokeRenderCache::default();
        cache
            .apply_delta(&fixed_delta(18, 1, 0, &points[..5]))
            .unwrap();

        let work = cache
            .apply_delta(&fixed_delta(18, 2, 5, &points[5..]))
            .unwrap();

        assert!(work.recomputed_primitives <= 3);
        assert!(!work.full_redraw);
    }

    /// 验证单调长笔每次扩张全局 bounds 时仍只重算有界尾部，而不遍历完整 cubic。
    #[test]
    fn fixed_monotonic_growth_keeps_per_revision_work_bounded() {
        let mut cache = ActiveStrokeRenderCache::default();
        cache
            .apply_delta(&fixed_delta(19, 1, 0, &[CanvasPoint::new(0.0, 32.0)]))
            .unwrap();
        let mut previous_right = cache.bounds.expect("首点必须建立 bounds").right;

        for index in 1..1_024 {
            let work = cache
                .apply_delta(&fixed_delta(
                    19,
                    index as u64 + 1,
                    index,
                    &[CanvasPoint::new(index as f32, 32.0)],
                ))
                .unwrap();
            let right = cache.bounds.expect("长笔必须保留 bounds").right;
            assert!(right > previous_right);
            assert!(work.recomputed_primitives <= 4);
            assert!(!work.full_fallback);
            previous_right = right;
        }

        let mut full = ActiveStrokeRenderCache::default();
        full.apply_full(
            19,
            1_024,
            ActiveStrokeStyle::Fixed {
                color: InkColor::Red,
                width: PenWidth::Px4,
            },
            &cache.raw_points,
        )
        .unwrap();
        assert_eq!(cache.primitives, full.primitives);
    }

    /// 验证 bounds 扩张会更新已登记的旧 clamp-sensitive 段且保持完整构建等价。
    #[test]
    fn fixed_expansion_reclamps_only_registered_sensitive_segments() {
        let initial = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(12.0, 0.0),
            CanvasPoint::new(12.0, 12.0),
            CanvasPoint::new(0.0, 12.0),
            CanvasPoint::new(0.0, 0.0),
        ];
        let mut cache = ActiveStrokeRenderCache::default();
        cache.apply_delta(&fixed_delta(20, 1, 0, &initial)).unwrap();
        assert!(
            cache
                .clamp_sensitive
                .iter()
                .any(|indices| !indices.is_empty())
        );
        let previous_bounds = cache.bounds.expect("初始固定宽笔画必须拥有 bounds");
        let previous_sensitive = cache.clamp_sensitive.clone();
        let old_len = cache.filtered_points.len();

        let work = cache
            .apply_delta(&fixed_delta(
                20,
                2,
                initial.len(),
                &[CanvasPoint::new(24.0, 0.0)],
            ))
            .unwrap();
        let new_bounds = cache.bounds.expect("扩张后的固定宽笔画必须拥有 bounds");
        let segment_count = cache.primitives.len();
        let changed_start = old_len
            .saturating_sub(1)
            .saturating_sub(2)
            .min(segment_count);
        let mut expected: HashSet<usize> = HashSet::from_iter(changed_start..segment_count);
        for side in new_bounds.changed_expansion_sides(previous_bounds) {
            expected.extend(
                previous_sensitive[side]
                    .iter()
                    .copied()
                    .filter(|index| *index < segment_count),
            );
        }
        let mut full = ActiveStrokeRenderCache::default();
        let mut all_points = initial.to_vec();
        all_points.push(CanvasPoint::new(24.0, 0.0));
        full.apply_full(
            20,
            2,
            ActiveStrokeStyle::Fixed {
                color: InkColor::Red,
                width: PenWidth::Px4,
            },
            &all_points,
        )
        .unwrap();

        assert_eq!(cache.primitives, full.primitives);
        assert_eq!(work.recomputed_primitives, expected.len());
        assert!(work.recomputed_primitives < cache.primitives.len());
        assert!(!work.full_fallback);
    }

    /// 验证远距离 clamp-sensitive 旧段与新增尾部形成大跨度时执行可观测 full fallback。
    #[test]
    fn fixed_large_affected_span_executes_observable_full_fallback() {
        let mut initial = vec![
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(12.0, 0.0),
            CanvasPoint::new(12.0, 12.0),
            CanvasPoint::new(0.0, 12.0),
            CanvasPoint::new(0.0, 0.0),
        ];
        initial.extend(std::iter::repeat_n(CanvasPoint::new(6.0, 6.0), 300));
        let mut cache = ActiveStrokeRenderCache::default();
        cache.apply_delta(&fixed_delta(22, 1, 0, &initial)).unwrap();
        let previous_bounds = cache.bounds.expect("长笔必须拥有 bounds");
        let (side, oldest_sensitive) = cache
            .clamp_sensitive
            .iter()
            .enumerate()
            .filter_map(|(side, indices)| indices.iter().copied().min().map(|index| (side, index)))
            .min_by_key(|(_, index)| *index)
            .expect("测试笔画必须产生早期 clamp-sensitive 段");
        assert!(oldest_sensitive + MAX_LOCAL_REPLAY_POINTS < cache.primitives.len());
        let expansion = match side {
            CLAMP_LEFT => CanvasPoint::new(previous_bounds.left - 100.0, 6.0),
            CLAMP_TOP => CanvasPoint::new(6.0, previous_bounds.top - 100.0),
            CLAMP_RIGHT => CanvasPoint::new(previous_bounds.right + 100.0, 6.0),
            CLAMP_BOTTOM => CanvasPoint::new(6.0, previous_bounds.bottom + 100.0),
            _ => unreachable!("clamp side 索引必须有效"),
        };

        let work = cache
            .apply_delta(&fixed_delta(22, 2, initial.len(), &[expansion]))
            .unwrap();

        assert!(work.full_fallback);
        assert!(work.full_redraw);
        assert_eq!(work.recomputed_primitives, cache.primitives.len());
    }

    /// 验证有界固定宽重放把左右两个人工 Round cap 都扩展到 dirty 像素之外。
    #[test]
    fn bounded_fixed_replay_keeps_both_artificial_caps_outside_dirty_pixels() {
        let segments: Vec<_> = (0..20)
            .map(|index| {
                let start = CanvasPoint::new(index as f32 * 10.0, 0.0);
                let end = CanvasPoint::new((index + 1) as f32 * 10.0, 0.0);
                CubicBezierSegment {
                    start,
                    control1: start,
                    control2: end,
                    end,
                }
            })
            .collect();
        let dirty = InkBounds::from_xywh(55.0, -4.0, 90.0, 8.0);
        let range = bounded_fixed_replay_range(&segments, 10..11, dirty, 4.0)
            .expect("双侧有界 halo 应可找到");
        let cap_radius = 4.0 / 2.0 + ACTIVE_AA_PAD;

        assert!(range.start > 0);
        assert!(range.end < segments.len());
        assert!(!circle_intersects_bounds(
            segments[range.start].start,
            cap_radius,
            dirty
        ));
        assert!(!circle_intersects_bounds(
            segments[range.end - 1].end,
            cap_radius,
            dirty
        ));
    }

    /// 验证 filtered bounds 收缩时执行并上报完整 fallback，不保留旧 clamp 结果。
    #[test]
    fn fixed_bounds_shrink_executes_observable_full_fallback() {
        let mut cache = ActiveStrokeRenderCache::default();
        cache
            .apply_delta(&fixed_delta(
                21,
                1,
                0,
                &[CanvasPoint::new(0.0, 0.0), CanvasPoint::new(100.0, 0.0)],
            ))
            .unwrap();

        let work = cache
            .apply_delta(&fixed_delta(21, 2, 2, &[CanvasPoint::new(0.0, 0.0)]))
            .unwrap();
        let mut full = ActiveStrokeRenderCache::default();
        full.apply_full(
            21,
            2,
            ActiveStrokeStyle::Fixed {
                color: InkColor::Red,
                width: PenWidth::Px4,
            },
            &[
                CanvasPoint::new(0.0, 0.0),
                CanvasPoint::new(100.0, 0.0),
                CanvasPoint::new(0.0, 0.0),
            ],
        )
        .unwrap();

        assert!(work.full_fallback);
        assert!(work.full_redraw);
        assert_eq!(cache.primitives, full.primitives);
    }

    /// 验证尾部穿过稳定前缀时 fixed/natural 放弃会清除交叉像素的局部 Src replay。
    #[test]
    fn stable_prefix_crossing_executes_full_fallback_for_fixed_and_natural() {
        let points = long_path_before_stable_prefix_crossing();
        assert!(points.len() > MAX_LOCAL_REPLAY_POINTS);
        for style in [
            ActiveStrokeStyle::Fixed {
                color: InkColor::Red,
                width: PenWidth::Px8,
            },
            ActiveStrokeStyle::Natural {
                color: InkColor::Red,
                body_width: PenWidth::Px8,
            },
        ] {
            let mut cache = ActiveStrokeRenderCache::default();
            cache.apply_full(31, 1, style, &points).unwrap();

            let work = cache
                .apply_delta(&ActiveStrokeDelta {
                    gesture_id: 31,
                    revision: 2,
                    from_sample: points.len(),
                    samples: vec![CanvasPoint::new(100.0, 12.0)],
                    style,
                    full_resync: false,
                })
                .unwrap();

            assert!(work.full_fallback, "{style:?}");
            assert!(work.full_redraw, "{style:?}");
            assert!(cache.replay_regions().is_empty(), "{style:?}");
        }
    }

    /// 验证序列断裂不会修改 retained 几何或重复上报上一帧工作量。
    #[test]
    fn discontinuous_delta_is_rejected_without_silent_geometry_corruption() {
        let mut cache = ActiveStrokeRenderCache::default();
        cache
            .apply_delta(&fixed_delta(2, 1, 0, &[CanvasPoint::new(0.0, 0.0)]))
            .unwrap();
        let error = cache
            .apply_delta(&fixed_delta(2, 2, 3, &[CanvasPoint::new(4.0, 0.0)]))
            .unwrap_err();
        assert_eq!(error, ActiveStrokeSequenceError::Discontinuous);
        assert_eq!(cache.last_work(), ActiveStrokeWork::default());
    }

    /// 验证完整自然笔锋宽度始终有限且为正。
    #[test]
    fn natural_widths_match_geometry_only_formula() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(8.0, 0.0),
            CanvasPoint::new(40.0, 0.0),
            CanvasPoint::new(80.0, 0.0),
        ];
        let mut cache = ActiveStrokeRenderCache::default();
        cache
            .apply_full(
                3,
                1,
                ActiveStrokeStyle::Natural {
                    color: InkColor::Red,
                    body_width: PenWidth::Px8,
                },
                &points,
            )
            .unwrap();
        assert_eq!(cache.sample_count(), points.len());
        assert!(
            cache
                .geometry()
                .1
                .iter()
                .all(|point| point.width.is_finite() && point.width > 0.0)
        );
    }

    /// 验证逐点自然笔锋更新与一次性完整构建严格等价。
    #[test]
    fn incremental_natural_geometry_matches_full_geometry() {
        let points = [
            CanvasPoint::new(0.0, 0.0),
            CanvasPoint::new(8.0, 0.0),
            CanvasPoint::new(40.0, 4.0),
            CanvasPoint::new(80.0, 8.0),
            CanvasPoint::new(120.0, 4.0),
            CanvasPoint::new(180.0, 0.0),
        ];
        let style = ActiveStrokeStyle::Natural {
            color: InkColor::Red,
            body_width: PenWidth::Px8,
        };
        let mut incremental = ActiveStrokeRenderCache::default();
        incremental
            .apply_delta(&ActiveStrokeDelta {
                gesture_id: 5,
                revision: 1,
                from_sample: 0,
                samples: points[..1].to_vec(),
                style,
                full_resync: false,
            })
            .unwrap();
        for (index, point) in points.iter().enumerate().skip(1) {
            incremental
                .apply_delta(&ActiveStrokeDelta {
                    gesture_id: 5,
                    revision: index as u64 + 1,
                    from_sample: index,
                    samples: vec![*point],
                    style,
                    full_resync: false,
                })
                .unwrap();
        }
        let mut full = ActiveStrokeRenderCache::default();
        full.apply_full(5, points.len() as u64, style, &points)
            .unwrap();
        assert_eq!(incremental.geometry().1, full.geometry().1);
    }

    /// 验证总弧长从 60px 增长到 100px 后旧收笔点恢复 8px body width。
    #[test]
    fn natural_growth_restores_samples_that_leave_the_old_end_taper() {
        let style = ActiveStrokeStyle::Natural {
            color: InkColor::Red,
            body_width: PenWidth::Px8,
        };
        let mut cache = ActiveStrokeRenderCache::default();
        cache
            .apply_delta(&ActiveStrokeDelta {
                gesture_id: 8,
                revision: 1,
                from_sample: 0,
                samples: vec![
                    CanvasPoint::new(0.0, 0.0),
                    CanvasPoint::new(45.0, 0.0),
                    CanvasPoint::new(60.0, 0.0),
                ],
                style,
                full_resync: false,
            })
            .unwrap();
        assert!(cache.geometry().1[1].width < 8.0);

        cache
            .apply_delta(&ActiveStrokeDelta {
                gesture_id: 8,
                revision: 2,
                from_sample: 3,
                samples: vec![CanvasPoint::new(100.0, 0.0)],
                style,
                full_resync: false,
            })
            .unwrap();

        assert_eq!(cache.geometry().1[1].width, 8.0);
    }

    /// 验证四档宽度、全部自然笔锋阈值和混合批次都与完整 builder 严格等价。
    #[test]
    fn natural_threshold_and_batched_growth_match_full_builder_exactly() {
        for width in [PenWidth::Px4, PenWidth::Px6, PenWidth::Px8, PenWidth::Px16] {
            let body = width.pixels();
            let totals = [
                body * 3.0 - 1.0,
                body * 3.0,
                body * 3.0 + 1.0,
                body * 7.5 - 1.0,
                body * 7.5 + 1.0,
                body * 3.0 / 0.35 - 1.0,
                body * 3.0 / 0.35 + 1.0,
            ];
            for total in totals {
                let mut points = vec![CanvasPoint::new(0.0, 32.0)];
                let mut x = 1.0;
                while x < total {
                    points.push(CanvasPoint::new(x, 32.0));
                    x += 1.0;
                }
                if total - points.last().expect("测试点集必须非空").x < 0.5 {
                    points.last_mut().expect("测试点集必须非空").x = total;
                } else {
                    points.push(CanvasPoint::new(total, 32.0));
                }

                let style = ActiveStrokeStyle::Natural {
                    color: InkColor::Red,
                    body_width: width,
                };
                let mut cache = ActiveStrokeRenderCache::default();
                let mut builder = NaturalStrokeBuilder::new(points[0], body).unwrap();
                let mut consumed: usize = 0;
                let mut revision = 1;
                for batch_size in [1, 3, 7, usize::MAX] {
                    let end = consumed.saturating_add(batch_size).min(points.len());
                    if end == consumed {
                        continue;
                    }
                    for point in &points[consumed.max(1)..end] {
                        assert!(builder.push(*point));
                    }
                    cache
                        .apply_delta(&ActiveStrokeDelta {
                            gesture_id: 44,
                            revision,
                            from_sample: consumed,
                            samples: points[consumed..end].to_vec(),
                            style,
                            full_resync: false,
                        })
                        .unwrap();
                    let expected = light_filter_variable_points(&builder.finalized_points())
                        .expect("完整自然笔锋应能滤波");
                    assert_eq!(cache.geometry().1, expected);
                    consumed = end;
                    revision += 1;
                    if consumed == points.len() {
                        break;
                    }
                }
            }
        }
    }

    /// 验证 sample-zero full resync 实际重建全部点并报告一次真实 fallback。
    #[test]
    fn explicit_full_resync_executes_and_reports_real_fallback() {
        let style = ActiveStrokeStyle::Fixed {
            color: InkColor::Red,
            width: PenWidth::Px6,
        };
        let points = [
            CanvasPoint::new(2.0, 2.0),
            CanvasPoint::new(8.0, 5.0),
            CanvasPoint::new(16.0, 4.0),
        ];
        let mut cache = ActiveStrokeRenderCache::default();
        cache
            .apply_delta(&ActiveStrokeDelta {
                gesture_id: 9,
                revision: 1,
                from_sample: 0,
                samples: points[..2].to_vec(),
                style,
                full_resync: false,
            })
            .unwrap();

        let work = cache
            .apply_delta(&ActiveStrokeDelta {
                gesture_id: 9,
                revision: 2,
                from_sample: 0,
                samples: points.to_vec(),
                style,
                full_resync: true,
            })
            .unwrap();

        assert!(work.full_fallback);
        assert!(work.full_redraw);
        assert_eq!(cache.sample_count(), points.len());
    }

    /// 验证新 gesture ID 从 sample zero 替换旧缓存，End/Cancel 共用的 clear 清空几何。
    #[test]
    fn new_gesture_replaces_old_cache_and_clear_removes_preview() {
        let mut cache = ActiveStrokeRenderCache::default();
        cache
            .apply_delta(&fixed_delta(
                31,
                1,
                0,
                &[CanvasPoint::new(2.0, 2.0), CanvasPoint::new(8.0, 4.0)],
            ))
            .unwrap();

        let replacement = cache
            .apply_delta(&fixed_delta(32, 1, 0, &[CanvasPoint::new(20.0, 20.0)]))
            .unwrap();
        assert!(replacement.full_redraw);
        assert_eq!(cache.sample_count(), 1);
        assert_eq!(cache.geometry().0, &[CanvasPoint::new(20.0, 20.0)]);

        cache.clear();
        assert_eq!(cache.sample_count(), 0);
        assert_eq!(cache.geometry(), (&[][..], &[][..]));
        assert!(cache.style().is_none());
    }
}
