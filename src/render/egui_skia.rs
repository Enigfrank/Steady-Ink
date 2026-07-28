use std::collections::HashMap;

use egui::{
    ClippedPrimitive, ImageData, TextureFilter, TextureId, TexturesDelta, ViewportId,
    ViewportOutput,
    epaint::{Mesh16, Primitive, Vertex, WHITE_UV},
};
use egui_winit::EventResponse;
use skia_safe::{
    AlphaType, BlendMode, Canvas, ClipOp, Color, ColorType, Data, FilterMode, Image, ImageInfo,
    M44, Matrix, MipmapMode, Paint, Picture, PictureRecorder, Point, Rect, SamplingOptions,
    TileMode, Vertices, vertices::VertexMode,
};
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::Window};

use crate::{error::AppError, ui};

const RETAINED_UI_PICTURE_BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// egui 纹理及其对应的 Skia shader paint。
struct TexturePaint {
    image: Image,
    paint: Paint,
}

/// 保存最近一个已转换完成、可精确复用的 egui 显示列表。
struct RetainedUiFrame {
    shapes: Vec<egui::epaint::ClippedShape>,
    pixels_per_point: f32,
    target_size: [i32; 2],
    picture: Picture,
}

impl RetainedUiFrame {
    /// 判断输入是否与录制该显示列表时的完整绘制条件一致。
    fn matches(
        &self,
        shapes: &[egui::epaint::ClippedShape],
        pixels_per_point: f32,
        target_size: [i32; 2],
    ) -> bool {
        self.shapes == shapes
            && self.pixels_per_point.to_bits() == pixels_per_point.to_bits()
            && self.target_size == target_size
    }
}

/// UI 线程交给 Skia painter 的单帧 owned 输出。
pub struct EguiFrame {
    pub shapes: Vec<egui::epaint::ClippedShape>,
    pub pixels_per_point: f32,
    pub texture_deltas: Vec<TexturesDelta>,
}

/// 在事件线程使用 egui-winit 收集输入并执行 UI 布局。
pub struct EguiUiState {
    context: egui::Context,
    state: egui_winit::State,
    viewport_info: egui::ViewportInfo,
}

/// 在渲染线程把 egui mesh 和纹理绘制到 Skia canvas。
pub struct EguiSkiaPainter {
    context: egui::Context,
    textures: HashMap<TextureId, TexturePaint>,
    retained_frame: Option<RetainedUiFrame>,
}

impl EguiUiState {
    /// 创建与唯一 winit 根 viewport 绑定的 egui 输入和布局状态。
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
        }
    }

    /// 把 winit 事件转交给 egui，并返回事件消费和重绘状态。
    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> EventResponse {
        self.state.on_window_event(window, event)
    }

    /// 执行本帧 egui 布局并返回渲染线程需要的 owned frame。
    pub fn run_ui(&mut self, window: &Window, run_ui: impl FnMut(&mut egui::Ui)) -> EguiFrame {
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
        EguiFrame {
            shapes,
            pixels_per_point,
            texture_deltas: vec![textures_delta],
        }
    }

    /// 返回 egui 上下文，供等待型事件循环和 painter 共享。
    pub const fn context(&self) -> &egui::Context {
        &self.context
    }

    /// 清空 egui 当前指针状态，供原生手掌分类取消已暂存的 UI 接触。
    pub fn cancel_pointer(&self) {
        self.context
            .input_mut(|input| input.pointer = egui::PointerState::default());
    }
}

impl EguiSkiaPainter {
    /// 创建只持有 Skia 绘制资源的 egui painter。
    pub fn new(context: egui::Context) -> Self {
        Self {
            context,
            textures: HashMap::new(),
            retained_frame: None,
        }
    }

