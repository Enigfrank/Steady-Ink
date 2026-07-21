use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

#[cfg(windows)]
mod windows;

/// HKLM Run 项中 Steady Ink 使用的固定值名。
pub const AUTOSTART_VALUE_NAME: &str = "Steady Ink";
/// 所有用户登录启动项所在的 64 位注册表子键。
pub const AUTOSTART_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 受控提权辅助模式使用的唯一命令开关。
pub const HELPER_SWITCH: &str = "--machine-autostart";

/// 系统级开机启动项当前在 Windows 中的实际状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineAutostartState {
    /// 目标值不存在。
    Disabled,
    /// 目标值存在且指向当前可执行文件。
    Enabled,
    /// 目标值存在，但指向了其他路径，需要重新启用才能修复。
    EnabledPathMismatch,
}

impl MachineAutostartState {
    /// 返回注册表值是否存在；路径异常仍视为已启用。
    pub const fn enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// 返回设置页使用的状态说明。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Disabled => "当前已关闭",
            Self::Enabled => "当前已开启",
            Self::EnabledPathMismatch => "已开启，但路径需要修复",
        }
    }
}

/// 自启动边界可能报告的可诊断错误。
#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    #[error("无法获取当前可执行文件路径: {0}")]
    CurrentExecutable(#[source] std::io::Error),

    #[error("可执行文件路径无效: {0}")]
    InvalidExecutablePath(String),

    #[error("注册表操作失败: {0}")]
    Registry(String),

    #[error("请求管理员权限失败或已取消: {0}")]
    Elevation(String),

    #[error("提权辅助进程退出码为 {0}")]
    HelperExitCode(u32),

    #[error("开机启动辅助参数无效")]
    InvalidHelperArguments,

    #[error("当前平台不支持系统级开机启动")]
    UnsupportedPlatform,
}

/// 返回当前进程的绝对可执行文件路径。
fn current_executable() -> Result<PathBuf, AutostartError> {
    std::env::current_exe().map_err(AutostartError::CurrentExecutable)
}

/// 把当前可执行文件路径格式化为 Run 值使用的带引号命令。
pub fn format_run_command(path: &Path) -> Result<String, AutostartError> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(AutostartError::InvalidExecutablePath(
            path.display().to_string(),
        ));
    }
    let path = path.to_string_lossy();
    if path.contains('"') || path.contains('\0') {
        return Err(AutostartError::InvalidExecutablePath(path.into_owned()));
    }
    Ok(format!(r#""{path}""#))
}

/// 从 Run 值中读取第一个可执行文件路径，不执行任何 shell 解析。
pub fn parse_registered_command(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    if let Some(quoted) = command.strip_prefix('"') {
        let end = quoted.find('"')?;
        return Some(PathBuf::from(&quoted[..end]));
    }
    command.split_whitespace().next().map(PathBuf::from)
}

/// 根据注册表值和当前可执行文件路径生成稳定的状态分类。
fn classify_state(command: Option<&str>, expected_path: &Path) -> MachineAutostartState {
    let Some(command) = command else {
        return MachineAutostartState::Disabled;
    };
    let Some(registered_path) = parse_registered_command(command) else {
        return MachineAutostartState::EnabledPathMismatch;
    };
    if paths_equal_for_windows(&registered_path, expected_path) {
        MachineAutostartState::Enabled
    } else {
        MachineAutostartState::EnabledPathMismatch
    }
}

/// 以 Windows 不区分大小写、允许斜杠差异的规则比较两个路径。
fn paths_equal_for_windows(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    normalize(left) == normalize(right)
}

/// 为提权辅助进程返回固定且不可注入的参数值。
pub const fn helper_mode_argument(enabled: bool) -> &'static str {
    if enabled { "enable" } else { "disable" }
}

/// 解析进程启动参数；普通启动返回 `None`，辅助模式返回启用意图。
pub fn parse_helper_mode(arguments: &[OsString]) -> Result<Option<bool>, AutostartError> {
    let Some(switch) = arguments.get(1) else {
        return Ok(None);
    };
    if switch != OsStr::new(HELPER_SWITCH) {
        return Ok(None);
    }
    if arguments.len() != 3 {
        return Err(AutostartError::InvalidHelperArguments);
    }
    match arguments[2].to_str() {
        Some("enable") => Ok(Some(true)),
        Some("disable") => Ok(Some(false)),
        _ => Err(AutostartError::InvalidHelperArguments),
    }
}

/// 用可替换的注册表后端读取并分类自启动状态。
fn query_with_backend<B: RegistryBackend>(
    backend: &B,
    expected_path: &Path,
) -> Result<MachineAutostartState, AutostartError> {
    let command = backend.read_value().map_err(AutostartError::Registry)?;
    Ok(classify_state(command.as_deref(), expected_path))
}

/// 用可替换的注册表后端执行幂等写入或删除。
fn change_with_backend<B: RegistryBackend>(
    backend: &B,
    enabled: bool,
    executable_path: &Path,
) -> Result<(), AutostartError> {
    if enabled {
        let command = format_run_command(executable_path)?;
        backend
            .write_value(&command)
            .map_err(AutostartError::Registry)
    } else {
        backend.delete_value().map_err(AutostartError::Registry)
    }
}

/// 隔离真实 Windows 注册表的最小后端合同。
trait RegistryBackend {
    fn read_value(&self) -> Result<Option<String>, String>;
    fn write_value(&self, command: &str) -> Result<(), String>;
    fn delete_value(&self) -> Result<(), String>;
}

