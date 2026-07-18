use serde::{Deserialize, Serialize};

use crate::ink::{EraserSize, InkColor, PenWidth};

/// 可由用户保存并立即应用的 tracing 最大详细程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    /// 返回设置页按稳定顺序展示的全部日志级别。
    pub const ALL: [Self; 4] = [Self::Error, Self::Warn, Self::Info, Self::Debug];

    /// 返回设置页使用的中文级别名称。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Error => "错误",
            Self::Warn => "警告",
            Self::Info => "信息",
            Self::Debug => "调试",
        }
    }

    /// 返回用于构造 tracing `EnvFilter` 的全局指令。
    pub const fn filter_directive(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }
}

impl Default for LogLevel {
    /// 返回兼顾常规诊断与文件体积的信息级默认值。
    fn default() -> Self {
        Self::Info
    }
}

/// 可跨应用重启保存的默认工具偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolPreferences {
    pub color: InkColor,
    pub pen_width: PenWidth,
    pub eraser_size: EraserSize,
}

impl Default for ToolPreferences {
    /// 返回产品确认的红色、4pt 画笔和 48px 橡皮擦默认值。
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
    pub log_level: LogLevel,
    pub readable_mode: bool,
}

impl Default for UserSettings {
    /// 返回启用放映联动的默认设置。
    fn default() -> Self {
        Self {
            tools: ToolPreferences::default(),
            slideshow_integration_enabled: true,
            log_level: LogLevel::default(),
            readable_mode: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UserSettings;

    #[test]
    fn missing_readable_mode_uses_the_default() {
        let settings: UserSettings = toml::from_str(
            r#"
                slideshow_integration_enabled = true
            "#,
        )
        .expect("旧版设置应能反序列化");

        assert!(!settings.readable_mode);
    }

    #[test]
    fn readable_mode_survives_a_toml_round_trip() {
        let settings = UserSettings {
            readable_mode: true,
            ..UserSettings::default()
        };
        let serialized = toml::to_string(&settings).expect("设置应能序列化");
        let reloaded: UserSettings = toml::from_str(&serialized).expect("设置应能反序列化");

        assert!(reloaded.readable_mode);
    }
}
