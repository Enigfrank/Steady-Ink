use egui::{Color32, CornerRadius, FontId, Frame, Margin, Response, Stroke, TextStyle, Ui};

pub const INTERFACE_SCALE: f64 = 0.8;
pub const TOOLBAR_ZOOM_FACTOR: f32 = INTERFACE_SCALE as f32;
pub const SETTINGS_ZOOM_FACTOR: f32 = 1.0;
pub const OPAQUE_INTERFACE_ALPHA: u8 = u8::MAX;

/// 浮动表面在材质和易读模式下承担的层级角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialRole {
    Floating,
    Popover,
    Page,
    Control,
}

/// 同一层级的填充、状态和边框颜色，避免视图各自判断透明度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterialPalette {
    pub fill: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub selected: Color32,
    pub disabled: Color32,
    pub border: Color32,
    pub selected_border: Color32,
}

/// 一组按页面比例计算的界面尺寸，避免设置页 100% 比例影响其他 80% 工具界面。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InterfaceMetrics {
    pub scale: f32,
    pub text_xs: f32,
    pub text_sm: f32,
    pub text_base: f32,
    pub option_text: f32,
    pub text_lg: f32,
    pub text_xl: f32,
    pub space_1: f32,
    pub space_2: f32,
    pub space_3: f32,
    pub space_4: f32,
    pub space_6: f32,
    pub space_8: f32,
    pub margin_space_2: i8,
    pub margin_space_4: i8,
    pub card_radius: u8,
    pub button_radius: u8,
    pub touch_target: f32,
    pub icon_size: f32,
    pub diagnostic_label_width: f32,
    pub action_button_height: f32,
}

impl InterfaceMetrics {
    /// 从原设计比例生成符合固定字号和 4px 间距网格的页面尺寸。
    pub const fn from_scale(scale: f32) -> Self {
        Self {
            scale,
            text_xs: 12.0 * scale,
            text_sm: 14.0 * scale,
            text_base: 16.0 * scale,
            option_text: 14.0 * scale,
            text_lg: 20.0 * scale,
            text_xl: 24.0 * scale,
            space_1: 4.0 * scale,
            space_2: 8.0 * scale,
            space_3: 12.0 * scale,
            space_4: 16.0 * scale,
            space_6: 24.0 * scale,
            space_8: 32.0 * scale,
            margin_space_2: scaled_integer(8, scale) as i8,
            margin_space_4: scaled_integer(16, scale) as i8,
            card_radius: scaled_integer(8, scale),
            button_radius: scaled_integer(6, scale),
            touch_target: 64.0 * scale,
            icon_size: 20.0 * scale,
            diagnostic_label_width: 112.0 * scale,
            action_button_height: 48.0 * scale,
        }
    }

    /// 按当前页面比例缩放单个原始设计尺寸。
    pub const fn points(self, value: f32) -> f32 {
        value * self.scale
    }
}

pub const TOOL_METRICS: InterfaceMetrics = InterfaceMetrics::from_scale(1.0);
pub const SETTINGS_METRICS: InterfaceMetrics = InterfaceMetrics {
    option_text: 16.0,
    ..InterfaceMetrics::from_scale(1.0)
};

pub const COLOR_TEXT_PRIMARY: Color32 = Color32::from_rgb(17, 24, 39);
pub const COLOR_TEXT_SECONDARY: Color32 = Color32::from_rgb(107, 114, 128);
pub const COLOR_TEXT_TERTIARY: Color32 = Color32::from_rgb(156, 163, 175);
pub const COLOR_PRIMARY: Color32 = Color32::from_rgb(37, 99, 235);
pub const COLOR_BORDER_INPUT: Color32 = Color32::from_rgb(209, 213, 219);
pub const COLOR_ERROR: Color32 = Color32::from_rgb(220, 38, 38);
pub const COLOR_ERROR_SURFACE: Color32 = Color32::from_rgba_unmultiplied_const(220, 38, 38, 220);
pub const OPAQUE_COLOR_BACKGROUND: Color32 =
    Color32::from_rgba_unmultiplied_const(248, 249, 250, OPAQUE_INTERFACE_ALPHA);
pub const OPAQUE_COLOR_SURFACE: Color32 =
    Color32::from_rgba_unmultiplied_const(255, 255, 255, OPAQUE_INTERFACE_ALPHA);
pub const OPAQUE_COLOR_HOVER: Color32 =
    Color32::from_rgba_unmultiplied_const(243, 244, 246, OPAQUE_INTERFACE_ALPHA);

