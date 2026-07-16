use egui::{Align, CornerRadius, Frame, Layout, Margin, Stroke, Ui};

use super::{
    design_tokens as tokens,
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
    Frame::new()
        .fill(tokens::OPAQUE_COLOR_BACKGROUND)
        .stroke(Stroke::new(1.0, tokens::OPAQUE_COLOR_BORDER))
        .corner_radius(CornerRadius::same(tokens::CARD_RADIUS))
        .inner_margin(Margin::same(tokens::MARGIN_SPACE_2))
        .show(ui, |ui| {
            tokens::apply_opaque_widget_style(ui);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.set_min_width(tokens::QUICK_SETTINGS_CONTENT_WIDTH);
                ui.set_max_width(tokens::QUICK_SETTINGS_CONTENT_WIDTH);
                render_tool_preferences(ui, view.tools)
            })
            .inner
        })
        .inner
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use egui::{Context, RawInput, Rect, vec2};

    use super::*;
    use crate::{
        app::AppMode,
        ui::{IdlePanel, ToolState},
        window::{GraphicsDiagnostics, QUICK_SETTINGS_HEIGHT_POINTS, QUICK_SETTINGS_WIDTH_POINTS},
    };

    /// 构造快捷设置布局测试所需的最小只读 UI 状态。
    fn test_view<'a>(dock_side: DockSide, diagnostics: &'a GraphicsDiagnostics) -> UiViewState<'a> {
        UiViewState {
            mode: AppMode::IdleFloatingToolbar,
            idle_panel: IdlePanel::QuickSettings,
            dock_side,
            tools: ToolState::default(),
            slideshow_integration_enabled: true,
            slide_page_numbers: None,
            slideshow_controls_enabled: false,
            dismiss_slideshow_confirmation: false,
            com_diagnostics: None,
            slideshow_connection_error: None,
            slideshow_control_error: None,
            settings_error: None,
            settings_path: Path::new("settings.toml"),
            graphics_diagnostics: diagnostics,
        }
    }

    /// 验证左右停靠时快捷设置的所有绘制内容均留在固定窗口内。
    #[test]
    fn quick_settings_shapes_fit_window_for_both_dock_sides() {
        let diagnostics = GraphicsDiagnostics {
            vendor: String::new(),
            renderer: String::new(),
            version: String::new(),
            software_fallback: false,
        };
        let viewport = Rect::from_min_size(
            egui::Pos2::ZERO,
            vec2(
                QUICK_SETTINGS_WIDTH_POINTS as f32,
                QUICK_SETTINGS_HEIGHT_POINTS as f32,
            ),
        );

        for dock_side in [DockSide::Left, DockSide::Right] {
            let context = Context::default();
            let output = context.run_ui(
                RawInput {
                    screen_rect: Some(viewport),
                    ..Default::default()
                },
                |ui| {
                    let _ = render(ui, test_view(dock_side, &diagnostics));
                },
            );
            let viewport_with_stroke_tolerance = viewport.expand(1.1);

            assert!(!output.shapes.is_empty());
            for clipped_shape in output.shapes {
                let bounds = clipped_shape.shape.visual_bounding_rect();
                if bounds.is_positive() {
                    assert!(
                        viewport_with_stroke_tolerance.contains_rect(bounds),
                        "{dock_side:?} 快捷设置绘制越界: {bounds:?}"
                    );
                }
            }
        }
    }
}
