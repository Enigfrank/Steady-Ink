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
    let preferences_slot_size = egui::vec2(
        tokens::QUICK_SETTINGS_CONTENT_WIDTH + f32::from(tokens::MARGIN_SPACE_2) * 2.0,
        ui.available_height(),
    );
    let layout = if view.dock_side == DockSide::Left {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    };
    ui.with_layout(layout, |ui| {
        let preferences_command = ui
            .allocate_ui_with_layout(preferences_slot_size, Layout::top_down(Align::Min), |ui| {
                render_preferences_panel(ui, view)
            })
            .inner;
        let toolbar_command = render_idle_toolbar(ui, view.readable_mode);
        preferences_command.or(toolbar_command)
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
                    false,
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

#[cfg(test)]
mod tests {
    use egui::{RawInput, Rect, Shape, pos2, vec2};

    use super::*;
    use crate::{
        app::AppMode,
        performance::PerformanceSnapshot,
        settings::{LogLevel, PalmSizePreset},
        ui::{IdlePanel, ToolState},
        window::{GraphicsDiagnostics, QUICK_SETTINGS_HEIGHT_POINTS, QUICK_SETTINGS_WIDTH_POINTS},
    };

    /// 返回绘制形状或其递归子形状中指定文本的可见边界。
    fn text_bounds(shape: &Shape, expected: &str) -> Option<Rect> {
        match shape {
            Shape::Text(text) => text
                .galley
                .job
                .text
                .contains(expected)
                .then(|| text.visual_bounding_rect()),
            Shape::Vec(shapes) => shapes.iter().find_map(|shape| text_bounds(shape, expected)),
            _ => None,
        }
    }

    /// 断言形状中的全部文本都完整落在快捷设置视口内。
    fn assert_text_inside_viewport(shape: &Shape, viewport: Rect, dock_side: DockSide) {
        match shape {
            Shape::Text(text) => {
                let bounds = text.visual_bounding_rect();
                assert!(
                    viewport.expand(1.1).contains_rect(bounds),
                    "{dock_side:?} 吸附时文本 {:?} 越出视口: {bounds:?}",
                    text.galley.job.text
                );
            }
            Shape::Vec(shapes) => {
                for shape in shapes {
                    assert_text_inside_viewport(shape, viewport, dock_side);
                }
            }
            _ => {}
        }
    }

    /// 验证左右吸附都保留完整设置面板，防止先渲染工具栏耗尽横向空间。
    #[test]
    fn both_dock_sides_render_preferences_panel() {
        let graphics_diagnostics = GraphicsDiagnostics {
            vendor: "test".to_owned(),
            renderer: "test".to_owned(),
            device_info: "test".to_owned(),
            software_fallback: false,
        };
        let viewport_size = vec2(
            (QUICK_SETTINGS_WIDTH_POINTS / f64::from(tokens::TOOLBAR_ZOOM_FACTOR)) as f32,
            (QUICK_SETTINGS_HEIGHT_POINTS / f64::from(tokens::TOOLBAR_ZOOM_FACTOR)) as f32,
        );

        for dock_side in [DockSide::Left, DockSide::Right] {
            let context = egui::Context::default();
            crate::ui::configure_context(&context);
            let view = UiViewState {
                mode: AppMode::IdleFloatingToolbar,
                idle_panel: IdlePanel::QuickSettings,
                dock_side,
                tools: ToolState::default(),
                palm_size_preset: PalmSizePreset::default(),
                slideshow_integration_enabled: true,
                log_level: LogLevel::default(),
                readable_mode: false,
                performance_monitoring_enabled: false,
                performance_snapshot: PerformanceSnapshot::default(),
                performance_export_status: None,
                performance_export_failed: false,
                ink_rendering_error: None,
                slideshow_session_generation: None,
                slide_page_numbers: None,
                slideshow_controls_enabled: false,
                dismiss_slideshow_confirmation: false,
                com_diagnostics: None,
                slideshow_connection_error: None,
                slideshow_control_error: None,
                machine_autostart_state: None,
                machine_autostart_error: None,
                graphics_diagnostics: &graphics_diagnostics,
            };
            let output = context.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), viewport_size)),
                    ..RawInput::default()
                },
                |ui| {
                    render(ui, view);
                },
            );
            let viewport = context.content_rect();
            let preferences_bounds = output
                .shapes
                .iter()
                .find_map(|clipped| text_bounds(&clipped.shape, "易读模式"))
                .expect("快捷设置面板不应被工具栏挤出视口");
            let toolbar_bounds = output
                .shapes
                .iter()
                .find_map(|clipped| text_bounds(&clipped.shape, "开始批注"))
                .expect("快捷设置工具栏不应被面板挤出视口");
            match dock_side {
                DockSide::Left => {
                    assert!(toolbar_bounds.center().x < preferences_bounds.center().x)
                }
                DockSide::Right => {
                    assert!(preferences_bounds.center().x < toolbar_bounds.center().x)
                }
            }
            for clipped in &output.shapes {
                assert_text_inside_viewport(&clipped.shape, viewport, dock_side);
            }
        }
    }
}
