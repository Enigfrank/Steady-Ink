use std::{cell::Cell, ffi::CStr, num::NonZeroU32, sync::Arc};

use egui_glow::glow;
use glutin::{
    config::{Config, ConfigTemplateBuilder, GlConfig},
    context::{ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext},
    display::{GetGlDisplay, GlDisplay},
    prelude::GlSurface,
    surface::{Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface},
};
use glutin_winit::DisplayBuilder;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event_loop::ActiveEventLoop,
    monitor::MonitorHandle,
    window::{Window, WindowAttributes, WindowLevel},
};

use crate::error::AppError;

pub(crate) const IDLE_WIDTH_POINTS: f64 = 88.0;
pub(crate) const IDLE_HEIGHT_POINTS: f64 = 248.0;
pub(crate) const QUICK_SETTINGS_WIDTH_POINTS: f64 = 544.0;
pub(crate) const QUICK_SETTINGS_HEIGHT_POINTS: f64 = 336.0;
const SETTINGS_WIDTH_POINTS: f64 = 560.0;
const SETTINGS_HEIGHT_POINTS: f64 = 640.0;
const EDGE_MARGIN_POINTS: f64 = 16.0;
const LAYERED_COLOR_KEY_RGBA: [u8; 4] = [1, 2, 3, 255];

/// Windows 窗口实际使用的透明合成后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransparencyBackend {
    DwmAlpha,
    LayeredColorKey,
}

impl TransparencyBackend {
    /// 返回 Skia 每帧清理窗口 framebuffer 时必须使用的 RGBA 颜色。
    const fn clear_rgba(self) -> [u8; 4] {
        match self {
            Self::DwmAlpha => [0, 0, 0, 0],
            Self::LayeredColorKey => LAYERED_COLOR_KEY_RGBA,
        }
    }
}

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

/// 启动时记录的 OpenGL 驱动信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlDiagnostics {
    pub vendor: String,
    pub renderer: String,
    pub version: String,
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

/// 持有透明 winit 窗口、WGL 上下文和交换 surface。
pub struct GlWindowContext {
    gl: Arc<glow::Context>,
    gl_surface: Surface<WindowSurface>,
    gl_context: PossiblyCurrentContext,
    gl_display: glutin::display::Display,
    window: Window,
    geometry: WindowGeometry,
    diagnostics: GlDiagnostics,
    dock_side: Cell<DockSide>,
    num_samples: usize,
    stencil_size: usize,
    transparency_backend: TransparencyBackend,
}

impl GlWindowContext {
    /// 创建透明、无边框、置顶的右侧悬浮窗口和共享 OpenGL 上下文。
    pub fn new(event_loop: &ActiveEventLoop) -> Result<Self, AppError> {
        let monitor = event_loop
            .primary_monitor()
            .or_else(|| event_loop.available_monitors().next())
            .ok_or_else(|| AppError::Graphics("没有检测到可用显示器".to_owned()))?;
        let geometry = WindowGeometry::from_monitor(&monitor);
        let attributes = WindowAttributes::default()
            .with_title("Steady Ink")
            .with_inner_size(geometry.idle_size)
            .with_min_inner_size(geometry.idle_size)
            .with_position(geometry.idle_position)
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_visible(false);

        let template = ConfigTemplateBuilder::new()
            .with_alpha_size(8)
            .with_depth_size(0)
            .with_stencil_size(8)
            .with_transparency(true);
        let display_builder = DisplayBuilder::new().with_window_attributes(Some(attributes));
        let (window, config) = display_builder
            .build(event_loop, template, choose_gl_config)
            .map_err(|error| AppError::Graphics(format!("OpenGL 配置创建失败: {error}")))?;
        let window = window.ok_or_else(|| AppError::Graphics("OpenGL 未创建窗口".to_owned()))?;
        let transparency_backend = select_transparency_backend(config.supports_transparency());
        // 首次 WM_GETMINMAXINFO 早于 winit 窗口状态初始化，创建完成后必须再次应用窄窗约束。
        window.set_min_inner_size(Some(geometry.idle_size));
        window.set_outer_position(geometry.idle_position);
        let _ = window.request_inner_size(geometry.idle_size);
        let gl_display = config.display();
        let window_handle = window
            .window_handle()
            .map_err(|error| AppError::Graphics(format!("窗口句柄读取失败: {error}")))?;
        let raw_window_handle = window_handle.as_raw();

        let context_attributes = ContextAttributesBuilder::new().build(Some(raw_window_handle));
        let fallback_attributes = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(None))
            .build(Some(raw_window_handle));
        // SAFETY: 配置、显示器和原始窗口句柄均由同一 glutin 创建流程提供，且窗口仍存活。
        let not_current_context = unsafe {
            gl_display
                .create_context(&config, &context_attributes)
                .or_else(|_| gl_display.create_context(&config, &fallback_attributes))
        }
        .map_err(|error| AppError::Graphics(format!("OpenGL 上下文创建失败: {error}")))?;

