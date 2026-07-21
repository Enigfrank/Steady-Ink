use std::mem;

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WAIT_OBJECT_0},
        System::{
            Registry::{
                HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_SZ,
                REG_VALUE_TYPE, RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
                RegSetValueExW,
            },
            Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        },
        UI::{
            Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::{PCWSTR, w},
};

use super::{
    AUTOSTART_SUBKEY, AUTOSTART_VALUE_NAME, AutostartError, HELPER_SWITCH, RegistryBackend,
    change_with_backend, current_executable, helper_mode_argument, query_with_backend,
};

/// 生产环境使用的 64 位 HKLM Run 注册表后端。
pub(super) struct WindowsRegistryBackend;

impl RegistryBackend for WindowsRegistryBackend {
    /// 读取 Steady Ink 的 REG_SZ 值，不展开或执行其中的命令。
    fn read_value(&self) -> Result<Option<String>, String> {
        let Some(key) = open_run_key(KEY_READ | KEY_WOW64_64KEY)? else {
            return Ok(None);
        };
        let value_name = to_wide_string(AUTOSTART_VALUE_NAME);
        let mut value_type = REG_VALUE_TYPE::default();
        let mut byte_len = 0u32;
        let result = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(value_name.as_ptr()),
                None,
                Some(&mut value_type),
                None,
                Some(&mut byte_len),
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if result != ERROR_SUCCESS {
            return Err(registry_error("读取自启动值大小", result.0));
        }
        if value_type != REG_SZ {
            return Err(format!("读取自启动值类型不是 REG_SZ: {}", value_type.0));
        }

        let mut bytes = vec![0u8; byte_len as usize];
        let result = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(value_name.as_ptr()),
                None,
                Some(&mut value_type),
                Some(bytes.as_mut_ptr()),
                Some(&mut byte_len),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(registry_error("读取自启动值", result.0));
        }
        decode_registry_string(&bytes[..byte_len as usize]).map(Some)
    }

    /// 用 REG_SZ 写入完整带引号的可执行文件路径。
    fn write_value(&self, command: &str) -> Result<(), String> {
        let Some(key) = open_run_key(KEY_SET_VALUE | KEY_WOW64_64KEY)? else {
            return Err("系统级 Run 子键不存在，无法写入自启动值".to_owned());
        };
        let value_name = to_wide_string(AUTOSTART_VALUE_NAME);
        let units = command.encode_utf16().chain([0]).collect::<Vec<_>>();
        let bytes = units
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect::<Vec<_>>();
        let result = unsafe {
            RegSetValueExW(
                key.0,
                PCWSTR(value_name.as_ptr()),
                None,
                REG_SZ,
                Some(&bytes),
            )
        };
        if result == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(registry_error("写入自启动值", result.0))
        }
    }

    /// 只删除 Steady Ink 自己的值，不触碰 Run 子键中的其他项。
    fn delete_value(&self) -> Result<(), String> {
        let Some(key) = open_run_key(KEY_SET_VALUE | KEY_WOW64_64KEY)? else {
            return Ok(());
        };
        let value_name = to_wide_string(AUTOSTART_VALUE_NAME);
        let result = unsafe { RegDeleteValueW(key.0, PCWSTR(value_name.as_ptr())) };
        if result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(registry_error("删除自启动值", result.0))
        }
    }
}

/// 读取系统级自启动状态并与当前发布版路径比较。
pub(super) fn query() -> Result<super::MachineAutostartState, AutostartError> {
    let executable_path = current_executable()?;
    query_with_backend(&WindowsRegistryBackend, &executable_path)
}

/// 通过 runas 辅助进程请求一次系统级自启动变更。
pub(super) fn request_change(enabled: bool) -> Result<(), AutostartError> {
    let executable_path = current_executable()?;
    let executable = to_wide_string(&executable_path.to_string_lossy());
    let parameters = to_wide_string(&format!(
        "{HELPER_SWITCH} {}",
        helper_mode_argument(enabled)
    ));
    let mut execute_info = SHELLEXECUTEINFOW {
        cbSize: mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(executable.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    unsafe { ShellExecuteExW(&mut execute_info) }
        .map_err(|error| AutostartError::Elevation(error.to_string()))?;
    if execute_info.hProcess.is_invalid() {
        return Err(AutostartError::Elevation(
            "提权辅助进程没有返回有效句柄".to_owned(),
        ));
    }

    let wait_result = unsafe { WaitForSingleObject(execute_info.hProcess, INFINITE) };
    if wait_result != WAIT_OBJECT_0 {
        unsafe {
            let _ = CloseHandle(execute_info.hProcess);
        }
        return Err(AutostartError::Elevation(format!(
            "等待辅助进程失败: {:?}",
            wait_result
        )));
    }
    let mut exit_code = 1u32;
    let exit_result = unsafe { GetExitCodeProcess(execute_info.hProcess, &mut exit_code) };
    unsafe {
        let _ = CloseHandle(execute_info.hProcess);
    }
    exit_result.map_err(|error| AutostartError::Elevation(error.to_string()))?;
    if exit_code == 0 {
        Ok(())
    } else {
        Err(AutostartError::HelperExitCode(exit_code))
    }
}

/// 执行不带 UI 的辅助模式写入，供主入口在初始化前调用。
pub(super) fn run_helper(enabled: bool) -> Result<(), AutostartError> {
    let executable_path = current_executable()?;
    change_with_backend(&WindowsRegistryBackend, enabled, &executable_path)
}

/// 打开 64 位 HKLM Run 子键，并在离开作用域时关闭句柄；缺失子键返回 `None`。
fn open_run_key(
    access: windows::Win32::System::Registry::REG_SAM_FLAGS,
) -> Result<Option<RegistryKey>, String> {
    let mut key = HKEY::default();
    let subkey = to_wide_string(AUTOSTART_SUBKEY);
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            None,
            access,
            &mut key,
        )
    };
    if result == ERROR_SUCCESS {
        Ok(Some(RegistryKey(key)))
    } else if result == ERROR_FILE_NOT_FOUND {
        Ok(None)
    } else {
        Err(registry_error("打开系统级 Run 子键", result.0))
    }
}

/// 通过明确的 UTF-16 规则解码 REG_SZ 数据。
fn decode_registry_string(bytes: &[u8]) -> Result<String, String> {
    if !bytes.len().is_multiple_of(2) {
        return Err("注册表字符串字节数不是偶数".to_owned());
    }
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    String::from_utf16(&units[..end])
        .map_err(|error| format!("注册表字符串不是有效 UTF-16: {error}"))
}

/// 将路径或参数转换为带终止 NUL 的 Windows UTF-16 缓冲区。
fn to_wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

/// 统一格式化 Win32 错误码，方便设置页显示诊断。
fn registry_error(operation: &str, code: u32) -> String {
    format!("{operation}失败 (Win32 错误码 {code})")
}

/// 由 Drop 自动关闭原生注册表句柄。
struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
