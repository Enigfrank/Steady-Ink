use egui::{
    Color32, CornerRadius, Frame, InnerResponse, Painter, Pos2, Rect, Shape, Stroke, StrokeKind,
    Ui, epaint::RectShape,
};

/// 将 egui 逻辑点映射到当前 DPI 的物理像素网格。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PixelGrid {
    pixels_per_point: f32,
}

impl PixelGrid {
    /// 使用当前帧的物理像素比例创建像素网格。
    pub(crate) fn new(pixels_per_point: f32) -> Self {
        debug_assert!(pixels_per_point.is_finite() && pixels_per_point > 0.0);
        let pixels_per_point = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
            pixels_per_point
        } else {
            1.0
        };
        Self { pixels_per_point }
    }

    /// 从 UI painter 读取当前帧的 DPI 比例。
    pub(crate) fn from_ui(ui: &Ui) -> Self {
        Self::new(ui.pixels_per_point())
    }

    /// 将填充或裁剪矩形的可见边缘对齐到整数物理像素。
    pub(crate) fn snap_filled_rect(self, rect: Rect) -> Rect {
        self.snap_rect_edges(rect)
    }

    /// 按 inside 描边的中心线契约对齐矩形外边缘。
    pub(crate) fn snap_inside_stroke_rect(self, rect: Rect, stroke_width: f32) -> Rect {
        if !Self::can_snap_rect(rect) {
            return rect;
        }
        if !stroke_width.is_finite() || stroke_width <= 0.0 {
            return self.snap_filled_rect(rect);
        }

        let width_pixels = self.quantized_stroke_width_pixels(stroke_width);
        let width_points = width_pixels as f32 / self.pixels_per_point;
        let half_width = width_points / 2.0;
        let left_center = self.snap_stroke_center(rect.left() + half_width, width_pixels);
        let right_center = self.snap_stroke_center(rect.right() - half_width, width_pixels);
        let top_center = self.snap_stroke_center(rect.top() + half_width, width_pixels);
        let bottom_center = self.snap_stroke_center(rect.bottom() - half_width, width_pixels);

        self.ensure_visible_rect(Rect::from_min_max(
            Pos2::new(left_center - half_width, top_center - half_width),
            Pos2::new(right_center + half_width, bottom_center + half_width),
        ))
    }

    /// 将可见描边宽度量化为至少一个整数物理像素。
    pub(crate) fn quantize_stroke(self, stroke: Stroke) -> Stroke {
        if !stroke.width.is_finite() || stroke.width <= 0.0 || stroke.color == Color32::TRANSPARENT
        {
            return stroke;
        }

        Stroke::new(
            self.quantized_stroke_width_pixels(stroke.width) as f32 / self.pixels_per_point,
            stroke.color,
        )
    }

    /// 对齐水平或垂直线段；非轴对齐线段保持原几何和线宽。
    pub(crate) fn snap_axis_aligned_line(
        self,
        points: [Pos2; 2],
        stroke: Stroke,
    ) -> ([Pos2; 2], Stroke) {
        if !stroke.width.is_finite() || stroke.width <= 0.0 || stroke.color == Color32::TRANSPARENT
        {
            return (points, stroke);
        }

        let horizontal = (points[0].y - points[1].y).abs() <= f32::EPSILON;
        let vertical = (points[0].x - points[1].x).abs() <= f32::EPSILON;
        if !horizontal && !vertical {
            return (points, stroke);
        }

        let width_pixels = self.quantized_stroke_width_pixels(stroke.width);
        let snapped_stroke = Stroke::new(width_pixels as f32 / self.pixels_per_point, stroke.color);
        if horizontal {
            let center_y = self.snap_stroke_center(points[0].y, width_pixels);
            (
                [
                    Pos2::new(self.snap_edge(points[0].x), center_y),
                    Pos2::new(self.snap_edge(points[1].x), center_y),
                ],
                snapped_stroke,
            )
        } else {
            let center_x = self.snap_stroke_center(points[0].x, width_pixels);
            (
                [
                    Pos2::new(center_x, self.snap_edge(points[0].y)),
                    Pos2::new(center_x, self.snap_edge(points[1].y)),
                ],
                snapped_stroke,
            )
        }
    }

    /// 将一个逻辑坐标对齐到最近整数物理像素。
    fn snap_edge(self, value: f32) -> f32 {
        (value * self.pixels_per_point).round() / self.pixels_per_point
    }

    /// 返回名义线宽对应的整数物理像素数量。
    fn quantized_stroke_width_pixels(self, stroke_width: f32) -> u32 {
        (stroke_width * self.pixels_per_point).round().max(1.0) as u32
    }

    /// 按物理线宽奇偶性对齐描边中心线。
    fn snap_stroke_center(self, value: f32, width_pixels: u32) -> f32 {
        let phase = if width_pixels.is_multiple_of(2) {
            0.0
        } else {
            0.5
        };
        ((value * self.pixels_per_point - phase).round() + phase) / self.pixels_per_point
    }

    /// 对齐矩形四条可见边缘，并为正尺寸输入保留至少一个物理像素。
    fn snap_rect_edges(self, rect: Rect) -> Rect {
        if !Self::can_snap_rect(rect) {
            return rect;
        }

        self.ensure_visible_rect(Rect::from_min_max(
            Pos2::new(self.snap_edge(rect.min.x), self.snap_edge(rect.min.y)),
            Pos2::new(self.snap_edge(rect.max.x), self.snap_edge(rect.max.y)),
        ))
    }

    /// 避免独立对齐两侧边缘后产生零尺寸或反向矩形。
    fn ensure_visible_rect(self, mut rect: Rect) -> Rect {
        let pixel_size = 1.0 / self.pixels_per_point;
        if rect.max.x <= rect.min.x {
            rect.max.x = rect.min.x + pixel_size;
        }
        if rect.max.y <= rect.min.y {
            rect.max.y = rect.min.y + pixel_size;
        }
        rect
    }

    /// 判断矩形是否具有可安全对齐的有限正尺寸。
    fn can_snap_rect(rect: Rect) -> bool {
        rect.min.x.is_finite()
            && rect.min.y.is_finite()
            && rect.max.x.is_finite()
            && rect.max.y.is_finite()
            && rect.width() > 0.0
            && rect.height() > 0.0
    }
}

