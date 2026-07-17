use std::collections::HashMap;

use egui::{
    ClippedPrimitive, ImageData, TextureFilter, TextureId, TexturesDelta, ViewportId,
    ViewportOutput,
    epaint::{Mesh16, Primitive, Vertex, WHITE_UV},
};
use egui_winit::EventResponse;
use skia_safe::{
    AlphaType, BlendMode, Canvas, ClipOp, Color, ColorType, Data, FilterMode, Image, ImageInfo,
    M44, Matrix, MipmapMode, Paint, Point, Rect, SamplingOptions, TileMode, Vertices,
    vertices::VertexMode,
};
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::Window};

use crate::{error::AppError, ui};

/// egui 纹理及其对应的 Skia shader paint。
struct TexturePaint {
    image: Image,
    paint: Paint,
}

/// 使用 egui-winit 收集输入，并把 egui mesh 绘制到任意 Skia canvas。
pub struct EguiSkiaRenderer {
    context: egui::Context,
    state: egui_winit::State,
    viewport_info: egui::ViewportInfo,
    shapes: Vec<egui::epaint::ClippedShape>,
    pixels_per_point: f32,
    textures_delta: TexturesDelta,
    textures: HashMap<TextureId, TexturePaint>,
}

impl EguiSkiaRenderer {
    /// 创建与唯一 winit 根 viewport 绑定的 egui 输入和 Skia 绘制状态。
    pub fn new(event_loop: &ActiveEventLoop, window: &Window) -> Self {
        let context = egui::Context::default();
        let state = egui_winit::State::new(
            context.clone(),
            ViewportId::ROOT,
            event_loop,
            Some(window.scale_factor() as f32),
            event_loop.system_theme(),
            None,
        );
        ui::configure_context(&context);
        Self {
            context,
            state,
            viewport_info: egui::ViewportInfo::default(),
            shapes: Vec::new(),
            pixels_per_point: window.scale_factor() as f32,
            textures_delta: TexturesDelta::default(),
            textures: HashMap::new(),
        }
    }

    /// 把 winit 事件转交给 egui，并返回事件消费和重绘状态。
    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> EventResponse {
        self.state.on_window_event(window, event)
    }

    /// 执行本帧 egui 布局并保存稍后绘制所需的 mesh 和纹理增量。
    pub fn run_ui(&mut self, window: &Window, run_ui: impl FnMut(&mut egui::Ui)) {
        let raw_input = self.state.take_egui_input(window);
        let egui::FullOutput {
            platform_output,
            textures_delta,
            shapes,
            pixels_per_point,
            viewport_output,
        } = self.context.run_ui(raw_input, run_ui);

        if viewport_output.len() > 1 {
            tracing::warn!("当前 DirectComposition renderer 不支持额外 egui viewport");
        }
        for (_, ViewportOutput { commands, .. }) in viewport_output {
            let mut actions_requested = Default::default();
            egui_winit::process_viewport_commands(
                &self.context,
                &mut self.viewport_info,
                commands,
                window,
                &mut actions_requested,
            );
            for action in actions_requested {
                tracing::warn!(
                    ?action,
                    "当前 DirectComposition renderer 不支持该 viewport 动作"
                );
            }
        }

        self.state.handle_platform_output(window, platform_output);
        self.shapes = shapes;
        self.pixels_per_point = pixels_per_point;
        self.textures_delta.append(textures_delta);
    }

    /// 把上次布局产生的纹理和 mesh 绘制到当前 Skia canvas。
    pub fn paint(&mut self, canvas: &Canvas) -> Result<(), AppError> {
        let mut textures_delta = std::mem::take(&mut self.textures_delta);
        for (id, image_delta) in textures_delta.set.drain(..) {
            self.update_texture(id, image_delta)?;
        }

        let shapes = std::mem::take(&mut self.shapes);
        let primitives = self.context.tessellate(shapes, self.pixels_per_point);
        self.paint_primitives(canvas, &primitives)?;

        for id in textures_delta.free.drain(..) {
            self.textures.remove(&id);
        }
        Ok(())
    }

    /// 返回 egui 上下文，供等待型事件循环安装按需重绘回调。
    pub const fn context(&self) -> &egui::Context {
        &self.context
    }

    /// 清空 egui 当前指针状态，供原生手掌分类取消已暂存的 UI 接触。
    pub fn cancel_pointer(&self) {
        self.context
            .input_mut(|input| input.pointer = egui::PointerState::default());
    }

