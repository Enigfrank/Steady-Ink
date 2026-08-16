use std::{cell::Cell, sync::Arc};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::{
    Win32::{
        Foundation::{HWND, POINT},
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
            Gdi::ClientToScreen,
        },
        UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_NOZORDER, SetWindowLongW, SetWindowPos, WS_EX_APPWINDOW, WS_EX_LAYERED,
            WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
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

/// 事件线程提取并交给渲染线程的 Win32 合成目标快照。
#[derive(Debug, Clone, Copy)]
pub struct D3DRenderTarget {
    hwnd: isize,
    initial_size: PhysicalSize<u32>,
}

impl D3DRenderTarget {
    /// 创建一个只包含 HWND 整数快照和初始客户区尺寸的渲染目标。
    pub(crate) const fn new(hwnd: isize, initial_size: PhysicalSize<u32>) -> Self {
        Self { hwnd, initial_size }
    }

    /// 将可跨线程传递的整数句柄恢复为 Windows API 使用的 HWND。
    pub(crate) fn hwnd(self) -> HWND {
        HWND(self.hwnd as *mut std::ffi::c_void)
    }

    /// 返回可交给 winit owner-window 属性的原始 HWND 整数。
    pub(crate) const fn raw_hwnd(self) -> isize {
        self.hwnd
    }
}

/// 单显示器窗口在悬浮和全屏批注模式下的物理几何。
#[derive(Debug, Clone, Copy)]
struct WindowGeometry {
    idle_left_position: PhysicalPosition<i32>,
    idle_position: PhysicalPosition<i32>,
    idle_size: PhysicalSize<u32>,
    quick_settings_size: PhysicalSize<u32>,
    settings_position: PhysicalPosition<i32>,
    settings_size: PhysicalSize<u32>,
    annotation_position: PhysicalPosition<i32>,
    annotation_size: PhysicalSize<u32>,
}

/// 一次原生窗口操作需要提交的最终物理位置和客户区尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowPlacement {
    pub(crate) position: PhysicalPosition<i32>,
    pub(crate) size: PhysicalSize<u32>,
}

impl WindowPlacement {
    /// 返回 HWND 从当前几何移动到目标几何时保持旧 visual 屏幕位置所需的反向偏移。
    pub(crate) fn visual_offset_to(self, target: Self) -> PhysicalPosition<i32> {
        PhysicalPosition::new(
            self.position.x - target.position.x,
            self.position.y - target.position.y,
        )
    }
}

impl WindowGeometry {
    /// 根据主显示器和 DPI 缩放计算各窗口模式的稳定物理几何。
    fn from_monitor(monitor: &MonitorHandle) -> Self {
        Self::from_monitor_metrics(monitor.position(), monitor.size(), monitor.scale_factor())
    }

    /// 根据显示器物理边界和缩放因子计算各窗口模式的稳定物理几何。
    fn from_monitor_metrics(
        monitor_position: PhysicalPosition<i32>,
        monitor_size: PhysicalSize<u32>,
        scale_factor: f64,
    ) -> Self {
        let idle_size = LogicalSize::new(IDLE_WIDTH_POINTS, IDLE_HEIGHT_POINTS)
            .to_physical::<u32>(scale_factor);
        let quick_settings_size =
            LogicalSize::new(QUICK_SETTINGS_WIDTH_POINTS, QUICK_SETTINGS_HEIGHT_POINTS)
                .to_physical::<u32>(scale_factor);
        let settings_size = LogicalSize::new(SETTINGS_WIDTH_POINTS, SETTINGS_HEIGHT_POINTS)
            .to_physical::<u32>(scale_factor);
        let edge_margin = (EDGE_MARGIN_POINTS * scale_factor).round() as i32;
        let idle_left_position =
            left_centered_position(monitor_position, monitor_size, idle_size, edge_margin);
        let idle_position =
            right_centered_position(monitor_position, monitor_size, idle_size, edge_margin);
        let settings_position = centered_position(monitor_position, monitor_size, settings_size);
        Self {
            idle_left_position,
            idle_position,
            idle_size,
            quick_settings_size,
            settings_position,
            settings_size,
            annotation_position: monitor_position,
            annotation_size: monitor_size,
        }
    }

