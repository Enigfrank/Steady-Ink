use std::{ffi::CString, sync::Arc};

use egui_glow::{EguiGlow, EventResponse, glow};
use skia_safe::{
    Color, ColorType, Surface,
    gpu::{self, DirectContext, SurfaceOrigin, backend_render_targets, gl::FramebufferInfo},
};
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::Window};

use crate::{
    error::AppError,
    ink::{ActiveInkPreview, InkBounds, InkDocument, InkRenderCache},
    ui,
    window::GlWindowContext,
};

/// 在同一 OpenGL framebuffer 中按 Skia 后 egui 的顺序合成一帧。
pub struct Compositor {
    egui: EguiGlow,
    ink_cache: InkRenderCache,
    window_surface: Surface,
    gr_context: DirectContext,
    framebuffer_info: FramebufferInfo,
    num_samples: usize,
    stencil_size: usize,
}

impl Compositor {
    /// 从已经激活的 glutin 上下文创建 Skia 和 egui 共享呈现器。
    pub fn new(
        event_loop: &ActiveEventLoop,
        gl_window: &GlWindowContext,
    ) -> Result<Self, AppError> {
        let interface = gpu::gl::Interface::new_load_with(|name| {
            if name == "eglGetCurrentDisplay" {
                return std::ptr::null();
            }
            CString::new(name)
                .map(|name| gl_window.get_proc_address(name.as_c_str()))
                .unwrap_or(std::ptr::null())
        })
        .ok_or_else(|| AppError::Graphics("无法创建 Skia OpenGL interface".to_owned()))?;
        let mut gr_context = gpu::direct_contexts::make_gl(interface, None)
            .ok_or_else(|| AppError::Graphics("无法创建 Skia DirectContext".to_owned()))?;
        let gl = gl_window.gl();
        let framebuffer_info = read_framebuffer_info(&gl);
        let size: [u32; 2] = gl_window.window().inner_size().into();
        let window_surface = create_window_surface(
            &mut gr_context,
            size,
            framebuffer_info,
            gl_window.num_samples(),
            gl_window.stencil_size(),
        )?;
        let ink_cache = InkRenderCache::new(&mut gr_context, size)?;
        let egui = EguiGlow::new(
            event_loop,
            Arc::clone(&gl),
            None,
            Some(gl_window.window().scale_factor() as f32),
            true,
        );
        ui::configure_context(&egui.egui_ctx);

        Ok(Self {
            egui,
            ink_cache,
            window_surface,
            gr_context,
            framebuffer_info,
            num_samples: gl_window.num_samples(),
            stencil_size: gl_window.stencil_size(),
        })
    }

    /// 把 winit 事件转交给 egui，并返回该事件是否需要重绘或已被 UI 消费。
    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> EventResponse {
        self.egui.on_window_event(window, event)
    }

    /// 执行本帧 egui 布局，但暂不修改 OpenGL framebuffer。
    pub fn run_ui(&mut self, window: &Window, run_ui: impl FnMut(&mut egui::Ui)) {
        self.egui.run(window, run_ui);
    }

    /// 返回 egui 上下文，供等待型事件循环安装按需重绘回调。
    pub const fn egui_context(&self) -> &egui::Context {
        &self.egui.egui_ctx
    }

    /// 清空 egui 当前指针状态，供原生手掌分类取消已暂存的 UI 接触。
    pub fn cancel_egui_pointer(&self) {
        self.egui
            .egui_ctx
            .input_mut(|input| input.pointer = egui::PointerState::default());
    }

    /// 在尺寸变化后重建窗口 Skia surface 和唯一活动页 GPU 墨迹层。
    pub fn resize(&mut self, size: [u32; 2]) -> Result<(), AppError> {
        self.window_surface = create_window_surface(
            &mut self.gr_context,
            size,
            self.framebuffer_info,
            self.num_samples,
            self.stencil_size,
        )?;
        self.ink_cache = InkRenderCache::new(&mut self.gr_context, size)?;
        Ok(())
    }

    /// 完成透明清理、Skia 墨迹、Skia flush 和 egui UI 的一帧合成。
    pub fn paint(
        &mut self,
        window: &Window,
        document: &InkDocument,
        active_preview: Option<ActiveInkPreview<'_>>,
    ) {
        self.gr_context.reset(None);
        self.ink_cache.sync(document);

        let canvas = self.window_surface.canvas();
        canvas.clear(Color::TRANSPARENT);
        let ink_image = self.ink_cache.snapshot();
        canvas.draw_image(ink_image, (0.0, 0.0), None);
        if let Some(preview) = active_preview {
            crate::ink::renderer::draw_active_preview(canvas, preview);
        }
        self.gr_context.flush_and_submit();

        self.egui.paint(window);
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

impl Drop for Compositor {
    /// 在 OpenGL 窗口销毁前释放 egui 和 Skia GPU 资源。
    fn drop(&mut self) {
        self.egui.destroy();
        self.gr_context.release_resources_and_abandon();
    }
}

/// 包装当前默认 framebuffer 为 Skia 窗口 surface。
fn create_window_surface(
    context: &mut DirectContext,
    size: [u32; 2],
    framebuffer_info: FramebufferInfo,
    num_samples: usize,
    stencil_size: usize,
) -> Result<Surface, AppError> {
    let dimensions = (
        i32::try_from(size[0].max(1)).map_err(|error| AppError::Graphics(error.to_string()))?,
        i32::try_from(size[1].max(1)).map_err(|error| AppError::Graphics(error.to_string()))?,
    );
    let target =
        backend_render_targets::make_gl(dimensions, num_samples, stencil_size, framebuffer_info);
    gpu::surfaces::wrap_backend_render_target(
        context,
        &target,
        SurfaceOrigin::BottomLeft,
        ColorType::RGBA8888,
        None,
        None,
    )
    .ok_or_else(|| AppError::Graphics("无法创建 Skia 窗口 surface".to_owned()))
}

/// 读取当前 OpenGL 默认 framebuffer 标识和 RGBA8 格式。
fn read_framebuffer_info(gl: &glow::Context) -> FramebufferInfo {
    use glow::HasContext as _;

    // SAFETY: 调用发生在已经激活的 OpenGL 上下文线程上。
    let fboid = unsafe { gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) };
    FramebufferInfo {
        fboid: u32::try_from(fboid).unwrap_or_default(),
        format: gpu::gl::Format::RGBA8.into(),
        ..Default::default()
    }
}
