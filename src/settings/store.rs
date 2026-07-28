use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use directories::BaseDirs;

use crate::error::AppError;

use super::UserSettings;

const SETTINGS_DIRECTORY: &str = "Steady-Ink";
const SETTINGS_FILE: &str = "settings.toml";
const LOGS_DIRECTORY: &str = "logs";
const RECOVERY_DIRECTORY: &str = "recovery";

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

    /// 创建设置文件父目录并返回其稳定路径。
    pub fn ensure_directory(&self) -> Result<&Path, AppError> {
        let directory = self
            .path
            .parent()
            .ok_or_else(|| AppError::Settings("设置文件路径缺少父目录".to_owned()))?;
        fs::create_dir_all(directory).map_err(|error| {
            AppError::Settings(format!("创建 {} 失败: {error}", directory.display()))
        })?;
        Ok(directory)
    }

    /// 创建配置目录下的日志目录，并返回其稳定路径。
    pub fn ensure_logs_directory(&self) -> Result<PathBuf, AppError> {
        let directory = self.ensure_directory()?.to_owned();
        let logs_directory = directory.join(LOGS_DIRECTORY);
        fs::create_dir_all(&logs_directory).map_err(|error| {
            AppError::Settings(format!(
                "创建日志目录 {} 失败: {error}",
                logs_directory.display()
            ))
        })?;
        Ok(logs_directory)
    }

    /// 返回与 settings.toml 分离的墨迹崩溃恢复目录路径。
    pub fn recovery_directory(&self) -> Result<PathBuf, AppError> {
        let directory = self
            .path
            .parent()
            .ok_or_else(|| AppError::Settings("设置文件路径缺少父目录".to_owned()))?;
        Ok(directory.join(RECOVERY_DIRECTORY))
    }

    /// 确保配置目录存在后，通过 Windows 文件资源管理器打开该目录。
    pub fn open_directory(&self) -> Result<(), AppError> {
        let directory = self.ensure_directory()?;
        Command::new("explorer.exe")
            .arg(directory)
            .spawn()
            .map_err(|error| {
                AppError::Settings(format!(
                    "打开配置目录 {} 失败: {error}",
                    directory.display()
                ))
            })?;
        Ok(())
    }
}