const MATERIAL_FLOATING_FILL: Color32 = Color32::from_rgba_unmultiplied_const(248, 249, 250, 220);
const MATERIAL_POPOVER_FILL: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 238);
const MATERIAL_PAGE_FILL: Color32 = Color32::from_rgba_unmultiplied_const(248, 249, 250, 246);
const MATERIAL_CONTROL_FILL: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 244);
const MATERIAL_HOVER_FILL: Color32 = Color32::from_rgba_unmultiplied_const(243, 244, 246, 250);
const MATERIAL_PRESSED_FILL: Color32 = Color32::from_rgba_unmultiplied_const(229, 231, 235, 255);
const MATERIAL_SELECTED_FILL: Color32 = Color32::from_rgba_unmultiplied_const(219, 234, 254, 255);
const MATERIAL_DISABLED_FILL: Color32 = Color32::from_rgba_unmultiplied_const(243, 244, 246, 236);
const MATERIAL_BORDER: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 230);
const READABLE_BORDER: Color32 = Color32::from_rgb(156, 163, 175);

/// 返回指定显示模式和表面层级的统一视觉状态。
pub const fn material_palette(readable_mode: bool, role: MaterialRole) -> MaterialPalette {
    let fill = if readable_mode {
        match role {
            MaterialRole::Floating | MaterialRole::Page => OPAQUE_COLOR_BACKGROUND,
            MaterialRole::Popover | MaterialRole::Control => OPAQUE_COLOR_SURFACE,
        }
    } else {
        match role {
            MaterialRole::Floating => MATERIAL_FLOATING_FILL,
            MaterialRole::Popover => MATERIAL_POPOVER_FILL,
            MaterialRole::Page => MATERIAL_PAGE_FILL,
            MaterialRole::Control => MATERIAL_CONTROL_FILL,
        }
    };
    MaterialPalette {
        fill,
        hover: if readable_mode {
            OPAQUE_COLOR_HOVER
        } else {
            MATERIAL_HOVER_FILL
        },
        pressed: MATERIAL_PRESSED_FILL,
        selected: MATERIAL_SELECTED_FILL,
        disabled: if readable_mode {
            OPAQUE_COLOR_HOVER
        } else {
            MATERIAL_DISABLED_FILL
        },
        border: if readable_mode {
            READABLE_BORDER
        } else {
            MATERIAL_BORDER
        },
        selected_border: COLOR_PRIMARY,
    }
}

/// 创建具有统一材质层级、圆角和像素对齐边框的外层框架。
pub fn material_frame(
    readable_mode: bool,
    role: MaterialRole,
    corner_radius: CornerRadius,
    inner_margin: Margin,
) -> Frame {
    let palette = material_palette(readable_mode, role);
    Frame::new()
        .fill(palette.fill)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(corner_radius)
        .inner_margin(inner_margin)
        .shadow(material_shadow(readable_mode, role))
}

/// 返回仅用于最外层浮动材质的克制阴影。
fn material_shadow(readable_mode: bool, role: MaterialRole) -> egui::epaint::Shadow {
    if readable_mode || !matches!(role, MaterialRole::Floating | MaterialRole::Popover) {
        return egui::epaint::Shadow {
            offset: [0, 0],
            blur: 0,
            spread: 0,
            color: Color32::TRANSPARENT,
        };
    }
    egui::epaint::Shadow {
        offset: [0, 2],
        blur: 8,
        spread: 0,
        color: Color32::from_black_alpha(36),
    }
}

/// 根据按钮的当前交互响应返回实体控件填充与边框。
pub fn button_colors(
    readable_mode: bool,
    selected: bool,
    response: &Response,
    enabled: bool,
) -> (Color32, Color32) {
    let palette = material_palette(readable_mode, MaterialRole::Control);
    let fill = if !enabled {
        palette.disabled
    } else if response.is_pointer_button_down_on() {
        palette.pressed
    } else if selected {
        palette.selected
    } else if response.hovered() {
        palette.hover
    } else {
        palette.fill
    };
    let border = if selected {
        palette.selected_border
    } else {
        palette.border
    };
    (fill, border)
}

pub const TEXT_SM: f32 = TOOL_METRICS.text_sm;
pub const TEXT_BASE: f32 = TOOL_METRICS.text_base;
pub const SPACE_1: f32 = TOOL_METRICS.space_1;
pub const SPACE_2: f32 = TOOL_METRICS.space_2;
pub const SPACE_3: f32 = TOOL_METRICS.space_3;
pub const SPACE_6: f32 = TOOL_METRICS.space_6;
pub const MARGIN_SPACE_2: i8 = TOOL_METRICS.margin_space_2;
pub const MARGIN_SPACE_4: i8 = TOOL_METRICS.margin_space_4;
pub const CARD_RADIUS: u8 = TOOL_METRICS.card_radius;
pub const CAPSULE_RADIUS: u8 = 16;
pub const BUTTON_RADIUS: u8 = TOOL_METRICS.button_radius;
pub const TOUCH_TARGET: f32 = TOOL_METRICS.touch_target;
pub const ICON_SIZE: f32 = TOOL_METRICS.icon_size;
pub const PAGE_NUMBER_WIDTH: f32 = scale_points(80.0);
pub const QUICK_SETTINGS_CONTENT_WIDTH: f32 = scale_points(424.0);
pub const SLIDESHOW_TOOLBAR_ANIMATION_SECONDS: f32 = 0.2;
pub const PERFORMANCE_OVERLAY_WIDTH: f32 = scale_points(240.0);
pub const PERFORMANCE_CHART_HEIGHT: f32 = scale_points(48.0);
pub const PERFORMANCE_OVERLAY_MARGIN: f32 = SPACE_6;

