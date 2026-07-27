use std::cell::Cell;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::{
    Win32::{
        Foundation::HWND,
        Graphics::{
            Direct3D::D3D_FEATURE_LEVEL_11_0,
            Direct3D12::{D3D12CreateDevice, ID3D12CommandQueue, ID3D12Device},
            DirectComposition::{
                DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget,
                IDCompositionVisual,
            },
            Dxgi::{
                Common::{
                    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
                },
                CreateDXGIFactory1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_PRESENT,
                DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter1, IDXGIDevice, IDXGIFactory4,
                IDXGIOutput, IDXGISwapChain3,
            },
        },
        UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_NOZORDER, SetWindowLongW, SetWindowPos, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        },
    },
    core::Interface,
};
use winit::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event_loop::ActiveEventLoop,
    monitor::MonitorHandle,
    platform::windows::{WindowAttributesExtWindows, WindowExtWindows},
    window::{Icon, Window, WindowAttributes, WindowLevel},
};

use crate::{error::AppError, ui::design_tokens::scale_window_points};

pub(crate) const IDLE_WIDTH_POINTS: f64 = scale_window_points(88.0);
pub(crate) const IDLE_HEIGHT_POINTS: f64 = scale_window_points(248.0);
// 额外保留一个缩放后的基础网格，容纳双卡片边框和 egui 横向布局舍入。
pub(crate) const QUICK_SETTINGS_WIDTH_POINTS: f64 = scale_window_points(544.0 + 4.0);
pub(crate) const QUICK_SETTINGS_HEIGHT_POINTS: f64 = scale_window_points(420.0);
pub(crate) const SETTINGS_WIDTH_POINTS: f64 = 560.0;
pub(crate) const SETTINGS_HEIGHT_POINTS: f64 = 640.0;
const EDGE_MARGIN_POINTS: f64 = scale_window_points(16.0);
pub(crate) const SWAP_CHAIN_BUFFER_COUNT: usize = 2;

/// 非批注模式下窗口需要承载的可见界面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleWindowView {
    Toolbar,
    QuickSettings,
    Settings,
}

/// 悬浮工具栏当前吸附的主显示器边缘。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSide {
    Left,
    Right,
}

/// 启动时记录的 Direct3D 图形设备信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsDiagnostics {
    pub vendor: String,
    pub renderer: String,
    pub device_info: String,
    pub software_fallback: bool,
}

/// 单显示器窗口在悬浮和全屏批注模式下的物理几何。
#[derive(Debug, Clone, Copy)]
struct WindowGeometry {
    idle_left_position: PhysicalPosition<i32>,
    idle_position: PhysicalPosition<i32>,
    idle_size: PhysicalSize<u32>,
    quick_settings_left_position: PhysicalPosition<i32>,
    quick_settings_position: PhysicalPosition<i32>,
    quick_settings_size: PhysicalSize<u32>,
    settings_position: PhysicalPosition<i32>,
    settings_size: PhysicalSize<u32>,
    annotation_position: PhysicalPosition<i32>,
    annotation_size: PhysicalSize<u32>,
}

impl WindowGeometry {
    /// 根据主显示器和 DPI 缩放计算两个窗口模式的稳定物理尺寸。
    fn from_monitor(monitor: &MonitorHandle) -> Self {
        let scale_factor = monitor.scale_factor();
        let idle_size = LogicalSize::new(IDLE_WIDTH_POINTS, IDLE_HEIGHT_POINTS)
            .to_physical::<u32>(scale_factor);
        let quick_settings_size =
            LogicalSize::new(QUICK_SETTINGS_WIDTH_POINTS, QUICK_SETTINGS_HEIGHT_POINTS)
                .to_physical::<u32>(scale_factor);
        let settings_size = LogicalSize::new(SETTINGS_WIDTH_POINTS, SETTINGS_HEIGHT_POINTS)
            .to_physical::<u32>(scale_factor);
        let edge_margin = (EDGE_MARGIN_POINTS * scale_factor).round() as i32;
        let monitor_position = monitor.position();
        let monitor_size = monitor.size();
        let idle_left_position =
            left_centered_position(monitor_position, monitor_size, idle_size, edge_margin);
        let idle_position =
            right_centered_position(monitor_position, monitor_size, idle_size, edge_margin);
        let quick_settings_left_position = left_centered_position(
            monitor_position,
            monitor_size,
            quick_settings_size,
            edge_margin,
        );
        let quick_settings_position = right_centered_position(
            monitor_position,
            monitor_size,
            quick_settings_size,
            edge_margin,
        );
        let settings_position = centered_position(monitor_position, monitor_size, settings_size);
        Self {
            idle_left_position,
            idle_position,
            idle_size,
            quick_settings_left_position,
            quick_settings_position,
            quick_settings_size,
            settings_position,
            settings_size,
            annotation_position: monitor_position,
            annotation_size: monitor_size,
        }
    }
}

