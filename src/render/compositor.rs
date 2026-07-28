use std::time::{Duration, Instant};

use skia_safe::{
    BlendMode, Color, ColorType, Rect, Surface,
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
    ink::{
        ActiveInkPreview, BASE_PREVIEW_TILE_SIZE, InkBounds, InkDocument, InkPreviewCache,
        InkRenderCache, SurfacePool, VelocityTracker, active_preview_bounds, draw_active_preview,
        draw_image_rect_logical, preview_replaces_region, preview_tile_size_for_velocity,
    },
    settings::InkAntialiasingMode,
    window::{D3DWindowContext, SWAP_CHAIN_BUFFER_COUNT},
};

const MAX_IDLE_PREVIEW_SURFACES: usize = 5;
const MAX_PREVIEW_POOL_BYTES: usize = 5 * 1024 * 1024;
const PREVIEW_POOL_GC_TIMEOUT: Duration = Duration::from_secs(30);

/// 保持 Skia surface 与其外部 DXGI backend target 的生命周期一致。
struct SwapChainSurface {
    surface: Surface,
    _backend_target: BackendRenderTarget,
}

/// 在同一 D3D12 back buffer 中按 Skia 墨迹后 egui 的顺序合成一帧。
pub struct Compositor {
    egui: EguiSkiaRenderer,
    ink_cache: InkRenderCache,
    ink_mode: InkAntialiasingMode,
    preferred_ink_mode: InkAntialiasingMode,
    annotation_resources_enabled: bool,
    ink_rendering_error: Option<String>,
    preview_cache: Option<InkPreviewCache>,
    preview_surface_pool: SurfacePool,
    preview_velocity: VelocityTracker,
    preview_tile_size: u32,
    window_surfaces: Vec<SwapChainSurface>,
    gr_context: DirectContext,
}

/// 一次创建持久墨迹与增强模式活动预览所需的 GPU 资源。
struct InkResources {
    cache: InkRenderCache,
    preview_cache: Option<InkPreviewCache>,
    mode: InkAntialiasingMode,
    error: Option<String>,
}

/// 描述已经准备好的活动预览图像及其逻辑目标区域。
struct PreviewComposite {
    image: skia_safe::Image,
    origin: crate::ink::CanvasPoint,
    logical_size: [u32; 2],
    render_size: [u32; 2],
    replace_region: bool,
}

impl Compositor {
    /// 从 D3D12 device/queue 创建 Skia DirectContext、交换链 surface 和 egui painter。
    pub fn new(
        event_loop: &ActiveEventLoop,
        window_context: &D3DWindowContext,
        requested_mode: InkAntialiasingMode,
    ) -> Result<(Self, InkAntialiasingMode, Option<String>), AppError> {
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
        let resources = create_ink_resources(&mut gr_context, size, InkAntialiasingMode::Off)?;
        let egui = EguiSkiaRenderer::new(event_loop, window_context.window());

        let applied_mode = requested_mode;
        let error = resources.error.clone();
        Ok((
            Self {
                egui,
                ink_cache: resources.cache,
                ink_mode: resources.mode,
                preferred_ink_mode: requested_mode,
                annotation_resources_enabled: false,
                ink_rendering_error: resources.error,
                preview_cache: resources.preview_cache,
                preview_surface_pool: SurfacePool::new(
                    MAX_IDLE_PREVIEW_SURFACES,
                    MAX_PREVIEW_POOL_BYTES,
                ),
                preview_velocity: VelocityTracker::new(),
                preview_tile_size: BASE_PREVIEW_TILE_SIZE,
                window_surfaces,
                gr_context,
            },
            applied_mode,
            error,
        ))
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
        self.release_preview_cache();
        self.window_surfaces.clear();
        window_context.recreate_swap_chain(physical_size)?;
        self.window_surfaces = create_window_surfaces(&mut self.gr_context, window_context, size)?;
        let target_mode =
            desired_ink_mode(self.annotation_resources_enabled, self.preferred_ink_mode);
        let resources = create_ink_resources(&mut self.gr_context, size, target_mode)?;
        self.ink_cache = resources.cache;
        self.ink_mode = resources.mode;
        if self.annotation_resources_enabled {
            self.preferred_ink_mode = resources.mode;
        }
        self.ink_rendering_error = resources.error;
        self.preview_cache = resources.preview_cache;
        self.preview_surface_pool.gc(PREVIEW_POOL_GC_TIMEOUT);
        self.log_surface_pool_stats("窗口 resize 后预览资源池状态");
        Ok(())
    }

    /// 运行时切换墨迹抗锯齿模式，成功后由下一帧完整重放当前文档。
    pub fn set_ink_antialiasing(
        &mut self,
        window_context: &D3DWindowContext,
        requested_mode: InkAntialiasingMode,
    ) -> Result<(), AppError> {
        if requested_mode == self.preferred_ink_mode && self.ink_rendering_error.is_none() {
            return Ok(());
        }
        self.preferred_ink_mode = requested_mode;
        if !self.annotation_resources_enabled {
            self.ink_rendering_error = None;
            return Ok(());
        }
        self.release_preview_cache();
        let size: [u32; 2] = window_context.window().inner_size().into();
        let resources = create_ink_resources(&mut self.gr_context, size, requested_mode)?;
        self.ink_cache = resources.cache;
        self.ink_mode = resources.mode;
        self.preferred_ink_mode = resources.mode;
        self.ink_rendering_error = resources.error;
        self.preview_cache = resources.preview_cache;
        Ok(())
    }

