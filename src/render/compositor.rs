use super::egui_skia::{EguiFrame, EguiSkiaPainter};
use crate::{
    error::AppError,
    ink::{
        ActiveInkPreview, InkBounds, InkDocument, InkRenderCache, InkSyncKind, draw_active_preview,
    },
    window::{D3DRenderContext, SWAP_CHAIN_BUFFER_COUNT},
};
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

/// 保持 Skia surface 与其外部 DXGI backend target 的生命周期一致。
struct SwapChainSurface {
    surface: Surface,
    _backend_target: BackendRenderTarget,
}

/// 在同一 D3D12 back buffer 中按 Skia 墨迹后 egui 的顺序合成一帧。
pub struct Compositor {
    egui: EguiSkiaPainter,
    ink_cache: InkRenderCache,
    annotation_resources_enabled: bool,
    reset_graphics_on_next_resize: bool,
    ink_rendering_error: Option<String>,
    window_surfaces: Vec<SwapChainSurface>,
    gr_context: DirectContext,
}

/// 仅为独立放映控件窗口合成透明背景和 egui，不创建墨迹缓存。
pub(crate) struct UiCompositor {
    egui: EguiSkiaPainter,
    window_surfaces: Vec<SwapChainSurface>,
    gr_context: DirectContext,
}

impl UiCompositor {
    /// 从第二个 D3D target 创建只含 egui 资源的轻量合成器。
    pub fn new(
        window_context: &D3DRenderContext,
        egui_context: egui::Context,
    ) -> Result<Self, AppError> {
        let mut gr_context = create_direct_context(window_context)?;
        let size: [u32; 2] = window_context.swap_chain_size().into();
        let window_surfaces = create_window_surfaces(&mut gr_context, window_context, size)?;
        Ok(Self {
            egui: EguiSkiaPainter::new(egui_context),
            window_surfaces,
            gr_context,
        })
    }

    /// 调整控件 swap chain 并重建对应的 Skia back-buffer surface。
    pub fn resize(
        &mut self,
        window_context: &mut D3DRenderContext,
        size: [u32; 2],
    ) -> Result<(), AppError> {
        let physical_size = winit::dpi::PhysicalSize::new(size[0].max(1), size[1].max(1));
        if window_context.swap_chain_size() == physical_size {
            return Ok(());
        }
        self.gr_context.flush_and_submit();
        self.window_surfaces.clear();
        window_context.recreate_swap_chain(physical_size)?;
        self.window_surfaces = create_window_surfaces(&mut self.gr_context, window_context, size)?;
        Ok(())
    }

    /// 清空透明背景并把一帧 egui 绘制到控件窗口 back buffer。
    pub fn paint(
        &mut self,
        window_context: &D3DRenderContext,
        egui_frame: EguiFrame,
    ) -> Result<(), AppError> {
        self.gr_context.reset(None);
        let index = window_context.current_back_buffer_index();
        let surface_count = self.window_surfaces.len();
        let target = self.window_surfaces.get_mut(index).ok_or_else(|| {
            AppError::Graphics(format!(
                "放映控件 DXGI 返回了无效 back buffer 索引 {index}，Skia surface 数量为 {surface_count}"
            ))
        })?;
        let canvas = target.surface.canvas();
        canvas.reset_matrix();
        canvas.clear(Color::TRANSPARENT);
        self.egui.paint(canvas, egui_frame)?;
        self.gr_context
            .flush_and_submit_surface(&mut target.surface, None);
        Ok(())
    }

    /// 估算控件交换链与 egui 上传纹理的受管 GPU 字节数。
    pub fn estimated_managed_gpu_bytes(&self, window_context: &D3DRenderContext) -> u64 {
        let size = window_context.swap_chain_size();
        let swap_chain_bytes = u64::from(size.width)
            .saturating_mul(u64::from(size.height))
            .saturating_mul(4)
            .saturating_mul(u64::try_from(self.window_surfaces.len()).unwrap_or(u64::MAX));
        swap_chain_bytes
            .saturating_add(u64::try_from(self.egui.estimated_texture_bytes()).unwrap_or(u64::MAX))
    }
}

