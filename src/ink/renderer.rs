use skia_safe::{
    AlphaType, BlendMode, Canvas, ClipOp, Color, ColorType, FilterMode, ImageInfo, MipmapMode,
    Paint, PaintCap, PaintJoin, PaintStyle, PathBuilder, Rect, SamplingOptions, Surface,
    canvas::SrcRectConstraint,
    gpu::{self, Budgeted, DirectContext, SurfaceOrigin},
    surface::BackendHandleAccess,
};

use super::{
    BatchDrawer, CanvasPoint, EraseSample, EraserSize, InkBounds, InkColor, InkDocument,
    InkOperation, InkSpatialIndex, InkTool, OperationId, PenWidth, VariableStrokePoint,
    stroke_geometry::{StrokeSegment, variable_outline, visit_smoothed_segments},
};
use crate::error::AppError;
use crate::settings::InkAntialiasingMode;

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

const ERASE_RADIUS_EPSILON: f32 = 0.01;
const PALM_INTERPOLATION_STEP_FRACTION: f32 = 0.75;
const PALM_INTERPOLATION_MIN_STEP: f32 = 4.0;
const PALM_INTERPOLATION_MAX_STEP: f32 = 24.0;
const SUPER_SAMPLE_SCALE: f32 = 1.5;
const PREVIEW_TILE_SIZE: f32 = 512.0;
const MAX_INCREMENTAL_REBUILD_AREA_RATIO: f32 = 0.1;

/// 描述墨迹离屏 surface 的逻辑尺寸、渲染尺寸和实际多采样配置。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct InkSurfaceConfig {
    pub mode: InkAntialiasingMode,
    pub render_scale: f32,
    pub sample_count: usize,
}

impl InkSurfaceConfig {
    /// 将用户可见的三档设置映射为固定的 GPU surface 参数。
    pub const fn for_mode(mode: InkAntialiasingMode) -> Self {
        match mode {
            InkAntialiasingMode::Off => Self {
                mode,
                render_scale: 1.0,
                sample_count: 0,
            },
            InkAntialiasingMode::Msaa => Self {
                mode,
                render_scale: 1.0,
                sample_count: 4,
            },
            InkAntialiasingMode::Supersample => Self {
                mode,
                render_scale: SUPER_SAMPLE_SCALE,
                sample_count: 0,
            },
        }
    }

    /// 计算向上取整后的实际 GPU 尺寸，拒绝超出 Skia `i32` 范围的请求。
    pub fn render_size(self, logical_size: [u32; 2]) -> Option<[u32; 2]> {
        Some([
            scaled_dimension(logical_size[0], self.render_scale)?,
            scaled_dimension(logical_size[1], self.render_scale)?,
        ])
    }
}

/// 将一个逻辑像素尺寸按固定渲染倍率转换为 GPU 像素尺寸。
fn scaled_dimension(value: u32, scale: f32) -> Option<u32> {
    let scaled = (f64::from(value.max(1)) * f64::from(scale)).ceil();
    (scaled.is_finite() && scaled >= 1.0 && scaled <= f64::from(i32::MAX)).then_some(scaled as u32)
}

/// 当前活动页唯一的持久 Skia GPU 墨迹层。
pub struct InkRenderCache {
    surface: Surface,
    logical_size: [u32; 2],
    render_size: [u32; 2],
    config: InkSurfaceConfig,
    applied_operation_count: usize,
    last_operation_id: Option<OperationId>,
    last_operation_was_clear: bool,
    spatial_index: InkSpatialIndex,
    batch_drawer: BatchDrawer,
    pending_region_rebuild: Option<InkBounds>,
    full_rebuild_requested: bool,
}

impl InkRenderCache {
    /// 为指定逻辑尺寸创建透明的 GPU 墨迹层，并验证请求的 MSAA sample count。
    pub fn new(
        context: &mut DirectContext,
        logical_size: [u32; 2],
        mode: InkAntialiasingMode,
    ) -> Result<Self, AppError> {
        let config = InkSurfaceConfig::for_mode(mode);
        let render_size = config
            .render_size(logical_size)
            .ok_or_else(|| AppError::Graphics("墨迹 surface 尺寸超出 Skia 支持范围".to_owned()))?;
        let mut surface = create_gpu_surface(context, render_size, config)?;
        surface.canvas().clear(Color::TRANSPARENT);
        Ok(Self {
            surface,
            logical_size: [logical_size[0].max(1), logical_size[1].max(1)],
            render_size,
            config,
            applied_operation_count: 0,
            last_operation_id: None,
            last_operation_was_clear: false,
            spatial_index: InkSpatialIndex::new(logical_size),
            batch_drawer: BatchDrawer::new(),
            pending_region_rebuild: None,
            full_rebuild_requested: false,
        })
    }