/// 持有透明 winit 窗口、D3D12 设备和 DirectComposition 交换链。
pub struct D3DWindowContext {
    window: Window,
    geometry: WindowGeometry,
    diagnostics: GraphicsDiagnostics,
    dock_side: Cell<DockSide>,
    floating_top: Cell<i32>,
    adapter: IDXGIAdapter1,
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    factory: IDXGIFactory4,
    swap_chain: IDXGISwapChain3,
    swap_chain_size: PhysicalSize<u32>,
    composition_device: IDCompositionDevice,
    _composition_target: IDCompositionTarget,
    composition_visual: IDCompositionVisual,
}

impl D3DWindowContext {
    /// 创建无边框置顶窗口，并把预乘 alpha DXGI 交换链接入 DirectComposition。
    pub fn new(event_loop: &ActiveEventLoop) -> Result<Self, AppError> {
        let monitor = event_loop
            .primary_monitor()
            .or_else(|| event_loop.available_monitors().next())
            .ok_or_else(|| AppError::Graphics("没有检测到可用显示器".to_owned()))?;
        let geometry = WindowGeometry::from_monitor(&monitor);
        let window_icon = Icon::from_rgba(
            include_bytes!(concat!(env!("OUT_DIR"), "/steady-ink-window.rgba")).to_vec(),
            256,
            256,
        )
        .map_err(|error| AppError::Graphics(format!("窗口图标资源无效: {error}")))?;
        let attributes = WindowAttributes::default()
            .with_title("Steady Ink")
            .with_inner_size(geometry.idle_size)
            .with_min_inner_size(geometry.idle_size)
            .with_position(geometry.idle_position)
            .with_resizable(false)
            .with_decorations(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_visible(false)
            .with_window_icon(Some(window_icon.clone()))
            .with_skip_taskbar(true)
            .with_taskbar_icon(Some(window_icon))
            .with_no_redirection_bitmap(true);
        let window = event_loop
            .create_window(attributes)
            .map_err(|error| AppError::Graphics(format!("窗口创建失败: {error}")))?;

        // 先把创建期默认尺寸压回目标值，再重放最小尺寸约束，避免异步窗口状态恢复到系统默认宽度。
        window.set_outer_position(geometry.idle_position);
        let _ = window.request_inner_size(geometry.idle_size);
        window.set_min_inner_size(Some(geometry.idle_size));
        let _ = window.request_inner_size(geometry.idle_size);

        let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory1() }
            .map_err(|error| graphics_error("无法创建 DXGI factory", error))?;
        let (adapter, device, software_fallback) = create_device(&factory)?;
        let queue = unsafe { device.CreateCommandQueue(&Default::default()) }
            .map_err(|error| graphics_error("无法创建 D3D12 command queue", error))?;
        let size = window.inner_size();
        let swap_chain = create_swap_chain(&factory, &queue, size)?;
        let hwnd = window_hwnd(&window)?;
        let (composition_device, composition_target, composition_visual) =
            attach_direct_composition(hwnd, &swap_chain)?;
        let diagnostics = read_graphics_diagnostics(&adapter, software_fallback)?;

        tracing::info!(
            vendor = diagnostics.vendor,
            renderer = diagnostics.renderer,
            device_info = diagnostics.device_info,
            software_fallback = diagnostics.software_fallback,
            alpha_mode = "premultiplied",
            swap_effect = "flip-sequential",
            "DirectComposition 呈现后端已创建"
        );
        if diagnostics.software_fallback {
            tracing::warn!("检测到 WARP 软件 D3D12，不能作为核显性能验收结果");
        }

        Ok(Self {
            window,
            geometry,
            diagnostics,
            dock_side: Cell::new(DockSide::Right),
            floating_top: Cell::new(geometry.idle_position.y),
            adapter,
            device,
            queue,
            factory,
            swap_chain,
            swap_chain_size: size,
            composition_device,
            _composition_target: composition_target,
            composition_visual,
        })
    }