    /// 把一个 owned egui frame 的纹理和 mesh 绘制到当前 Skia canvas。
    pub fn paint(&mut self, canvas: &Canvas, frame: EguiFrame) -> Result<(), AppError> {
        let EguiFrame {
            shapes,
            pixels_per_point,
            mut texture_deltas,
        } = frame;
        let can_reuse_retained = texture_deltas.iter().all(texture_delta_allows_reuse);
        let can_retain_recording = texture_deltas.iter().all(texture_delta_allows_retention);
        if !can_reuse_retained {
            self.retained_frame = None;
        }
        let mut textures_delta = texture_deltas.pop().unwrap_or_default();
        for mut skipped_delta in texture_deltas {
            self.apply_texture_sets(&mut skipped_delta)?;
            self.apply_texture_frees(&mut skipped_delta);
        }
        self.apply_texture_sets(&mut textures_delta)?;

        let target = canvas.base_layer_size();
        let target_size = [target.width, target.height];
        let cache_hit = can_reuse_retained
            && self
                .retained_frame
                .as_ref()
                .is_some_and(|retained| retained.matches(&shapes, pixels_per_point, target_size));

        if cache_hit {
            self.retained_frame
                .as_ref()
                .expect("命中状态必须持有 egui retained frame")
                .picture
                .playback(canvas);
        } else {
            self.retained_frame = None;
            let primitives = self.context.tessellate(shapes.clone(), pixels_per_point);
            let recording_bounds =
                Rect::from_xywh(0.0, 0.0, target_size[0] as f32, target_size[1] as f32);
            let mut recorder = PictureRecorder::new();
            self.paint_primitives(
                recorder.begin_recording(recording_bounds, false),
                primitives,
                pixels_per_point,
            )?;
            let picture = recorder
                .finish_recording_as_picture(None)
                .ok_or_else(|| AppError::Graphics("无法完成 egui Skia Picture 录制".to_owned()))?;
            let picture_bytes = picture.approximate_bytes_used();
            picture.playback(canvas);
            if can_retain_recording && retained_picture_fits_budget(picture_bytes) {
                self.retained_frame = Some(RetainedUiFrame {
                    shapes,
                    pixels_per_point,
                    target_size,
                    picture,
                });
            }
        }

        self.apply_texture_frees(&mut textures_delta);
        Ok(())
    }

    /// 保守估算当前 egui 纹理可能占用的 RGBA GPU 上传字节数。
    pub(crate) fn estimated_texture_bytes(&self) -> usize {
        self.textures.values().fold(0_usize, |total, texture| {
            let width = usize::try_from(texture.image.width().max(0)).unwrap_or(usize::MAX);
            let height = usize::try_from(texture.image.height().max(0)).unwrap_or(usize::MAX);
            total.saturating_add(width.saturating_mul(height).saturating_mul(4))
        })
    }

    /// 按 egui 产生顺序应用一个纹理 delta 的 set 阶段。
    fn apply_texture_sets(&mut self, textures_delta: &mut TexturesDelta) -> Result<(), AppError> {
        for (id, image_delta) in textures_delta.set.drain(..) {
            self.update_texture(id, image_delta)?;
        }
        Ok(())
    }

