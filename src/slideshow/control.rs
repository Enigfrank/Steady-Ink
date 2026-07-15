/// 放映工具栏允许请求的控制动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlideShowControlAction {
    Previous,
    Next,
    Exit,
}

/// detector 实际完成控制时使用的后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlideShowControlBackend {
    Com,
    SimulatedKey,
}
