use egui::{Align, CornerRadius, Layout, Margin, Ui};

use super::{
    design_tokens as tokens, pixel_snap,
    settings_controls::render_tool_preferences,
    toolbar::{UiCommand, UiViewState, render_idle_toolbar},
};
use crate::window::DockSide;

/// 在紧凑悬浮工具栏内侧绘制快捷工具偏好面板。
pub fn render(ui: &mut Ui, view: UiViewState<'_>) -> Option<UiCommand> {
    ui.set_min_size(ui.available_size());
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        if view.dock_side == DockSide::Left {
            let toolbar_command = render_idle_toolbar(ui, view.readable_mode);
            let preferences_command = render_preferences_panel(ui, view);
            toolbar_command.or(preferences_command)
        } else {
            let preferences_command = render_preferences_panel(ui, view);
            let toolbar_command = render_idle_toolbar(ui, view.readable_mode);
            preferences_command.or(toolbar_command)
        }
    })
    .inner
}

/// 绘制快捷设置中的工具偏好卡片。
fn render_preferences_panel(ui: &mut Ui, view: UiViewState<'_>) -> Option<UiCommand> {
    pixel_snap::show_pixel_aligned_frame(
        ui,
        tokens::material_frame(
            view.readable_mode,
            tokens::MaterialRole::Popover,
            CornerRadius::same(tokens::CARD_RADIUS),
            Margin::same(tokens::MARGIN_SPACE_2),
        ),
        |ui| {
            tokens::apply_widget_style(ui, view.readable_mode, tokens::MaterialRole::Popover);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.set_min_width(tokens::QUICK_SETTINGS_CONTENT_WIDTH);
                ui.set_max_width(tokens::QUICK_SETTINGS_CONTENT_WIDTH);
                let mut readable_mode = view.readable_mode;
                let readable_changed = ui
                    .horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("快捷设置")
                                .size(tokens::TEXT_BASE)
                                .strong(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add(egui::Checkbox::new(&mut readable_mode, "易读模式"))
                                .changed()
                        })
                        .inner
                    })
                    .inner;
                ui.add_space(tokens::SPACE_2);
                ui.separator();
                ui.add_space(tokens::SPACE_2);
                let preferences_command = render_tool_preferences(
                    ui,
                    view.tools,
                    tokens::TOOL_METRICS,
                    view.readable_mode,
                );
                if readable_changed {
                    Some(UiCommand::SetReadableMode(readable_mode))
                } else {
                    preferences_command
                }
            })
            .inner
        },
    )
    .inner
}