    /// 返回 winit 窗口引用。
    pub const fn window(&self) -> &Window {
        &self.window
    }

    /// 显示已经连接 DirectComposition visual tree 的窗口，并从 Windows shell 隐藏该工具窗口。
    pub fn show(&self) -> Result<(), AppError> {
        self.window.set_visible(true);
        self.window
            .set_min_inner_size(Some(self.geometry.idle_size));
        self.window
            .set_outer_position(self.idle_position(IdleWindowView::Toolbar));
        let _ = self.window.request_inner_size(self.geometry.idle_size);
        apply_tool_window_style(&self.window)?;
        self.window.set_skip_taskbar(true);
        Ok(())
    }

    /// 返回 Skia D3D backend context 使用的 DXGI adapter。
    pub const fn adapter(&self) -> &IDXGIAdapter1 {
        &self.adapter
    }

    /// 返回 Skia D3D backend context 使用的 D3D12 device。
    pub const fn device(&self) -> &ID3D12Device {
        &self.device
    }

    /// 返回 Skia D3D backend context 使用的 D3D12 command queue。
    pub const fn queue(&self) -> &ID3D12CommandQueue {
        &self.queue
    }

    /// 返回启动时记录的图形设备诊断。
    pub const fn diagnostics(&self) -> &GraphicsDiagnostics {
        &self.diagnostics
    }

    /// 返回当前 DXGI back buffer 索引。
    pub fn current_back_buffer_index(&self) -> usize {
        unsafe { self.swap_chain.GetCurrentBackBufferIndex() as usize }
    }

    /// 返回当前 composition swap chain 的物理像素尺寸。
    pub const fn swap_chain_size(&self) -> PhysicalSize<u32> {
        self.swap_chain_size
    }

    /// 返回指定 DXGI back buffer 的 D3D12 resource。
    pub fn back_buffer(
        &self,
        index: usize,
    ) -> Result<windows::Win32::Graphics::Direct3D12::ID3D12Resource, AppError> {
        unsafe { self.swap_chain.GetBuffer(index as u32) }
            .map_err(|error| graphics_error("无法读取 DXGI back buffer", error))
    }

    /// 为新窗口尺寸创建交换链并原子替换 visual content，避免 ResizeBuffers 外部引用限制。
    pub fn recreate_swap_chain(&mut self, size: PhysicalSize<u32>) -> Result<(), AppError> {
        let swap_chain = create_swap_chain(&self.factory, &self.queue, size)?;
        unsafe { self.composition_visual.SetContent(&swap_chain) }
            .map_err(|error| graphics_error("无法替换 DirectComposition swap chain", error))?;
        unsafe { self.composition_device.Commit() }
            .map_err(|error| graphics_error("无法提交 DirectComposition 尺寸更新", error))
            .map(|()| {
                self.swap_chain = swap_chain;
                self.swap_chain_size = PhysicalSize::new(size.width.max(1), size.height.max(1));
            })
    }

    /// 提交当前 back buffer，透明像素由 DirectComposition 按预乘 alpha 合成。
    pub fn present(&self) -> Result<(), AppError> {
        unsafe { self.swap_chain.Present(1, DXGI_PRESENT::default()) }
            .ok()
            .map_err(|error| graphics_error("DXGI Present 失败", error))
    }

