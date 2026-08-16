use skia_safe::{
    AlphaType, BlendMode, Canvas, ClipOp, Color, ColorType, IRect, ImageInfo, Paint, PaintCap,
    PaintJoin, PaintStyle, PathBuilder, Rect, RoundOut, SamplingOptions, Surface,
    gpu::{self, Budgeted, DirectContext, SurfaceOrigin},
};

use super::{
    BatchDrawer, CanvasPoint, EraseSample, EraseStroke, EraserSize, InkBounds, InkColor,
    InkDocument, InkOperation, InkSpatialIndex, InkTool, OperationId, PenWidth,
    VariableStrokePoint,
    active_stroke::{
        ActiveStrokeRenderCache, ActiveStrokeReplay, ActiveStrokeStyle, fixed_ink_bounds,
        variable_ink_bounds,
    },
    stroke_geometry::{
        CubicBezierSegment, append_closed_bezier_path, append_open_bezier_path,
        light_filter_points, light_filter_variable_points, variable_outline,
    },
};
use crate::error::AppError;

/// 正在输入、尚未提交为 operation 的墨迹预览。
#[derive(Debug, Clone, Copy)]
pub enum ActiveInkPreview<'a> {
    Tool {
        points: &'a [CanvasPoint],
        tool: InkTool,
        color: InkColor,
        pen_width: PenWidth,
        eraser_size: EraserSize,
    },
    VariableTool {
        points: &'a [VariableStrokePoint],
        color: InkColor,
        eraser_size: EraserSize,
    },
    PalmErase {
        samples: &'a [EraseSample],
    },
}

/// 可安全移交渲染线程的 owned 活动墨迹预览。
#[derive(Debug, Clone, PartialEq)]
pub enum OwnedActiveInkPreview {
    Tool {
        points: Vec<CanvasPoint>,
        tool: InkTool,
        color: InkColor,
        pen_width: PenWidth,
        eraser_size: EraserSize,
    },
    VariableTool {
        points: Vec<VariableStrokePoint>,
        color: InkColor,
        eraser_size: EraserSize,
    },
    PalmErase {
        samples: Vec<EraseSample>,
    },
}

impl OwnedActiveInkPreview {
    /// 返回借用当前 owned 数据的渲染预览描述。
    pub fn as_borrowed(&self) -> ActiveInkPreview<'_> {
        match self {
            Self::Tool {
                points,
                tool,
                color,
                pen_width,
                eraser_size,
            } => ActiveInkPreview::Tool {
                points,
                tool: *tool,
                color: *color,
                pen_width: *pen_width,
                eraser_size: *eraser_size,
            },
            Self::VariableTool {
                points,
                color,
                eraser_size,
            } => ActiveInkPreview::VariableTool {
                points,
                color: *color,
                eraser_size: *eraser_size,
            },
            Self::PalmErase { samples } => ActiveInkPreview::PalmErase { samples },
        }
    }
}

impl From<ActiveInkPreview<'_>> for OwnedActiveInkPreview {
    /// 复制活动手势的当前采样，形成与 UI 状态解耦的帧快照。
    fn from(preview: ActiveInkPreview<'_>) -> Self {
        match preview {
            ActiveInkPreview::Tool {
                points,
                tool,
                color,
                pen_width,
                eraser_size,
            } => Self::Tool {
                points: points.to_vec(),
                tool,
                color,
                pen_width,
                eraser_size,
            },
            ActiveInkPreview::VariableTool {
                points,
                color,
                eraser_size,
            } => Self::VariableTool {
                points: points.to_vec(),
                color,
                eraser_size,
            },
            ActiveInkPreview::PalmErase { samples } => Self::PalmErase {
                samples: samples.to_vec(),
            },
        }
    }
}

/// 返回活动预览是否仍是刚刚提交为待定擦除的同一手势。
fn active_preview_matches_erase(preview: ActiveInkPreview<'_>, stroke: &EraseStroke) -> bool {
    match preview {
        ActiveInkPreview::Tool {
            points,
            tool: InkTool::RegionEraser,
            eraser_size,
            ..
        } => {
            stroke.samples.len() == points.len()
                && stroke.samples.iter().zip(points).all(|(sample, point)| {
                    *sample == EraseSample::circle(*point, eraser_size.pixels())
                })
        }
        ActiveInkPreview::PalmErase { samples } => stroke.samples.as_slice() == samples,
        ActiveInkPreview::Tool {
            tool: InkTool::Pen, ..
        }
        | ActiveInkPreview::VariableTool { .. } => false,
    }
}

const ERASE_RADIUS_EPSILON: f32 = 0.01;
const PALM_INTERPOLATION_STEP_FRACTION: f32 = 0.75;
const PALM_INTERPOLATION_MIN_STEP: f32 = 4.0;
const PALM_INTERPOLATION_MAX_STEP: f32 = 24.0;
const MAX_INCREMENTAL_REBUILD_AREA_RATIO: f32 = 0.1;

/// 返回 1x 墨迹 surface 的有效像素尺寸，并拒绝超出 Skia `i32` 范围的请求。
fn surface_size(logical_size: [u32; 2]) -> Option<[u32; 2]> {
    let width = logical_size[0].max(1);
    let height = logical_size[1].max(1);
    (width <= i32::MAX as u32 && height <= i32::MAX as u32).then_some([width, height])
}

/// 保守估算 1x RGBA 墨迹 surface 的颜色缓冲字节数。
fn estimate_surface_bytes(render_size: [u32; 2]) -> usize {
    (render_size[0] as usize)
        .saturating_mul(render_size[1] as usize)
        .saturating_mul(4)
}

/// 当前活动页唯一的持久 Skia GPU 墨迹层。
pub struct InkRenderCache {
    surface: Surface,
    logical_size: [u32; 2],
    render_size: [u32; 2],
    applied_operation_count: usize,
    last_operation_id: Option<OperationId>,
    last_operation_was_clear: bool,
    spatial_index: InkSpatialIndex,
    batch_drawer: BatchDrawer,
    pending_region_rebuild: Option<InkBounds>,
    full_rebuild_requested: bool,
    deferred_erase: Option<EraseStroke>,
}

/// 一帧同步对持久墨迹缓存执行的工作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InkSyncKind {
    Unchanged,
    Incremental,
    RegionRebuild,
    FullRebuild,
}

