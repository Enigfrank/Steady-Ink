use serde::{Deserialize, Serialize};

/// 画布中的物理像素坐标。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CanvasPoint {
    pub x: f32,
    pub y: f32,
}

impl CanvasPoint {
    /// 创建一个画布坐标点。
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// 一个墨迹操作影响到的轴对齐包围框。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InkBounds {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl InkBounds {
    /// 从点集和额外半径计算包围框；空点集返回 `None`。
    pub fn from_points(points: &[CanvasPoint], radius: f32) -> Option<Self> {
        let first = points.first()?;
        let mut bounds = Self {
            left: first.x,
            top: first.y,
            right: first.x,
            bottom: first.y,
        };

        for point in &points[1..] {
            bounds.left = bounds.left.min(point.x);
            bounds.top = bounds.top.min(point.y);
            bounds.right = bounds.right.max(point.x);
            bounds.bottom = bounds.bottom.max(point.y);
        }

        Some(bounds.expanded(radius))
    }

    /// 返回向四周扩展指定物理像素后的包围框。
    pub const fn expanded(self, amount: f32) -> Self {
        Self {
            left: self.left - amount,
            top: self.top - amount,
            right: self.right + amount,
            bottom: self.bottom + amount,
        }
    }

    /// 返回同时覆盖当前包围框和另一个包围框的并集。
    pub fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    /// 返回两个轴对齐包围框是否存在交集。
    pub const fn intersects(self, other: Self) -> bool {
        self.left <= other.right
            && self.right >= other.left
            && self.top <= other.bottom
            && self.bottom >= other.top
    }

    /// 从左上角坐标和宽高创建包围框。
    pub const fn from_xywh(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        }
    }

    /// 返回包围框的宽度。
    pub const fn width(&self) -> f32 {
        self.right - self.left
    }

    /// 返回包围框的高度。
    pub const fn height(&self) -> f32 {
        self.bottom - self.top
    }

    /// 返回边界是否由有限数值组成且没有轴向倒置。
    pub(crate) fn is_valid(self) -> bool {
        self.left.is_finite()
            && self.top.is_finite()
            && self.right.is_finite()
            && self.bottom.is_finite()
            && self.left <= self.right
            && self.top <= self.bottom
    }
}

/// 快速画笔颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InkColor {
    Red,
    Yellow,
    Blue,
    Green,
    Black,
    White,
}

impl InkColor {
    /// 返回供 Skia 和 egui 共用的非预乘 RGBA 颜色。
    pub const fn rgba(self) -> [u8; 4] {
        match self {
            Self::Red => [220, 38, 38, 255],
            Self::Yellow => [250, 204, 21, 255],
            Self::Blue => [37, 99, 235, 255],
            Self::Green => [22, 163, 74, 255],
            Self::Black => [17, 24, 39, 255],
            Self::White => [255, 255, 255, 255],
        }
    }
}

impl Default for InkColor {
    /// 返回产品默认画笔颜色红色。
    fn default() -> Self {
        Self::Red
    }
}

/// 固定画笔粗细档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PenWidth {
    Px4,
    Px6,
    Px8,
    #[serde(alias = "px24")]
    Px16,
}

impl PenWidth {
    /// 返回当前档位对应的物理像素宽度。
    pub const fn pixels(self) -> f32 {
        match self {
            Self::Px4 => 4.0,
            Self::Px6 => 6.0,
            Self::Px8 => 8.0,
            Self::Px16 => 16.0,
        }
    }
}

impl Default for PenWidth {
    /// 返回产品默认画笔粗细 4px 对应的最细档位。
    fn default() -> Self {
        Self::Px4
    }
}

/// 固定区域橡皮擦直径档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EraserSize {
    Px24,
    Px48,
    Px72,
}

impl EraserSize {
    /// 返回当前档位对应的物理像素直径。
    pub const fn pixels(self) -> f32 {
        match self {
            Self::Px24 => 24.0,
            Self::Px48 => 48.0,
            Self::Px72 => 72.0,
        }
    }
}

impl Default for EraserSize {
    /// 返回产品默认橡皮擦大小 48px。
    fn default() -> Self {
        Self::Px48
    }
}

/// 当前选择的墨迹工具。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InkTool {
    Pen,
    RegionEraser,
}

impl Default for InkTool {
    /// 返回默认画笔工具。
    fn default() -> Self {
        Self::Pen
    }
}

/// 文档内单调递增的墨迹操作标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OperationId(u64);

impl OperationId {
    /// 从文档分配器生成新的操作标识。
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 返回标识的原始整数值，供诊断和性能记录使用。
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// 一条速度笔锋笔画中的确定位置和物理像素宽度。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VariableStrokePoint {
    pub point: CanvasPoint,
    pub width: f32,
}