    /// 将尚未应用的新 operation 增量绘制到持久 GPU surface。
    pub fn sync(&mut self, document: &InkDocument) {
        if self.full_rebuild_requested {
            self.rebuild(document);
            return;
        }
        if let Some(bounds) = self.pending_region_rebuild.take() {
            self.rebuild_region(document, bounds);
            return;
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
            return;
        }

        let new_operations = &history[self.applied_operation_count..];
        let started_at = (!new_operations.is_empty() && tracing::enabled!(tracing::Level::DEBUG))
            .then(std::time::Instant::now);
        let draw_calls = draw_operations_with_config(
            self.surface.canvas(),
            new_operations.iter(),
            self.config,
            &mut self.batch_drawer,
        );
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
    }

    /// 返回当前 GPU 墨迹层快照，供同一上下文合成到窗口 framebuffer。
    pub fn snapshot(&mut self) -> skia_safe::Image {
        if self.config.sample_count > 0 {
            gpu::surfaces::resolve_msaa(&mut self.surface);
        }
        self.surface.image_snapshot()
    }

    /// 返回缓存使用的逻辑尺寸。
    pub const fn logical_size(&self) -> [u32; 2] {
        self.logical_size
    }

    /// 返回缓存使用的实际 GPU 尺寸。
    pub const fn render_size(&self) -> [u32; 2] {
        self.render_size
    }

