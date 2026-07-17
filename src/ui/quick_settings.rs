use egui::{Align, CornerRadius, Frame, Layout, Margin, Stroke, Ui};

use super::{
    design_tokens as tokens, pixel_snap,
    settings_controls::render_tool_preferences,
    toolbar::{UiCommand, UiViewState, render_opaque_idle_toolbar},
};
use crate::window::DockSide;

/// 在紧凑悬浮工具栏内侧绘制快捷工具偏好面板。
pub fn render(ui: &mut Ui, view: UiViewState<'_>) -> Option<UiCommand> {
    ui.set_min_size(ui.available_size());
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        if view.dock_side == DockSide::Left {
            let toolbar_command = render_opaque_idle_toolbar(ui);
            let preferences_command = render_preferences_panel(ui, view);
            toolbar_command.or(preferences_command)
        } else {
            let preferences_command = render_preferences_panel(ui, view);
            let toolbar_command = render_opaque_idle_toolbar(ui);
            preferences_command.or(toolbar_command)
        }
    })
    .inner
}

/// 绘制快捷设置中的工具偏好卡片。
fn render_preferences_panel(ui: &mut Ui, view: UiViewState<'_>) -> Option<UiCommand> {
    pixel_snap::show_pixel_aligned_frame(
        ui,
        Frame::new()
            .fill(tokens::OPAQUE_COLOR_BACKGROUND)
            .stroke(Stroke::new(1.0, tokens::OPAQUE_COLOR_BORDER))
            .corner_radius(CornerRadius::same(tokens::CARD_RADIUS))
            .inner_margin(Margin::same(tokens::MARGIN_SPACE_2)),
        |ui| {
            tokens::apply_opaque_widget_style(ui);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.set_min_width(tokens::QUICK_SETTINGS_CONTENT_WIDTH);
                ui.set_max_width(tokens::QUICK_SETTINGS_CONTENT_WIDTH);
                render_tool_preferences(ui, view.tools, tokens::TOOL_METRICS)
            })
            .inner
        },
    )
    .inner
}
