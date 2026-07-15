use egui::{Align, CornerRadius, Frame, Layout, Margin, Stroke, Ui};

use super::{
    design_tokens as tokens,
    settings_controls::render_tool_preferences,
    toolbar::{UiCommand, UiViewState, render_idle_toolbar},
};
use crate::window::DockSide;

/// 在紧凑悬浮工具栏内侧绘制快捷工具偏好面板。
pub fn render(ui: &mut Ui, view: UiViewState<'_>) -> Option<UiCommand> {
    ui.set_min_size(ui.available_size());
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.add_space(tokens::SPACE_2);
        if view.dock_side == DockSide::Left {
            let toolbar_command = render_idle_toolbar(ui);
            ui.add_space(tokens::SPACE_2);
            let preferences_command = render_preferences_panel(ui, view);
            toolbar_command.or(preferences_command)
        } else {
            let preferences_command = render_preferences_panel(ui, view);
            ui.add_space(tokens::SPACE_2);
            let toolbar_command = render_idle_toolbar(ui);
            preferences_command.or(toolbar_command)
        }
    })
    .inner
}

/// 绘制快捷设置中的工具偏好卡片。
fn render_preferences_panel(ui: &mut Ui, view: UiViewState<'_>) -> Option<UiCommand> {
    Frame::new()
        .fill(tokens::COLOR_BACKGROUND)
        .stroke(Stroke::new(1.0, tokens::COLOR_BORDER))
        .corner_radius(CornerRadius::same(tokens::CARD_RADIUS))
        .inner_margin(Margin::same(tokens::SPACE_2 as i8))
        .show(ui, |ui| {
            ui.set_min_width(tokens::QUICK_SETTINGS_CONTENT_WIDTH);
            ui.set_max_width(tokens::QUICK_SETTINGS_CONTENT_WIDTH);
            render_tool_preferences(ui, view.tools)
        })
        .inner
}
