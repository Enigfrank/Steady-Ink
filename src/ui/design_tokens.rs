use egui::{Color32, Stroke, Style, Ui};

pub const INTERFACE_SCALE: f64 = 0.8;
pub const SETTINGS_INTERFACE_SCALE: f32 = 1.0;
pub const INTERFACE_ALPHA: u8 = 128;
pub const OPAQUE_INTERFACE_ALPHA: u8 = u8::MAX;

/// 一组按页面比例计算的界面尺寸，避免设置页 100% 比例影响其他 80% 工具界面。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InterfaceMetrics {
    pub scale: f32,
    pub text_xs: f32,
    pub text_sm: f32,
    pub text_base: f32,
    pub text_lg: f32,
    pub text_xl: f32,
    pub space_1: f32,
    pub space_2: f32,
    pub space_3: f32,
    pub space_4: f32,
    pub space_6: f32,
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
            text_lg: 20.0 * scale,
            text_xl: 24.0 * scale,
            space_1: 4.0 * scale,
            space_2: 8.0 * scale,
            space_3: 12.0 * scale,
            space_4: 16.0 * scale,
            space_6: 24.0 * scale,
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

pub const TOOL_METRICS: InterfaceMetrics = InterfaceMetrics::from_scale(INTERFACE_SCALE as f32);
pub const SETTINGS_METRICS: InterfaceMetrics =
    InterfaceMetrics::from_scale(SETTINGS_INTERFACE_SCALE);

pub const COLOR_BACKGROUND: Color32 =
    Color32::from_rgba_unmultiplied_const(248, 249, 250, INTERFACE_ALPHA);
pub const COLOR_SURFACE: Color32 =
    Color32::from_rgba_unmultiplied_const(255, 255, 255, INTERFACE_ALPHA);
pub const COLOR_BORDER: Color32 =
    Color32::from_rgba_unmultiplied_const(229, 231, 235, INTERFACE_ALPHA);
pub const COLOR_TEXT_PRIMARY: Color32 = Color32::from_rgb(17, 24, 39);
pub const COLOR_TEXT_SECONDARY: Color32 = Color32::from_rgb(107, 114, 128);
pub const COLOR_TEXT_TERTIARY: Color32 = Color32::from_rgb(156, 163, 175);
pub const COLOR_SELECTED: Color32 =
    Color32::from_rgba_unmultiplied_const(229, 231, 235, INTERFACE_ALPHA);
pub const COLOR_HOVER: Color32 =
    Color32::from_rgba_unmultiplied_const(243, 244, 246, INTERFACE_ALPHA);
pub const COLOR_PRIMARY: Color32 = Color32::from_rgb(37, 99, 235);
pub const COLOR_ERROR: Color32 = Color32::from_rgb(220, 38, 38);
pub const COLOR_ERROR_SURFACE: Color32 =
    Color32::from_rgba_unmultiplied_const(220, 38, 38, INTERFACE_ALPHA);
pub const OPAQUE_COLOR_BACKGROUND: Color32 =
    Color32::from_rgba_unmultiplied_const(248, 249, 250, OPAQUE_INTERFACE_ALPHA);
pub const OPAQUE_COLOR_SURFACE: Color32 =
    Color32::from_rgba_unmultiplied_const(255, 255, 255, OPAQUE_INTERFACE_ALPHA);
pub const OPAQUE_COLOR_BORDER: Color32 =
    Color32::from_rgba_unmultiplied_const(229, 231, 235, OPAQUE_INTERFACE_ALPHA);
pub const OPAQUE_COLOR_SELECTED: Color32 =
    Color32::from_rgba_unmultiplied_const(229, 231, 235, OPAQUE_INTERFACE_ALPHA);
pub const OPAQUE_COLOR_HOVER: Color32 =
    Color32::from_rgba_unmultiplied_const(243, 244, 246, OPAQUE_INTERFACE_ALPHA);
pub const OPAQUE_COLOR_PRIMARY_SURFACE: Color32 =
    Color32::from_rgba_unmultiplied_const(37, 99, 235, OPAQUE_INTERFACE_ALPHA);

