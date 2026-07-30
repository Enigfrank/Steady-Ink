/// 放映交互控件在窗口客户区中的物理像素矩形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PhysicalHitRect {
    pub(crate) min_x: i32,
    pub(crate) min_y: i32,
    pub(crate) max_x: i32,
    pub(crate) max_y: i32,
}

impl PhysicalHitRect {
    /// 返回该物理矩形是否具有正面积。
    pub(crate) const fn is_valid(self) -> bool {
        self.min_x < self.max_x && self.min_y < self.max_y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证物理 UI 区域只接受正面积矩形。
    #[test]
    fn physical_hit_rect_requires_positive_area() {
        assert!(
            PhysicalHitRect {
                min_x: 10,
                min_y: 20,
                max_x: 30,
                max_y: 40,
            }
            .is_valid()
        );
        assert!(
            !PhysicalHitRect {
                min_x: 10,
                min_y: 20,
                max_x: 10,
                max_y: 40,
            }
            .is_valid()
        );
    }
}