    /// 将窗口切换到主显示器全屏批注几何或悬浮工具栏几何。
    pub fn set_annotation_mode(&self, annotation_enabled: bool) {
        let (position, size) = if annotation_enabled {
            (
                self.geometry.annotation_position,
                self.geometry.annotation_size,
            )
        } else {
            (
                self.idle_position(IdleWindowView::Toolbar),
                self.geometry.idle_size,
            )
        };
        self.window.set_outer_position(position);
        let _ = self.window.request_inner_size(size);
        self.window.request_redraw();
    }

    /// 在非批注模式下切换紧凑工具栏、快捷设置或完整设置窗口几何。
    pub fn set_idle_window_view(&self, view: IdleWindowView) {
        let size = self.idle_size(view);
        let position = self.idle_position(view);
        self.window.set_outer_position(position);
        let _ = self.window.request_inner_size(size);
        self.window.request_redraw();
    }

    /// 在创建期异步 Win32 消息把窄窗恢复到系统最小宽度后重新请求目标尺寸。
    pub fn correct_idle_size(&self, view: IdleWindowView, actual_size: PhysicalSize<u32>) -> bool {
        let expected_size = self.idle_size(view);
        if actual_size == expected_size {
            return false;
        }
        let _ = self.window.request_inner_size(expected_size);
        true
    }

    /// 请求 Windows 进入原生窗口拖动循环。
    pub fn begin_window_drag(&self) {
        if let Err(error) = self.window.drag_window() {
            tracing::warn!(%error, "无法开始悬浮工具栏拖动");
        }
    }

    /// 根据窗口中心吸附到主显示器左侧或右侧，并返回新的边缘。
    pub fn finish_idle_window_drag(&self, view: IdleWindowView) -> DockSide {
        let window_position = self
            .window
            .outer_position()
            .unwrap_or_else(|_| self.idle_position(view));
        let window_size = self.window.inner_size();
        let monitor_center =
            self.geometry.annotation_position.x + self.geometry.annotation_size.width as i32 / 2;
        let window_center = window_position.x + window_size.width as i32 / 2;
        let side = if window_center < monitor_center {
            DockSide::Left
        } else {
            DockSide::Right
        };
        self.floating_top.set(clamp_window_top(
            window_position.y,
            self.geometry.annotation_position,
            self.geometry.annotation_size,
            window_size,
        ));
        self.dock_side.set(side);
        self.set_idle_window_view(view);
        side
    }

    /// 返回当前悬浮工具栏吸附边缘。
    pub fn dock_side(&self) -> DockSide {
        self.dock_side.get()
    }

    /// 更新全屏普通批注工具栏选择的吸附边缘。
    pub fn set_dock_side(&self, side: DockSide) {
        self.dock_side.set(side);
    }

    /// 返回指定非批注窗口视图在当前吸附边缘的位置。
    fn idle_position(&self, view: IdleWindowView) -> PhysicalPosition<i32> {
        let base_position = match (view, self.dock_side.get()) {
            (IdleWindowView::Toolbar, DockSide::Left) => self.geometry.idle_left_position,
            (IdleWindowView::Toolbar, DockSide::Right) => self.geometry.idle_position,
            (IdleWindowView::QuickSettings, DockSide::Left) => {
                self.geometry.quick_settings_left_position
            }
            (IdleWindowView::QuickSettings, DockSide::Right) => {
                self.geometry.quick_settings_position
            }
            (IdleWindowView::Settings, _) => self.geometry.settings_position,
        };
        if view == IdleWindowView::Settings {
            return base_position;
        }
        PhysicalPosition::new(
            base_position.x,
            clamp_window_top(
                self.floating_top.get(),
                self.geometry.annotation_position,
                self.geometry.annotation_size,
                self.idle_size(view),
            ),
        )
    }

    /// 返回指定非批注窗口视图的固定物理尺寸。
    const fn idle_size(&self, view: IdleWindowView) -> PhysicalSize<u32> {
        match view {
            IdleWindowView::Toolbar => self.geometry.idle_size,
            IdleWindowView::QuickSettings => self.geometry.quick_settings_size,
            IdleWindowView::Settings => self.geometry.settings_size,
        }
    }
}