    /// 创建或局部更新一个 egui 纹理，并重建对应的 Skia shader。
    fn update_texture(
        &mut self,
        id: TextureId,
        image_delta: egui::epaint::ImageDelta,
    ) -> Result<(), AppError> {
        let delta_image = image_to_skia(&image_delta.image)?;
        let image = if let Some([x, y]) = image_delta.pos {
            let old = self.textures.remove(&id).ok_or_else(|| {
                AppError::Graphics(format!("egui 局部更新引用了不存在的纹理 {id:?}"))
            })?;
            merge_texture_delta(old.image, delta_image, [x, y])?
        } else {
            delta_image
        };
        let sampling = SamplingOptions::new(
            texture_filter(image_delta.options.magnification),
            image_delta
                .options
                .mipmap_mode
                .map_or(MipmapMode::None, texture_mipmap),
        );
        let local_matrix = Matrix::scale((1.0 / image.width() as f32, 1.0 / image.height() as f32));
        let shader = image
            .to_shader((TileMode::Clamp, TileMode::Clamp), sampling, &local_matrix)
            .ok_or_else(|| AppError::Graphics("无法创建 egui Skia 纹理 shader".to_owned()))?;
        let mut paint = Paint::default();
        paint.set_shader(shader);
        paint.set_color(Color::WHITE);
        self.textures.insert(id, TexturePaint { image, paint });
        Ok(())
    }

    /// 按 egui clip rect、DPI scale 和纹理 id 绘制全部三角网格。
    fn paint_primitives(
        &self,
        canvas: &Canvas,
        primitives: &[ClippedPrimitive],
    ) -> Result<(), AppError> {
        let mut white_paint = Paint::default();
        white_paint.set_color(Color::WHITE);

        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                tracing::debug!("忽略未注册的 egui paint callback");
                continue;
            };
            let clip_rect = Rect::new(
                primitive.clip_rect.min.x,
                primitive.clip_rect.min.y,
                primitive.clip_rect.max.x,
                primitive.clip_rect.max.y,
            );
            let clipped_canvas = skia_safe::AutoCanvasRestore::guard(canvas, true);
            clipped_canvas.set_matrix(M44::new_identity().set_scale(
                self.pixels_per_point,
                self.pixels_per_point,
                1.0,
            ));
            clipped_canvas.clip_rect(clip_rect, ClipOp::Intersect, true);

            for mesh in mesh
                .clone()
                .split_to_u16()
                .into_iter()
                .flat_map(split_font_mesh_by_texture_usage)
            {
                let positions: Vec<_> = mesh
                    .vertices
                    .iter()
                    .map(|vertex| {
                        let x = if vertex.pos.x.is_finite() {
                            vertex.pos.x
                        } else {
                            0.0
                        };
                        let y = if vertex.pos.y.is_finite() {
                            vertex.pos.y
                        } else {
                            0.0
                        };
                        Point::new(x, y)
                    })
                    .collect();
                let texture_coordinates: Vec<_> = mesh
                    .vertices
                    .iter()
                    .map(|vertex| Point::new(vertex.uv.x, vertex.uv.y))
                    .collect();
                let colors = skia_vertex_colors(&mesh);
                let vertices = Vertices::new_copy(
                    VertexMode::Triangles,
                    &positions,
                    &texture_coordinates,
                    &colors,
                    Some(&mesh.indices),
                );
                let paint = if font_mesh_uses_white_paint(&mesh) {
                    &white_paint
                } else {
                    &self
                        .textures
                        .get(&mesh.texture_id)
                        .ok_or_else(|| {
                            AppError::Graphics(format!(
                                "egui mesh 引用了不存在的纹理 {:?}",
                                mesh.texture_id
                            ))
                        })?
                        .paint
                };
                clipped_canvas.draw_vertices(&vertices, BlendMode::Modulate, paint);
            }
        }
        Ok(())
    }
}

/// 把字体纹理中的纯色三角形与字形三角形分组，规避 Skia 相同 UV 采样缺陷。
fn split_font_mesh_by_texture_usage(mesh: Mesh16) -> Vec<Mesh16> {
    if mesh.texture_id != TextureId::default() {
        return vec![mesh];
    }

    debug_assert_eq!(mesh.indices.len() % 3, 0);
    let mut groups = Vec::<Mesh16>::new();
    let mut current_uses_white = None;
    for triangle in mesh.indices.chunks_exact(3) {
        let uses_white = triangle.iter().all(|index| {
            mesh.vertices
                .get(*index as usize)
                .is_some_and(vertex_uses_white_texel)
        });
        if current_uses_white != Some(uses_white) {
            groups.push(Mesh16 {
                indices: Vec::new(),
                vertices: Vec::new(),
                texture_id: mesh.texture_id,
            });
            current_uses_white = Some(uses_white);
        }
        let group = groups
            .last_mut()
            .expect("每个三角形都必须拥有目标 mesh 分组");
        for index in triangle {
            if let Some(vertex) = mesh.vertices.get(*index as usize) {
                group.vertices.push(*vertex);
                group.indices.push(group.indices.len() as u16);
            }
        }
    }
    groups
}