/// 绘制不改变布局矩形的物理像素对齐填充和 inside 边框。
pub(crate) fn paint_pixel_aligned_rect(
    ui: &Ui,
    rect: Rect,
    corner_radius: impl Into<CornerRadius>,
    fill: Color32,
    stroke: Stroke,
) {
    ui.painter().add(pixel_aligned_rect_shape(
        PixelGrid::from_ui(ui),
        rect,
        corner_radius.into(),
        fill,
        stroke,
    ));
}

/// 绘制轴对齐线段，并让非轴对齐线段继续走正常抗锯齿路径。
pub(crate) fn paint_pixel_aligned_line(painter: &Painter, points: [Pos2; 2], stroke: Stroke) {
    let (points, stroke) =
        PixelGrid::new(painter.pixels_per_point()).snap_axis_aligned_line(points, stroke);
    painter.line_segment(points, stroke);
}

/// 使用原始 Frame 布局数据绘制像素对齐背景和边框。
pub(crate) fn show_pixel_aligned_frame<R>(
    ui: &mut Ui,
    frame: Frame,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    let background_shape = ui.painter().add(Shape::Noop);
    let mut prepared = frame.begin(ui);
    let inner = add_contents(&mut prepared.content_ui);
    let content_rect = prepared.content_ui.min_rect();
    let widget_rect = frame.widget_rect(content_rect);
    let response = prepared.allocate_space(ui);

    if ui.is_rect_visible(widget_rect) {
        let grid = PixelGrid::from_ui(ui);
        let rect_shape = pixel_aligned_rect_shape(
            grid,
            widget_rect,
            frame.corner_radius,
            frame.fill,
            frame.stroke,
        );
        let shape = if frame.shadow == Default::default() {
            rect_shape
        } else {
            let rect = aligned_rect(grid, widget_rect, frame.stroke);
            Shape::Vec(vec![
                Shape::from(frame.shadow.as_shape(rect, frame.corner_radius)),
                rect_shape,
            ])
        };
        ui.painter().set(background_shape, shape);
    }

    InnerResponse::new(inner, response)
}

/// 创建像素对齐的 egui 矩形 Shape，供直接绘制和 Frame 包装器复用。
fn pixel_aligned_rect_shape(
    grid: PixelGrid,
    rect: Rect,
    corner_radius: CornerRadius,
    fill: Color32,
    stroke: Stroke,
) -> Shape {
    let rect = aligned_rect(grid, rect, stroke);
    let stroke = grid.quantize_stroke(stroke);
    Shape::Rect(RectShape::new(
        rect,
        corner_radius,
        fill,
        stroke,
        StrokeKind::Inside,
    ))
}

/// 根据是否存在可见描边选择填充边缘或 inside 描边矩形契约。
fn aligned_rect(grid: PixelGrid, rect: Rect, stroke: Stroke) -> Rect {
    if stroke.width.is_finite() && stroke.width > 0.0 && stroke.color != Color32::TRANSPARENT {
        grid.snap_inside_stroke_rect(rect, stroke.width)
    } else {
        grid.snap_filled_rect(rect)
    }
}
