use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{CombineRgn, CreateRectRgn, DeleteObject, RGN_OR, SetWindowRgn},
    UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SetWindowLongW, SetWindowPos, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
    },
};
use winit::{
    event_loop::ActiveEventLoop,
    platform::windows::{WindowAttributesExtWindows, WindowExtWindows},
    window::{Window, WindowAttributes, WindowId, WindowLevel},
};

use crate::error::AppError;

use super::{D3DRenderTarget, PhysicalHitRect, WindowPlacement};

/// 持有放映期间独立呈现并命中软件控件的原生窗口。
pub struct SlideshowUiWindow {
    window: Window,
    render_target: D3DRenderTarget,
    visible: bool,
}

impl SlideshowUiWindow {
    /// 创建与主画布同尺寸、由主 HWND 拥有且初始隐藏的控件窗口。
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        owner_hwnd: isize,
        placement: WindowPlacement,
    ) -> Result<Self, AppError> {
        let attributes = WindowAttributes::default()
            .with_title("Steady Ink Slideshow Controls")
            .with_inner_size(placement.size)
            .with_min_inner_size(placement.size)
            .with_position(placement.position)
            .with_resizable(false)
            .with_decorations(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_visible(false)
            .with_skip_taskbar(true)
            .with_no_redirection_bitmap(true)
            .with_owner_window(owner_hwnd);
        let window = event_loop
            .create_window(attributes)
            .map_err(|error| AppError::Graphics(format!("放映控件窗口创建失败: {error}")))?;
        window.set_outer_position(placement.position);
        let _ = window.request_inner_size(placement.size);
        window.set_min_inner_size(Some(placement.size));
        let hwnd = window_hwnd(&window)?;
        apply_control_window_style(hwnd)?;
        let render_target = D3DRenderTarget::new(hwnd.0 as isize, window.inner_size());

        Ok(Self {
            window,
            render_target,
            visible: false,
        })
    }

    /// 返回放映控件窗口引用，供 egui-winit 收集输入。
    pub(crate) const fn window(&self) -> &Window {
        &self.window
    }

    /// 返回事件循环用于分流的窗口标识。
    pub(crate) fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// 返回事件线程已提取的第二个 DirectComposition target。
    pub(crate) const fn render_target(&self) -> D3DRenderTarget {
        self.render_target
    }

    /// 用实际 UI 物理矩形替换窗口区域，并在首次有效区域发布后显示窗口。
    pub(crate) fn update_regions(&mut self, regions: &[PhysicalHitRect]) -> Result<(), AppError> {
        if regions.is_empty() {
            return self.hide();
        }
        apply_window_regions(self.render_target.hwnd(), regions)?;
        if !self.visible {
            self.window.set_visible(true);
            self.window.set_skip_taskbar(true);
            self.visible = true;
        }
        Ok(())
    }

    /// 隐藏并清空控件窗口区域，避免离开放映后残留命中。
    pub(crate) fn hide(&mut self) -> Result<(), AppError> {
        if !self.visible {
            return Ok(());
        }
        self.window.set_visible(false);
        self.visible = false;
        apply_window_regions(self.render_target.hwnd(), &[])
    }
}

/// 从 winit 窗口读取当前 Win32 HWND。
fn window_hwnd(window: &Window) -> Result<HWND, AppError> {
    let raw_handle = window
        .window_handle()
        .map_err(|error| AppError::Graphics(format!("放映控件窗口句柄读取失败: {error}")))?
        .as_raw();
    let RawWindowHandle::Win32(handle) = raw_handle else {
        return Err(AppError::Graphics(
            "放映控件窗口仅支持 Win32 HWND".to_owned(),
        ));
    };
    Ok(HWND(handle.hwnd.get() as *mut std::ffi::c_void))
}

/// 应用不激活、无任务栏按钮的工具窗口样式，保持演示程序前台焦点。
fn apply_control_window_style(hwnd: HWND) -> Result<(), AppError> {
    let current_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) };
    let expected_style = (current_style & !(WS_EX_APPWINDOW.0 as i32))
        | WS_EX_TOOLWINDOW.0 as i32
        | WS_EX_NOACTIVATE.0 as i32;
    unsafe { SetWindowLongW(hwnd, GWL_EXSTYLE, expected_style) };
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
        )
    }
    .map_err(|error| AppError::Graphics(format!("刷新放映控件窗口样式失败: {error}")))
}

/// 将若干客户区矩形合并为 HWND 区域；成功后区域句柄由系统接管。
fn apply_window_regions(hwnd: HWND, regions: &[PhysicalHitRect]) -> Result<(), AppError> {
    let combined = unsafe { CreateRectRgn(0, 0, 0, 0) };
    if combined.is_invalid() {
        return Err(AppError::Graphics("创建放映控件窗口区域失败".to_owned()));
    }

    for region in regions.iter().copied().filter(|region| region.is_valid()) {
        let part = unsafe { CreateRectRgn(region.min_x, region.min_y, region.max_x, region.max_y) };
        if part.is_invalid() {
            let _ = unsafe { DeleteObject(combined.into()) };
            return Err(AppError::Graphics("创建放映控件子区域失败".to_owned()));
        }
        let result = unsafe { CombineRgn(Some(combined), Some(combined), Some(part), RGN_OR) };
        let _ = unsafe { DeleteObject(part.into()) };
        if result.0 == 0 {
            let _ = unsafe { DeleteObject(combined.into()) };
            return Err(AppError::Graphics("合并放映控件窗口区域失败".to_owned()));
        }
    }

    if unsafe { SetWindowRgn(hwnd, Some(combined), true) } == 0 {
        let _ = unsafe { DeleteObject(combined.into()) };
        return Err(AppError::Graphics("提交放映控件窗口区域失败".to_owned()));
    }
    Ok(())
}