impl Compositor {
    /// 从渲染线程的 D3D12 device/queue 创建 Skia DirectContext 和交换链 surface。
    pub fn new(
        window_context: &D3DRenderContext,
        egui_context: egui::Context,
    ) -> Result<(Self, Option<String>), AppError> {
        let mut gr_context = create_direct_context(window_context)?;
        let size: [u32; 2] = window_context.swap_chain_size().into();
        let window_surfaces = create_window_surfaces(&mut gr_context, window_context, size)?;
        let ink_cache = InkRenderCache::new(&mut gr_context, size)?;
        let egui = EguiSkiaPainter::new(egui_context);

        Ok((
            Self {
                egui,
                ink_cache,
                annotation_resources_enabled: false,
                reset_graphics_on_next_resize: false,
                ink_rendering_error: None,
                window_surfaces,
                gr_context,
            },
            None,
        ))
    }

    /// 释放旧 back buffer 包装，调整 DXGI swap chain，并重建窗口和墨迹 surface。
    pub fn resize(
        &mut self,
        window_context: &mut D3DRenderContext,
        size: [u32; 2],
    ) -> Result<(), AppError> {
        let physical_size = winit::dpi::PhysicalSize::new(size[0].max(1), size[1].max(1));
        let reset_graphics =
            self.reset_graphics_on_next_resize && !self.annotation_resources_enabled;
        if window_context.swap_chain_size() == physical_size && !reset_graphics {
            return Ok(());
        }
        if can_reuse_annotation_capacity(
            self.annotation_resources_enabled,
            window_context.swap_chain_size(),
            physical_size,
        ) {
            return Ok(());
        }
        self.gr_context.flush_and_submit();
        self.window_surfaces.clear();
        if reset_graphics {
            self.gr_context.free_gpu_resources();
            window_context.recreate_graphics_device(physical_size)?;
            let mut replacement_context = create_direct_context(window_context)?;
            let window_surfaces =
                create_window_surfaces(&mut replacement_context, window_context, size)?;
            let ink_cache = InkRenderCache::new(&mut replacement_context, [1, 1])?;
            self.window_surfaces = window_surfaces;
            self.ink_cache = ink_cache;
            self.ink_rendering_error = None;
            self.gr_context.release_resources_and_abandon();
            self.gr_context = replacement_context;
            self.reset_graphics_on_next_resize = false;
            return Ok(());
        }
        self.gr_context
            .purge_unlocked_resources(gpu::PurgeResourceOptions::AllResources);
        window_context.recreate_swap_chain(physical_size)?;
        self.window_surfaces = create_window_surfaces(&mut self.gr_context, window_context, size)?;
        self.ink_cache = InkRenderCache::new(&mut self.gr_context, size)?;
        self.ink_rendering_error = None;
        Ok(())
    }

    /// 在批注与 idle 模式之间切换大型墨迹资源的驻留策略。
    pub fn set_annotation_resources_enabled(&mut self, enabled: bool) -> Result<(), AppError> {
        if self.annotation_resources_enabled == enabled {
            return Ok(());
        }
        self.annotation_resources_enabled = enabled;
        self.reset_graphics_on_next_resize = !enabled;
        if enabled {
            return Ok(());
        }

        self.ink_cache = InkRenderCache::new(&mut self.gr_context, [1, 1])?;
        self.gr_context.flush_and_submit();
        self.gr_context
            .purge_unlocked_resources(gpu::PurgeResourceOptions::AllResources);
        Ok(())
    }

    /// 返回最近一次墨迹增强资源错误，供设置页非阻塞展示。
    pub fn ink_rendering_error(&self) -> Option<&str> {
        self.ink_rendering_error.as_deref()
    }

