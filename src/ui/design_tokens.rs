use egui::Color32;

pub const INTERFACE_SCALE: f64 = 0.8;
pub const INTERFACE_ALPHA: u8 = 128;

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
pub const COLOR_PRIMARY_SURFACE: Color32 =
    Color32::from_rgba_unmultiplied_const(37, 99, 235, INTERFACE_ALPHA);
pub const COLOR_ERROR: Color32 = Color32::from_rgb(220, 38, 38);
pub const COLOR_ERROR_SURFACE: Color32 =
    Color32::from_rgba_unmultiplied_const(220, 38, 38, INTERFACE_ALPHA);

pub const TEXT_XS: f32 = scale_points(12.0);
pub const TEXT_SM: f32 = scale_points(14.0);
pub const TEXT_BASE: f32 = scale_points(16.0);
pub const TEXT_LG: f32 = scale_points(20.0);
pub const SPACE_1: f32 = scale_points(4.0);
pub const SPACE_2: f32 = scale_points(8.0);
pub const SPACE_3: f32 = scale_points(12.0);
pub const SPACE_4: f32 = scale_points(16.0);
pub const SPACE_6: f32 = scale_points(24.0);
pub const MARGIN_SPACE_2: i8 = scale_integer_points(8) as i8;
pub const MARGIN_SPACE_4: i8 = scale_integer_points(16) as i8;
pub const CARD_RADIUS: u8 = scale_integer_points(8);
pub const CAPSULE_RADIUS: u8 = scale_integer_points(16);
pub const BUTTON_RADIUS: u8 = scale_integer_points(6);
pub const TOUCH_TARGET: f32 = scale_points(64.0);
pub const ICON_SIZE: f32 = scale_points(20.0);
pub const PAGE_NUMBER_WIDTH: f32 = scale_points(80.0);
pub const DIAGNOSTIC_LABEL_WIDTH: f32 = scale_points(112.0);
pub const QUICK_SETTINGS_CONTENT_WIDTH: f32 = scale_points(424.0);
pub const SLIDESHOW_TOOLBAR_ANIMATION_SECONDS: f32 = 0.2;

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
    (value as f64 * INTERFACE_SCALE + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证全局界面缩放和表面透明度保持产品要求。
    #[test]
    fn interface_scale_and_alpha_match_product_settings() {
        assert_eq!(INTERFACE_SCALE, 0.8);
        assert_eq!(INTERFACE_ALPHA, 128);
        assert!((TOUCH_TARGET - 51.2).abs() < f32::EPSILON);
        assert_eq!(MARGIN_SPACE_2, 6);
        assert_eq!(MARGIN_SPACE_4, 13);
        assert_eq!(CARD_RADIUS, 6);
        assert_eq!(COLOR_SURFACE.a(), INTERFACE_ALPHA);
        assert_eq!(COLOR_PRIMARY_SURFACE.a(), INTERFACE_ALPHA);
        assert_eq!(COLOR_ERROR_SURFACE.a(), INTERFACE_ALPHA);
    }
}