impl InkRenderCache {
    /// 为指定逻辑尺寸创建透明的 1x GPU 墨迹层。
    pub fn new(context: &mut DirectContext, logical_size: [u32; 2]) -> Result<Self, AppError> {
        let render_size = surface_size(logical_size)
            .ok_or_else(|| AppError::Graphics("墨迹 surface 尺寸超出 Skia 支持范围".to_owned()))?;
        let mut surface = create_gpu_surface(context, render_size)?;
        surface.canvas().clear(Color::TRANSPARENT);
        Ok(Self {
            surface,
            logical_size: [logical_size[0].max(1), logical_size[1].max(1)],
            render_size,
            applied_operation_count: 0,
            last_operation_id: None,
            last_operation_was_clear: false,
            spatial_index: InkSpatialIndex::new(logical_size),
            batch_drawer: BatchDrawer::new(),
            pending_region_rebuild: None,
            full_rebuild_requested: false,
            deferred_erase: None,
        })
    }

    /// 将尚未应用的新 operation 增量绘制到持久 GPU surface。
    pub fn sync(&mut self, document: &InkDocument) -> InkSyncKind {
        if self.full_rebuild_requested {
            self.rebuild(document);
            return InkSyncKind::FullRebuild;
        }
        if let Some(bounds) = self.pending_region_rebuild.take() {
            self.rebuild_region(document, bounds);
            return InkSyncKind::RegionRebuild;
        }

        let history = document.operations();
        let prefix_still_matches = self.applied_operation_count <= history.len()
            && match (self.last_operation_id, self.applied_operation_count) {
                (Some(last_id), count) if count > 0 => history[count - 1].id() == last_id,
                (None, 0) => true,
                _ => false,
            };

        if !prefix_still_matches {
            self.rebuild(document);
            return InkSyncKind::FullRebuild;
        }

        let new_operations = &history[self.applied_operation_count..];
        if new_operations.is_empty() {
            return InkSyncKind::Unchanged;
        }
        let started_at = tracing::enabled!(tracing::Level::DEBUG).then(std::time::Instant::now);
        self.commit_deferred_erase();
        let deferred_erase = new_operations.last().and_then(|operation| match operation {
            InkOperation::EraseStroke(stroke) => Some(stroke.clone()),
            _ => None,
        });
        let operations_to_draw = if deferred_erase.is_some() {
            &new_operations[..new_operations.len() - 1]
        } else {
            new_operations
        };
        let draw_calls = draw_operations(
            self.surface.canvas(),
            operations_to_draw.iter(),
            &mut self.batch_drawer,
        );
        self.deferred_erase = deferred_erase;
        for operation in new_operations {
            self.index_operation(operation);
        }
        if let Some(started_at) = started_at {
            tracing::debug!(
                operations = new_operations.len(),
                draw_calls,
                elapsed_micros = started_at.elapsed().as_micros(),
                "增量墨迹渲染完成"
            );
        }
        self.mark_document_applied(document);
        InkSyncKind::Incremental
    }

    /// 返回当前 GPU 墨迹层快照，供同一上下文合成到窗口 framebuffer。
    pub fn snapshot(&mut self, context: &mut DirectContext) -> skia_safe::Image {
        context.flush_and_submit_surface(&mut self.surface, None);
        self.surface.image_snapshot()
    }

    /// 把最后一次待定擦除绘制到窗口墨迹层之上，但不修改持久 GPU surface。
    pub(crate) fn draw_deferred_erase(&self, canvas: &Canvas) {
        if let Some(stroke) = self.deferred_erase.as_ref() {
            draw_erase_samples(canvas, &stroke.samples);
        }
    }

    /// 新手势开始前固化待定擦除；刚提交手势的残留预览不会提前固化。
    pub(crate) fn commit_deferred_erase_before_preview(&mut self, preview: ActiveInkPreview<'_>) {
        if self
            .deferred_erase
            .as_ref()
            .is_some_and(|stroke| active_preview_matches_erase(preview, stroke))
        {
            return;
        }
        self.commit_deferred_erase();
    }

    /// 在后续事实操作开始前，按文档顺序固化待定擦除。
    fn commit_deferred_erase(&mut self) {
        let Some(stroke) = self.deferred_erase.take() else {
            return;
        };
        draw_erase_samples(self.surface.canvas(), &stroke.samples);
    }

    /// 返回缓存使用的逻辑尺寸。
    pub const fn logical_size(&self) -> [u32; 2] {
        self.logical_size
    }

    /// 返回缓存使用的实际 GPU 尺寸。
    pub const fn render_size(&self) -> [u32; 2] {
        self.render_size
    }

    /// 返回持久墨迹 surface 的保守颜色缓冲字节估算。
    pub(crate) fn estimated_bytes(&self) -> usize {
        estimate_surface_bytes(self.render_size)
    }

    /// 强制下次同步从文档事实历史重建缓存。
    pub fn invalidate(&mut self) {
        self.full_rebuild_requested = true;
        self.pending_region_rebuild = None;
    }

    /// 请求下次同步只重建指定受影响区域。
    pub fn invalidate_region(&mut self, bounds: InkBounds) {
        if self.full_rebuild_requested {
            return;
        }
        let combined_bounds = self
            .pending_region_rebuild
            .map_or(bounds, |current| current.union(bounds));
        if is_small_rebuild_region(self.logical_size, combined_bounds) {
            self.pending_region_rebuild = Some(combined_bounds);
        } else {
            self.invalidate();
        }
    }

    /// 从最近一次清屏后的可见操作重建整个活动页 GPU surface。
    fn rebuild(&mut self, document: &InkDocument) {
        self.deferred_erase = None;
        self.surface.canvas().clear(Color::TRANSPARENT);
        let operations = document.replay_operations();
        let started_at = tracing::enabled!(tracing::Level::DEBUG).then(std::time::Instant::now);
        let draw_calls = draw_operations(
            self.surface.canvas(),
            operations.iter(),
            &mut self.batch_drawer,
        );
        self.rebuild_spatial_index(document);
        if let Some(started_at) = started_at {
            tracing::debug!(
                operations = operations.len(),
                draw_calls,
                elapsed_micros = started_at.elapsed().as_micros(),
                "全量墨迹重建完成"
            );
        }
        self.mark_document_applied(document);
        self.pending_region_rebuild = None;
        self.full_rebuild_requested = false;
    }