        let size = window.inner_size();
        let surface_attributes = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle,
            non_zero_dimension(size.width),
            non_zero_dimension(size.height),
        );
        // SAFETY: surface 使用仍然存活的窗口句柄及其匹配的 glutin 配置。
        let gl_surface = unsafe { gl_display.create_window_surface(&config, &surface_attributes) }
            .map_err(|error| {
                AppError::Graphics(format!("OpenGL 窗口 surface 创建失败: {error}"))
            })?;
        let gl_context = not_current_context
            .make_current(&gl_surface)
            .map_err(|error| AppError::Graphics(format!("OpenGL 上下文激活失败: {error}")))?;
        if let Err(error) =
            gl_surface.set_swap_interval(&gl_context, SwapInterval::Wait(NonZeroU32::MIN))
        {
            tracing::warn!(%error, "无法启用垂直同步");
        }

        // SAFETY: 上下文已经在当前线程激活，加载器在整个 glow 生命周期内保持有效。
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                let Ok(name) = std::ffi::CString::new(name) else {
                    return std::ptr::null();
                };
                gl_display.get_proc_address(name.as_c_str())
            })
        };
        let gl = Arc::new(gl);
        let diagnostics = read_gl_diagnostics(&gl);
        let num_samples = usize::from(config.num_samples());
        let stencil_size = usize::from(config.stencil_size());

        tracing::info!(
            vendor = diagnostics.vendor,
            renderer = diagnostics.renderer,
            version = diagnostics.version,
            software_fallback = diagnostics.software_fallback,
            alpha_size = config.alpha_size(),
            supports_transparency = ?config.supports_transparency(),
            ?transparency_backend,
            num_samples,
            stencil_size,
            "OpenGL 呈现后端已创建"
        );
        if diagnostics.software_fallback {
            tracing::warn!("检测到软件 OpenGL，不能作为核显性能验收结果");
        }

        Ok(Self {
            gl,
            gl_surface,
            gl_context,
            gl_display,
            window,
            geometry,
            diagnostics,
            dock_side: Cell::new(DockSide::Right),
            num_samples,
            stencil_size,
            transparency_backend,
        })
    }

    /// 返回 winit 窗口引用。
    pub const fn window(&self) -> &Window {
        &self.window
    }

    /// 显示窗口后重新应用原生透明后端，避免 winit 的可见性样式更新覆盖 layered 位。
    pub fn show(&self) -> Result<(), AppError> {
        self.window.set_visible(true);
        apply_transparency_backend(&self.window, self.transparency_backend)
    }

    /// 返回共享 glow 上下文。
    pub fn gl(&self) -> Arc<glow::Context> {
        Arc::clone(&self.gl)
    }

    /// 返回 Skia 创建 GL interface 所需的过程地址。
    pub fn get_proc_address(&self, name: &CStr) -> *const std::ffi::c_void {
        self.gl_display.get_proc_address(name)
    }

    /// 返回启动时记录的 OpenGL 诊断。
    pub const fn diagnostics(&self) -> &GlDiagnostics {
        &self.diagnostics
    }

    /// 返回窗口 framebuffer 的样本数。
    pub const fn num_samples(&self) -> usize {
        self.num_samples
    }

    /// 返回窗口 framebuffer 的模板位数。
    pub const fn stencil_size(&self) -> usize {
        self.stencil_size
    }

    /// 返回当前 Windows 透明后端要求的 framebuffer 清屏色。
    pub const fn transparency_clear_rgba(&self) -> [u8; 4] {
        self.transparency_backend.clear_rgba()
    }

    /// 将窗口切换到主显示器全屏批注几何或右侧悬浮几何。
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
        let size = match view {
            IdleWindowView::Toolbar => self.geometry.idle_size,
            IdleWindowView::QuickSettings => self.geometry.quick_settings_size,
            IdleWindowView::Settings => self.geometry.settings_size,
        };
        let position = self.idle_position(view);
        self.window.set_outer_position(position);
        let _ = self.window.request_inner_size(size);
        self.window.request_redraw();
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
        match (view, self.dock_side.get()) {
            (IdleWindowView::Toolbar, DockSide::Left) => self.geometry.idle_left_position,
            (IdleWindowView::Toolbar, DockSide::Right) => self.geometry.idle_position,
            (IdleWindowView::QuickSettings, DockSide::Left) => {
                self.geometry.quick_settings_left_position
            }
            (IdleWindowView::QuickSettings, DockSide::Right) => {
                self.geometry.quick_settings_position
            }
            (IdleWindowView::Settings, _) => self.geometry.settings_position,
        }
    }

    /// 在 winit 报告尺寸变化后调整 OpenGL drawable。
    pub fn resize(&self, size: PhysicalSize<u32>) {
        self.gl_surface.resize(
            &self.gl_context,
            non_zero_dimension(size.width),
            non_zero_dimension(size.height),
        );
    }

    /// 交换当前窗口前后缓冲区。
    pub fn swap_buffers(&self) -> Result<(), AppError> {
        self.gl_surface
            .swap_buffers(&self.gl_context)
            .map_err(|error| AppError::Graphics(format!("交换 OpenGL 缓冲失败: {error}")))
    }
}

