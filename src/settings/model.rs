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

/// 用户可持久化的手掌尺寸分类预设。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PalmSizePreset {
    Small,
    Standard,
    Large,
}

impl PalmSizePreset {
    /// 返回设置页按尺寸递增顺序展示的全部预设。
    pub const ALL: [Self; 3] = [Self::Small, Self::Standard, Self::Large];

    /// 返回三档手掌尺寸的中文名称。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Small => "小",
            Self::Standard => "标准",
            Self::Large => "大",
        }
    }
}

impl Default for PalmSizePreset {
    /// 返回兼顾漏判和误判的标准手掌尺寸。
    fn default() -> Self {
        Self::Standard
    }
}

/// 可跨应用重启保存的默认工具偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolPreferences {
    pub color: InkColor,
    pub pen_width: PenWidth,
    pub eraser_size: EraserSize,
    #[serde(alias = "speed_taper_enabled")]
    pub natural_taper_enabled: bool,
}

impl Default for ToolPreferences {
    /// 返回产品确认的红色、4px 画笔和 72px 橡皮擦默认值。
    fn default() -> Self {
        Self {
            color: InkColor::default(),
            pen_width: PenWidth::default(),
            eraser_size: EraserSize::default(),
            natural_taper_enabled: false,
        }
    }
}

/// 应用允许持久化的用户设置；该结构不包含任何墨迹。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    pub tools: ToolPreferences,
    pub palm_size_preset: PalmSizePreset,
    pub slideshow_integration_enabled: bool,
    pub log_level: LogLevel,
    pub readable_mode: bool,
    pub performance_monitoring_enabled: bool,
}