    /// 清理指定矩形并按事实历史顺序重放所有与该矩形相交的可见操作。
    fn rebuild_region(&mut self, document: &InkDocument, bounds: InkBounds) {
        let started_at = tracing::enabled!(tracing::Level::DEBUG).then(std::time::Instant::now);
        if self.discard_undone_deferred_erase(document) {
            if let Some(started_at) = started_at {
                tracing::debug!(
                    operations = 0,
                    draw_calls = 0,
                    elapsed_micros = started_at.elapsed().as_micros(),
                    "局部墨迹重建完成"
                );
            }
            self.mark_document_applied(document);
            return;
        }
        self.prepare_spatial_index_for_region(document);
        let mut operation_ids = self.spatial_index.query(bounds);
        operation_ids.sort_unstable_by_key(|id| id.get());
        let operations: Vec<_> = operation_ids
            .into_iter()
            .filter_map(|id| document.operation(id))
            .collect();
        let clip_rect = Rect::new(bounds.left, bounds.top, bounds.right, bounds.bottom);
        let canvas = self.surface.canvas();
        let save_count = canvas.save();
        canvas.clip_rect(clip_rect, ClipOp::Intersect, false);

        let mut clear_paint = Paint::default();
        clear_paint.set_style(PaintStyle::Fill);
        clear_paint.set_blend_mode(BlendMode::Clear);
        canvas.draw_rect(clip_rect, &clear_paint);
        let draw_calls =
            draw_operations(canvas, operations.iter().copied(), &mut self.batch_drawer);
        canvas.restore_to_count(save_count);
        let draw_calls = draw_calls + 1;
        if let Some(started_at) = started_at {
            tracing::debug!(
                operations = operations.len(),
                draw_calls,
                elapsed_micros = started_at.elapsed().as_micros(),
                "局部墨迹重建完成"
            );
        }
        self.mark_document_applied(document);
    }

    /// 把一个新事实操作同步到当前可见操作的空间索引。
    fn index_operation(&mut self, operation: &InkOperation) {
        if matches!(operation, InkOperation::Clear(_)) {
            self.spatial_index.clear();
        } else if let Some(bounds) = operation.bounds() {
            self.spatial_index.insert(operation.id(), bounds);
        }
    }

    /// 按最近一次清屏后的事实历史重建空间索引。
    fn rebuild_spatial_index(&mut self, document: &InkDocument) {
        self.spatial_index.rebuild(
            document
                .replay_operations()
                .iter()
                .filter_map(|operation| operation.bounds().map(|bounds| (operation.id(), bounds))),
        );
    }

    /// 为局部重建同步索引，优先处理无变化或单次尾部撤销。
    fn prepare_spatial_index_for_region(&mut self, document: &InkDocument) {
        let history = document.operations();
        let unchanged = self.applied_operation_count == history.len()
            && self.last_operation_id == history.last().map(InkOperation::id);
        if unchanged {
            return;
        }

        let current_tail_precedes_applied = match (history.last(), self.last_operation_id) {
            (Some(current), Some(applied)) => current.id().get() < applied.get(),
            (None, Some(_)) => true,
            _ => false,
        };
        let single_tail_removal = !self.last_operation_was_clear
            && self.applied_operation_count == history.len() + 1
            && current_tail_precedes_applied;
        if single_tail_removal && let Some(removed_id) = self.last_operation_id {
            self.spatial_index.remove(removed_id);
        } else {
            self.deferred_erase = None;
            self.rebuild_spatial_index(document);
        }
    }

    /// 丢弃尚未写入持久 surface 的尾部擦除，并同步移除空间索引项。
    fn discard_undone_deferred_erase(&mut self, document: &InkDocument) -> bool {
        let history = document.operations();
        let Some(deferred) = self.deferred_erase.as_ref() else {
            return false;
        };
        let deferred_id = deferred.id;
        let current_tail_precedes_deferred = history
            .last()
            .is_none_or(|operation| operation.id().get() < deferred_id.get());
        let single_tail_removal = self.applied_operation_count == history.len() + 1
            && self.last_operation_id == Some(deferred_id)
            && current_tail_precedes_deferred;
        if !single_tail_removal {
            return false;
        }

        self.deferred_erase = None;
        self.spatial_index.remove(deferred_id);
        true
    }

    /// 记录缓存和索引已经同步到文档事实历史末尾。
    fn mark_document_applied(&mut self, document: &InkDocument) {
        let history = document.operations();
        self.applied_operation_count = history.len();
        self.last_operation_id = history.last().map(InkOperation::id);
        self.last_operation_was_clear = history
            .last()
            .is_some_and(|operation| matches!(operation, InkOperation::Clear(_)));
    }
}

/// 返回目标区域裁剪到逻辑画布后是否适合局部重建。
fn is_small_rebuild_region(logical_size: [u32; 2], bounds: InkBounds) -> bool {
    let canvas_width = logical_size[0].max(1) as f32;
    let canvas_height = logical_size[1].max(1) as f32;
    let width = bounds.right.min(canvas_width) - bounds.left.max(0.0);
    let height = bounds.bottom.min(canvas_height) - bounds.top.max(0.0);
    let affected_area = width.max(0.0) * height.max(0.0);
    affected_area.is_finite()
        && affected_area < canvas_width * canvas_height * MAX_INCREMENTAL_REBUILD_AREA_RATIO
}

/// 创建 1x 单采样活动页墨迹离屏 GPU surface；边缘抗锯齿由 Skia Paint 提供。
pub(crate) fn create_gpu_surface(
    context: &mut DirectContext,
    size: [u32; 2],
) -> Result<Surface, AppError> {
    let dimensions = (
        i32::try_from(size[0].max(1)).map_err(|error| AppError::Graphics(error.to_string()))?,
        i32::try_from(size[1].max(1)).map_err(|error| AppError::Graphics(error.to_string()))?,
    );
    let image_info = ImageInfo::new(
        dimensions,
        ColorType::BGRA8888,
        AlphaType::Premul,
        skia_safe::ColorSpace::new_srgb(),
    );
    gpu::surfaces::render_target(
        context,
        Budgeted::Yes,
        &image_info,
        0,
        SurfaceOrigin::TopLeft,
        None,
        false,
        false,
    )
    .ok_or_else(|| AppError::Graphics("无法创建 Skia GPU 墨迹层".to_owned()))
}

/// 按事实顺序绘制操作，并合并连续同属性的固定宽度笔画。
fn draw_operations<'a>(
    canvas: &Canvas,
    operations: impl IntoIterator<Item = &'a InkOperation>,
    batch_drawer: &mut BatchDrawer,
) -> usize {
    let mut draw_calls = 0;
    for operation in operations {
        if batch_drawer.try_add(operation) {
            continue;
        }
        draw_calls += batch_drawer.flush(canvas);
        if batch_drawer.try_add(operation) {
            continue;
        }
        draw_operation(canvas, operation);
        draw_calls += 1;
    }
    draw_calls + batch_drawer.flush(canvas)
}