/// 将设置类界面的内建 egui 控件改为不透明表面，且不影响全局工具栏样式。
pub fn apply_widget_style(ui: &mut Ui, readable_mode: bool, role: MaterialRole) {
    let palette = material_palette(readable_mode, role);
    let visuals = &mut ui.style_mut().visuals;
    visuals.window_fill = palette.fill;
    visuals.window_stroke = Stroke::new(1.0, palette.border);
    visuals.widgets.noninteractive.bg_fill = palette.fill;
    visuals.widgets.inactive.bg_fill = palette.fill;
    visuals.widgets.hovered.bg_fill = palette.hover;
    visuals.widgets.active.bg_fill = palette.pressed;
    visuals.widgets.open.bg_fill = palette.selected;
}

/// 应用设置页的放大选项尺寸、布局间距和当前外观模式控件表面。
pub fn apply_settings_widget_style(ui: &mut Ui, readable_mode: bool) {
    let style = ui.style_mut();
    let spacing = &mut style.spacing;
    spacing.window_margin = egui::Margin::same(SETTINGS_METRICS.margin_space_2);
    spacing.menu_margin = egui::Margin::same(SETTINGS_METRICS.margin_space_2);
    spacing.item_spacing = egui::vec2(SETTINGS_METRICS.space_2, SETTINGS_METRICS.space_2);
    spacing.button_padding = egui::vec2(SETTINGS_METRICS.space_2, SETTINGS_METRICS.space_1);
    spacing.interact_size.y = SETTINGS_METRICS.touch_target;
    spacing.icon_width = SETTINGS_METRICS.icon_size;
    spacing.icon_width_inner = SETTINGS_METRICS.space_3;
    spacing.icon_spacing = SETTINGS_METRICS.space_2;
    for text_style in [TextStyle::Body, TextStyle::Button] {
        style.text_styles.insert(
            text_style,
            FontId::proportional(SETTINGS_METRICS.option_text),
        );
    }
    apply_widget_style(ui, readable_mode, MaterialRole::Page);
    let control_border = Stroke::new(
        1.0,
        material_palette(readable_mode, MaterialRole::Control).border,
    );
    let widgets = &mut ui.style_mut().visuals.widgets;
    widgets.noninteractive.bg_stroke = control_border;
    widgets.inactive.bg_stroke = control_border;
    widgets.hovered.bg_stroke = control_border;
    widgets.active.bg_stroke = control_border;
    widgets.open.bg_stroke = control_border;
}

/// 返回工具界面的原始 egui 逻辑点；显示缩放由 Context zoom 统一处理。
pub const fn scale_points(value: f32) -> f32 {
    value
}

/// 按统一界面比例缩放 winit 使用的双精度逻辑点尺寸。
pub const fn scale_window_points(value: f64) -> f64 {
    value * INTERFACE_SCALE
}

/// 把整数 token 按指定比例四舍五入到最近的逻辑点。
const fn scaled_integer(value: u8, scale: f32) -> u8 {
    (value as f32 * scale + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::{MaterialRole, SETTINGS_METRICS, TOOL_METRICS, material_palette};

    #[test]
    fn readable_mode_uses_opaque_surfaces() {
        let palette = material_palette(true, MaterialRole::Floating);

        assert_eq!(palette.fill.a(), u8::MAX);
        assert_eq!(palette.border.a(), u8::MAX);
    }

    #[test]
    fn material_mode_keeps_floating_surface_translucent() {
        let palette = material_palette(false, MaterialRole::Floating);

        assert!(palette.fill.a() < u8::MAX);
        assert_ne!(palette.selected, palette.fill);
    }

    /// 验证只放大设置页选项文字，不改变工具界面的共享尺寸。
    #[test]
    fn settings_options_use_larger_text_than_tool_options() {
        assert_eq!(TOOL_METRICS.option_text, 14.0);
        assert_eq!(SETTINGS_METRICS.option_text, 16.0);
        assert_eq!(SETTINGS_METRICS.icon_size, 20.0);
        assert_eq!(SETTINGS_METRICS.touch_target, 64.0);
        assert_eq!(SETTINGS_METRICS.space_8, 32.0);
    }
}