    /// 返回批注资源应恢复的用户首选抗锯齿模式。
    pub const fn ink_antialiasing_mode(&self) -> InkAntialiasingMode {
        self.preferred_ink_mode
    }

    /// 在批注与 idle 模式之间切换大型墨迹资源的驻留策略。
    pub fn set_annotation_resources_enabled(&mut self, enabled: bool) -> Result<(), AppError> {
        if self.annotation_resources_enabled == enabled {
            return Ok(());
        }
        self.annotation_resources_enabled = enabled;
        self.release_preview_cache();
        self.preview_velocity.reset();
        self.preview_tile_size = BASE_PREVIEW_TILE_SIZE;
        if enabled {
            return Ok(());
        }

        self.ink_cache =
            InkRenderCache::new(&mut self.gr_context, [1, 1], InkAntialiasingMode::Off)?;
        self.ink_mode = InkAntialiasingMode::Off;
        self.preview_surface_pool.clear();
        self.gr_context.flush_and_submit();
        self.gr_context
            .purge_unlocked_resources(gpu::PurgeResourceOptions::AllResources);
        Ok(())
    }

    /// 返回最近一次墨迹增强资源错误，供设置页非阻塞展示。
    pub fn ink_rendering_error(&self) -> Option<&str> {
        self.ink_rendering_error.as_deref()
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

        if let Some(preview) = active_preview {
            if let Some(position) = preview.latest_position() {
                self.preview_velocity.update(position, Instant::now());
                self.preview_tile_size = self.preview_tile_size.max(
                    preview_tile_size_for_velocity(self.preview_velocity.velocity()),
                );
            }
        } else {
            self.preview_velocity.reset();
            self.preview_tile_size = BASE_PREVIEW_TILE_SIZE;
        }

        let mut ink_image = self.ink_cache.snapshot();
        let mut preview_composite = None;
        if active_preview.is_none() && self.ink_mode != InkAntialiasingMode::Off {
            let reset_error = if let Some(preview_cache) = self.preview_cache.as_mut() {
                preview_cache
                    .reset_to_base(
                        &mut self.gr_context,
                        self.ink_cache.logical_size(),
                        &mut self.preview_surface_pool,
                    )
                    .err()
            } else {
                None
            };
            if let Some(error) = reset_error {
                self.fallback_to_off(document, format!("活动墨迹预览回收失败: {error}"))?;
                ink_image = self.ink_cache.snapshot();
            }
        }
        if let Some(preview) = active_preview
            && self.ink_mode != InkAntialiasingMode::Off
        {
            match self.prepare_preview(preview, &ink_image) {
                Ok(composite) => preview_composite = Some(composite),
                Err(error) => {
                    self.fallback_to_off(document, format!("活动墨迹预览创建失败: {error}"))?;
                    ink_image = self.ink_cache.snapshot();
                }
            }
        }

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
        let logical_size = self.ink_cache.logical_size();
        let config = self.ink_cache.config();
        if config.mode == InkAntialiasingMode::Off {
            canvas.draw_image(&ink_image, (0.0, 0.0), None);
        } else {
            draw_image_rect_logical(
                canvas,
                &ink_image,
                self.ink_cache.render_size(),
                Rect::from_xywh(0.0, 0.0, logical_size[0] as f32, logical_size[1] as f32),
                config.mode == InkAntialiasingMode::Supersample,
                BlendMode::SrcOver,
            );
        }
        if let Some(composite) = preview_composite {
            let origin = composite.origin;
            let size = composite.logical_size;
            draw_image_rect_logical(
                canvas,
                &composite.image,
                composite.render_size,
                Rect::from_xywh(origin.x, origin.y, size[0] as f32, size[1] as f32),
                config.mode == InkAntialiasingMode::Supersample,
                if composite.replace_region {
                    BlendMode::Src
                } else {
                    BlendMode::SrcOver
                },
            );
        } else if let Some(preview) = active_preview {
            draw_active_preview(canvas, preview);
        }
        self.egui.paint(canvas)?;
        self.gr_context
            .flush_and_submit_surface(&mut target.surface, None);
        Ok(())
    }

