use std::{fs, path::PathBuf};

use directories::BaseDirs;

use crate::error::AppError;

use super::UserSettings;

const SETTINGS_DIRECTORY: &str = "Steady-Ink";
const SETTINGS_FILE: &str = "settings.toml";

/// 管理当前 Windows 用户的 TOML 偏好文件，不接触任何墨迹数据。
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// 根据当前用户 roaming 配置目录创建设置存储。
    pub fn new() -> Result<Self, AppError> {
        let base_dirs = BaseDirs::new()
            .ok_or_else(|| AppError::Settings("无法定位当前用户配置目录".to_owned()))?;
        Ok(Self {
            path: base_dirs
                .config_dir()
                .join(SETTINGS_DIRECTORY)
                .join(SETTINGS_FILE),
        })
    }

    /// 从磁盘读取设置；文件尚不存在时返回产品默认值。
    pub fn load(&self) -> Result<UserSettings, AppError> {
        if !self.path.exists() {
            return Ok(UserSettings::default());
        }
        let source = fs::read_to_string(&self.path).map_err(|error| {
            AppError::Settings(format!("读取 {} 失败: {error}", self.path.display()))
        })?;
        toml::from_str(&source).map_err(|error| {
            AppError::Settings(format!("解析 {} 失败: {error}", self.path.display()))
        })
    }

    /// 创建父目录并覆盖写入完整设置快照。
    pub fn save(&self, settings: &UserSettings) -> Result<(), AppError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| AppError::Settings("设置文件路径缺少父目录".to_owned()))?;
        fs::create_dir_all(parent).map_err(|error| {
            AppError::Settings(format!("创建 {} 失败: {error}", parent.display()))
        })?;
        let serialized = toml::to_string_pretty(settings)
            .map_err(|error| AppError::Settings(format!("序列化设置失败: {error}")))?;
        fs::write(&self.path, serialized).map_err(|error| {
            AppError::Settings(format!("写入 {} 失败: {error}", self.path.display()))
        })
    }

    /// 返回诊断界面可显示的实际设置文件路径。
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::ink::{EraserSize, InkColor, PenWidth};

    /// 验证工具默认值和联动开关可以完整写入并读回 TOML。
    #[test]
    fn settings_round_trip_preserves_user_preferences() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间应晚于 Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "steady-ink-settings-test-{}-{unique}",
            std::process::id()
        ));
        let store = SettingsStore {
            path: directory.join(SETTINGS_FILE),
        };
        let settings = UserSettings {
            tools: crate::settings::ToolPreferences {
                color: InkColor::Blue,
                pen_width: PenWidth::Px24,
                eraser_size: EraserSize::Px72,
            },
            slideshow_integration_enabled: false,
        };

        store.save(&settings).expect("测试设置应成功写入");
        assert_eq!(store.load().expect("测试设置应成功读回"), settings);

        std::fs::remove_dir_all(directory).expect("测试临时目录应可清理");
    }
}