/// 在 winit 完成可见性样式更新后应用已选定的 Windows 透明后端。
fn apply_transparency_backend(
    window: &Window,
    backend: TransparencyBackend,
) -> Result<(), AppError> {
    match backend {
        TransparencyBackend::DwmAlpha => enable_full_client_transparency(window)?,
        TransparencyBackend::LayeredColorKey => enable_layered_color_key_transparency(window)?,
    }
    Ok(())
}

/// 只有 WGL 明确报告透明支持时才使用默认 framebuffer alpha。
const fn select_transparency_backend(supports_transparency: Option<bool>) -> TransparencyBackend {
    if matches!(supports_transparency, Some(true)) {
        TransparencyBackend::DwmAlpha
    } else {
        TransparencyBackend::LayeredColorKey
    }
}

/// 把整个 Win32 客户区扩展为 DWM glass，使默认 framebuffer 的 alpha 参与桌面合成。
fn enable_full_client_transparency(window: &Window) -> Result<(), AppError> {
    use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;

    let hwnd = window_hwnd(window)?;
    let margins = full_client_glass_margins();
    // SAFETY: HWND 来自仍存活且由当前线程创建的 winit 窗口，MARGINS 在调用期间有效。
    unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) }
        .map_err(|error| AppError::Graphics(format!("无法启用 DWM 全客户区透明合成: {error}")))?;
    tracing::info!("DWM 全客户区透明合成已启用");
    Ok(())
}

/// 在 DWM 无法使用 WGL alpha 时启用硬件窗口 color-key 透明兜底。
fn enable_layered_color_key_transparency(window: &Window) -> Result<(), AppError> {
    use windows::Win32::{
        Foundation::COLORREF,
        Graphics::Dwm::{DWM_BB_ENABLE, DWM_BLURBEHIND, DwmEnableBlurBehindWindow},
        UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, LWA_COLORKEY, SetLayeredWindowAttributes,
            SetWindowLongPtrW, WS_EX_LAYERED,
        },
    };

    let hwnd = window_hwnd(window)?;
    let blur = DWM_BLURBEHIND {
        dwFlags: DWM_BB_ENABLE,
        fEnable: false.into(),
        ..Default::default()
    };
    // SAFETY: HWND 来自当前存活窗口，blur 在调用期间保持有效。
    if let Err(error) = unsafe { DwmEnableBlurBehindWindow(hwnd, &raw const blur) } {
        tracing::warn!(%error, "无法关闭 DWM blur，继续尝试 layered color-key");
    }

    // SAFETY: 只修改当前窗口的扩展样式，并保留 winit 已设置的其他位。
    let current_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let layered_style = current_style | WS_EX_LAYERED.0 as isize;
    // SAFETY: HWND 有效，GWL_EXSTYLE 接受合并后的扩展样式值。
    if unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, layered_style) } == 0 {
        return Err(AppError::Graphics(format!(
            "无法启用 Windows layered 窗口: {}",
            windows::core::Error::from_thread()
        )));
    }

    let color_key = COLORREF(colorref_from_rgba(LAYERED_COLOR_KEY_RGBA));
    // SAFETY: 窗口已启用 WS_EX_LAYERED，color key 与 Skia 清屏色使用同一常量。
    unsafe { SetLayeredWindowAttributes(hwnd, color_key, 255, LWA_COLORKEY) }
        .map_err(|error| AppError::Graphics(format!("无法启用 Windows color-key 透明: {error}")))?;
    tracing::info!(
        color_key = "#010203",
        "Windows layered color-key 透明已启用"
    );
    Ok(())
}

/// 从 winit 窗口读取当前 Win32 HWND。
fn window_hwnd(window: &Window) -> Result<windows::Win32::Foundation::HWND, AppError> {
    use windows::Win32::Foundation::HWND;

    let raw_handle = window
        .window_handle()
        .map_err(|error| AppError::Graphics(format!("透明窗口句柄读取失败: {error}")))?
        .as_raw();
    let RawWindowHandle::Win32(handle) = raw_handle else {
        return Err(AppError::Graphics("透明合成仅支持 Win32 HWND".to_owned()));
    };
    Ok(HWND(handle.hwnd.get() as *mut std::ffi::c_void))
}