    /// 保守估算交换链、墨迹和 egui 持有的 GPU 渲染资源字节数。
    pub(crate) fn estimated_managed_gpu_bytes(&self, window_context: &D3DRenderContext) -> u64 {
        let size = window_context.swap_chain_size();
        let surface_count = u64::try_from(self.window_surfaces.len()).unwrap_or(u64::MAX);
        let swap_chain_bytes = u64::from(size.width)
            .saturating_mul(u64::from(size.height))
            .saturating_mul(4)
            .saturating_mul(surface_count);
        let offscreen_bytes = self
            .ink_cache
            .estimated_bytes()
            .saturating_add(self.egui.estimated_texture_bytes());
        swap_chain_bytes.saturating_add(u64::try_from(offscreen_bytes).unwrap_or(u64::MAX))
    }

    /// 完成透明清理、Skia 墨迹、egui、Skia submit 的一帧 D3D12 合成。
    pub fn paint(
        &mut self,
        window_context: &D3DRenderContext,
        document: &InkDocument,
        active_preview: Option<ActiveInkPreview<'_>>,
        egui_frame: EguiFrame,
    ) -> Result<InkSyncKind, AppError> {
        self.gr_context.reset(None);
        let ink_sync = self.ink_cache.sync(document);

        if let Some(preview) = active_preview {
            self.ink_cache.commit_deferred_erase_before_preview(preview);
        }
        let ink_image = self.ink_cache.snapshot(&mut self.gr_context);

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
        canvas.draw_image(&ink_image, (0.0, 0.0), None);
        self.ink_cache.draw_deferred_erase(canvas);
        if let Some(preview) = active_preview {
            draw_active_preview(canvas, preview);
        }
        self.egui.paint(canvas, egui_frame)?;
        self.gr_context
            .flush_and_submit_surface(&mut target.surface, None);
        Ok(ink_sync)
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

/// 返回现有批注 swap chain 是否足以覆盖新的可见客户区。
fn can_reuse_annotation_capacity(
    annotation_resources_enabled: bool,
    current: winit::dpi::PhysicalSize<u32>,
    requested: winit::dpi::PhysicalSize<u32>,
) -> bool {
    annotation_resources_enabled
        && requested.width <= current.width
        && requested.height <= current.height
}

/// 从窗口当前 D3D12 设备创建 Skia DirectContext。
fn create_direct_context(window_context: &D3DRenderContext) -> Result<DirectContext, AppError> {
    let backend_context = BackendContext {
        adapter: window_context.adapter().clone(),
        device: window_context.device().clone(),
        queue: window_context.queue().clone(),
        memory_allocator: None,
        protected_context: Protected::No,
    };
    unsafe { gpu::direct_contexts::make_d3d(&backend_context, None) }
        .ok_or_else(|| AppError::Graphics("无法创建 Skia D3D12 DirectContext".to_owned()))
}

/// 为双缓冲 DXGI swap chain 的每个 D3D12 resource 创建 Skia surface。
fn create_window_surfaces(
    context: &mut DirectContext,
    window_context: &D3DRenderContext,
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
            skia_safe::ColorSpace::new_srgb(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证批注 resize 只在两个维度都不超过当前容量时复用现有 surface。
    #[test]
    fn annotation_resize_reuses_only_sufficient_capacity() {
        let current = winit::dpi::PhysicalSize::new(1920, 1080);

        assert!(can_reuse_annotation_capacity(
            true,
            current,
            winit::dpi::PhysicalSize::new(1600, 900),
        ));
        assert!(!can_reuse_annotation_capacity(
            true,
            current,
            winit::dpi::PhysicalSize::new(2560, 1080),
        ));
        assert!(!can_reuse_annotation_capacity(
            false,
            current,
            winit::dpi::PhysicalSize::new(1600, 900),
        ));
    }
}
