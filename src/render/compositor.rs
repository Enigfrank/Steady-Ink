use super::egui_skia::{EguiFrame, EguiSkiaPainter};
use crate::{
    error::AppError,
    ink::{
        ActiveInkPreview, ActiveStrokeDelta, ActiveStrokeRenderCache, ActiveStrokeStyle,
        EraserSize, InkBounds, InkDocument, InkRenderCache, InkSyncKind, InkTool,
        create_gpu_surface, draw_active_filtered_preview, draw_active_preview,
        replay_active_stroke_regions,
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
    active_stroke_cache: ActiveStrokeRenderCache,
    active_surface: Option<Surface>,
    active_surface_size: [u32; 2],
    active_resync_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveSurfacePlan {
    Unavailable,
    Reuse,
    Recreate([u32; 2]),
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
    pub(crate) fn paint(
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
                active_stroke_cache: ActiveStrokeRenderCache::default(),
                active_surface: None,
                active_surface_size: size,
                active_resync_requested: false,
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
            self.active_surface = None;
            self.active_surface_size = size;
            self.lose_active_stroke_cache();
            self.reset_graphics_on_next_resize = false;
            return Ok(());
        }
        self.gr_context
            .purge_unlocked_resources(gpu::PurgeResourceOptions::AllResources);
        window_context.recreate_swap_chain(physical_size)?;
        self.window_surfaces = create_window_surfaces(&mut self.gr_context, window_context, size)?;
        self.ink_cache = InkRenderCache::new(&mut self.gr_context, size)?;
        self.active_surface = None;
        self.active_surface_size = size;
        self.lose_active_stroke_cache();
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
        self.active_surface = None;
        self.active_surface_size = [1, 1];
        self.lose_active_stroke_cache();
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
            .saturating_add(estimate_active_surface_bytes(
                self.active_surface.is_some(),
                self.active_surface_size,
            ))
            .saturating_add(self.egui.estimated_texture_bytes());
        swap_chain_bytes.saturating_add(u64::try_from(offscreen_bytes).unwrap_or(u64::MAX))
    }

    /// 完成透明清理、Skia 墨迹、egui、Skia submit 的一帧 D3D12 合成。
    pub(crate) fn paint(
        &mut self,
        window_context: &D3DRenderContext,
        document: &InkDocument,
        active_preview: Option<ActiveInkPreview<'_>>,
        active_stroke: Option<ActiveStrokeDelta>,
        egui_frame: EguiFrame,
    ) -> Result<InkSyncKind, AppError> {
        self.gr_context.reset(None);
        let ink_sync = self.ink_cache.sync(document);

        self.update_active_stroke(
            active_stroke.as_ref(),
            window_context.swap_chain_size().into(),
        )?;
        if let Some(preview) = active_preview {
            self.ink_cache.commit_deferred_erase_before_preview(preview);
        } else if active_stroke.is_some() {
            self.commit_deferred_erase_for_retained_preview();
        }
        let ink_image = self.ink_cache.snapshot(&mut self.gr_context);

        let active_image = self.active_surface_image();
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
        if let Some(active_image) = active_image {
            canvas.draw_image(&active_image, (0.0, 0.0), None);
        }
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

    /// 返回最近一次活动增量的采样、primitive 和 full fallback 工作量。
    pub(crate) fn active_stroke_diagnostics(&self) -> (usize, usize, bool) {
        let work = self.active_stroke_cache.last_work();
        (
            self.active_stroke_cache.sample_count(),
            work.recomputed_primitives,
            work.full_fallback,
        )
    }

    /// 消费一次由 resize、设备重建或序列断裂产生的活动笔画重同步请求。
    pub(crate) fn take_active_stroke_resync_requested(&mut self) -> bool {
        std::mem::take(&mut self.active_resync_requested)
    }

    /// 应用增量几何并在透明 retained surface 中保留最后一次活动预览。
    fn update_active_stroke(
        &mut self,
        delta: Option<&ActiveStrokeDelta>,
        target_size: [u32; 2],
    ) -> Result<(), AppError> {
        let Some(delta) = delta else {
            self.active_stroke_cache.clear();
            self.active_surface = None;
            self.active_resync_requested = false;
            return Ok(());
        };
        if !self.ensure_active_surface(target_size)? {
            self.active_resync_requested = true;
            return Ok(());
        }
        let work = match self.active_stroke_cache.apply_delta(delta) {
            Ok(work) => work,
            Err(_) => {
                self.active_resync_requested = true;
                return Ok(());
            }
        };
        if delta.from_sample == 0 {
            self.active_resync_requested = false;
        }
        if work.appended_samples == 0 && !work.full_redraw {
            return Ok(());
        }
        let surface = self
            .active_surface
            .as_mut()
            .expect("批注活动笔画必须拥有 retained surface");
        if work.full_redraw {
            surface.canvas().clear(Color::TRANSPARENT);
            draw_cached_active_stroke(surface.canvas(), &self.active_stroke_cache);
        } else if !replay_active_stroke_regions(surface.canvas(), &self.active_stroke_cache) {
            surface.canvas().clear(Color::TRANSPARENT);
            draw_cached_active_stroke(surface.canvas(), &self.active_stroke_cache);
            self.active_stroke_cache.record_render_full_fallback();
        }
        Ok(())
    }

    /// 在固定宽或自然笔锋活动预览开始前固化上一笔待定擦除。
    fn commit_deferred_erase_for_retained_preview(&mut self) {
        let Some(style) = self.active_stroke_cache.style() else {
            return;
        };
        let (fixed, natural) = self.active_stroke_cache.geometry();
        match style {
            ActiveStrokeStyle::Fixed { color, width } if !fixed.is_empty() => {
                self.ink_cache
                    .commit_deferred_erase_before_preview(ActiveInkPreview::Tool {
                        points: fixed,
                        tool: InkTool::Pen,
                        color,
                        pen_width: width,
                        eraser_size: EraserSize::default(),
                    });
            }
            ActiveStrokeStyle::Natural { color, .. } if !natural.is_empty() => {
                self.ink_cache.commit_deferred_erase_before_preview(
                    ActiveInkPreview::VariableTool {
                        points: natural,
                        color,
                        eraser_size: EraserSize::default(),
                    },
                );
            }
            _ => {}
        }
    }

    /// 提交活动 surface 的写入并取得当前透明图像快照。
    fn active_surface_image(&mut self) -> Option<skia_safe::Image> {
        let surface = self.active_surface.as_mut()?;
        self.gr_context.flush_and_submit_surface(surface, None);
        Some(surface.image_snapshot())
    }

    /// 首次活动笔画到达时按当前批注 swap-chain 容量延迟创建透明 surface。
    fn ensure_active_surface(&mut self, target_size: [u32; 2]) -> Result<bool, AppError> {
        let plan = active_surface_plan(
            self.annotation_resources_enabled,
            self.active_surface.is_some(),
            self.active_surface_size,
            target_size,
        );
        let size = match plan {
            ActiveSurfacePlan::Unavailable => return Ok(false),
            ActiveSurfacePlan::Reuse => return Ok(true),
            ActiveSurfacePlan::Recreate(size) => size,
        };
        self.active_surface = None;
        self.active_surface_size = size;
        self.lose_active_stroke_cache();
        let mut surface = create_gpu_surface(&mut self.gr_context, size)?;
        surface.canvas().clear(Color::TRANSPARENT);
        self.active_surface = Some(surface);
        Ok(true)
    }

    /// 丢失 render-side retained 状态时保留一次显式 sample-zero 重同步请求。
    fn lose_active_stroke_cache(&mut self) {
        self.active_resync_requested |= self.active_stroke_cache.sample_count() > 0;
        self.active_stroke_cache.clear();
    }
}

/// 使用现有滤波、宽度、Skia Paint AA 和路径 helper 完整绘制 retained 活动几何。
fn draw_cached_active_stroke(canvas: &skia_safe::Canvas, cache: &ActiveStrokeRenderCache) {
    let (fixed, natural) = cache.geometry();
    match cache.style() {
        Some(ActiveStrokeStyle::Fixed { color, width }) if !fixed.is_empty() => {
            draw_active_filtered_preview(
                canvas,
                ActiveInkPreview::Tool {
                    points: fixed,
                    tool: InkTool::Pen,
                    color,
                    pen_width: width,
                    eraser_size: EraserSize::default(),
                },
            );
        }
        Some(ActiveStrokeStyle::Natural { color, .. }) if !natural.is_empty() => {
            draw_active_filtered_preview(
                canvas,
                ActiveInkPreview::VariableTool {
                    points: natural,
                    color,
                    eraser_size: EraserSize::default(),
                },
            );
        }
        _ => {}
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

/// 估算透明活动 surface 的颜色缓冲字节数。
fn estimate_surface_bytes(size: [u32; 2]) -> usize {
    (size[0] as usize)
        .saturating_mul(size[1] as usize)
        .saturating_mul(4)
}

/// 只有批注资源启用后才允许按当前窗口容量创建活动 surface。
fn active_surface_allocation_size(
    annotation_resources_enabled: bool,
    size: [u32; 2],
) -> Option<[u32; 2]> {
    annotation_resources_enabled.then_some(size)
}

/// 依据当前 back buffer 容量决定活动层是否可用、可复用或必须重建。
fn active_surface_plan(
    annotation_resources_enabled: bool,
    allocated: bool,
    retained_size: [u32; 2],
    target_size: [u32; 2],
) -> ActiveSurfacePlan {
    let Some(target_size) =
        active_surface_allocation_size(annotation_resources_enabled, target_size)
    else {
        return ActiveSurfacePlan::Unavailable;
    };
    if allocated && retained_size == target_size {
        ActiveSurfacePlan::Reuse
    } else {
        ActiveSurfacePlan::Recreate(target_size)
    }
}

/// 只统计实际已分配的活动 surface，空闲占位状态保持零额外驻留。
fn estimate_active_surface_bytes(allocated: bool, size: [u32; 2]) -> usize {
    if allocated {
        estimate_surface_bytes(size)
    } else {
        0
    }
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

    /// 验证 idle 不创建活动 surface，批注启用后才使用当前窗口容量。
    #[test]
    fn active_surface_allocation_requires_annotation_resources() {
        let size = [3840, 2160];

        assert_eq!(active_surface_allocation_size(false, size), None);
        assert_eq!(active_surface_allocation_size(true, size), Some(size));
        assert_eq!(estimate_active_surface_bytes(false, size), 0);
        assert_eq!(estimate_active_surface_bytes(true, size), 3840 * 2160 * 4);
    }

    /// 验证活动层不会复用资源释放阶段留下的 1x1 尺寸，并始终覆盖当前 back buffer。
    #[test]
    fn active_surface_plan_rebuilds_stale_surface_at_back_buffer_size() {
        let target_size = [3840, 2160];

        assert_eq!(
            active_surface_plan(true, false, [1, 1], target_size),
            ActiveSurfacePlan::Recreate(target_size)
        );
        assert_eq!(
            active_surface_plan(true, true, [1, 1], target_size),
            ActiveSurfacePlan::Recreate(target_size)
        );
        assert_eq!(
            active_surface_plan(true, true, target_size, target_size),
            ActiveSurfacePlan::Reuse
        );
        assert_eq!(
            active_surface_plan(false, false, [1, 1], target_size),
            ActiveSurfacePlan::Unavailable
        );
    }
}