    /// 将活动预览绘制到局部增强 surface，并返回其目标区域。
    fn prepare_preview(
        &mut self,
        preview: ActiveInkPreview<'_>,
        persistent_image: &skia_safe::Image,
    ) -> Result<PreviewComposite, AppError> {
        let pool_stats_before = self.preview_surface_pool.stats();
        let bounds = active_preview_bounds(preview)
            .ok_or_else(|| AppError::Graphics("活动墨迹预览没有有效采样".to_owned()))?;
        let logical_size = self.ink_cache.logical_size();
        let source_render_size = self.ink_cache.render_size();
        let source_config = self.ink_cache.config();
        if self.preview_cache.is_none() {
            self.preview_cache = Some(InkPreviewCache::for_bounds(
                &mut self.gr_context,
                bounds,
                logical_size,
                self.preview_tile_size,
                source_config,
                &mut self.preview_surface_pool,
            )?);
        }
        let preview_cache = self
            .preview_cache
            .as_mut()
            .ok_or_else(|| AppError::Graphics("增强墨迹模式缺少活动预览 surface".to_owned()))?;
        preview_cache.ensure(
            &mut self.gr_context,
            bounds,
            logical_size,
            self.preview_tile_size,
            &mut self.preview_surface_pool,
        )?;
        if preview_replaces_region(preview) {
            preview_cache.seed_from_image(
                persistent_image,
                source_render_size,
                logical_size,
                source_config.mode == InkAntialiasingMode::Supersample,
            );
        } else {
            preview_cache.clear();
        }
        preview_cache.draw(preview);
        let composite = PreviewComposite {
            image: preview_cache.snapshot(),
            origin: preview_cache.origin(),
            logical_size: preview_cache.logical_size(),
            render_size: preview_cache.render_size(),
            replace_region: preview_replaces_region(preview),
        };
        if self.preview_surface_pool.stats() != pool_stats_before {
            self.log_surface_pool_stats("活动预览资源变化");
        }
        Ok(composite)
    }

    /// 增强资源创建或扩展失败时立即恢复关闭模式并重放文档。
    fn fallback_to_off(&mut self, document: &InkDocument, detail: String) -> Result<(), AppError> {
        self.release_preview_cache();
        let size = self.ink_cache.logical_size();
        let mut cache = InkRenderCache::new(&mut self.gr_context, size, InkAntialiasingMode::Off)?;
        cache.sync(document);
        self.ink_cache = cache;
        self.ink_mode = InkAntialiasingMode::Off;
        self.preferred_ink_mode = InkAntialiasingMode::Off;
        self.ink_rendering_error = Some(detail);
        Ok(())
    }

    /// 把当前增强预览 surface 归还精确匹配资源池。
    fn release_preview_cache(&mut self) {
        if let Some(preview_cache) = self.preview_cache.take() {
            preview_cache.release(&mut self.preview_surface_pool);
        }
    }

    /// 在 debug 级别记录预览池容量、命中率和淘汰数。
    fn log_surface_pool_stats(&self, message: &'static str) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }
        let stats = self.preview_surface_pool.stats();
        tracing::debug!(
            event = message,
            idle_surfaces = stats.idle_count,
            estimated_bytes = stats.estimated_bytes,
            reused = stats.reused_count,
            created = stats.created_count,
            evicted = stats.eviction_count,
            hit_rate = stats.hit_rate(),
            "预览资源池状态"
        );
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

/// 按请求模式创建持久墨迹和基础活动预览资源，增强失败时只回退到关闭模式。
fn create_ink_resources(
    context: &mut DirectContext,
    size: [u32; 2],
    requested_mode: InkAntialiasingMode,
) -> Result<InkResources, AppError> {
    let requested_result = InkRenderCache::new(context, size, requested_mode);
    let cache = match requested_result {
        Ok(cache) => cache,
        Err(error) if requested_mode != InkAntialiasingMode::Off => {
            let cache = InkRenderCache::new(context, size, InkAntialiasingMode::Off)?;
            return Ok(InkResources {
                cache,
                preview_cache: None,
                mode: InkAntialiasingMode::Off,
                error: Some(format!(
                    "墨迹抗锯齿 {} 初始化失败，已关闭：{error}",
                    requested_mode.label()
                )),
            });
        }
        Err(error) => return Err(error),
    };

    Ok(InkResources {
        cache,
        preview_cache: None,
        mode: requested_mode,
        error: None,
    })
}

/// 返回当前应用资源模式应使用的实际墨迹质量。
const fn desired_ink_mode(
    annotation_resources_enabled: bool,
    preferred_mode: InkAntialiasingMode,
) -> InkAntialiasingMode {
    if annotation_resources_enabled {
        preferred_mode
    } else {
        InkAntialiasingMode::Off
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 idle 模式始终使用最小 Off 资源，同时保留用户首选配置。
    #[test]
    fn idle_resource_mode_uses_off_quality() {
        assert_eq!(
            desired_ink_mode(false, InkAntialiasingMode::Supersample),
            InkAntialiasingMode::Off
        );
        assert_eq!(
            desired_ink_mode(false, InkAntialiasingMode::Msaa),
            InkAntialiasingMode::Off
        );
    }

    /// 验证批注模式恢复用户首选抗锯齿配置。
    #[test]
    fn annotation_resource_mode_restores_preferred_quality() {
        assert_eq!(
            desired_ink_mode(true, InkAntialiasingMode::Supersample),
            InkAntialiasingMode::Supersample
        );
    }
}