    /// 返回指定非批注视图、吸附边和目标纵坐标对应的最终几何。
    fn idle_placement(
        self,
        view: IdleWindowView,
        dock_side: DockSide,
        floating_top: i32,
    ) -> WindowPlacement {
        let size = self.idle_size(view);
        let base_position = match (view, dock_side) {
            (IdleWindowView::Toolbar, DockSide::Left) => self.idle_left_position,
            (IdleWindowView::Toolbar, DockSide::Right) => self.idle_position,
            (IdleWindowView::QuickSettings, DockSide::Left) => self.idle_left_position,
            (IdleWindowView::QuickSettings, DockSide::Right) => PhysicalPosition::new(
                self.idle_position.x + self.idle_size.width as i32 - size.width as i32,
                self.idle_position.y,
            ),
            (IdleWindowView::Settings, _) => self.settings_position,
        };
        let position = if view == IdleWindowView::Settings {
            base_position
        } else {
            PhysicalPosition::new(
                base_position.x,
                clamp_window_top(
                    floating_top,
                    self.annotation_position,
                    self.annotation_size,
                    size,
                ),
            )
        };
        WindowPlacement { position, size }
    }

    /// 返回全屏批注使用的主显示器最终几何。
    fn annotation_placement(self) -> WindowPlacement {
        WindowPlacement {
            position: self.annotation_position,
            size: self.annotation_size,
        }
    }

    /// 返回指定非批注窗口视图的固定物理尺寸。
    const fn idle_size(self, view: IdleWindowView) -> PhysicalSize<u32> {
        match view {
            IdleWindowView::Toolbar => self.idle_size,
            IdleWindowView::QuickSettings => self.quick_settings_size,
            IdleWindowView::Settings => self.settings_size,
        }
    }
}

/// 持有事件线程使用的透明 winit 窗口和窗口几何状态。
pub struct D3DWindowContext {
    window: Arc<Window>,
    render_target: D3DRenderTarget,
    geometry: WindowGeometry,
    dock_side: Cell<DockSide>,
    floating_top: Cell<i32>,
    idle_size_correction_pending: Cell<bool>,
    slideshow_mouse_mode: Cell<bool>,
}

/// 持有渲染线程独占的 D3D12 设备和 DirectComposition 交换链。
pub struct D3DRenderContext {
    diagnostics: GraphicsDiagnostics,
    adapter: IDXGIAdapter1,
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    factory: IDXGIFactory4,
    swap_chain: IDXGISwapChain3,
    swap_chain_size: PhysicalSize<u32>,
    composition_device: IDCompositionDevice,
    _composition_target: IDCompositionTarget,
    composition_visual: IDCompositionVisual,
    visual_content_pending: bool,
    visual_offset_reset_armed: bool,
}