/// 判断字体 mesh 是否只使用 egui 约定的无纹理白色 texel。
fn font_mesh_uses_white_paint(mesh: &Mesh16) -> bool {
    mesh.texture_id == TextureId::default()
        && !mesh.vertices.is_empty()
        && mesh.vertices.iter().all(vertex_uses_white_texel)
}

/// 判断一个 egui 顶点是否引用字体纹理的固定白色 texel。
fn vertex_uses_white_texel(vertex: &Vertex) -> bool {
    vertex.uv == WHITE_UV
}

/// 转换 egui 顶点色，并为透明抗锯齿顶点延展同三角形的边缘 RGB。
fn skia_vertex_colors(mesh: &Mesh16) -> Vec<Color> {
    let mut colors: Vec<_> = mesh
        .vertices
        .iter()
        .map(|vertex| unpremultiplied_skia_color(vertex.color))
        .collect();
    if !font_mesh_uses_white_paint(mesh) {
        return colors;
    }

    for triangle in mesh.indices.chunks_exact(3) {
        let edge_color = triangle
            .iter()
            .filter_map(|index| mesh.vertices.get(*index as usize))
            .filter(|vertex| vertex.color.a() > 0)
            .max_by_key(|vertex| vertex.color.a())
            .map(|vertex| unpremultiplied_skia_color(vertex.color));
        let Some(edge_color) = edge_color else {
            continue;
        };

        for index in triangle {
            let index = *index as usize;
            if mesh
                .vertices
                .get(index)
                .is_some_and(|vertex| vertex.color == egui::Color32::TRANSPARENT)
            {
                colors[index] = Color::from_argb(0, edge_color.r(), edge_color.g(), edge_color.b());
            }
        }
    }
    colors
}

/// 把 egui ColorImage 或 FontImage 转成 Skia 预乘 alpha raster image。
fn image_to_skia(image: &ImageData) -> Result<Image, AppError> {
    let (width, height, pixels) = match image {
        ImageData::Color(image) => (
            image.width(),
            image.height(),
            image
                .pixels
                .iter()
                .flat_map(|pixel| pixel.to_array())
                .collect::<Vec<_>>(),
        ),
    };
    skia_safe::images::raster_from_data(
        &ImageInfo::new(
            (width as i32, height as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        ),
        Data::new_copy(&pixels),
        width * 4,
    )
    .ok_or_else(|| AppError::Graphics("无法创建 egui Skia 纹理".to_owned()))
}

/// 在较小 raster surface 上应用 egui 局部纹理更新，不触碰全屏 framebuffer。
fn merge_texture_delta(old: Image, delta: Image, position: [usize; 2]) -> Result<Image, AppError> {
    let mut surface = skia_safe::surfaces::raster_n32_premul((old.width(), old.height()))
        .ok_or_else(|| AppError::Graphics("无法创建 egui 局部纹理更新 surface".to_owned()))?;
    let canvas = surface.canvas();
    canvas.draw_image(&old, (0.0, 0.0), None);
    let rect = Rect::from_xywh(
        position[0] as f32,
        position[1] as f32,
        delta.width() as f32,
        delta.height() as f32,
    );
    let save_count = canvas.save();
    canvas.clip_rect(rect, ClipOp::Intersect, false);
    canvas.clear(Color::TRANSPARENT);
    canvas.draw_image(&delta, (position[0] as f32, position[1] as f32), None);
    canvas.restore_to_count(save_count);
    Ok(surface.image_snapshot())
}

/// 把 egui 纹理过滤选项映射为 Skia 过滤模式。
const fn texture_filter(filter: TextureFilter) -> FilterMode {
    match filter {
        TextureFilter::Nearest => FilterMode::Nearest,
        TextureFilter::Linear => FilterMode::Linear,
    }
}

/// 把 egui mipmap 过滤选项映射为 Skia mipmap 模式。
const fn texture_mipmap(filter: TextureFilter) -> MipmapMode {
    match filter {
        TextureFilter::Nearest => MipmapMode::Nearest,
        TextureFilter::Linear => MipmapMode::Linear,
    }
}

/// 把 egui 预乘 sRGBA 顶点色转换为 Skia vertices 需要的非预乘颜色。
fn unpremultiplied_skia_color(color: egui::Color32) -> Color {
    let [red, green, blue, alpha] = color.to_array();
    if alpha == 0 {
        return Color::TRANSPARENT;
    }
    let unpremultiply = |component: u8| {
        ((u16::from(component) * 255 + u16::from(alpha) / 2) / u16::from(alpha)).min(255) as u8
    };
    Color::from_argb(
        alpha,
        unpremultiply(red),
        unpremultiply(green),
        unpremultiply(blue),
    )
}