/// 创建优先使用硬件 adapter、失败时明确标记 WARP 的 D3D12 device。
fn create_device(factory: &IDXGIFactory4) -> Result<(IDXGIAdapter1, ID3D12Device, bool), AppError> {
    for index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        let desc = unsafe { adapter.GetDesc1() }
            .map_err(|error| graphics_error("无法读取 DXGI adapter", error))?;
        if DXGI_ADAPTER_FLAG(desc.Flags as i32).contains(DXGI_ADAPTER_FLAG_SOFTWARE) {
            continue;
        }
        if let Some(device) = try_create_device(&adapter) {
            return Ok((adapter, device, false));
        }
    }

    let adapter: IDXGIAdapter1 = unsafe { factory.EnumWarpAdapter() }
        .map_err(|error| graphics_error("没有可用的 D3D12 hardware adapter 或 WARP", error))?;
    let device = try_create_device(&adapter)
        .ok_or_else(|| AppError::Graphics("WARP adapter 不支持所需 D3D12 功能级别".to_owned()))?;
    Ok((adapter, device, true))
}

/// 尝试在一个 DXGI adapter 上创建最低功能级别为 11_0 的 D3D12 device。
fn try_create_device(adapter: &IDXGIAdapter1) -> Option<ID3D12Device> {
    let mut device = None;
    unsafe { D3D12CreateDevice(adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }
        .is_ok()
        .then_some(device)
        .flatten()
}

/// 创建供 DirectComposition 使用的预乘 alpha flip-model swap chain。
fn create_swap_chain(
    factory: &IDXGIFactory4,
    queue: &ID3D12CommandQueue,
    size: PhysicalSize<u32>,
) -> Result<IDXGISwapChain3, AppError> {
    let description = DXGI_SWAP_CHAIN_DESC1 {
        Width: size.width.max(1),
        Height: size.height.max(1),
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: SWAP_CHAIN_BUFFER_COUNT as u32,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        Flags: 0,
    };
    let swap_chain = unsafe {
        factory.CreateSwapChainForComposition(queue, &raw const description, None::<&IDXGIOutput>)
    }
    .map_err(|error| graphics_error("无法创建 DXGI composition swap chain", error))?;
    swap_chain
        .cast()
        .map_err(|error| graphics_error("无法读取 IDXGISwapChain3", error))
}

/// 把 DXGI composition swap chain 设置为 HWND visual tree 的根内容。
fn attach_direct_composition(
    hwnd: HWND,
    swap_chain: &IDXGISwapChain3,
) -> Result<
    (
        IDCompositionDevice,
        IDCompositionTarget,
        IDCompositionVisual,
    ),
    AppError,
> {
    let device: IDCompositionDevice = unsafe { DCompositionCreateDevice(None::<&IDXGIDevice>) }
        .map_err(|error| graphics_error("无法创建 DirectComposition device", error))?;
    let target = unsafe { device.CreateTargetForHwnd(hwnd, true) }
        .map_err(|error| graphics_error("无法创建 DirectComposition HWND target", error))?;
    let visual = unsafe { device.CreateVisual() }
        .map_err(|error| graphics_error("无法创建 DirectComposition visual", error))?;
    unsafe { visual.SetContent(swap_chain) }
        .map_err(|error| graphics_error("无法绑定 DirectComposition swap chain", error))?;
    unsafe { target.SetRoot(&visual) }
        .map_err(|error| graphics_error("无法设置 DirectComposition root visual", error))?;
    unsafe { device.Commit() }
        .map_err(|error| graphics_error("无法提交 DirectComposition visual tree", error))?;
    Ok((device, target, visual))
}

/// 从 winit 窗口读取当前 Win32 HWND。
fn window_hwnd(window: &Window) -> Result<HWND, AppError> {
    let raw_handle = window
        .window_handle()
        .map_err(|error| AppError::Graphics(format!("窗口句柄读取失败: {error}")))?
        .as_raw();
    let RawWindowHandle::Win32(handle) = raw_handle else {
        return Err(AppError::Graphics(
            "DirectComposition 仅支持 Win32 HWND".to_owned(),
        ));
    };
    Ok(HWND(handle.hwnd.get() as *mut std::ffi::c_void))
}

/// 在窗口显示后应用工具窗口扩展样式，避免 shell 根据 `WS_EX_APPWINDOW` 重建任务栏按钮。
fn apply_tool_window_style(window: &Window) -> Result<(), AppError> {
    let hwnd = window_hwnd(window)?;
    let current_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) };
    let expected_style = tool_window_ex_style(current_style);
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
    .map_err(|error| graphics_error("无法刷新工具窗口扩展样式", error))?;

    let applied_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) };
    if applied_style & WS_EX_APPWINDOW.0 as i32 != 0
        || applied_style & WS_EX_TOOLWINDOW.0 as i32 == 0
    {
        return Err(AppError::Graphics(format!(
            "工具窗口扩展样式应用失败: 0x{applied_style:08X}"
        )));
    }
    Ok(())
}