    /// 返回缓存的固定 surface 配置。
    pub(crate) const fn config(&self) -> InkSurfaceConfig {
        self.config
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
        self.surface.canvas().clear(Color::TRANSPARENT);
        let operations = document.replay_operations();
        let started_at = tracing::enabled!(tracing::Level::DEBUG).then(std::time::Instant::now);
        let draw_calls = draw_operations_with_config(
            self.surface.canvas(),
            operations.iter(),
            self.config,
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
        self.prepare_spatial_index_for_region(document);
        let mut operation_ids = self.spatial_index.query(bounds);
        operation_ids.sort_unstable_by_key(|id| id.get());
        let operations: Vec<_> = operation_ids
            .into_iter()
            .filter_map(|id| document.operation(id))
            .collect();
        let clip_rect = Rect::new(bounds.left, bounds.top, bounds.right, bounds.bottom);
        let started_at = tracing::enabled!(tracing::Level::DEBUG).then(std::time::Instant::now);
        let draw_calls = with_logical_canvas(self.surface.canvas(), self.config, |canvas| {
            let save_count = canvas.save();
            canvas.clip_rect(clip_rect, ClipOp::Intersect, false);

            let mut clear_paint = Paint::default();
            clear_paint.set_style(PaintStyle::Fill);
            clear_paint.set_blend_mode(BlendMode::Clear);
            canvas.draw_rect(clip_rect, &clear_paint);
            let draw_calls =
                draw_operations(canvas, operations.iter().copied(), &mut self.batch_drawer);
            canvas.restore_to_count(save_count);
            draw_calls + 1
        });
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
            self.rebuild_spatial_index(document);
        }
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

/// 创建活动页或局部预览的离屏 GPU surface，并校验实际 MSAA 配置。
fn create_gpu_surface(
    context: &mut DirectContext,
    size: [u32; 2],
    config: InkSurfaceConfig,
) -> Result<Surface, AppError> {
    let dimensions = (
        i32::try_from(size[0].max(1)).map_err(|error| AppError::Graphics(error.to_string()))?,
        i32::try_from(size[1].max(1)).map_err(|error| AppError::Graphics(error.to_string()))?,
    );
    let image_info = ImageInfo::new(dimensions, ColorType::RGBA8888, AlphaType::Premul, None);
    if config.sample_count > 0
        && context.max_surface_sample_count_for_color_type(ColorType::RGBA8888)
            < config.sample_count
    {
        return Err(AppError::Graphics(format!(
            "Skia 后端不支持墨迹 {}（最大 sample count 不足）",
            config.mode.label()
        )));
    }
    gpu::surfaces::render_target(
        context,
        Budgeted::Yes,
        &image_info,
        config.sample_count,
        SurfaceOrigin::TopLeft,
        None,
        false,
        false,
    )
    .ok_or_else(|| AppError::Graphics("无法创建 Skia GPU 墨迹层".to_owned()))
    .and_then(|mut surface| {
        if config.sample_count > 0 {
            let actual = gpu::surfaces::get_backend_render_target(
                &mut surface,
                BackendHandleAccess::FlushWrite,
            )
            .map_or(0, |target| target.sample_count());
            if actual != config.sample_count {
                return Err(AppError::Graphics(format!(
                    "Skia 后端实际 sample count 为 {actual}，无法满足 {}",
                    config.mode.label()
                )));
            }
        }
        Ok(surface)
    })
}

/// 在需要时给逻辑墨迹绘制设置固定的渲染倍率。
fn with_logical_canvas<T>(
    canvas: &Canvas,
    config: InkSurfaceConfig,
    draw: impl FnOnce(&Canvas) -> T,
) -> T {
    if (config.render_scale - 1.0).abs() <= f32::EPSILON {
        return draw(canvas);
    }
    let save_count = canvas.save();
    canvas.scale((config.render_scale, config.render_scale));
    let result = draw(canvas);
    canvas.restore_to_count(save_count);
    result
}

/// 通过指定 surface 的逻辑坐标系批量绘制一组事实操作。
fn draw_operations_with_config<'a>(
    canvas: &Canvas,
    operations: impl IntoIterator<Item = &'a InkOperation>,
    config: InkSurfaceConfig,
    batch_drawer: &mut BatchDrawer,
) -> usize {
    with_logical_canvas(canvas, config, |canvas| {
        draw_operations(canvas, operations, batch_drawer)
    })
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

/// 返回活动预览在逻辑坐标中的保守影响区域，用于按块分配临时 surface。
pub(crate) fn active_preview_bounds(preview: ActiveInkPreview<'_>) -> Option<InkBounds> {
    match preview {
        ActiveInkPreview::Tool {
            points,
            tool: InkTool::Pen,
            pen_width,
            ..
        } => InkBounds::from_points(points, pen_width.pixels() / 2.0),
        ActiveInkPreview::Tool {
            points,
            tool: InkTool::RegionEraser,
            eraser_size,
            ..
        } => InkBounds::from_points(points, eraser_size.pixels() / 2.0 + 2.0),
        ActiveInkPreview::VariableTool { points, .. } => {
            let first = points.first()?;
            let mut bounds = InkBounds {
                left: first.point.x,
                top: first.point.y,
                right: first.point.x,
                bottom: first.point.y,
            };
            let mut max_radius = first.width.max(0.0) / 2.0;
            for sample in &points[1..] {
                bounds.left = bounds.left.min(sample.point.x);
                bounds.top = bounds.top.min(sample.point.y);
                bounds.right = bounds.right.max(sample.point.x);
                bounds.bottom = bounds.bottom.max(sample.point.y);
                max_radius = max_radius.max(sample.width.max(0.0) / 2.0);
            }
            Some(bounds.expanded(max_radius + 2.0))
        }
        ActiveInkPreview::PalmErase { samples } => {
            let first = samples.first()?;
            let mut bounds = InkBounds {
                left: first.center.x - first.radius_x,
                top: first.center.y - first.radius_y,
                right: first.center.x + first.radius_x,
                bottom: first.center.y + first.radius_y,
            };
            for sample in &samples[1..] {
                bounds.left = bounds.left.min(sample.center.x - sample.radius_x);
                bounds.top = bounds.top.min(sample.center.y - sample.radius_y);
                bounds.right = bounds.right.max(sample.center.x + sample.radius_x);
                bounds.bottom = bounds.bottom.max(sample.center.y + sample.radius_y);
            }
            Some(bounds.expanded(2.0))
        }
    }
}

/// 判断活动预览是否需要用 `Src` 替换目标区域中的持久墨迹。
pub(crate) const fn preview_replaces_region(preview: ActiveInkPreview<'_>) -> bool {
    matches!(
        preview,
        ActiveInkPreview::Tool {
            tool: InkTool::RegionEraser,
            ..
        } | ActiveInkPreview::PalmErase { .. }
    )
}

/// 将离屏墨迹图像按逻辑坐标合成到目标 canvas，并在超采样时使用线性采样。
pub(crate) fn draw_image_rect_logical(
    canvas: &Canvas,
    image: &skia_safe::Image,
    source_render_size: [u32; 2],
    destination: Rect,
    linear: bool,
    blend_mode: BlendMode,
) {
    let source = Rect::from_xywh(
        0.0,
        0.0,
        source_render_size[0] as f32,
        source_render_size[1] as f32,
    );
    let sampling = if linear {
        SamplingOptions::new(FilterMode::Linear, MipmapMode::None)
    } else {
        SamplingOptions::default()
    };
    let mut paint = Paint::default();
    paint.set_blend_mode(blend_mode);
    canvas.draw_image_rect_with_sampling_options(
        image,
        Some((&source, SrcRectConstraint::Strict)),
        destination,
        sampling,
        &paint,
    );
}

/// 保存并合成增强模式下的局部活动预览 surface。
pub(crate) struct InkPreviewCache {
    surface: Surface,
    origin: CanvasPoint,
    logical_size: [u32; 2],
    render_size: [u32; 2],
    config: InkSurfaceConfig,
}

impl InkPreviewCache {
    /// 创建一个按 512px 块对齐的局部预览 surface。
    pub(crate) fn new(
        context: &mut DirectContext,
        origin: CanvasPoint,
        logical_size: [u32; 2],
        config: InkSurfaceConfig,
    ) -> Result<Self, AppError> {
        let render_size = config
            .render_size(logical_size)
            .ok_or_else(|| AppError::Graphics("活动墨迹预览尺寸超出 Skia 支持范围".to_owned()))?;
        let mut surface = create_gpu_surface(context, render_size, config)?;
        surface.canvas().clear(Color::TRANSPARENT);
        Ok(Self {
            surface,
            origin,
            logical_size,
            render_size,
            config,
        })
    }

    /// 确保预览区域覆盖目标 bounds，越界时按固定块重建一次 surface。
    pub(crate) fn ensure(
        &mut self,
        context: &mut DirectContext,
        bounds: InkBounds,
        window_size: [u32; 2],
    ) -> Result<(), AppError> {
        let (origin, logical_size) = preview_region(bounds, window_size);
        let current_right = self.origin.x + self.logical_size[0] as f32;
        let current_bottom = self.origin.y + self.logical_size[1] as f32;
        let requested_right = origin.x + logical_size[0] as f32;
        let requested_bottom = origin.y + logical_size[1] as f32;
        if self.origin.x <= origin.x
            && self.origin.y <= origin.y
            && current_right >= requested_right
            && current_bottom >= requested_bottom
        {
            return Ok(());
        }
        let union_bounds = InkBounds {
            left: self.origin.x.min(origin.x),
            top: self.origin.y.min(origin.y),
            right: current_right.max(requested_right),
            bottom: current_bottom.max(requested_bottom),
        };
        let (expanded_origin, expanded_size) = preview_region(union_bounds, window_size);
        let replacement = Self::new(context, expanded_origin, expanded_size, self.config)?;
        *self = replacement;
        Ok(())
    }

    /// 手势结束后把临时 surface 缩回窗口左上角的基础块，避免长期保留大区域。
    pub(crate) fn reset_to_base(
        &mut self,
        context: &mut DirectContext,
        window_size: [u32; 2],
    ) -> Result<(), AppError> {
        let origin = CanvasPoint::new(0.0, 0.0);
        let logical_size = [window_size[0].clamp(1, 512), window_size[1].clamp(1, 512)];
        if self.origin == origin && self.logical_size == logical_size {
            return Ok(());
        }
        let replacement = Self::new(context, origin, logical_size, self.config)?;
        *self = replacement;
        Ok(())
    }

    /// 以透明底清理当前局部预览 surface。
    pub(crate) fn clear(&mut self) {
        self.surface.canvas().clear(Color::TRANSPARENT);
    }

    /// 将持久墨迹图像复制到局部 surface，供橡皮擦预览执行透明清除。
    pub(crate) fn seed_from_image(
        &mut self,
        image: &skia_safe::Image,
        source_render_size: [u32; 2],
        source_logical_size: [u32; 2],
        linear: bool,
    ) {
        self.clear();
        let save_count = self.surface.canvas().save();
        self.surface
            .canvas()
            .scale((self.config.render_scale, self.config.render_scale));
        self.surface
            .canvas()
            .translate((-self.origin.x, -self.origin.y));
        draw_image_rect_logical(
            self.surface.canvas(),
            image,
            source_render_size,
            Rect::from_xywh(
                0.0,
                0.0,
                source_logical_size[0] as f32,
                source_logical_size[1] as f32,
            ),
            linear,
            BlendMode::Src,
        );
        self.surface.canvas().restore_to_count(save_count);
    }

    /// 在当前局部坐标中绘制一帧活动预览。
    pub(crate) fn draw(&mut self, preview: ActiveInkPreview<'_>) {
        let save_count = self.surface.canvas().save();
        self.surface
            .canvas()
            .scale((self.config.render_scale, self.config.render_scale));
        self.surface
            .canvas()
            .translate((-self.origin.x, -self.origin.y));
        draw_active_preview(self.surface.canvas(), preview);
        self.surface.canvas().restore_to_count(save_count);
    }

    /// 返回当前局部预览的图像快照。
    pub(crate) fn snapshot(&mut self) -> skia_safe::Image {
        if self.config.sample_count > 0 {
            gpu::surfaces::resolve_msaa(&mut self.surface);
        }
        self.surface.image_snapshot()
    }

    /// 返回局部预览的逻辑原点。
    pub(crate) const fn origin(&self) -> CanvasPoint {
        self.origin
    }

    /// 返回局部预览的逻辑尺寸。
    pub(crate) const fn logical_size(&self) -> [u32; 2] {
        self.logical_size
    }

    /// 返回局部预览的实际 GPU 尺寸。
    pub(crate) const fn render_size(&self) -> [u32; 2] {
        self.render_size
    }
}

/// 计算覆盖活动 bounds 的 512px 对齐逻辑区域，并裁剪到窗口尺寸内。
fn preview_region(bounds: InkBounds, window_size: [u32; 2]) -> (CanvasPoint, [u32; 2]) {
    let window_width = window_size[0].max(1) as f32;
    let window_height = window_size[1].max(1) as f32;
    let left = bounds.left.max(0.0).min(window_width - 1.0);
    let top = bounds.top.max(0.0).min(window_height - 1.0);
    let right = bounds.right.max(left + 1.0).min(window_width);
    let bottom = bounds.bottom.max(top + 1.0).min(window_height);
    let origin_x = (left / PREVIEW_TILE_SIZE).floor() * PREVIEW_TILE_SIZE;
    let origin_y = (top / PREVIEW_TILE_SIZE).floor() * PREVIEW_TILE_SIZE;
    let end_x = (right / PREVIEW_TILE_SIZE).ceil() * PREVIEW_TILE_SIZE;
    let end_y = (bottom / PREVIEW_TILE_SIZE).ceil() * PREVIEW_TILE_SIZE;
    let width = end_x.min(window_width).max(origin_x + 1.0) - origin_x;
    let height = end_y.min(window_height).max(origin_y + 1.0) - origin_y;
    (
        CanvasPoint::new(origin_x, origin_y),
        [width.ceil() as u32, height.ceil() as u32],
    )
}

/// 连续清除普通圆形橡皮擦路径，避免快速移动时在采样点之间留下间隙。
fn draw_circle_erase_path(canvas: &Canvas, points: &[CanvasPoint], diameter: f32) {
    let Some(first) = points.first() else {
        return;
    };
    let mut paint = clear_paint(PaintStyle::Stroke);
    paint.set_stroke_width(diameter);
    paint.set_stroke_cap(PaintCap::Round);
    paint.set_stroke_join(PaintJoin::Round);

    if points.len() == 1 {
        paint.set_style(PaintStyle::Fill);
        canvas.draw_circle((first.x, first.y), diameter / 2.0, &paint);
        return;
    }

    let mut path_builder = PathBuilder::new();
    path_builder.move_to((first.x, first.y));
    for point in &points[1..] {
        path_builder.line_to((point.x, point.y));
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

/// 使用固定宽度和圆角连接绘制画笔路径。
fn draw_pen_path(canvas: &Canvas, points: &[CanvasPoint], color: InkColor, width: PenWidth) {
    let Some(first) = points.first() else {
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

    if points.len() == 1 {
        canvas.draw_circle((first.x, first.y), width.pixels() / 2.0, &paint);
        return;
    }

    let mut path_builder = PathBuilder::new();
    path_builder.move_to((first.x, first.y));
    visit_smoothed_segments(points, |segment| match segment {
        StrokeSegment::LineTo(point) => {
            path_builder.line_to((point.x, point.y));
        }
        StrokeSegment::QuadTo { control, end } => {
            path_builder.quad_to((control.x, control.y), (end.x, end.y));
        }
    });
    let path = path_builder.detach();
    canvas.draw_path(&path, &paint);
}

/// 使用单个填充路径绘制逐点宽度速度笔锋，避免每点独立 GPU 绘制。
fn draw_variable_pen_path(canvas: &Canvas, points: &[VariableStrokePoint], color: InkColor) {
    let Some(first) = points.first() else {
        return;
    };
    let rgba = color.rgba();
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(rgba[3], rgba[0], rgba[1], rgba[2]));
    paint.set_style(PaintStyle::Fill);

    if points.len() == 1 {
        canvas.draw_circle((first.point.x, first.point.y), first.width / 2.0, &paint);
        return;
    }
    let Some(outline) = variable_outline(points) else {
        return;
    };
    let Some(first_outline) = outline.first() else {
        return;
    };
    let mut path_builder = PathBuilder::new();
    path_builder.move_to((first_outline.x, first_outline.y));
    visit_smoothed_segments(&outline, |segment| match segment {
        StrokeSegment::LineTo(point) => {
            path_builder.line_to((point.x, point.y));
        }
        StrokeSegment::QuadTo { control, end } => {
            path_builder.quad_to((control.x, control.y), (end.x, end.y));
        }
    });
    path_builder.close();
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

    /// 验证三档设置映射到固定倍率和 sample count。
    #[test]
    fn surface_config_keeps_the_requested_quality_contract() {
        let off = InkSurfaceConfig::for_mode(InkAntialiasingMode::Off);
        assert_eq!(off.render_scale, 1.0);
        assert_eq!(off.sample_count, 0);

        let msaa = InkSurfaceConfig::for_mode(InkAntialiasingMode::Msaa);
        assert_eq!(msaa.render_scale, 1.0);
        assert_eq!(msaa.sample_count, 4);

        let supersample = InkSurfaceConfig::for_mode(InkAntialiasingMode::Supersample);
        assert_eq!(supersample.render_size([1919, 1079]), Some([2879, 1619]));
    }

    /// 验证超采样尺寸始终向上取整且不接受无法表示的尺寸。
    #[test]
    fn supersample_dimensions_round_up_without_overflow() {
        let config = InkSurfaceConfig::for_mode(InkAntialiasingMode::Supersample);
        assert_eq!(config.render_size([1, 1]), Some([2, 2]));
        assert_eq!(config.render_size([u32::MAX, 1]), None);
    }

    /// 验证活动预览按 512px 网格覆盖跨块笔迹，并裁剪到窗口边缘。
    #[test]
    fn preview_region_expands_in_fixed_tiles() {
        let (origin, size) = preview_region(
            InkBounds {
                left: 500.0,
                top: 10.0,
                right: 530.0,
                bottom: 30.0,
            },
            [900, 700],
        );
        assert_eq!(origin, CanvasPoint::new(0.0, 0.0));
        assert_eq!(size, [900, 512]);

        let (origin, size) = preview_region(
            InkBounds {
                left: 880.0,
                top: 680.0,
                right: 920.0,
                bottom: 720.0,
            },
            [900, 700],
        );
        assert_eq!(origin, CanvasPoint::new(512.0, 512.0));
        assert_eq!(size, [388, 188]);
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
