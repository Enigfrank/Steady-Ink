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

/// 墨迹层使用的可选增强抗锯齿模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InkAntialiasingMode {
    Off,
    Msaa,
    Supersample,
}

impl InkAntialiasingMode {
    /// 返回设置页按稳定顺序展示的全部墨迹抗锯齿模式。
    pub const ALL: [Self; 3] = [Self::Off, Self::Msaa, Self::Supersample];

    /// 返回设置页使用的中文模式名称。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "关闭",
            Self::Msaa => "MSAA 4x",
            Self::Supersample => "超采样 1.5x",
        }
    }
}

impl Default for InkAntialiasingMode {
    /// 默认保持当前单采样墨迹路径，避免旧配置产生额外 GPU 开销。
    fn default() -> Self {
        Self::Off
    }
}

/// 可跨应用重启保存的默认工具偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolPreferences {
    pub color: InkColor,
    pub pen_width: PenWidth,
    pub eraser_size: EraserSize,
    pub speed_taper_enabled: bool,
}

impl Default for ToolPreferences {
    /// 返回产品确认的红色、4pt 画笔和 48px 橡皮擦默认值。
    fn default() -> Self {
        Self {
            color: InkColor::default(),
            pen_width: PenWidth::default(),
            eraser_size: EraserSize::default(),
            speed_taper_enabled: false,
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
    pub ink_antialiasing: InkAntialiasingMode,
}

impl Default for UserSettings {
    /// 返回启用放映联动的默认设置。
    fn default() -> Self {
        Self {
            tools: ToolPreferences::default(),
            slideshow_integration_enabled: true,
            log_level: LogLevel::default(),
            readable_mode: false,
            ink_antialiasing: InkAntialiasingMode::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{InkAntialiasingMode, UserSettings};

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
    fn missing_new_ink_preferences_use_disabled_defaults() {
        let settings: UserSettings = toml::from_str(
            r#"
                slideshow_integration_enabled = true
            "#,
        )
        .expect("旧版设置应能反序列化");

        assert!(!settings.tools.speed_taper_enabled);
        assert_eq!(settings.ink_antialiasing, InkAntialiasingMode::Off);
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

    #[test]
    fn new_ink_preferences_survive_a_toml_round_trip() {
        let settings = UserSettings {
            tools: super::ToolPreferences {
                speed_taper_enabled: true,
                ..super::ToolPreferences::default()
            },
            ink_antialiasing: InkAntialiasingMode::Supersample,
            ..UserSettings::default()
        };
        let serialized = toml::to_string(&settings).expect("设置应能序列化");
        let reloaded: UserSettings = toml::from_str(&serialized).expect("设置应能反序列化");

        assert!(reloaded.tools.speed_taper_enabled);
        assert_eq!(reloaded.ink_antialiasing, InkAntialiasingMode::Supersample);
    }
}