/// 查询当前所有用户的系统级自启动状态。
pub fn query_machine_autostart() -> Result<MachineAutostartState, AutostartError> {
    #[cfg(windows)]
    {
        windows::query()
    }
    #[cfg(not(windows))]
    {
        Err(AutostartError::UnsupportedPlatform)
    }
}

/// 请求 UAC 提权并修改所有用户共享的系统级自启动状态。
pub fn request_machine_autostart_change(enabled: bool) -> Result<(), AutostartError> {
    #[cfg(windows)]
    {
        windows::request_change(enabled)
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err(AutostartError::UnsupportedPlatform)
    }
}

/// 在日志、窗口和 GPU 初始化前执行受控辅助模式。
pub fn run_helper_if_requested(arguments: &[OsString]) -> Option<Result<(), AutostartError>> {
    let enabled = match parse_helper_mode(arguments) {
        Ok(Some(enabled)) => enabled,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    #[cfg(windows)]
    {
        Some(windows::run_helper(enabled))
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Some(Err(AutostartError::UnsupportedPlatform))
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, ffi::OsString, path::Path};

    use super::{
        AutostartError, MachineAutostartState, RegistryBackend, change_with_backend,
        format_run_command, parse_helper_mode, parse_registered_command, query_with_backend,
    };

    #[derive(Default)]
    struct MemoryRegistry {
        value: RefCell<Option<String>>,
    }

    impl RegistryBackend for MemoryRegistry {
        fn read_value(&self) -> Result<Option<String>, String> {
            Ok(self.value.borrow().clone())
        }

        fn write_value(&self, command: &str) -> Result<(), String> {
            *self.value.borrow_mut() = Some(command.to_owned());
            Ok(())
        }

        fn delete_value(&self) -> Result<(), String> {
            self.value.borrow_mut().take();
            Ok(())
        }
    }

    /// 验证带空格和中文的绝对路径使用完整引号保存。
    #[test]
    fn run_command_quotes_unicode_path() {
        let path = Path::new(r"C:\Program Files\Steady Ink\中文\steady-ink.exe");
        assert_eq!(
            format_run_command(path).expect("absolute path should be accepted"),
            r#""C:\Program Files\Steady Ink\中文\steady-ink.exe""#
        );
        assert_eq!(
            parse_registered_command(
                r#""C:\Program Files\Steady Ink\中文\steady-ink.exe" --machine-autostart"#
            ),
            Some(path.into())
        );
    }

    /// 验证注册表值的路径比较不受大小写和斜杠方向影响。
    #[test]
    fn matching_path_is_enabled() {
        let registry = MemoryRegistry {
            value: RefCell::new(Some(
                r#""c:/PROGRAM FILES/STEADY INK/steady-ink.exe""#.to_owned(),
            )),
        };
        let state = query_with_backend(
            &registry,
            Path::new(r"C:\Program Files\Steady Ink\steady-ink.exe"),
        )
        .expect("memory query should succeed");
        assert_eq!(state, MachineAutostartState::Enabled);
    }

    /// 验证值存在但路径异常时仍显示已启用并提供可修复状态。
    #[test]
    fn mismatched_path_remains_enabled_for_repair() {
        let registry = MemoryRegistry {
            value: RefCell::new(Some(r#""C:\Old Steady Ink\steady-ink.exe""#.to_owned())),
        };
        let state = query_with_backend(
            &registry,
            Path::new(r"C:\Program Files\Steady Ink\steady-ink.exe"),
        )
        .expect("memory query should succeed");
        assert_eq!(state, MachineAutostartState::EnabledPathMismatch);
        assert!(state.enabled());
    }

    /// 验证启用、重复启用和关闭都通过同一个后端合同完成。
    #[test]
    fn memory_backend_change_is_idempotent() {
        let registry = MemoryRegistry::default();
        let executable = Path::new(r"C:\Program Files\Steady Ink\steady-ink.exe");
        change_with_backend(&registry, false, executable)
            .expect("disabling an absent value should succeed");
        change_with_backend(&registry, true, executable).expect("enable should succeed");
        change_with_backend(&registry, true, executable).expect("repeat enable should succeed");
        assert_eq!(
            query_with_backend(&registry, executable).expect("query should succeed"),
            MachineAutostartState::Enabled
        );
        change_with_backend(&registry, false, executable).expect("disable should succeed");
        assert_eq!(
            query_with_backend(&registry, executable).expect("query should succeed"),
            MachineAutostartState::Disabled
        );
    }

    /// 验证只允许固定的辅助模式参数，普通参数不会短路正常启动。
    #[test]
    fn helper_arguments_are_strictly_whitelisted() {
        let ordinary = vec![OsString::from("steady-ink.exe")];
        assert_eq!(
            parse_helper_mode(&ordinary).expect("ordinary args are valid"),
            None
        );

        let enable = vec![
            OsString::from("steady-ink.exe"),
            OsString::from("--machine-autostart"),
            OsString::from("enable"),
        ];
        assert_eq!(
            parse_helper_mode(&enable).expect("enable args are valid"),
            Some(true)
        );

        let invalid = vec![
            OsString::from("steady-ink.exe"),
            OsString::from("--machine-autostart"),
            OsString::from("enable"),
            OsString::from("extra"),
        ];
        assert!(matches!(
            parse_helper_mode(&invalid),
            Err(AutostartError::InvalidHelperArguments)
        ));
    }
}