/// 将一个事实 operation 应用到当前 GPU 墨迹层。
fn draw_operation(canvas: &Canvas, operation: &InkOperation) {
    match operation {
        InkOperation::DrawStroke(stroke) => match &stroke.shape {
            super::DrawStrokeShape::Fixed { points, width } => {
                draw_pen_path(canvas, points, stroke.color, *width);
            }
            super::DrawStrokeShape::Variable { points } => {
                draw_variable_pen_path(canvas, points, stroke.color);
            }
        },
        InkOperation::EraseStroke(stroke) => {
            draw_erase_samples(canvas, &stroke.samples);
        }
        InkOperation::Clear(_) => {
            canvas.clear(Color::TRANSPARENT);
        }
    }
}

/// 在窗口 surface 上绘制尚未提交的实时画笔或橡皮擦预览。
pub(crate) fn draw_active_preview(canvas: &Canvas, preview: ActiveInkPreview<'_>) {
    match preview {
        ActiveInkPreview::Tool {
            points,
            tool: InkTool::Pen,
            color,
            pen_width,
            ..
        } => draw_pen_path(canvas, points, color, pen_width),
        ActiveInkPreview::VariableTool { points, color, .. } => {
            draw_variable_pen_path(canvas, points, color)
        }
        ActiveInkPreview::Tool {
            points,
            tool: InkTool::RegionEraser,
            eraser_size,
            ..
        } => {
            draw_circle_erase_path(canvas, points, eraser_size.pixels());
            draw_eraser_outline(canvas, points, eraser_size);
        }
        ActiveInkPreview::PalmErase { samples } => {
            draw_erase_samples(canvas, samples);
            if let Some(sample) = samples.last().copied() {
                draw_erase_sample_outline(canvas, sample);
            }
        }
    }
}

/// 绘制已完成三点滤波的活动几何，供 retained cache 避免重复滤波。
pub(crate) fn draw_active_filtered_preview(canvas: &Canvas, preview: ActiveInkPreview<'_>) {
    match preview {
        ActiveInkPreview::Tool {
            points,
            tool: InkTool::Pen,
            color,
            pen_width,
            ..
        } => draw_pen_path_filtered(canvas, points, color, pen_width),
        ActiveInkPreview::VariableTool { points, color, .. } => {
            draw_variable_pen_path_filtered(canvas, points, color)
        }
        _ => {}
    }
}

/// 在兼容临时 surface 中无 dirty clip 光栅化，再以 Src 严格替换目标 dirty 像素。
pub(crate) fn replay_active_stroke_regions(
    canvas: &Canvas,
    cache: &ActiveStrokeRenderCache,
) -> bool {
    for replay in cache.replay_regions() {
        let dirty = replay.bounds();
        let Some(dirty_pixels) = clipped_replay_pixels(canvas, dirty) else {
            continue;
        };
        let raster_bounds =
            replay_raster_bounds(cache, replay).map_or(dirty, |geometry| dirty.union(geometry));
        let Some(raster_pixels) = clipped_replay_pixels(canvas, raster_bounds) else {
            continue;
        };
        let scratch_info = canvas.image_info().with_dimensions(raster_pixels.size());
        let Some(mut scratch) = canvas.new_surface(&scratch_info, None) else {
            return false;
        };
        let scratch_canvas = scratch.canvas();
        scratch_canvas.clear(Color::TRANSPARENT);
        scratch_canvas.translate((-(raster_pixels.left as f32), -(raster_pixels.top as f32)));

        match replay {
            ActiveStrokeReplay::Fixed { segment_range, .. } => {
                if let Some(ActiveStrokeStyle::Fixed { color, width }) = cache.style() {
                    let segments = &cache.fixed_primitives()[segment_range.clone()];
                    if segments.is_empty() {
                        let (points, _) = cache.geometry();
                        draw_pen_path_filtered(scratch_canvas, points, color, width);
                    } else {
                        draw_fixed_bezier_segments(scratch_canvas, segments, color, width);
                    }
                }
            }
            ActiveStrokeReplay::Natural { point_range, .. } => {
                if let Some(ActiveStrokeStyle::Natural { color, .. }) = cache.style() {
                    let (_, points) = cache.geometry();
                    draw_variable_pen_path_filtered(
                        scratch_canvas,
                        &points[point_range.clone()],
                        color,
                    );
                }
            }
        }

        let restore_count = canvas.save();
        canvas.clip_irect(dirty_pixels, ClipOp::Intersect);
        let mut replace = Paint::default();
        replace.set_blend_mode(BlendMode::Src);
        scratch.draw(
            canvas,
            (raster_pixels.left as f32, raster_pixels.top as f32),
            SamplingOptions::default(),
            Some(&replace),
        );
        canvas.restore_to_count(restore_count);
    }
    true
}

/// 返回局部 replay 新几何的完整 AA 栅格范围，不包含需要透明清理的旧范围。
fn replay_raster_bounds(
    cache: &ActiveStrokeRenderCache,
    replay: &ActiveStrokeReplay,
) -> Option<InkBounds> {
    match replay {
        ActiveStrokeReplay::Fixed { segment_range, .. } => {
            let Some(ActiveStrokeStyle::Fixed { width, .. }) = cache.style() else {
                return None;
            };
            let segments = &cache.fixed_primitives()[segment_range.clone()];
            let (points, _) = cache.geometry();
            fixed_ink_bounds(points, segments, width.pixels())
        }
        ActiveStrokeReplay::Natural { point_range, .. } => {
            let (_, points) = cache.geometry();
            variable_ink_bounds(points, point_range)
        }
    }
}

/// 把浮点 replay 范围向外取整并裁到 retained surface 的有效像素。
fn clipped_replay_pixels(canvas: &Canvas, bounds: InkBounds) -> Option<IRect> {
    let rect = Rect::new(bounds.left, bounds.top, bounds.right, bounds.bottom);
    let pixels: IRect = rect.round_out();
    IRect::intersect(&pixels, &IRect::from_size(canvas.base_layer_size()))
}