impl D3DWindowContext {
    /// 在事件线程创建无边框置顶窗口，GPU 后端稍后由渲染线程初始化。
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
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| AppError::Graphics(format!("窗口创建失败: {error}")))?,
        );

        // 先把创建期默认尺寸压回目标值，再重放最小尺寸约束，避免异步窗口状态恢复到系统默认宽度。
        window.set_outer_position(geometry.idle_position);
        let _ = window.request_inner_size(geometry.idle_size);
        window.set_min_inner_size(Some(geometry.idle_size));
        let _ = window.request_inner_size(geometry.idle_size);
        let hwnd = window_hwnd(&window)?;
        let render_target = D3DRenderTarget::new(hwnd.0 as isize, window.inner_size());

        Ok(Self {
            window,
            render_target,
            geometry,
            dock_side: Cell::new(DockSide::Right),
            floating_top: Cell::new(geometry.idle_position.y),
            idle_size_correction_pending: Cell::new(true),
            slideshow_mouse_mode: Cell::new(false),
        })
    }

    /// 返回 winit 窗口引用。
    pub fn window(&self) -> &Window {
        self.window.as_ref()
    }

    /// 返回在事件线程提取的 Win32 合成目标，避免跨线程调用 winit 句柄 API。
    pub const fn render_target(&self) -> D3DRenderTarget {
        self.render_target
    }

    /// 设置放映鼠标模式下主画布是否整体穿透到底层窗口。
    pub(crate) fn set_slideshow_mouse_mode(&self, enabled: bool) -> Result<(), AppError> {
        if self.slideshow_mouse_mode.get() == enabled {
            return Ok(());
        }

        match self.apply_slideshow_cursor_hittest(enabled) {
            Ok(style) => {
                self.slideshow_mouse_mode.set(enabled);
                log_slideshow_cursor_hittest_style(enabled, style);
                Ok(())
            }
            Err(error) => match self.apply_slideshow_cursor_hittest(!enabled) {
                Ok(_) => Err(error),
                Err(rollback_error) => Err(AppError::Graphics(format!(
                    "{error}; 恢复放映画布上一输入模式失败: {rollback_error}"
                ))),
            },
        }
    }

    /// 通过 winit 的窗口状态机切换命中行为，并复核 Win32 最终样式。
    fn apply_slideshow_cursor_hittest(&self, enabled: bool) -> Result<i32, AppError> {
        self.window
            .set_cursor_hittest(!enabled)
            .map_err(|error| AppError::Graphics(format!("切换放映画布光标命中失败: {error}")))?;
        apply_tool_window_style(self.render_target.hwnd())?;

        let style = unsafe { GetWindowLongW(self.render_target.hwnd(), GWL_EXSTYLE) };
        if !slideshow_cursor_hittest_style_matches(enabled, style) {
            return Err(AppError::Graphics(format!(
                "放映画布光标命中样式复核失败: enabled={enabled}, actual={style:#010x}"
            )));
        }
        Ok(style)
    }

    /// 以运行时选定的稳定几何显示窗口，并从 Windows shell 隐藏该工具窗口。
    pub(crate) fn show(&self, placement: WindowPlacement) -> Result<(), AppError> {
        self.window
            .set_min_inner_size(Some(self.geometry.idle_size));
        self.apply_window_placement_inner(placement)?;
        self.window.set_visible(true);
        apply_tool_window_style(self.render_target.hwnd())?;
        self.window.set_skip_taskbar(true);
        Ok(())
    }
}

