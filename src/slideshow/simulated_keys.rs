use std::{ffi::c_void, mem::size_of};

use windows::Win32::{
    Foundation::HWND,
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
            KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_ESCAPE, VK_LEFT, VK_RIGHT,
        },
        WindowsAndMessaging::SetForegroundWindow,
    },
};

use super::{SlideShowControlAction, SlideShowKey};

/// 在 COM 检测已经确认的放映窗口上发送一次控制按键。
pub(crate) fn send_simulated_control_key(
    show_key: &SlideShowKey,
    action: SlideShowControlAction,
) -> Result<(), String> {
    if show_key.window_id == 0 {
        return Err("COM 未提供可用于按键兜底的放映窗口句柄".to_owned());
    }

    let window_value = usize::try_from(show_key.window_id)
        .map_err(|_| "COM 返回了无效的放映窗口句柄".to_owned())?;
    let window = HWND(window_value as *mut c_void);
    // SAFETY: HWND 来自当前 COM 放映窗口；调用只请求系统切换前台窗口。
    if !unsafe { SetForegroundWindow(window) }.as_bool() {
        return Err("无法把已确认的放映窗口切换到前台".to_owned());
    }

    let (virtual_key, extended) = simulated_key(action);
    let down_flags = if extended {
        KEYEVENTF_EXTENDEDKEY
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    let inputs = [
        keyboard_input(virtual_key, down_flags),
        keyboard_input(virtual_key, down_flags | KEYEVENTF_KEYUP),
    ];
    let input_size = i32::try_from(size_of::<INPUT>())
        .map_err(|error| format!("SendInput 结构尺寸无效: {error}"))?;
    // SAFETY: 输入数组和结构尺寸在调用期间保持有效。
    let sent = unsafe { SendInput(&inputs, input_size) };
    if sent == inputs.len() as u32 {
        Ok(())
    } else {
        Err(format!(
            "SendInput 仅发送了 {sent}/{} 个事件，可能被权限级别拦截",
            inputs.len()
        ))
    }
}

/// 构造一次虚拟键按下或抬起的 Windows INPUT。
fn keyboard_input(virtual_key: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// 返回每个放映动作使用的虚拟键和扩展键标记。
const fn simulated_key(action: SlideShowControlAction) -> (VIRTUAL_KEY, bool) {
    match action {
        SlideShowControlAction::Previous => (VK_LEFT, true),
        SlideShowControlAction::Next => (VK_RIGHT, true),
        SlideShowControlAction::Exit => (VK_ESCAPE, false),
    }
}
