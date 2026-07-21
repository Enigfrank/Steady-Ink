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
        ActiveInkPreview, InkBounds, InkDocument, InkPreviewCache, InkRenderCache,
        InkSurfaceConfig, active_preview_bounds, draw_active_preview, draw_image_rect_logical,
        preview_replaces_region,
    },
    settings::InkAntialiasingMode,
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
    ink_mode: InkAntialiasingMode,
    ink_rendering_error: Option<String>,
    preview_cache: Option<InkPreviewCache>,
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
        let resources = create_ink_resources(&mut gr_context, size, requested_mode)?;
        let egui = EguiSkiaRenderer::new(event_loop, window_context.window());

        let applied_mode = resources.mode;
        let error = resources.error.clone();
        Ok((
            Self {
                egui,
                ink_cache: resources.cache,
                ink_mode: resources.mode,
                ink_rendering_error: resources.error,
                preview_cache: resources.preview_cache,
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
        self.window_surfaces.clear();
        self.gr_context
            .purge_unlocked_resources(gpu::PurgeResourceOptions::AllResources);
        window_context.recreate_swap_chain(physical_size)?;
        self.window_surfaces = create_window_surfaces(&mut self.gr_context, window_context, size)?;
        let resources = create_ink_resources(&mut self.gr_context, size, self.ink_mode)?;
        self.ink_cache = resources.cache;
        self.ink_mode = resources.mode;
        self.ink_rendering_error = resources.error;
        self.preview_cache = resources.preview_cache;
        Ok(())
    }

    /// 运行时切换墨迹抗锯齿模式，成功后由下一帧完整重放当前文档。
    pub fn set_ink_antialiasing(
        &mut self,
        window_context: &D3DWindowContext,
        requested_mode: InkAntialiasingMode,
    ) -> Result<(), AppError> {
        if requested_mode == self.ink_mode && self.ink_rendering_error.is_none() {
            return Ok(());
        }
        let size: [u32; 2] = window_context.window().inner_size().into();
        let resources = create_ink_resources(&mut self.gr_context, size, requested_mode)?;
        self.ink_cache = resources.cache;
        self.ink_mode = resources.mode;
        self.ink_rendering_error = resources.error;
        self.preview_cache = resources.preview_cache;
        Ok(())
    }

    /// 返回当前实际生效的墨迹抗锯齿模式。
    pub const fn ink_antialiasing_mode(&self) -> InkAntialiasingMode {
        self.ink_mode
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

        let mut ink_image = self.ink_cache.snapshot();
        let mut preview_composite = None;
        if active_preview.is_none() && self.ink_mode != InkAntialiasingMode::Off {
            let reset_error = if let Some(preview_cache) = self.preview_cache.as_mut() {
                preview_cache
                    .reset_to_base(&mut self.gr_context, self.ink_cache.logical_size())
                    .err()
            } else {
                Some(AppError::Graphics(
                    "增强墨迹模式缺少基础预览 surface".to_owned(),
                ))
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
        let bounds = active_preview_bounds(preview)
            .ok_or_else(|| AppError::Graphics("活动墨迹预览没有有效采样".to_owned()))?;
        let logical_size = self.ink_cache.logical_size();
        let source_render_size = self.ink_cache.render_size();
        let source_config = self.ink_cache.config();
        let preview_cache = self
            .preview_cache
            .as_mut()
            .ok_or_else(|| AppError::Graphics("增强墨迹模式缺少活动预览 surface".to_owned()))?;
        preview_cache.ensure(&mut self.gr_context, bounds, logical_size)?;
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
        Ok(PreviewComposite {
            image: preview_cache.snapshot(),
            origin: preview_cache.origin(),
            logical_size: preview_cache.logical_size(),
            render_size: preview_cache.render_size(),
            replace_region: preview_replaces_region(preview),
        })
    }

    /// 增强资源创建或扩展失败时立即恢复关闭模式并重放文档。
    fn fallback_to_off(&mut self, document: &InkDocument, detail: String) -> Result<(), AppError> {
        let size = self.ink_cache.logical_size();
        let mut cache = InkRenderCache::new(&mut self.gr_context, size, InkAntialiasingMode::Off)?;
        cache.sync(document);
        self.ink_cache = cache;
        self.ink_mode = InkAntialiasingMode::Off;
        self.ink_rendering_error = Some(detail);
        self.preview_cache = None;
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

    if requested_mode == InkAntialiasingMode::Off {
        return Ok(InkResources {
            cache,
            preview_cache: None,
            mode: InkAntialiasingMode::Off,
            error: None,
        });
    }

    let config = InkSurfaceConfig::for_mode(requested_mode);
    let (origin, preview_size) = preview_region_for_window(size);
    match InkPreviewCache::new(context, origin, preview_size, config) {
        Ok(preview_cache) => Ok(InkResources {
            cache,
            preview_cache: Some(preview_cache),
            mode: requested_mode,
            error: None,
        }),
        Err(error) => {
            let off_cache = InkRenderCache::new(context, size, InkAntialiasingMode::Off)?;
            Ok(InkResources {
                cache: off_cache,
                preview_cache: None,
                mode: InkAntialiasingMode::Off,
                error: Some(format!(
                    "墨迹抗锯齿 {} 的活动预览初始化失败，已关闭：{error}",
                    requested_mode.label()
                )),
            })
        }
    }
}

/// 返回启动时覆盖窗口左上角的基础 512px 预览区域。
fn preview_region_for_window(size: [u32; 2]) -> (crate::ink::CanvasPoint, [u32; 2]) {
    (
        crate::ink::CanvasPoint::new(0.0, 0.0),
        [size[0].clamp(1, 512), size[1].clamp(1, 512)],
    )
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