impl D3DRenderContext {
    /// 在调用线程创建 D3D12、swap chain 和 DirectComposition visual tree。
    pub fn new(target: D3DRenderTarget) -> Result<Self, AppError> {
        let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory1() }
            .map_err(|error| graphics_error("无法创建 DXGI factory", error))?;
        let (adapter, device, software_fallback) = create_device(&factory)?;
        let queue = unsafe { device.CreateCommandQueue(&Default::default()) }
            .map_err(|error| graphics_error("无法创建 D3D12 command queue", error))?;
        let size = target.initial_size;
        let swap_chain = create_swap_chain(&factory, &queue, size)?;
        let (composition_device, composition_target, composition_visual) =
            attach_direct_composition(target.hwnd(), &swap_chain)?;
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
            diagnostics,
            adapter,
            device,
            queue,
            factory,
            swap_chain,
            swap_chain_size: size,
            composition_device,
            _composition_target: composition_target,
            composition_visual,
            visual_content_pending: false,
            visual_offset_reset_armed: false,
        })
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

    /// 为新窗口尺寸准备交换链，等首帧呈现成功后再替换 visual content。
    pub fn recreate_swap_chain(&mut self, size: PhysicalSize<u32>) -> Result<(), AppError> {
        let swap_chain = create_swap_chain(&self.factory, &self.queue, size)?;
        self.swap_chain = swap_chain;
        self.swap_chain_size = PhysicalSize::new(size.width.max(1), size.height.max(1));
        self.visual_content_pending = true;
        Ok(())
    }

    /// 为 idle 模式重建完整 D3D12 设备栈，使旧设备资源可以在驱动稳定期释放。
    pub fn recreate_graphics_device(&mut self, size: PhysicalSize<u32>) -> Result<(), AppError> {
        let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory1() }
            .map_err(|error| graphics_error("无法重建 DXGI factory", error))?;
        let (adapter, device, software_fallback) = create_device(&factory)?;
        let queue = unsafe { device.CreateCommandQueue(&Default::default()) }
            .map_err(|error| graphics_error("无法重建 D3D12 command queue", error))?;
        let swap_chain = create_swap_chain(&factory, &queue, size)?;
        let diagnostics = read_graphics_diagnostics(&adapter, software_fallback)?;

        self.factory = factory;
        self.adapter = adapter;
        self.device = device;
        self.queue = queue;
        self.swap_chain = swap_chain;
        self.swap_chain_size = PhysicalSize::new(size.width.max(1), size.height.max(1));
        self.diagnostics = diagnostics;
        self.visual_content_pending = true;
        Ok(())
    }

    /// 提交旧 visual 的临时偏移，使 HWND 改位后旧画面仍停留在原屏幕位置。
    /// 不等待合成完成：窗口已在同一事件批次内先移动到目标位置，本 Commit 与窗口位置
    /// 会落在同一个 DWM 合成周期内原子生效，避免 offset 先生效而窗口未动产生的闪现帧。
    pub fn hold_visual_offset(&mut self, offset: PhysicalPosition<i32>) -> Result<(), AppError> {
        self.commit_visual_offset_async(offset, "无法提交 DirectComposition 旧画面冻结")?;
        self.visual_offset_reset_armed = false;
        Ok(())
    }

    /// 标记目标首帧呈现时需要在同一次 composition commit 中清除临时偏移。
    pub const fn arm_visual_offset_reset(&mut self) {
        self.visual_offset_reset_armed = true;
    }

    /// 提交当前 back buffer，并原子替换新内容及清除旧 visual 的临时偏移。
    pub fn present(&mut self) -> Result<(), AppError> {
        unsafe { self.swap_chain.Present(1, DXGI_PRESENT::default()) }
            .ok()
            .map_err(|error| graphics_error("DXGI Present 失败", error))?;

        if self.visual_content_pending {
            unsafe { self.composition_visual.SetContent(&self.swap_chain) }.map_err(|error| {
                graphics_error("无法替换已完成首帧的 DirectComposition swap chain", error)
            })?;
        }
        if self.visual_offset_reset_armed {
            unsafe { self.composition_visual.SetOffsetX2(0.0) }.map_err(|error| {
                graphics_error("无法清除 DirectComposition visual 横向偏移", error)
            })?;
            unsafe { self.composition_visual.SetOffsetY2(0.0) }.map_err(|error| {
                graphics_error("无法清除 DirectComposition visual 纵向偏移", error)
            })?;
        }
        if self.visual_content_pending || self.visual_offset_reset_armed {
            unsafe { self.composition_device.Commit() }
                .map_err(|error| graphics_error("无法提交 DirectComposition 目标首帧", error))?;
            self.visual_content_pending = false;
            self.visual_offset_reset_armed = false;
        }
        Ok(())
    }

    /// 设置并提交 DirectComposition visual 的物理像素偏移，不等待合成完成。
    fn commit_visual_offset_async(
        &self,
        offset: PhysicalPosition<i32>,
        context: &str,
    ) -> Result<(), AppError> {
        unsafe { self.composition_visual.SetOffsetX2(offset.x as f32) }
            .map_err(|error| graphics_error("无法设置 DirectComposition visual 横向偏移", error))?;
        unsafe { self.composition_visual.SetOffsetY2(offset.y as f32) }
            .map_err(|error| graphics_error("无法设置 DirectComposition visual 纵向偏移", error))?;
        unsafe { self.composition_device.Commit() }.map_err(|error| graphics_error(context, error))
    }

    /// 返回一份供事件线程展示的图形设备诊断快照。
    pub fn diagnostics_snapshot(&self) -> GraphicsDiagnostics {
        self.diagnostics.clone()
    }
}

impl D3DWindowContext {
    /// 返回窗口当前实际物理位置和客户区尺寸。
    pub(crate) fn current_placement(&self) -> Result<WindowPlacement, AppError> {
        let position = self
            .window
            .outer_position()
            .map_err(|error| AppError::Graphics(format!("无法读取当前窗口位置: {error}")))?;
        Ok(WindowPlacement {
            position,
            size: self.window.inner_size(),
        })
    }