/// 使用 retained 的全局 clamp 后 cubic 段连续绘制一个固定宽局部子路径。
fn draw_fixed_bezier_segments(
    canvas: &Canvas,
    segments: &[CubicBezierSegment],
    color: InkColor,
    width: PenWidth,
) {
    let Some(first) = segments.first() else {
        return;
    };
    let mut path = PathBuilder::new();
    path.move_to((first.start.x, first.start.y));
    for segment in segments {
        path.cubic_to(
            (segment.control1.x, segment.control1.y),
            (segment.control2.x, segment.control2.y),
            (segment.end.x, segment.end.y),
        );
    }
    let rgba = color.rgba();
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(rgba[3], rgba[0], rgba[1], rgba[2]));
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_width(width.pixels());
    paint.set_stroke_cap(PaintCap::Round);
    paint.set_stroke_join(PaintJoin::Round);
    canvas.draw_path(&path.detach(), &paint);
}

/// 滤波后以贝塞尔路径连续清除普通圆形橡皮擦路径，避免采样点之间留下间隙。
fn draw_circle_erase_path(canvas: &Canvas, points: &[CanvasPoint], diameter: f32) {
    let Some(filtered_points) = light_filter_points(points) else {
        return;
    };
    let Some(first) = filtered_points.first() else {
        return;
    };
    let mut paint = clear_paint(PaintStyle::Stroke);
    paint.set_stroke_width(diameter);
    paint.set_stroke_cap(PaintCap::Round);
    paint.set_stroke_join(PaintJoin::Round);

    if filtered_points.len() == 1 {
        paint.set_style(PaintStyle::Fill);
        canvas.draw_circle((first.x, first.y), diameter / 2.0, &paint);
        return;
    }

    let mut path_builder = PathBuilder::new();
    if !append_open_bezier_path(&mut path_builder, &filtered_points) {
        return;
    }
    canvas.draw_path(&path_builder.detach(), &paint);
}

/// 清除一次普通或动态手掌擦除采样，并在动态椭圆之间补足扫掠区域。
fn draw_erase_samples(canvas: &Canvas, samples: &[EraseSample]) {
    if let Some(diameter) = uniform_circle_diameter(samples) {
        let points: Vec<_> = samples.iter().map(|sample| sample.center).collect();
        draw_circle_erase_path(canvas, &points, diameter);
        return;
    }

    let Some(first) = samples.first().copied() else {
        return;
    };
    let paint = clear_paint(PaintStyle::Fill);
    draw_clear_ellipse(canvas, first, &paint);
    for pair in samples.windows(2) {
        let previous = pair[0];
        let next = pair[1];
        let steps = palm_interpolation_steps(previous, next);
        for step in 1..=steps {
            let progress = step as f32 / steps as f32;
            draw_clear_ellipse(
                canvas,
                interpolate_erase_sample(previous, next, progress),
                &paint,
            );
        }
    }
}

/// 返回全部采样是否属于相同尺寸的普通圆形橡皮擦。
fn uniform_circle_diameter(samples: &[EraseSample]) -> Option<f32> {
    let first = samples.first()?;
    let radius = first.radius_x;
    let is_uniform_circle = samples.iter().all(|sample| {
        sample.rotation_radians.abs() <= ERASE_RADIUS_EPSILON
            && (sample.radius_x - sample.radius_y).abs() <= ERASE_RADIUS_EPSILON
            && (sample.radius_x - radius).abs() <= ERASE_RADIUS_EPSILON
    });
    is_uniform_circle.then_some(radius * 2.0)
}

/// 计算两个动态手掌椭圆之间需要插入的最小采样段数。
fn palm_interpolation_steps(previous: EraseSample, next: EraseSample) -> u32 {
    let delta_x = next.center.x - previous.center.x;
    let delta_y = next.center.y - previous.center.y;
    let distance = delta_x.mul_add(delta_x, delta_y * delta_y).sqrt();
    let minor_radius = previous
        .radius_x
        .min(previous.radius_y)
        .min(next.radius_x.min(next.radius_y));
    let max_step = (minor_radius * PALM_INTERPOLATION_STEP_FRACTION)
        .clamp(PALM_INTERPOLATION_MIN_STEP, PALM_INTERPOLATION_MAX_STEP);
    (distance / max_step).ceil().max(1.0) as u32
}

/// 在两个动态擦除采样之间线性插值中心、半径和方向。
fn interpolate_erase_sample(
    previous: EraseSample,
    next: EraseSample,
    progress: f32,
) -> EraseSample {
    EraseSample {
        center: CanvasPoint::new(
            previous.center.x + (next.center.x - previous.center.x) * progress,
            previous.center.y + (next.center.y - previous.center.y) * progress,
        ),
        radius_x: previous.radius_x + (next.radius_x - previous.radius_x) * progress,
        radius_y: previous.radius_y + (next.radius_y - previous.radius_y) * progress,
        rotation_radians: interpolate_axis_rotation(
            previous.rotation_radians,
            next.rotation_radians,
            progress,
        ),
    }
}

/// 在椭圆轴等价的半周范围内沿最短方向插值旋转角度。
fn interpolate_axis_rotation(previous: f32, next: f32, progress: f32) -> f32 {
    let half_turn = std::f32::consts::PI;
    let start = previous.rem_euclid(half_turn);
    let end = next.rem_euclid(half_turn);
    let delta = (end - start + half_turn / 2.0).rem_euclid(half_turn) - half_turn / 2.0;
    (start + delta * progress).rem_euclid(half_turn)
}

/// 创建用于擦除透明墨迹的 Skia paint。
fn clear_paint(style: PaintStyle) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_style(style);
    paint.set_blend_mode(BlendMode::Clear);
    paint
}

/// 使用 clear blend 绘制一个旋转椭圆采样。
fn draw_clear_ellipse(canvas: &Canvas, sample: EraseSample, paint: &Paint) {
    let save_count = canvas.save();
    canvas.translate((sample.center.x, sample.center.y));
    canvas.rotate(sample.rotation_radians.to_degrees(), None);
    canvas.draw_oval(
        Rect::from_xywh(
            -sample.radius_x,
            -sample.radius_y,
            sample.radius_x * 2.0,
            sample.radius_y * 2.0,
        ),
        paint,
    );
    canvas.restore_to_count(save_count);
}

/// 对固定宽度笔画执行一次轻量滤波并使用贝塞尔路径绘制。
fn draw_pen_path(canvas: &Canvas, points: &[CanvasPoint], color: InkColor, width: PenWidth) {
    let Some(filtered_points) = light_filter_points(points) else {
        return;
    };
    draw_pen_path_filtered(canvas, &filtered_points, color, width);
}