/// 把 RGBA 颜色转为 Win32 COLORREF 使用的 0x00BBGGRR 布局。
const fn colorref_from_rgba(rgba: [u8; 4]) -> u32 {
    u32::from_le_bytes([rgba[0], rgba[1], rgba[2], 0])
}

/// 返回 Win32 约定的全客户区 glass 边距；仅首字段为 -1。
fn full_client_glass_margins() -> windows::Win32::UI::Controls::MARGINS {
    windows::Win32::UI::Controls::MARGINS {
        cxLeftWidth: -1,
        ..Default::default()
    }
}

/// 从候选中优先选择透明、硬件加速且 MSAA 最低的配置。
fn choose_gl_config(configs: Box<dyn Iterator<Item = Config> + '_>) -> Config {
    configs
        .max_by_key(|config| {
            let transparent_score =
                u32::from(config.supports_transparency().unwrap_or(false)) * 10_000;
            let hardware_score = u32::from(config.hardware_accelerated()) * 1_000;
            transparent_score + hardware_score + (255 - u32::from(config.num_samples()))
        })
        .expect("glutin 至少应返回一个 OpenGL 配置")
}

/// 把零尺寸转换为 OpenGL surface 可接受的最小非零尺寸。
fn non_zero_dimension(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(if value == 0 { 1 } else { value }) {
        Some(value) => value,
        None => NonZeroU32::MIN,
    }
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

/// 从当前 OpenGL 上下文读取驱动标识并识别常见软件 fallback。
fn read_gl_diagnostics(gl: &glow::Context) -> GlDiagnostics {
    use glow::HasContext as _;

    // SAFETY: 调用发生在已经激活的 OpenGL 上下文线程上。
    let (vendor, renderer, version) = unsafe {
        (
            gl.get_parameter_string(glow::VENDOR),
            gl.get_parameter_string(glow::RENDERER),
            gl.get_parameter_string(glow::VERSION),
        )
    };
    let renderer_lowercase = renderer.to_ascii_lowercase();
    let software_fallback = ["gdi generic", "llvmpipe", "softpipe", "software"]
        .iter()
        .any(|marker| renderer_lowercase.contains(marker));
    GlDiagnostics {
        vendor,
        renderer,
        version,
        software_fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证左右吸附位置保持相同垂直中心并遵守边缘间距。
    #[test]
    fn edge_positions_are_symmetric() {
        let monitor_position = PhysicalPosition::new(100, 50);
        let monitor_size = PhysicalSize::new(3_840, 2_160);
        let window_size = PhysicalSize::new(176, 496);
        let left = left_centered_position(monitor_position, monitor_size, window_size, 32);
        let right = right_centered_position(monitor_position, monitor_size, window_size, 32);

        assert_eq!(left.x, 132);
        assert_eq!(right.x, 3_732);
        assert_eq!(left.y, right.y);
    }

    /// 验证 DWM 全客户区 glass 使用 Win32 规定的单个 -1 哨兵值。
    #[test]
    fn full_client_glass_uses_left_margin_sentinel_only() {
        let margins = full_client_glass_margins();

        assert_eq!(margins.cxLeftWidth, -1);
        assert_eq!(margins.cxRightWidth, 0);
        assert_eq!(margins.cyTopHeight, 0);
        assert_eq!(margins.cyBottomHeight, 0);
    }

    /// 验证 WGL 未明确支持透明时不再进入会黑屏的 DWM alpha 路径。
    #[test]
    fn transparency_backend_falls_back_when_wgl_alpha_is_unavailable() {
        assert_eq!(
            select_transparency_backend(Some(true)),
            TransparencyBackend::DwmAlpha
        );
        assert_eq!(
            select_transparency_backend(Some(false)),
            TransparencyBackend::LayeredColorKey
        );
        assert_eq!(
            select_transparency_backend(None),
            TransparencyBackend::LayeredColorKey
        );
    }

    /// 验证 layered color-key 的 Win32 颜色布局与 Skia 清屏色严格一致。
    #[test]
    fn layered_color_key_uses_dedicated_opaque_clear_color() {
        assert_eq!(TransparencyBackend::DwmAlpha.clear_rgba(), [0, 0, 0, 0]);
        assert_eq!(
            TransparencyBackend::LayeredColorKey.clear_rgba(),
            [1, 2, 3, 255]
        );
        assert_eq!(colorref_from_rgba(LAYERED_COLOR_KEY_RGBA), 0x0003_0201);
    }
}