/// 清除任务栏应用窗口标志，同时保留其他扩展样式并加入工具窗口标志。
const fn tool_window_ex_style(style: i32) -> i32 {
    (style & !(WS_EX_APPWINDOW.0 as i32)) | WS_EX_TOOLWINDOW.0 as i32
}

/// 读取 DXGI adapter 描述并生成设置页使用的图形诊断。
fn read_graphics_diagnostics(
    adapter: &IDXGIAdapter1,
    software_fallback: bool,
) -> Result<GraphicsDiagnostics, AppError> {
    let description = unsafe { adapter.GetDesc1() }
        .map_err(|error| graphics_error("无法读取 DXGI adapter 诊断", error))?;
    let name_length = description
        .Description
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(description.Description.len());
    Ok(GraphicsDiagnostics {
        vendor: format!("PCI vendor 0x{:04X}", description.VendorId),
        renderer: String::from_utf16_lossy(&description.Description[..name_length]),
        device_info: format!(
            "D3D12 / DirectComposition (device 0x{:04X})",
            description.DeviceId
        ),
        software_fallback,
    })
}

/// 为 Windows 图形 API 错误补充稳定的上下文描述。
fn graphics_error(context: &str, error: windows::core::Error) -> AppError {
    AppError::Graphics(format!("{context}: {error}"))
}

/// 计算一个窗口在主显示器右侧边缘内缩后的垂直居中位置。
fn right_centered_position(
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
    window_size: PhysicalSize<u32>,
    edge_margin: i32,
) -> PhysicalPosition<i32> {
    PhysicalPosition::new(
        monitor_position.x + monitor_size.width as i32 - window_size.width as i32 - edge_margin,
        monitor_position.y + (monitor_size.height as i32 - window_size.height as i32) / 2,
    )
}

/// 计算一个窗口在主显示器左侧边缘内缩后的垂直居中位置。
fn left_centered_position(
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
    window_size: PhysicalSize<u32>,
    edge_margin: i32,
) -> PhysicalPosition<i32> {
    PhysicalPosition::new(
        monitor_position.x + edge_margin,
        monitor_position.y + (monitor_size.height as i32 - window_size.height as i32) / 2,
    )
}

/// 计算一个窗口在主显示器客户区域中的居中位置。
fn centered_position(
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
    window_size: PhysicalSize<u32>,
) -> PhysicalPosition<i32> {
    PhysicalPosition::new(
        monitor_position.x + (monitor_size.width as i32 - window_size.width as i32) / 2,
        monitor_position.y + (monitor_size.height as i32 - window_size.height as i32) / 2,
    )
}

/// 保留悬浮窗口的目标纵坐标，并把窗口完整约束在主显示器范围内。
fn clamp_window_top(
    desired_top: i32,
    monitor_position: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
    window_size: PhysicalSize<u32>,
) -> i32 {
    let minimum = monitor_position.y;
    let maximum = monitor_position.y + monitor_size.height as i32 - window_size.height as i32;
    if minimum <= maximum {
        desired_top.clamp(minimum, maximum)
    } else {
        monitor_position.y + (monitor_size.height as i32 - window_size.height as i32) / 2
    }
}