/// 使用已滤波固定宽点集绘制 Skia AA 开放路径。
fn draw_pen_path_filtered(
    canvas: &Canvas,
    filtered_points: &[CanvasPoint],
    color: InkColor,
    width: PenWidth,
) {
    let Some(first) = filtered_points.first() else {
        return;
    };
    let rgba = color.rgba();
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(rgba[3], rgba[0], rgba[1], rgba[2]));
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_width(width.pixels());
    paint.set_stroke_cap(PaintCap::Round);
    paint.set_stroke_join(PaintJoin::Round);

    if filtered_points.len() == 1 {
        canvas.draw_circle((first.x, first.y), width.pixels() / 2.0, &paint);
        return;
    }

    let mut path_builder = PathBuilder::new();
    if !append_open_bezier_path(&mut path_builder, filtered_points) {
        return;
    }
    let path = path_builder.detach();
    canvas.draw_path(&path, &paint);
}

/// 对逐点宽度笔锋滤波位置并使用闭合贝塞尔轮廓绘制。
fn draw_variable_pen_path(canvas: &Canvas, points: &[VariableStrokePoint], color: InkColor) {
    let Some(filtered_points) = light_filter_variable_points(points) else {
        return;
    };
    draw_variable_pen_path_filtered(canvas, &filtered_points, color);
}

/// 使用已滤波自然笔锋点集绘制 Skia AA 闭合轮廓。
fn draw_variable_pen_path_filtered(
    canvas: &Canvas,
    filtered_points: &[VariableStrokePoint],
    color: InkColor,
) {
    let Some(first) = filtered_points.first() else {
        return;
    };
    let rgba = color.rgba();
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(rgba[3], rgba[0], rgba[1], rgba[2]));
    paint.set_style(PaintStyle::Fill);

    if filtered_points.len() == 1 {
        canvas.draw_circle((first.point.x, first.point.y), first.width / 2.0, &paint);
        return;
    }
    let Some(outline) = variable_outline(filtered_points) else {
        return;
    };
    let mut path_builder = PathBuilder::new();
    if !append_closed_bezier_path(&mut path_builder, &outline) {
        return;
    }
    canvas.draw_path(&path_builder.detach(), &paint);
}

/// 以中性描边显示活动区域橡皮擦范围，不修改持久墨迹层。
fn draw_eraser_outline(canvas: &Canvas, points: &[CanvasPoint], size: EraserSize) {
    let Some(point) = points.last() else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(180, 107, 114, 128));
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_width(2.0);
    canvas.draw_circle((point.x, point.y), size.pixels() / 2.0, &paint);
}