pub const TEXT_SM: f32 = TOOL_METRICS.text_sm;
pub const TEXT_BASE: f32 = TOOL_METRICS.text_base;
pub const SPACE_1: f32 = TOOL_METRICS.space_1;
pub const SPACE_2: f32 = TOOL_METRICS.space_2;
pub const SPACE_3: f32 = TOOL_METRICS.space_3;
pub const SPACE_6: f32 = TOOL_METRICS.space_6;
pub const MARGIN_SPACE_2: i8 = TOOL_METRICS.margin_space_2;
pub const MARGIN_SPACE_4: i8 = TOOL_METRICS.margin_space_4;
pub const CARD_RADIUS: u8 = TOOL_METRICS.card_radius;
pub const CAPSULE_RADIUS: u8 = scale_integer_points(16);
pub const BUTTON_RADIUS: u8 = TOOL_METRICS.button_radius;
pub const TOUCH_TARGET: f32 = TOOL_METRICS.touch_target;
pub const ICON_SIZE: f32 = TOOL_METRICS.icon_size;
pub const PAGE_NUMBER_WIDTH: f32 = scale_points(80.0);
pub const QUICK_SETTINGS_CONTENT_WIDTH: f32 = scale_points(424.0);
pub const SLIDESHOW_TOOLBAR_ANIMATION_SECONDS: f32 = 0.2;

/// 将设置类界面的内建 egui 控件改为不透明表面，且不影响全局工具栏样式。
pub fn apply_opaque_widget_style(ui: &mut Ui) {
    let visuals = &mut ui.style_mut().visuals;
    visuals.window_fill = OPAQUE_COLOR_BACKGROUND;
    visuals.window_stroke = Stroke::new(1.0, OPAQUE_COLOR_BORDER);
    visuals.widgets.noninteractive.bg_fill = OPAQUE_COLOR_SURFACE;
    visuals.widgets.inactive.bg_fill = OPAQUE_COLOR_SURFACE;
    visuals.widgets.hovered.bg_fill = OPAQUE_COLOR_HOVER;
    visuals.widgets.active.bg_fill = OPAQUE_COLOR_SELECTED;
    visuals.widgets.open.bg_fill = OPAQUE_COLOR_SELECTED;
}

/// 将当前继承的 80% egui 内建样式局部恢复为设置页 100% 尺寸并应用不透明表面。
pub fn apply_settings_widget_style(ui: &mut Ui) {
    scale_builtin_style(
        ui.style_mut(),
        SETTINGS_INTERFACE_SCALE / INTERFACE_SCALE as f32,
    );
    let spacing = &mut ui.style_mut().spacing;
    spacing.window_margin = egui::Margin::same(SETTINGS_METRICS.margin_space_2);
    spacing.menu_margin = egui::Margin::same(SETTINGS_METRICS.margin_space_2);
    spacing.item_spacing = egui::vec2(SETTINGS_METRICS.space_2, SETTINGS_METRICS.space_2);
    spacing.button_padding = egui::vec2(SETTINGS_METRICS.space_2, SETTINGS_METRICS.space_1);
    apply_opaque_widget_style(ui);
}

/// 按给定比例统一调整 egui 内建字体、交互控件、菜单和滚动条尺寸。
pub fn scale_builtin_style(style: &mut Style, scale: f32) {
    for font in style.text_styles.values_mut() {
        font.size *= scale;
    }
    style.spacing.indent *= scale;
    style.spacing.interact_size *= scale;
    style.spacing.slider_width *= scale;
    style.spacing.slider_rail_height *= scale;
    style.spacing.combo_width *= scale;
    style.spacing.text_edit_width *= scale;
    style.spacing.icon_width *= scale;
    style.spacing.icon_width_inner *= scale;
    style.spacing.icon_spacing *= scale;
    style.spacing.tooltip_width *= scale;
    style.spacing.menu_width *= scale;
    style.spacing.menu_spacing *= scale;
    style.spacing.combo_height *= scale;
    style.spacing.scroll.bar_width *= scale;
    style.spacing.scroll.handle_min_length *= scale;
    style.spacing.scroll.bar_inner_margin *= scale;
    style.spacing.scroll.bar_outer_margin *= scale;
    style.spacing.scroll.floating_width *= scale;
    style.spacing.scroll.floating_allocated_width *= scale;
}

/// 按统一界面比例缩放 egui 逻辑点尺寸。
pub const fn scale_points(value: f32) -> f32 {
    value * INTERFACE_SCALE as f32
}

/// 按统一界面比例缩放 winit 使用的双精度逻辑点尺寸。
pub const fn scale_window_points(value: f64) -> f64 {
    value * INTERFACE_SCALE
}

/// 按统一界面比例缩放 egui 整数尺寸，并四舍五入到最近的逻辑点。
const fn scale_integer_points(value: u8) -> u8 {
    scaled_integer(value, INTERFACE_SCALE as f32)
}

/// 把整数 token 按指定比例四舍五入到最近的逻辑点。
const fn scaled_integer(value: u8, scale: f32) -> u8 {
    (value as f32 * scale + 0.5) as u8
}