/// 画笔笔画的固定宽度或逐点宽度几何形状。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DrawStrokeShape {
    Fixed {
        points: Vec<CanvasPoint>,
        width: PenWidth,
    },
    Variable {
        points: Vec<VariableStrokePoint>,
    },
}

/// 一条固定颜色并保存确定几何形状的画笔笔画。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawStroke {
    pub id: OperationId,
    pub color: InkColor,
    pub shape: DrawStrokeShape,
    pub bounds: InkBounds,
}

impl DrawStroke {
    /// 创建画笔笔画，并预计算用于脏区重绘的包围框。
    pub(crate) fn new(
        id: OperationId,
        points: Vec<CanvasPoint>,
        color: InkColor,
        width: PenWidth,
    ) -> Option<Self> {
        let bounds = InkBounds::from_points(&points, width.pixels() / 2.0)?;
        Some(Self {
            id,
            color,
            shape: DrawStrokeShape::Fixed { points, width },
            bounds,
        })
    }

    /// 创建逐点宽度画笔笔画，并按最大宽度计算保守脏区范围。
    pub(crate) fn new_variable(
        id: OperationId,
        points: Vec<VariableStrokePoint>,
        color: InkColor,
    ) -> Option<Self> {
        if points.is_empty()
            || points.iter().any(|sample| {
                !sample.point.x.is_finite()
                    || !sample.point.y.is_finite()
                    || !sample.width.is_finite()
                    || sample.width < 0.0
            })
        {
            return None;
        }
        let centers: Vec<_> = points.iter().map(|sample| sample.point).collect();
        let max_width = points
            .iter()
            .map(|sample| sample.width)
            .filter(|width| width.is_finite() && *width >= 0.0)
            .fold(0.0_f32, f32::max);
        let bounds = InkBounds::from_points(&centers, max_width / 2.0)?;
        Some(Self {
            id,
            color,
            shape: DrawStrokeShape::Variable { points },
            bounds,
        })
    }
}

/// 区域橡皮擦的一次椭圆采样。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EraseSample {
    pub center: CanvasPoint,
    pub radius_x: f32,
    pub radius_y: f32,
    pub rotation_radians: f32,
}

impl EraseSample {
    /// 创建普通圆形橡皮擦采样。
    pub const fn circle(center: CanvasPoint, diameter: f32) -> Self {
        let radius = diameter / 2.0;
        Self {
            center,
            radius_x: radius,
            radius_y: radius,
            rotation_radians: 0.0,
        }
    }

    /// 返回覆盖该旋转椭圆的保守轴对齐包围框。
    pub fn bounds(self) -> InkBounds {
        let sin = self.rotation_radians.sin();
        let cos = self.rotation_radians.cos();
        let half_width = ((self.radius_x * cos).powi(2) + (self.radius_y * sin).powi(2)).sqrt();
        let half_height = ((self.radius_x * sin).powi(2) + (self.radius_y * cos).powi(2)).sqrt();
        InkBounds {
            left: self.center.x - half_width,
            top: self.center.y - half_height,
            right: self.center.x + half_width,
            bottom: self.center.y + half_height,
        }
    }
}

/// 一次完整普通或手掌区域擦除会话。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EraseStroke {
    pub id: OperationId,
    pub samples: Vec<EraseSample>,
    pub bounds: InkBounds,
}

impl EraseStroke {
    /// 创建区域擦除操作，并合并全部采样的影响范围。
    pub(crate) fn new(id: OperationId, samples: Vec<EraseSample>) -> Option<Self> {
        let mut sample_iter = samples.iter().copied();
        let first_bounds = sample_iter.next()?.bounds();
        let bounds = sample_iter.fold(first_bounds, |current, sample| {
            current.union(sample.bounds())
        });
        Some(Self {
            id,
            samples,
            bounds,
        })
    }
}

/// 清屏操作及其清屏前可见内容范围。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClearOperation {
    pub id: OperationId,
    pub affected_bounds: Option<InkBounds>,
}

/// 墨迹文档中的可撤销操作。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InkOperation {
    DrawStroke(DrawStroke),
    EraseStroke(EraseStroke),
    Clear(ClearOperation),
}

impl InkOperation {
    /// 返回操作的文档内标识。
    pub const fn id(&self) -> OperationId {
        match self {
            Self::DrawStroke(stroke) => stroke.id,
            Self::EraseStroke(stroke) => stroke.id,
            Self::Clear(clear) => clear.id,
        }
    }

    /// 返回操作影响到的画布区域。
    pub const fn bounds(&self) -> Option<InkBounds> {
        match self {
            Self::DrawStroke(stroke) => Some(stroke.bounds),
            Self::EraseStroke(stroke) => Some(stroke.bounds),
            Self::Clear(clear) => clear.affected_bounds,
        }
    }
}