    /// 返回主显示器全屏批注或悬浮工具栏的稳定目标几何。
    pub(crate) fn target_annotation_placement(&self, annotation_enabled: bool) -> WindowPlacement {
        if annotation_enabled {
            self.geometry.annotation_placement()
        } else {
            self.idle_placement(IdleWindowView::Toolbar)
        }
    }

    /// 返回非批注模式指定视图的稳定目标几何。
    pub(crate) fn target_idle_placement(&self, view: IdleWindowView) -> WindowPlacement {
        self.idle_placement(view)
    }

    /// 在创建期异步 Win32 消息把窄窗恢复到系统最小宽度后重新请求目标尺寸。
    pub fn correct_idle_size(&self, view: IdleWindowView, actual_size: PhysicalSize<u32>) -> bool {
        if !self.idle_size_correction_pending.replace(false) {
            return false;
        }
        let expected_size = self.geometry.idle_size(view);
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

    /// 将 winit 客户区物理坐标转换为当前 HWND 的屏幕物理坐标。
    pub(crate) fn client_to_screen(
        &self,
        position: PhysicalPosition<f64>,
    ) -> Result<PhysicalPosition<i32>, AppError> {
        let mut point = POINT {
            x: position.x.round() as i32,
            y: position.y.round() as i32,
        };
        // SAFETY: HWND 属于当前事件线程，POINT 指向本栈帧内的可写坐标。
        if !unsafe { ClientToScreen(self.render_target.hwnd(), &mut point) }.as_bool() {
            return Err(AppError::Graphics(
                "无法把工具栏触摸客户区坐标转换为屏幕坐标".to_owned(),
            ));
        }
        Ok(PhysicalPosition::new(point.x, point.y))
    }

    /// 返回手动触摸拖动开始时窗口的物理 outer 位置。
    pub(crate) fn outer_position(&self) -> Result<PhysicalPosition<i32>, AppError> {
        self.window
            .outer_position()
            .map_err(|error| AppError::Graphics(format!("无法读取当前窗口位置: {error}")))
    }

    /// 把非批注窗口移动到手动触摸状态机计算出的物理 outer 位置。
    pub(crate) fn set_outer_position(&self, position: PhysicalPosition<i32>) {
        self.window.set_outer_position(position);
    }

    /// 根据窗口中心吸附到主显示器左侧或右侧，并返回新的边缘。
    pub fn finish_idle_window_drag(
        &self,
        view: IdleWindowView,
        manual_touch_position: Option<PhysicalPosition<i32>>,
    ) -> Result<DockSide, AppError> {
        let window_position = manual_touch_position.unwrap_or_else(|| {
            self.window
                .outer_position()
                .unwrap_or_else(|_| self.idle_placement(view).position)
        });
        let window_size = self.window.inner_size();
        let monitor_center = i64::from(self.geometry.annotation_position.x)
            + i64::from(self.geometry.annotation_size.width) / 2;
        let window_center = i64::from(window_position.x) + i64::from(window_size.width) / 2;
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
        self.apply_window_placement(self.idle_placement(view))?;
        Ok(side)
    }

    /// 返回当前悬浮工具栏吸附边缘。
    pub fn dock_side(&self) -> DockSide {
        self.dock_side.get()
    }

    /// 更新全屏普通批注工具栏选择的吸附边缘。
    pub fn set_dock_side(&self, side: DockSide) {
        self.dock_side.set(side);
    }

    /// 返回指定非批注窗口视图在当前吸附状态下的最终几何。
    fn idle_placement(&self, view: IdleWindowView) -> WindowPlacement {
        self.geometry
            .idle_placement(view, self.dock_side.get(), self.floating_top.get())
    }

    /// 通过一次 Win32 调用同时提交窗口位置和尺寸，避免暴露中间几何。
    pub(crate) fn apply_window_placement(
        &self,
        placement: WindowPlacement,
    ) -> Result<(), AppError> {
        self.idle_size_correction_pending.set(false);
        self.apply_window_placement_inner(placement)
    }

    /// 通过一次 Win32 调用提交位置和尺寸，不改变创建期尺寸纠正状态。
    fn apply_window_placement_inner(&self, placement: WindowPlacement) -> Result<(), AppError> {
        let width = i32::try_from(placement.size.width).map_err(|_| {
            AppError::Graphics(format!(
                "窗口目标宽度超出 Win32 范围: {}",
                placement.size.width
            ))
        })?;
        let height = i32::try_from(placement.size.height).map_err(|_| {
            AppError::Graphics(format!(
                "窗口目标高度超出 Win32 范围: {}",
                placement.size.height
            ))
        })?;
        unsafe {
            SetWindowPos(
                self.render_target.hwnd(),
                None,
                placement.position.x,
                placement.position.y,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        }
        .map_err(|error| {
            graphics_error(
                &format!(
                    "无法更新窗口几何 x={} y={} width={} height={}",
                    placement.position.x,
                    placement.position.y,
                    placement.size.width,
                    placement.size.height
                ),
                error,
            )
        })
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
fn apply_tool_window_style(hwnd: HWND) -> Result<(), AppError> {
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

/// 返回 winit 光标命中切换后的扩展样式是否满足当前放映输入模式。
const fn slideshow_cursor_hittest_style_matches(enabled: bool, style: i32) -> bool {
    let layered = style & WS_EX_LAYERED.0 as i32 != 0;
    let transparent = style & WS_EX_TRANSPARENT.0 as i32 != 0;
    let no_redirection_bitmap = style & WS_EX_NOREDIRECTIONBITMAP.0 as i32 != 0;
    no_redirection_bitmap
        && if enabled {
            layered && transparent
        } else {
            !layered && !transparent
        }
}

/// 记录 winit 和 Win32 最终一致的放映画布命中样式。
fn log_slideshow_cursor_hittest_style(enabled: bool, style: i32) {
    tracing::info!(
        enabled,
        extended_style = style,
        layered = style & WS_EX_LAYERED.0 as i32 != 0,
        transparent = style & WS_EX_TRANSPARENT.0 as i32 != 0,
        no_redirection_bitmap = style & WS_EX_NOREDIRECTIONBITMAP.0 as i32 != 0,
        "放映画布原生输入模式已切换"
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 winit Mouse 模式同时保留 DirectComposition 并启用 layered 透明命中。
    #[test]
    fn slideshow_mouse_hittest_requires_winit_window_flags() {
        let style = WS_EX_NOREDIRECTIONBITMAP.0 as i32
            | WS_EX_LAYERED.0 as i32
            | WS_EX_TRANSPARENT.0 as i32;

        assert!(slideshow_cursor_hittest_style_matches(true, style));
        assert!(!slideshow_cursor_hittest_style_matches(
            true,
            style & !(WS_EX_TRANSPARENT.0 as i32)
        ));
        assert!(!slideshow_cursor_hittest_style_matches(
            true,
            style & !(WS_EX_NOREDIRECTIONBITMAP.0 as i32)
        ));
    }

    /// 验证 Ink 模式移除 winit 添加的 layered 和透明命中样式。
    #[test]
    fn slideshow_ink_hittest_restores_direct_composition_style() {
        let style = WS_EX_NOREDIRECTIONBITMAP.0 as i32;

        assert!(slideshow_cursor_hittest_style_matches(false, style));
        assert!(!slideshow_cursor_hittest_style_matches(
            false,
            style | WS_EX_LAYERED.0 as i32
        ));
    }

    /// 创建不依赖原生显示器句柄的窗口几何测试样本。
    fn geometry_fixture() -> WindowGeometry {
        WindowGeometry {
            idle_left_position: PhysicalPosition::new(16, 441),
            idle_position: PhysicalPosition::new(1_834, 441),
            idle_size: PhysicalSize::new(70, 198),
            quick_settings_size: PhysicalSize::new(440, 336),
            settings_position: PhysicalPosition::new(680, 220),
            settings_size: PhysicalSize::new(560, 640),
            annotation_position: PhysicalPosition::new(0, 0),
            annotation_size: PhysicalSize::new(1_920, 1_080),
        }
    }

    /// 验证快捷设置复用工具栏锚点，并始终向屏幕内侧展开。
    #[test]
    fn quick_settings_expand_inward_from_toolbar_anchor() {
        let geometry = geometry_fixture();

        assert_eq!(
            geometry.idle_placement(IdleWindowView::QuickSettings, DockSide::Left, 300),
            WindowPlacement {
                position: PhysicalPosition::new(16, 300),
                size: PhysicalSize::new(440, 336),
            }
        );
        assert_eq!(
            geometry.idle_placement(IdleWindowView::QuickSettings, DockSide::Right, 300),
            WindowPlacement {
                position: PhysicalPosition::new(1_464, 300),
                size: PhysicalSize::new(440, 336),
            }
        );
    }

    /// 验证工具栏和快捷设置往返时保留纵向位置并按各自高度夹取。
    #[test]
    fn idle_view_round_trip_preserves_and_clamps_floating_top() {
        let geometry = geometry_fixture();
        let toolbar_before = geometry.idle_placement(IdleWindowView::Toolbar, DockSide::Left, 800);
        let quick_settings =
            geometry.idle_placement(IdleWindowView::QuickSettings, DockSide::Left, 800);
        let toolbar_after = geometry.idle_placement(IdleWindowView::Toolbar, DockSide::Left, 800);

        assert_eq!(toolbar_before.position.y, 800);
        assert_eq!(quick_settings.position.y, 744);
        assert_eq!(toolbar_after, toolbar_before);
        assert_eq!(
            geometry
                .idle_placement(IdleWindowView::QuickSettings, DockSide::Right, -100)
                .position
                .y,
            0
        );
    }

    /// 验证设置页保持居中且全屏批注完整复用显示器几何。
    #[test]
    fn fixed_views_use_stable_geometry() {
        let geometry = geometry_fixture();
        assert_eq!(
            geometry.idle_placement(IdleWindowView::Settings, DockSide::Left, -500),
            WindowPlacement {
                position: PhysicalPosition::new(680, 220),
                size: PhysicalSize::new(560, 640),
            }
        );
        assert_eq!(
            geometry.annotation_placement(),
            WindowPlacement {
                position: PhysicalPosition::new(0, 0),
                size: PhysicalSize::new(1_920, 1_080),
            }
        );
    }

    /// 验证 HWND 移到目标位置后，反向 visual 偏移会把旧画面固定在原屏幕坐标。
    #[test]
    fn visual_offset_preserves_source_screen_position() {
        let source = WindowPlacement {
            position: PhysicalPosition::new(1_834, 441),
            size: PhysicalSize::new(70, 198),
        };
        let target = WindowPlacement {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(1_920, 1_080),
        };

        let offset = source.visual_offset_to(target);

        assert_eq!(offset, PhysicalPosition::new(1_834, 441));
        assert_eq!(target.position.x + offset.x, source.position.x);
        assert_eq!(target.position.y + offset.y, source.position.y);
    }

    /// 验证三档目标 DPI 下全部非批注视图在左右吸附时保持完整可见。
    #[test]
    fn supported_dpi_placements_stay_inside_monitor() {
        let monitor_position = PhysicalPosition::new(100, -200);
        let monitor_size = PhysicalSize::new(3_840, 2_160);

        for scale_factor in [1.0, 1.5, 2.0] {
            let geometry =
                WindowGeometry::from_monitor_metrics(monitor_position, monitor_size, scale_factor);
            for view in [
                IdleWindowView::Toolbar,
                IdleWindowView::QuickSettings,
                IdleWindowView::Settings,
            ] {
                for side in [DockSide::Left, DockSide::Right] {
                    let placement = geometry.idle_placement(view, side, i32::MAX);
                    assert_placement_inside_monitor(placement, monitor_position, monitor_size);
                }
            }
        }
    }

    /// 断言一个窗口最终几何完整落在指定显示器物理边界内。
    fn assert_placement_inside_monitor(
        placement: WindowPlacement,
        monitor_position: PhysicalPosition<i32>,
        monitor_size: PhysicalSize<u32>,
    ) {
        assert!(placement.position.x >= monitor_position.x);
        assert!(placement.position.y >= monitor_position.y);
        assert!(
            placement.position.x + placement.size.width as i32
                <= monitor_position.x + monitor_size.width as i32
        );
        assert!(
            placement.position.y + placement.size.height as i32
                <= monitor_position.y + monitor_size.height as i32
        );
    }
}