    /// 在对应帧完成后应用一个纹理 delta 的 free 阶段。
    fn apply_texture_frees(&mut self, textures_delta: &mut TexturesDelta) {
        for id in textures_delta.free.drain(..) {
            self.textures.remove(&id);
        }
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
        primitives: Vec<ClippedPrimitive>,
        pixels_per_point: f32,
    ) -> Result<(), AppError> {
        let mut white_paint = Paint::default();
        white_paint.set_color(Color::WHITE);

        for ClippedPrimitive {
            clip_rect: primitive_clip_rect,
            primitive,
        } in primitives
        {
            let Primitive::Mesh(mesh) = primitive else {
                tracing::debug!("忽略未注册的 egui paint callback");
                continue;
            };
            let clip_rect = Rect::new(
                primitive_clip_rect.min.x,
                primitive_clip_rect.min.y,
                primitive_clip_rect.max.x,
                primitive_clip_rect.max.y,
            );
            let clipped_canvas = skia_safe::AutoCanvasRestore::guard(canvas, true);
            clipped_canvas.set_matrix(M44::new_identity().set_scale(
                pixels_per_point,
                pixels_per_point,
                1.0,
            ));
            clipped_canvas.clip_rect(clip_rect, ClipOp::Intersect, true);

            for mesh in mesh
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

/// 只有完全没有纹理生命周期变更时才允许回放旧显示列表。
fn texture_delta_allows_reuse(textures_delta: &TexturesDelta) -> bool {
    textures_delta.is_empty()
}

/// 纹理释放会让当前显示列表持有过期资源，因此禁止保留该帧。
fn texture_delta_allows_retention(textures_delta: &TexturesDelta) -> bool {
    textures_delta.free.is_empty()
}

/// 判断 Picture 自报内存是否仍在单帧 retained 缓存预算内。
const fn retained_picture_fits_budget(approximate_bytes_used: usize) -> bool {
    approximate_bytes_used <= RETAINED_UI_PICTURE_BUDGET_BYTES
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, Pos2, Shape, TextureOptions, Vec2};

    /// 创建用于 retained frame key 比较的固定圆形。
    fn test_shape(center_x: f32) -> egui::epaint::ClippedShape {
        egui::epaint::ClippedShape {
            clip_rect: egui::Rect::from_min_size(Pos2::ZERO, Vec2::splat(64.0)),
            shape: Shape::circle_filled(Pos2::new(center_x, 16.0), 8.0, Color32::WHITE),
        }
    }

    /// 创建只用于 key 测试、不包含实际绘制指令的 retained frame。
    fn test_retained_frame(
        shapes: Vec<egui::epaint::ClippedShape>,
        pixels_per_point: f32,
        target_size: [i32; 2],
    ) -> RetainedUiFrame {
        RetainedUiFrame {
            shapes,
            pixels_per_point,
            target_size,
            picture: Picture::new_placeholder(Rect::from_xywh(
                0.0,
                0.0,
                target_size[0] as f32,
                target_size[1] as f32,
            )),
        }
    }

    /// 验证完全相同的形状、DPI 和目标尺寸可命中 retained frame。
    #[test]
    fn retained_frame_matches_identical_input() {
        let shapes = vec![test_shape(16.0)];
        let retained = test_retained_frame(shapes.clone(), 1.5, [800, 600]);

        assert!(retained.matches(&shapes, 1.5, [800, 600]));
    }

    /// 验证形状、DPI 位模式或目标尺寸任一变化都会使缓存失效。
    #[test]
    fn retained_frame_rejects_any_key_change() {
        let shapes = vec![test_shape(16.0)];
        let retained = test_retained_frame(shapes.clone(), 0.0, [800, 600]);

        assert!(!retained.matches(&[test_shape(17.0)], 0.0, [800, 600]));
        assert!(!retained.matches(&shapes, -0.0, [800, 600]));
        assert!(!retained.matches(&shapes, 0.0, [801, 600]));
    }

    /// 验证纹理 set 会禁止旧缓存命中，而 free 还会禁止保留当前帧。
    #[test]
    fn texture_deltas_follow_reuse_and_retention_contract() {
        let empty = TexturesDelta::default();
        assert!(texture_delta_allows_reuse(&empty));
        assert!(texture_delta_allows_retention(&empty));

        let mut set = TexturesDelta::default();
        set.set.push((
            TextureId::Managed(1),
            egui::epaint::ImageDelta::full(
                egui::ColorImage::filled([1, 1], Color32::WHITE),
                TextureOptions::LINEAR,
            ),
        ));
        assert!(!texture_delta_allows_reuse(&set));
        assert!(texture_delta_allows_retention(&set));

        let free = TexturesDelta {
            set: Vec::new(),
            free: vec![TextureId::Managed(1)],
        };
        assert!(!texture_delta_allows_reuse(&free));
        assert!(!texture_delta_allows_retention(&free));
    }

    /// 验证 Picture 仅在包含上限的 4MB 预算内被保留。
    #[test]
    fn retained_picture_budget_is_inclusive() {
        assert!(retained_picture_fits_budget(
            RETAINED_UI_PICTURE_BUDGET_BYTES
        ));
        assert!(!retained_picture_fits_budget(
            RETAINED_UI_PICTURE_BUDGET_BYTES + 1
        ));
    }

    /// 验证字体 mesh 在三角形边界按白色 texel 使用方式分组。
    #[test]
    fn font_mesh_groups_white_and_textured_triangles() {
        let white_vertex = Vertex {
            pos: Pos2::ZERO,
            uv: WHITE_UV,
            color: Color32::WHITE,
        };
        let textured_vertex = Vertex {
            uv: Pos2::new(0.5, 0.5),
            ..white_vertex
        };
        let mesh = Mesh16 {
            indices: (0..9).collect(),
            vertices: vec![
                white_vertex,
                white_vertex,
                white_vertex,
                textured_vertex,
                textured_vertex,
                textured_vertex,
                white_vertex,
                white_vertex,
                white_vertex,
            ],
            texture_id: TextureId::default(),
        };

        let groups = split_font_mesh_by_texture_usage(mesh);

        assert_eq!(groups.len(), 3);
        assert!(font_mesh_uses_white_paint(&groups[0]));
        assert!(!font_mesh_uses_white_paint(&groups[1]));
        assert!(font_mesh_uses_white_paint(&groups[2]));
        assert!(groups.iter().all(|group| group.indices.len() == 3));
    }

    /// 验证白色 texel 向量透明顶点继承可见边缘 RGB，纹理 mesh 不受影响。
    #[test]
    fn transparent_edge_rgb_only_extends_for_white_vector_meshes() {
        let opaque_red = Vertex {
            pos: Pos2::ZERO,
            uv: WHITE_UV,
            color: Color32::RED,
        };
        let transparent = Vertex {
            pos: Pos2::new(1.0, 0.0),
            uv: WHITE_UV,
            color: Color32::TRANSPARENT,
        };
        let vector_mesh = Mesh16 {
            indices: vec![0, 1, 2],
            vertices: vec![opaque_red, transparent, transparent],
            texture_id: TextureId::default(),
        };

        let vector_colors = skia_vertex_colors(&vector_mesh);
        assert_eq!(vector_colors[1], Color::from_argb(0, 255, 0, 0));

        let mut textured_mesh = vector_mesh;
        textured_mesh.texture_id = TextureId::Managed(2);
        let textured_colors = skia_vertex_colors(&textured_mesh);
        assert_eq!(textured_colors[1], Color::TRANSPARENT);
    }
}
