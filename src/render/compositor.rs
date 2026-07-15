use skia_safe::{
    Color, ColorType, Surface,
    gpu::{
        self, BackendRenderTarget, DirectContext, Protected, SurfaceOrigin,
        d3d::{BackendContext, TextureResourceInfo},
    },
};
use windows::Win32::Graphics::{
    Direct3D12::D3D12_RESOURCE_STATE_COMMON,
    Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_STANDARD_MULTISAMPLE_QUALITY_PATTERN},
};
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::Window};

use super::egui_skia::EguiSkiaRenderer;
use crate::{
    error::AppError,
    ink::{ActiveInkPreview, InkBounds, InkDocument, InkRenderCache},
    window::{D3DWindowContext, SWAP_CHAIN_BUFFER_COUNT},
};

/// 保持 Skia surface 与其外部 DXGI backend target 的生命周期一致。
struct SwapChainSurface {
    surface: Surface,
    _backend_target: BackendRenderTarget,
}

/// 在同一 D3D12 back buffer 中按 Skia 墨迹后 egui 的顺序合成一帧。
pub struct Compositor {
    egui: EguiSkiaRenderer,
    ink_cache: InkRenderCache,
    window_surfaces: Vec<SwapChainSurface>,
    gr_context: DirectContext,
}

impl Compositor {
    /// 从 D3D12 device/queue 创建 Skia DirectContext、交换链 surface 和 egui painter。
    pub fn new(
        event_loop: &ActiveEventLoop,
        window_context: &D3DWindowContext,
    ) -> Result<Self, AppError> {
        let backend_context = BackendContext {
            adapter: window_context.adapter().clone(),
            device: window_context.device().clone(),
            queue: window_context.queue().clone(),
            memory_allocator: None,
            protected_context: Protected::No,
        };
        let mut gr_context = unsafe { gpu::direct_contexts::make_d3d(&backend_context, None) }
            .ok_or_else(|| AppError::Graphics("无法创建 Skia D3D12 DirectContext".to_owned()))?;
        let size: [u32; 2] = window_context.window().inner_size().into();
        let window_surfaces = create_window_surfaces(&mut gr_context, window_context, size)?;
        let ink_cache = InkRenderCache::new(&mut gr_context, size)?;
        let egui = EguiSkiaRenderer::new(event_loop, window_context.window());

        Ok(Self {
            egui,
            ink_cache,
            window_surfaces,
            gr_context,
        })
    }

    /// 把 winit 事件转交给 egui，并返回该事件是否需要重绘或已被 UI 消费。
    pub fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        self.egui.on_window_event(window, event)
    }

    /// 执行本帧 egui 布局，但暂不修改 D3D12 back buffer。
    pub fn run_ui(&mut self, window: &Window, run_ui: impl FnMut(&mut egui::Ui)) {
        self.egui.run_ui(window, run_ui);
    }

    /// 返回 egui 上下文，供等待型事件循环安装按需重绘回调。
    pub const fn egui_context(&self) -> &egui::Context {
        self.egui.context()
    }

    /// 清空 egui 当前指针状态，供原生手掌分类取消已暂存的 UI 接触。
    pub fn cancel_egui_pointer(&self) {
        self.egui.cancel_pointer();
    }

    /// 释放旧 back buffer 包装，调整 DXGI swap chain，并重建窗口和墨迹 surface。
    pub fn resize(
        &mut self,
        window_context: &mut D3DWindowContext,
        size: [u32; 2],
    ) -> Result<(), AppError> {
        let physical_size = winit::dpi::PhysicalSize::new(size[0].max(1), size[1].max(1));
        if window_context.swap_chain_size() == physical_size {
            return Ok(());
        }
        self.gr_context.flush_and_submit();
        self.window_surfaces.clear();
        self.gr_context
            .purge_unlocked_resources(gpu::PurgeResourceOptions::AllResources);
        window_context.recreate_swap_chain(physical_size)?;
        self.window_surfaces = create_window_surfaces(&mut self.gr_context, window_context, size)?;
        self.ink_cache = InkRenderCache::new(&mut self.gr_context, size)?;
        Ok(())
    }

    /// 完成透明清理、Skia 墨迹、egui、Skia submit 的一帧 D3D12 合成。
    pub fn paint(
        &mut self,
        window_context: &D3DWindowContext,
        document: &InkDocument,
        active_preview: Option<ActiveInkPreview<'_>>,
    ) -> Result<(), AppError> {
        self.gr_context.reset(None);
        self.ink_cache.sync(document);

        let index = window_context.current_back_buffer_index();
        let surface_count = self.window_surfaces.len();
        let target = self.window_surfaces.get_mut(index).ok_or_else(|| {
            AppError::Graphics(format!(
                "DXGI 返回了无效 back buffer 索引 {index}，Skia surface 数量为 {surface_count}"
            ))
        })?;
        let canvas = target.surface.canvas();
        canvas.reset_matrix();
        canvas.clear(Color::TRANSPARENT);
        let ink_image = self.ink_cache.snapshot();
        canvas.draw_image(ink_image, (0.0, 0.0), None);
        if let Some(preview) = active_preview {
            crate::ink::renderer::draw_active_preview(canvas, preview);
        }
        self.egui.paint(canvas)?;
        self.gr_context
            .flush_and_submit_surface(&mut target.surface, None);
        Ok(())
    }

    /// 强制下次绘制从文档事实历史重建 GPU 墨迹缓存。
    pub fn invalidate_ink_cache(&mut self) {
        self.ink_cache.invalidate();
    }

    /// 请求下次绘制只重建一个墨迹受影响矩形。
    pub fn invalidate_ink_region(&mut self, bounds: InkBounds) {
        self.ink_cache.invalidate_region(bounds);
    }
}

/// 为双缓冲 DXGI swap chain 的每个 D3D12 resource 创建 Skia surface。
fn create_window_surfaces(
    context: &mut DirectContext,
    window_context: &D3DWindowContext,
    size: [u32; 2],
) -> Result<Vec<SwapChainSurface>, AppError> {
    let dimensions = (
        i32::try_from(size[0].max(1)).map_err(|error| AppError::Graphics(error.to_string()))?,
        i32::try_from(size[1].max(1)).map_err(|error| AppError::Graphics(error.to_string()))?,
    );
    let mut surfaces = Vec::with_capacity(SWAP_CHAIN_BUFFER_COUNT);
    for index in 0..SWAP_CHAIN_BUFFER_COUNT {
        let resource = window_context.back_buffer(index)?;
        let backend_target = gpu::backend_render_targets::make_d3d(
            dimensions,
            &TextureResourceInfo {
                resource,
                alloc: None,
                resource_state: D3D12_RESOURCE_STATE_COMMON,
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                sample_count: 1,
                level_count: 0,
                sample_quality_pattern: DXGI_STANDARD_MULTISAMPLE_QUALITY_PATTERN,
                protected: Protected::No,
            },
        );
        let surface = gpu::surfaces::wrap_backend_render_target(
            context,
            &backend_target,
            SurfaceOrigin::TopLeft,
            ColorType::BGRA8888,
            None,
            None,
        )
        .ok_or_else(|| {
            AppError::Graphics(format!("无法包装 DXGI back buffer {index} 为 Skia surface"))
        })?;
        surfaces.push(SwapChainSurface {
            surface,
            _backend_target: backend_target,
        });
    }
    Ok(surfaces)
}