/// 以中性描边显示动态旋转手掌接触椭圆，不修改持久墨迹层。
fn draw_erase_sample_outline(canvas: &Canvas, sample: EraseSample) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(180, 107, 114, 128));
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_width(2.0);
    let save_count = canvas.save();
    canvas.translate((sample.center.x, sample.center.y));
    canvas.rotate(sample.rotation_radians.to_degrees(), None);
    canvas.draw_oval(
        Rect::from_xywh(
            -sample.radius_x,
            -sample.radius_y,
            sample.radius_x * 2.0,
            sample.radius_y * 2.0,
        ),
        &paint,
    );
    canvas.restore_to_count(save_count);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读取 raster surface 的完整 N32 premultiplied 像素。
    fn raster_pixels(surface: &mut Surface, size: (i32, i32)) -> Vec<u8> {
        let info = ImageInfo::new_n32_premul(size, None);
        let row_bytes = info.min_row_bytes();
        let mut pixels = vec![0; info.compute_min_byte_size()];
        assert!(
            surface
                .canvas()
                .read_pixels(&info, &mut pixels, row_bytes, (0, 0))
        );
        pixels
    }

    /// 使用生产 full helper 重画当前 retained 几何，作为增量 surface 的像素 oracle。
    fn draw_active_cache_full(canvas: &Canvas, cache: &ActiveStrokeRenderCache) {
        let (fixed, natural) = cache.geometry();
        match cache.style() {
            Some(ActiveStrokeStyle::Fixed { color, width }) => draw_active_filtered_preview(
                canvas,
                ActiveInkPreview::Tool {
                    points: fixed,
                    tool: InkTool::Pen,
                    color,
                    pen_width: width,
                    eraser_size: EraserSize::default(),
                },
            ),
            Some(ActiveStrokeStyle::Natural { color, .. }) => draw_active_filtered_preview(
                canvas,
                ActiveInkPreview::VariableTool {
                    points: natural,
                    color,
                    eraser_size: EraserSize::default(),
                },
            ),
            None => {}
        }
    }

    /// 验证所有 dirty 矩形之外的 retained 像素在一次局部 replay 前后逐字节不变。
    fn assert_pixels_outside_dirty_unchanged(
        before: &[u8],
        after: &[u8],
        size: (i32, i32),
        dirty: &[IRect],
    ) {
        for y in 0..size.1 {
            for x in 0..size.0 {
                if dirty.iter().any(|rect| {
                    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
                }) {
                    continue;
                }
                let byte = ((y * size.0 + x) * 4) as usize;
                assert_eq!(
                    &before[byte..byte + 4],
                    &after[byte..byte + 4],
                    "dirty 外像素 ({x}, {y}) 被局部 Src replay 修改"
                );
            }
        }
    }

    /// 验证 dirty clear + halo replay 在每个 revision 与完整 analytic-AA 重画逐像素一致。
    fn assert_incremental_raster_matches_full(style: ActiveStrokeStyle, points: &[CanvasPoint]) {
        let size = (192, 128);
        let mut incremental = skia_safe::surfaces::raster_n32_premul(size).unwrap();
        incremental.canvas().clear(Color::TRANSPARENT);
        let mut oracle = skia_safe::surfaces::raster_n32_premul(size).unwrap();
        let mut cache = ActiveStrokeRenderCache::default();

        for (index, point) in points.iter().enumerate() {
            let before = raster_pixels(&mut incremental, size);
            let work = cache
                .apply_delta(&super::super::active_stroke::ActiveStrokeDelta {
                    gesture_id: 71,
                    revision: index as u64 + 1,
                    from_sample: index,
                    samples: vec![*point],
                    style,
                    full_resync: false,
                })
                .unwrap();
            if work.full_redraw {
                incremental.canvas().clear(Color::TRANSPARENT);
                draw_active_cache_full(incremental.canvas(), &cache);
            } else {
                let dirty = cache
                    .replay_regions()
                    .iter()
                    .filter_map(|replay| {
                        clipped_replay_pixels(incremental.canvas(), replay.bounds())
                    })
                    .collect::<Vec<_>>();
                assert!(replay_active_stroke_regions(incremental.canvas(), &cache));
                let after = raster_pixels(&mut incremental, size);
                assert_pixels_outside_dirty_unchanged(&before, &after, size, &dirty);
            }
            oracle.canvas().clear(Color::TRANSPARENT);
            draw_active_cache_full(oracle.canvas(), &cache);
            let actual = raster_pixels(&mut incremental, size);
            let expected = raster_pixels(&mut oracle, size);
            assert!(
                actual.iter().any(|byte| *byte != 0),
                "{style:?} revision {} 的累计活动笔迹不应透明",
                index + 1
            );
            if actual != expected {
                let first_difference = actual
                    .iter()
                    .zip(&expected)
                    .position(|(actual, expected)| actual != expected);
                let difference_count = actual
                    .iter()
                    .zip(&expected)
                    .filter(|(actual, expected)| actual != expected)
                    .count();
                panic!(
                    "{style:?} revision {} 的 dirty replay 与 full helper 有 {difference_count} 个字节不同，首个索引 {first_difference:?}",
                    index + 1
                );
            }
        }
    }

    /// 验证一次完整重画后的下一次局部 fixed/natural replay 不会清除稳定前缀。
    #[test]
    fn local_replay_after_full_redraw_preserves_prefix_for_fixed_and_natural() {
        let size = (192, 128);
        let points = [
            CanvasPoint::new(16.0, 64.0),
            CanvasPoint::new(36.0, 48.0),
            CanvasPoint::new(60.0, 72.0),
            CanvasPoint::new(88.0, 44.0),
            CanvasPoint::new(116.0, 76.0),
            CanvasPoint::new(152.0, 56.0),
        ];
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
            let mut incremental = skia_safe::surfaces::raster_n32_premul(size).unwrap();
            incremental.canvas().clear(Color::TRANSPARENT);
            let mut oracle = skia_safe::surfaces::raster_n32_premul(size).unwrap();
            let mut cache = ActiveStrokeRenderCache::default();
            cache.apply_full(91, 5, style, &points[..5]).unwrap();
            draw_active_cache_full(incremental.canvas(), &cache);
            let before = raster_pixels(&mut incremental, size);

            let work = cache
                .apply_delta(&super::super::active_stroke::ActiveStrokeDelta {
                    gesture_id: 91,
                    revision: 6,
                    from_sample: 5,
                    samples: vec![points[5]],
                    style,
                    full_resync: false,
                })
                .unwrap();
            assert!(!work.full_redraw, "追加尾点应走局部 replay");
            let dirty = cache
                .replay_regions()
                .iter()
                .filter_map(|replay| clipped_replay_pixels(incremental.canvas(), replay.bounds()))
                .collect::<Vec<_>>();
            assert!(replay_active_stroke_regions(incremental.canvas(), &cache));
            let actual = raster_pixels(&mut incremental, size);
            assert_pixels_outside_dirty_unchanged(&before, &actual, size, &dirty);

            oracle.canvas().clear(Color::TRANSPARENT);
            draw_active_cache_full(oracle.canvas(), &cache);
            assert_eq!(actual, raster_pixels(&mut oracle, size), "{style:?}");
        }
    }

    /// 验证四档固定/自然笔锋在多种急转和重复点序列中逐 revision 像素相同。
    #[test]
    fn dirty_region_raster_matches_full_fixed_and_natural_previews() {
        let paths: [&[CanvasPoint]; 3] = [
            &[
                CanvasPoint::new(16.0, 64.0),
                CanvasPoint::new(28.0, 48.0),
                CanvasPoint::new(44.0, 72.0),
                CanvasPoint::new(64.0, 40.0),
                CanvasPoint::new(88.0, 76.0),
                CanvasPoint::new(116.0, 52.0),
                CanvasPoint::new(152.0, 68.0),
            ],
            &[
                CanvasPoint::new(20.0, 20.0),
                CanvasPoint::new(160.0, 20.0),
                CanvasPoint::new(24.0, 24.0),
                CanvasPoint::new(156.0, 104.0),
                CanvasPoint::new(32.0, 100.0),
            ],
            &[
                CanvasPoint::new(24.0, 64.0),
                CanvasPoint::new(48.0, 64.0),
                CanvasPoint::new(48.0, 64.0),
                CanvasPoint::new(72.0, 32.0),
                CanvasPoint::new(72.0, 96.0),
                CanvasPoint::new(120.0, 64.0),
            ],
        ];
        for points in paths {
            for width in [PenWidth::Px4, PenWidth::Px6, PenWidth::Px8, PenWidth::Px16] {
                assert_incremental_raster_matches_full(
                    ActiveStrokeStyle::Fixed {
                        color: InkColor::Red,
                        width,
                    },
                    points,
                );
                assert_incremental_raster_matches_full(
                    ActiveStrokeStyle::Natural {
                        color: InkColor::Red,
                        body_width: width,
                    },
                    points,
                );
            }
        }
    }

    /// 创建不依赖 GPU 的最小墨迹缓存，供状态机回归测试使用。
    fn raster_cache(logical_size: [u32; 2]) -> InkRenderCache {
        let mut surface = skia_safe::surfaces::raster_n32_premul((
            logical_size[0] as i32,
            logical_size[1] as i32,
        ))
        .expect("测试 raster surface 应创建成功");
        surface.canvas().clear(Color::TRANSPARENT);
        InkRenderCache {
            surface,
            logical_size,
            render_size: logical_size,
            applied_operation_count: 0,
            last_operation_id: None,
            last_operation_was_clear: false,
            spatial_index: InkSpatialIndex::new(logical_size),
            batch_drawer: BatchDrawer::new(),
            pending_region_rebuild: None,
            full_rebuild_requested: false,
            deferred_erase: None,
        }
    }

    /// 验证刚提交的普通与手掌擦除预览不会被误判为后续新手势。
    #[test]
    fn active_preview_matches_only_the_committed_erase() {
        let points = [CanvasPoint::new(8.0, 12.0), CanvasPoint::new(16.0, 12.0)];
        let circle_samples: Vec<_> = points
            .iter()
            .copied()
            .map(|point| EraseSample::circle(point, EraserSize::Px36.pixels()))
            .collect();
        let circle_stroke = EraseStroke::new(OperationId::new(1), circle_samples)
            .expect("有效圆形擦除应创建事实操作");
        let circle_preview = ActiveInkPreview::Tool {
            points: &points,
            tool: InkTool::RegionEraser,
            color: InkColor::Red,
            pen_width: PenWidth::Px4,
            eraser_size: EraserSize::Px36,
        };
        assert!(active_preview_matches_erase(circle_preview, &circle_stroke));

        let palm_samples = [EraseSample {
            center: CanvasPoint::new(24.0, 20.0),
            radius_x: 12.0,
            radius_y: 8.0,
            rotation_radians: 0.25,
        }];
        let palm_stroke = EraseStroke::new(OperationId::new(2), palm_samples.to_vec())
            .expect("有效手掌擦除应创建事实操作");
        assert!(active_preview_matches_erase(
            ActiveInkPreview::PalmErase {
                samples: &palm_samples,
            },
            &palm_stroke
        ));
        assert!(!active_preview_matches_erase(
            ActiveInkPreview::Tool {
                points: &points[..1],
                tool: InkTool::RegionEraser,
                color: InkColor::Red,
                pen_width: PenWidth::Px4,
                eraser_size: EraserSize::Px36,
            },
            &circle_stroke
        ));
    }

    /// 验证空帧保留待定擦除，立即撤销只丢弃状态而不触发区域重放。
    #[test]
    fn deferred_erase_survives_empty_sync_and_discards_on_undo() {
        let mut document = InkDocument::new();
        document.append_draw_stroke(
            vec![CanvasPoint::new(4.0, 16.0), CanvasPoint::new(52.0, 16.0)],
            InkColor::Red,
            PenWidth::Px4,
        );
        let mut cache = raster_cache([64, 64]);
        cache.sync(&document);
        let erase_id = document
            .append_erase_stroke(vec![EraseSample::circle(
                CanvasPoint::new(16.0, 16.0),
                EraserSize::Px36.pixels(),
            )])
            .expect("有效擦除应创建事实操作");
        let erase_bounds = document
            .operation(erase_id)
            .and_then(InkOperation::bounds)
            .expect("擦除操作应有有效边界");

        cache.sync(&document);
        assert_eq!(
            cache.deferred_erase.as_ref().map(|stroke| stroke.id),
            Some(erase_id)
        );
        cache.sync(&document);
        assert_eq!(
            cache.deferred_erase.as_ref().map(|stroke| stroke.id),
            Some(erase_id)
        );
        let committed_points = [CanvasPoint::new(16.0, 16.0)];
        cache.commit_deferred_erase_before_preview(ActiveInkPreview::Tool {
            points: &committed_points,
            tool: InkTool::RegionEraser,
            color: InkColor::Red,
            pen_width: PenWidth::Px4,
            eraser_size: EraserSize::Px36,
        });
        assert_eq!(
            cache.deferred_erase.as_ref().map(|stroke| stroke.id),
            Some(erase_id)
        );

        document.undo();
        cache.invalidate_region(erase_bounds);
        cache.sync(&document);
        assert!(cache.deferred_erase.is_none());
        assert_eq!(cache.applied_operation_count, document.operations().len());
    }

    /// 创建仅旋转角不同的椭圆采样。
    fn sample(rotation_radians: f32) -> EraseSample {
        EraseSample {
            center: CanvasPoint::new(0.0, 0.0),
            radius_x: 20.0,
            radius_y: 12.0,
            rotation_radians,
        }
    }

    /// 验证跨过半周边界时沿椭圆轴的短路径插值。
    #[test]
    fn interpolated_rotation_crosses_half_turn_boundary_directly() {
        let interpolated =
            interpolate_erase_sample(sample(std::f32::consts::PI - 0.02), sample(0.02), 0.5);

        assert!(interpolated.rotation_radians.abs() < 0.001);
    }

    /// 验证正常方向插值保留中间角度和半周归一化范围。
    #[test]
    fn interpolated_rotation_preserves_non_wrapping_midpoint() {
        let interpolated = interpolate_erase_sample(sample(0.2), sample(0.6), 0.5);

        assert!((interpolated.rotation_radians - 0.4).abs() < 0.001);
        assert!((0.0..std::f32::consts::PI).contains(&interpolated.rotation_radians));
    }

    /// 验证墨迹 surface 始终按逻辑尺寸 1x 创建，并拒绝超出 Skia 范围的尺寸。
    #[test]
    fn surface_size_stays_at_logical_resolution() {
        assert_eq!(surface_size([1919, 1079]), Some([1919, 1079]));
        assert_eq!(surface_size([0, 0]), Some([1, 1]));
        assert_eq!(surface_size([u32::MAX, 1]), None);
    }

    /// 验证小脏区使用局部重建，而达到阈值时回退全量重建。
    #[test]
    fn dirty_region_strategy_uses_ten_percent_area_threshold() {
        assert!(is_small_rebuild_region(
            [1000, 1000],
            InkBounds::from_xywh(0.0, 0.0, 100.0, 100.0)
        ));
        assert!(!is_small_rebuild_region(
            [1000, 1000],
            InkBounds::from_xywh(0.0, 0.0, 500.0, 200.0)
        ));
    }

    /// 验证脏区按画布可见交集计算，画布外部分不会扩大重建范围。
    #[test]
    fn dirty_region_strategy_clips_area_to_canvas() {
        assert!(is_small_rebuild_region(
            [1000, 1000],
            InkBounds::from_xywh(-900.0, -900.0, 1000.0, 1000.0)
        ));
    }

    /// 验证生产批处理路径把 1000 条同属性笔画压缩为 10 次 Skia 提交。
    #[test]
    fn thousand_matching_strokes_use_ten_draw_calls() {
        let mut document = InkDocument::new();
        for index in 0..1000 {
            let offset = (index % 32) as f32;
            document.append_draw_stroke(
                vec![
                    CanvasPoint::new(offset, 0.0),
                    CanvasPoint::new(offset, 32.0),
                ],
                InkColor::Red,
                PenWidth::Px4,
            );
        }
        let mut surface = skia_safe::surfaces::raster_n32_premul((64, 64))
            .expect("测试 raster surface 应创建成功");

        let draw_calls = draw_operations(
            surface.canvas(),
            document.operations().iter(),
            &mut BatchDrawer::new(),
        );

        assert_eq!(draw_calls, 10);
        assert!(draw_calls <= 700);
    }
}
