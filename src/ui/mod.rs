pub(crate) mod design_tokens;
mod pixel_snap;
mod quick_settings;
mod settings_controls;
mod settings_view;
mod slideshow_toolbar;
mod toolbar;

pub use toolbar::{IdlePanel, ToolState, UiCommand, UiViewState, render};

use egui::{FontData, FontDefinitions, FontFamily};

/// 配置统一视觉 token、中文系统字体回退和基础交互样式。
pub fn configure_context(context: &egui::Context) {
    install_windows_chinese_font(context);

    context.set_theme(egui::Theme::Light);
    let mut style = (*context.style_of(egui::Theme::Light)).clone();
    design_tokens::scale_builtin_style(&mut style, design_tokens::INTERFACE_SCALE as f32);
    style.spacing.window_margin = egui::Margin::same(design_tokens::MARGIN_SPACE_2);
    style.spacing.menu_margin = egui::Margin::same(design_tokens::MARGIN_SPACE_2);
    style.spacing.item_spacing = egui::vec2(design_tokens::SPACE_2, design_tokens::SPACE_2);
    style.spacing.button_padding = egui::vec2(design_tokens::SPACE_2, design_tokens::SPACE_1);
    style.visuals.window_fill = design_tokens::COLOR_BACKGROUND;
    style.visuals.window_stroke = egui::Stroke::new(1.0, design_tokens::COLOR_BORDER);
    style.visuals.panel_fill = egui::Color32::TRANSPARENT;
    style.visuals.widgets.noninteractive.bg_fill = design_tokens::COLOR_SURFACE;
    style.visuals.widgets.inactive.bg_fill = design_tokens::COLOR_SURFACE;
    style.visuals.widgets.hovered.bg_fill = design_tokens::COLOR_HOVER;
    style.visuals.widgets.active.bg_fill = design_tokens::COLOR_SELECTED;
    style.visuals.widgets.open.bg_fill = design_tokens::COLOR_SELECTED;
    style.visuals.override_text_color = Some(design_tokens::COLOR_TEXT_PRIMARY);
    context.set_style_of(egui::Theme::Light, style);
}

/// 从 Windows 字体目录加载中文字体，不把系统字体复制进应用包。
fn install_windows_chinese_font(context: &egui::Context) {
    let font_paths = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    let Some(font_bytes) = font_paths.iter().find_map(|path| std::fs::read(path).ok()) else {
        tracing::warn!("未找到 Windows 中文系统字体，界面文字可能缺字");
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "steady_ink_cjk".to_owned(),
        std::sync::Arc::new(FontData::from_owned(font_bytes)),
    );
    if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
        family.insert(0, "steady_ink_cjk".to_owned());
    }
    if let Some(family) = fonts.families.get_mut(&FontFamily::Monospace) {
        family.push("steady_ink_cjk".to_owned());
    }
    context.set_fonts(fonts);
}
