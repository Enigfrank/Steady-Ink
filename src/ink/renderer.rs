use skia_safe::{
    AlphaType, BlendMode, Canvas, ClipOp, Color, ColorType, ImageInfo, Paint, PaintCap, PaintJoin,
    PaintStyle, PathBuilder, Rect, Surface,
    gpu::{self, Budgeted, DirectContext, SurfaceOrigin},
};

use super::{
    CanvasPoint, EraseSample, EraserSize, InkBounds, InkColor, InkDocument, InkOperation, InkTool,
    OperationId, PenWidth,
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
    PalmErase {
        samples: &'a [EraseSample],
    },
}

const ERASE_RADIUS_EPSILON: f32 = 0.01;
const PALM_INTERPOLATION_STEP_FRACTION: f32 = 0.75;
const PALM_INTERPOLATION_MIN_STEP: f32 = 4.0;
const PALM_INTERPOLATION_MAX_STEP: f32 = 24.0;

/// 当前活动页唯一的持久 Skia GPU 墨迹层。
pub struct InkRenderCache {
    surface: Surface,
    applied_operation_count: usize,
    last_operation_id: Option<OperationId>,
    pending_region_rebuild: Option<InkBounds>,
    full_rebuild_requested: bool,
}

impl InkRenderCache {
    /// 为指定物理像素尺寸创建透明、无 MSAA 的 GPU 墨迹层。
    pub fn new(context: &mut DirectContext, size: [u32; 2]) -> Result<Self, AppError> {
        let mut surface = create_gpu_surface(context, size)?;
        surface.canvas().clear(Color::TRANSPARENT);
        Ok(Self {
            surface,
            applied_operation_count: 0,
            last_operation_id: None,
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

        for operation in &history[self.applied_operation_count..] {
            draw_operation(self.surface.canvas(), operation);
        }
        self.applied_operation_count = history.len();
        self.last_operation_id = history.last().map(InkOperation::id);
    }

    /// 返回当前 GPU 墨迹层快照，供同一上下文合成到窗口 framebuffer。
    pub fn snapshot(&mut self) -> skia_safe::Image {
        self.surface.image_snapshot()
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
        self.pending_region_rebuild = Some(
            self.pending_region_rebuild
                .map_or(bounds, |current| current.union(bounds)),
        );
    }

    /// 从最近一次清屏后的可见操作重建整个活动页 GPU surface。
    fn rebuild(&mut self, document: &InkDocument) {
        self.surface.canvas().clear(Color::TRANSPARENT);
        for operation in document.replay_operations() {
            draw_operation(self.surface.canvas(), operation);
        }
        self.applied_operation_count = document.operations().len();
        self.last_operation_id = document.operations().last().map(InkOperation::id);
        self.pending_region_rebuild = None;
        self.full_rebuild_requested = false;
    }

    /// 清理指定矩形并按事实历史顺序重放所有与该矩形相交的可见操作。
    fn rebuild_region(&mut self, document: &InkDocument, bounds: InkBounds) {
        let clip_rect = Rect::new(bounds.left, bounds.top, bounds.right, bounds.bottom);
        let canvas = self.surface.canvas();
        let save_count = canvas.save();
        canvas.clip_rect(clip_rect, ClipOp::Intersect, false);

        let mut clear_paint = Paint::default();
        clear_paint.set_style(PaintStyle::Fill);
        clear_paint.set_blend_mode(BlendMode::Clear);
        canvas.draw_rect(clip_rect, &clear_paint);
        for operation in document.replay_operations() {
            if operation
                .bounds()
                .is_some_and(|operation_bounds| operation_bounds.intersects(bounds))
            {
                draw_operation(canvas, operation);
            }
        }
        canvas.restore_to_count(save_count);
        self.applied_operation_count = document.operations().len();
        self.last_operation_id = document.operations().last().map(InkOperation::id);
    }
}

/// 创建活动页持久离屏 GPU surface。
fn create_gpu_surface(context: &mut DirectContext, size: [u32; 2]) -> Result<Surface, AppError> {
    let dimensions = (
        i32::try_from(size[0].max(1)).map_err(|error| AppError::Graphics(error.to_string()))?,
        i32::try_from(size[1].max(1)).map_err(|error| AppError::Graphics(error.to_string()))?,
    );
    let image_info = ImageInfo::new(dimensions, ColorType::RGBA8888, AlphaType::Premul, None);
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

/// 将一个事实 operation 应用到当前 GPU 墨迹层。
fn draw_operation(canvas: &Canvas, operation: &InkOperation) {
    match operation {
        InkOperation::DrawStroke(stroke) => {
            draw_pen_path(canvas, &stroke.points, stroke.color, stroke.width);
        }
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
        rotation_radians: previous.rotation_radians
            + (next.rotation_radians - previous.rotation_radians) * progress,
    }
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
    for point in &points[1..] {
        path_builder.line_to((point.x, point.y));
    }
    let path = path_builder.detach();
    canvas.draw_path(&path, &paint);
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

    /// 验证普通固定圆形橡皮擦会走连续路径渲染分支。
    #[test]
    fn uniform_circle_samples_are_detected() {
        let samples = [
            EraseSample::circle(CanvasPoint::new(0.0, 0.0), 48.0),
            EraseSample::circle(CanvasPoint::new(80.0, 0.0), 48.0),
        ];

        assert_eq!(uniform_circle_diameter(&samples), Some(48.0));
    }

    /// 验证快速移动的动态手掌擦除会在两个原始采样之间补足椭圆。
    #[test]
    fn palm_interpolation_covers_large_sample_gaps() {
        let previous = EraseSample {
            center: CanvasPoint::new(0.0, 0.0),
            radius_x: 40.0,
            radius_y: 20.0,
            rotation_radians: 0.0,
        };
        let next = EraseSample {
            center: CanvasPoint::new(120.0, 0.0),
            radius_x: 40.0,
            radius_y: 20.0,
            rotation_radians: 0.5,
        };

        assert!(palm_interpolation_steps(previous, next) > 1);
        assert_eq!(interpolate_erase_sample(previous, next, 0.5).center.x, 60.0);
    }
}
