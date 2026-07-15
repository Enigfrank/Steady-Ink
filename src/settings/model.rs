use serde::{Deserialize, Serialize};

use crate::ink::{EraserSize, InkColor, PenWidth};

/// 可跨应用重启保存的默认工具偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolPreferences {
    pub color: InkColor,
    pub pen_width: PenWidth,
    pub eraser_size: EraserSize,
}

impl Default for ToolPreferences {
    /// 返回产品确认的红色、8px 画笔和 48px 橡皮擦默认值。
    fn default() -> Self {
        Self {
            color: InkColor::default(),
            pen_width: PenWidth::default(),
            eraser_size: EraserSize::default(),
        }
    }
}

/// 应用允许持久化的用户设置；该结构不包含任何墨迹。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    pub tools: ToolPreferences,
    pub slideshow_integration_enabled: bool,
}

impl Default for UserSettings {
    /// 返回启用放映联动的默认设置。
    fn default() -> Self {
        Self {
            tools: ToolPreferences::default(),
            slideshow_integration_enabled: true,
        }
    }
}