impl Default for UserSettings {
    /// 返回启用放映联动的默认设置。
    fn default() -> Self {
        Self {
            tools: ToolPreferences::default(),
            palm_size_preset: PalmSizePreset::default(),
            slideshow_integration_enabled: true,
            log_level: LogLevel::default(),
            readable_mode: false,
            performance_monitoring_enabled: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PalmSizePreset, UserSettings};
    use crate::ink::{EraserSize, PenWidth};

    #[test]
    fn missing_readable_mode_uses_the_default() {
        let settings: UserSettings = toml::from_str(
            r#"
                slideshow_integration_enabled = true
            "#,
        )
        .expect("旧版设置应能反序列化");

        assert!(!settings.readable_mode);
        assert!(!settings.performance_monitoring_enabled);
    }

    #[test]
    fn missing_new_ink_preferences_use_disabled_defaults() {
        let settings: UserSettings = toml::from_str(
            r#"
                slideshow_integration_enabled = true
            "#,
        )
        .expect("旧版设置应能反序列化");

        assert!(!settings.tools.natural_taper_enabled);
        assert_eq!(settings.palm_size_preset, PalmSizePreset::Standard);
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

    /// 验证性能监控默认关闭且启用值可稳定持久化。
    #[test]
    fn performance_monitoring_survives_a_toml_round_trip() {
        let settings = UserSettings {
            performance_monitoring_enabled: true,
            ..UserSettings::default()
        };
        let serialized = toml::to_string(&settings).expect("设置应能序列化");
        let reloaded: UserSettings = toml::from_str(&serialized).expect("设置应能反序列化");

        assert!(reloaded.performance_monitoring_enabled);
    }

    #[test]
    fn natural_taper_preference_survives_a_toml_round_trip() {
        let settings = UserSettings {
            tools: super::ToolPreferences {
                natural_taper_enabled: true,
                ..super::ToolPreferences::default()
            },
            ..UserSettings::default()
        };
        let serialized = toml::to_string(&settings).expect("设置应能序列化");
        let reloaded: UserSettings = toml::from_str(&serialized).expect("设置应能反序列化");

        assert!(reloaded.tools.natural_taper_enabled);
        assert!(serialized.contains("natural_taper_enabled = true"));
        assert!(!serialized.contains("speed_taper_enabled"));
    }

    /// 验证旧速度笔锋键的启用和关闭值都迁移为自然笔锋设置。
    #[test]
    fn legacy_speed_taper_preference_migrates_on_save() {
        for enabled in [false, true] {
            let source = format!(
                "[tools]\nspeed_taper_enabled = {}\n",
                if enabled { "true" } else { "false" }
            );
            let settings: UserSettings = toml::from_str(&source).expect("旧设置键应能反序列化");
            let serialized = toml::to_string(&settings).expect("迁移后的设置应能序列化");

            assert_eq!(settings.tools.natural_taper_enabled, enabled);
            assert!(serialized.contains(&format!("natural_taper_enabled = {enabled}")));
            assert!(!serialized.contains("speed_taper_enabled"));
        }
    }

    /// 验证三档手掌尺寸使用稳定名称完成设置往返。
    #[test]
    fn palm_size_presets_survive_a_toml_round_trip() {
        for preset in PalmSizePreset::ALL {
            let settings = UserSettings {
                palm_size_preset: preset,
                ..UserSettings::default()
            };
            let serialized = toml::to_string(&settings).expect("设置应能序列化");
            let reloaded: UserSettings = toml::from_str(&serialized).expect("设置应能反序列化");

            assert_eq!(reloaded.palm_size_preset, preset);
        }
    }

    /// 验证新增 6px 档位保持准确像素宽度和稳定的 TOML 名称。
    #[test]
    fn six_pixel_pen_width_survives_a_toml_round_trip() {
        let settings = UserSettings {
            tools: super::ToolPreferences {
                pen_width: PenWidth::Px6,
                ..super::ToolPreferences::default()
            },
            ..UserSettings::default()
        };

        assert_eq!(settings.tools.pen_width.pixels(), 6.0);

        let serialized = toml::to_string(&settings).expect("设置应能序列化");
        assert!(serialized.contains("pen_width = \"px6\""));

        let reloaded: UserSettings = toml::from_str(&serialized).expect("设置应能反序列化");
        assert_eq!(reloaded.tools.pen_width, PenWidth::Px6);
    }

    /// 验证旧版 24px 画笔档位迁移到最接近的保留档位，并在保存时清除旧值。
    #[test]
    fn legacy_24px_pen_width_migrates_to_16px_on_save() {
        let settings: UserSettings = toml::from_str(
            r#"
                [tools]
                pen_width = "px24"
            "#,
        )
        .expect("旧版 24px 画笔档位应能反序列化");

        assert_eq!(settings.tools.pen_width, PenWidth::Px16);

        let serialized = toml::to_string(&settings).expect("设置应能序列化");
        assert!(serialized.contains("pen_width = \"px16\""));
        assert!(!serialized.contains("px24"));
    }

    /// 验证旧版 24/48/72px 橡皮擦配置迁移到 36/72/144px 新档位。
    #[test]
    fn legacy_eraser_sizes_migrate_to_new_diameters() {
        // 旧 24px 和 48px 就近映射到新档位；旧 72px 保留同名档位。
        for (legacy, expected) in [
            ("px24", EraserSize::Px36),
            ("px48", EraserSize::Px72),
            ("px72", EraserSize::Px72),
        ] {
            let source = format!("[tools]\neraser_size = \"{legacy}\"\n");
            let settings: UserSettings =
                toml::from_str(&source).expect("旧版橡皮擦档位应能反序列化");

            assert_eq!(settings.tools.eraser_size, expected);
        }

        // 新档位使用稳定的 36/72/144 像素直径。
        assert_eq!(EraserSize::Px36.pixels(), 36.0);
        assert_eq!(EraserSize::Px72.pixels(), 72.0);
        assert_eq!(EraserSize::Px144.pixels(), 144.0);

        // 新配置往返使用稳定名称。
        let serialized = toml::to_string(&UserSettings {
            tools: super::ToolPreferences {
                eraser_size: EraserSize::Px144,
                ..super::ToolPreferences::default()
            },
            ..UserSettings::default()
        })
        .expect("设置应能序列化");
        assert!(serialized.contains("eraser_size = \"px144\""));
    }

    /// 验证旧版抗锯齿字段可被兼容忽略，保存时不再写回可变档位。
    #[test]
    fn legacy_antialiasing_preferences_are_removed_on_save() {
        let settings: UserSettings = toml::from_str(
            r#"
                slideshow_integration_enabled = true
                ink_antialiasing = "legacy"
                ink_quality_priority = "high_quality"
            "#,
        )
        .expect("旧版抗锯齿字段应被兼容忽略");

        let serialized = toml::to_string(&settings).expect("设置应能序列化");
        assert!(!serialized.contains("ink_antialiasing"));
        assert!(!serialized.contains("ink_quality_priority"));
    }
}
