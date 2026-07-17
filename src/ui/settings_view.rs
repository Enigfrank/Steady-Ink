use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Pos2, Response,
    ScrollArea, Sense, Stroke, Ui, Vec2,
};

use super::{
    design_tokens::{self as tokens, InterfaceMetrics},
    pixel_snap,
    settings_controls::render_tool_preferences,
    toolbar::{Icon, UiCommand, UiViewState, paint_icon},
};
use crate::slideshow::{ComCandidateStatus, ComDiagnostics};

/// 绘制完整尺寸的默认工具、联动开关、诊断和版本设置界面。
pub fn render(ui: &mut Ui, view: UiViewState<'_>) -> Option<UiCommand> {
    let metrics = tokens::SETTINGS_METRICS;
    ui.set_min_size(ui.available_size());
    pixel_snap::show_pixel_aligned_frame(
        ui,
        Frame::new()
            .fill(tokens::OPAQUE_COLOR_BACKGROUND)
            .stroke(Stroke::new(1.0, tokens::OPAQUE_COLOR_BORDER))
            .corner_radius(CornerRadius::same(metrics.card_radius))
            .inner_margin(Margin::same(metrics.margin_space_4)),
        |ui| {
            tokens::apply_settings_widget_style(ui);
            ui.set_min_size(ui.available_size());

            let mut command = render_header(ui, metrics);
            ui.add_space(metrics.space_2);
            let action_command = render_action_bar(ui, metrics);
            if command.is_none() {
                command = action_command;
            }
            ui.add_space(metrics.space_3);
            ui.separator();
            ui.add_space(metrics.space_2);

            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    section_heading(ui, "默认批注工具", metrics);
                    let preferences_command =
                        render_tool_preferences(ui, view.tools, tokens::SETTINGS_METRICS);
                    if command.is_none() {
                        command = preferences_command;
                    }

                    section_break(ui, metrics);
                    section_heading(ui, "演示联动", metrics);
                    let mut integration_enabled = view.slideshow_integration_enabled;
                    if ui
                        .add_sized(
                            [ui.available_width(), metrics.touch_target],
                            egui::Checkbox::new(
                                &mut integration_enabled,
                                "启用 PowerPoint / WPS 联动",
                            ),
                        )
                        .changed()
                        && command.is_none()
                    {
                        command = Some(UiCommand::SetSlideshowIntegrationEnabled(
                            integration_enabled,
                        ));
                    }
                    ui.label(
                        egui::RichText::new(
                            "检测使用 COM；控制优先使用 COM，仅在已确认放映中失败时模拟按键。页码不可靠时会完全隐藏。",
                        )
                        .size(metrics.text_xs)
                        .color(tokens::COLOR_TEXT_SECONDARY),
                    );

                    section_break(ui, metrics);
                    section_heading(ui, "诊断", metrics);
                    render_diagnostics(ui, view, metrics);

                    section_break(ui, metrics);
                    section_heading(ui, "关于", metrics);
                    diagnostic_row(
                        ui,
                        "版本",
                        env!("CARGO_PKG_VERSION"),
                        tokens::COLOR_TEXT_PRIMARY,
                        metrics,
                    );
                    diagnostic_row(
                        ui,
                        "应用",
                        "Steady Ink",
                        tokens::COLOR_TEXT_PRIMARY,
                        metrics,
                    );
                    ui.add_space(metrics.space_2);
                });
            command
        },
    )
    .inner
}

/// 绘制设置标题和右上角关闭按钮。
fn render_header(ui: &mut Ui, metrics: InterfaceMetrics) -> Option<UiCommand> {
    let mut command = None;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("设置")
                .size(metrics.text_xl)
                .strong()
                .color(tokens::COLOR_TEXT_PRIMARY),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if close_button(ui, metrics).clicked() {
                command = Some(UiCommand::CloseSettings);
            }
        });
    });
    command
}

/// 绘制配置目录和退出软件组成的顶部等宽操作栏。
fn render_action_bar(ui: &mut Ui, metrics: InterfaceMetrics) -> Option<UiCommand> {
    let mut command = None;
    ui.horizontal(|ui| {
        let button_width = equal_action_button_width(ui.available_width(), metrics);
        if settings_action_button(
            ui,
            "打开配置文件",
            Icon::Folder,
            SettingsActionStyle::Neutral,
            button_width,
            metrics,
        )
        .clicked()
        {
            command = Some(UiCommand::OpenSettingsDirectory);
        }
        if settings_action_button(
            ui,
            "退出软件",
            Icon::Power,
            SettingsActionStyle::Danger,
            button_width,
            metrics,
        )
        .clicked()
            && command.is_none()
        {
            command = Some(UiCommand::ExitApplication);
        }
    });
    command
}

/// 计算同一行两个设置操作按钮平分空间后的稳定宽度。
fn equal_action_button_width(available_width: f32, metrics: InterfaceMetrics) -> f32 {
    ((available_width - metrics.space_2) / 2.0).max(0.0)
}

/// 绘制图标和文字横向排列的设置页长条操作按钮。
fn settings_action_button(
    ui: &mut Ui,
    label: &str,
    icon: Icon,
    style: SettingsActionStyle,
    width: f32,
    metrics: InterfaceMetrics,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, metrics.action_button_height),
        Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let fill = if response.hovered() {
        tokens::OPAQUE_COLOR_HOVER
    } else {
        tokens::OPAQUE_COLOR_SURFACE
    };
    let (foreground, border) = match style {
        SettingsActionStyle::Neutral => (tokens::COLOR_TEXT_PRIMARY, tokens::OPAQUE_COLOR_BORDER),
        SettingsActionStyle::Danger => (tokens::COLOR_ERROR, tokens::COLOR_ERROR),
    };
    pixel_snap::paint_pixel_aligned_rect(
        ui,
        rect,
        CornerRadius::same(metrics.button_radius),
        fill,
        Stroke::new(1.0, border),
    );

    let icon_center = Pos2::new(
        rect.left() + metrics.space_4 + metrics.icon_size / 2.0,
        rect.center().y,
    );
    paint_icon(ui, icon_center, icon, None, foreground, foreground, metrics);
    ui.painter().text(
        Pos2::new(
            icon_center.x + metrics.icon_size / 2.0 + metrics.space_2,
            rect.center().y,
        ),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(metrics.text_base),
        foreground,
    );
    response
}

/// 绘制设置标题行右侧的关闭图标按钮。
fn close_button(ui: &mut Ui, metrics: InterfaceMetrics) -> Response {
    let size = Vec2::splat(metrics.action_button_height);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if response.hovered() {
            tokens::OPAQUE_COLOR_HOVER
        } else {
            tokens::OPAQUE_COLOR_SURFACE
        };
        pixel_snap::paint_pixel_aligned_rect(
            ui,
            rect,
            CornerRadius::same(metrics.button_radius),
            fill,
            Stroke::new(1.0, tokens::OPAQUE_COLOR_BORDER),
        );
        let half = metrics.icon_size / 2.0;
        for points in [
            [
                rect.center() + egui::vec2(-half, -half),
                rect.center() + egui::vec2(half, half),
            ],
            [
                rect.center() + egui::vec2(half, -half),
                rect.center() + egui::vec2(-half, half),
            ],
        ] {
            pixel_snap::paint_pixel_aligned_line(
                ui.painter(),
                points,
                Stroke::new(metrics.points(2.0), tokens::COLOR_TEXT_SECONDARY),
            );
        }
    }
    response.on_hover_text("关闭设置")
}

/// 绘制设置页各部分的层级标题并保留舒适的标题后间距。
fn section_heading(ui: &mut Ui, title: &str, metrics: InterfaceMetrics) {
    ui.label(
        egui::RichText::new(title)
            .size(metrics.text_lg)
            .strong()
            .color(tokens::COLOR_TEXT_PRIMARY),
    );
    ui.add_space(metrics.space_2);
}

/// 在设置分组之间加入统一的留白和分隔线。
fn section_break(ui: &mut Ui, metrics: InterfaceMetrics) {
    ui.add_space(metrics.space_6);
    ui.separator();
    ui.add_space(metrics.space_4);
}

/// 绘制 COM、控制、GPU、配置目录和设置文件状态。
fn render_diagnostics(ui: &mut Ui, view: UiViewState<'_>, metrics: InterfaceMetrics) {
    let (com_status, com_color) =
        com_status(view.slideshow_integration_enabled, view.com_diagnostics);
    diagnostic_row(ui, "COM 检测", com_status, com_color, metrics);
    diagnostic_row(
        ui,
        "当前连接",
        view.slideshow_connection_error.unwrap_or("未报告连接中断"),
        if view.slideshow_connection_error.is_some() {
            tokens::COLOR_ERROR
        } else {
            tokens::COLOR_TEXT_PRIMARY
        },
        metrics,
    );
    diagnostic_row(
        ui,
        "控制兜底",
        view.slideshow_control_error.unwrap_or("未报告控制失败"),
        if view.slideshow_control_error.is_some() {
            tokens::COLOR_ERROR
        } else {
            tokens::COLOR_TEXT_PRIMARY
        },
        metrics,
    );
    diagnostic_row(
        ui,
        "图形设备",
        &view.graphics_diagnostics.renderer,
        if view.graphics_diagnostics.software_fallback {
            tokens::COLOR_ERROR
        } else {
            tokens::COLOR_TEXT_PRIMARY
        },
        metrics,
    );
    diagnostic_row(
        ui,
        "设置文件",
        &view.settings_path.display().to_string(),
        tokens::COLOR_TEXT_PRIMARY,
        metrics,
    );
    diagnostic_row(
        ui,
        "设置保存",
        view.settings_error.unwrap_or("正常"),
        if view.settings_error.is_some() {
            tokens::COLOR_ERROR
        } else {
            tokens::COLOR_TEXT_PRIMARY
        },
        metrics,
    );
    diagnostic_row(
        ui,
        "目录操作",
        view.settings_directory_error.unwrap_or("正常"),
        if view.settings_directory_error.is_some() {
            tokens::COLOR_ERROR
        } else {
            tokens::COLOR_TEXT_PRIMARY
        },
        metrics,
    );
}

/// 根据联动开关和候选诊断生成设置页 COM 状态。
fn com_status(
    integration_enabled: bool,
    diagnostics: Option<&ComDiagnostics>,
) -> (&'static str, Color32) {
    if !integration_enabled {
        return ("已关闭", tokens::COLOR_TEXT_SECONDARY);
    }
    let Some(diagnostics) = diagnostics else {
        return ("正在检测", tokens::COLOR_TEXT_SECONDARY);
    };
    if diagnostics
        .candidates
        .iter()
        .any(|candidate| candidate.status == ComCandidateStatus::Connected)
    {
        return ("已连接", tokens::COLOR_TEXT_PRIMARY);
    }
    if diagnostics.candidates.iter().any(|candidate| {
        candidate.status == ComCandidateStatus::EventSubscriptionFailed
            || candidate.status == ComCandidateStatus::ConnectionFailed
    }) {
        return ("连接失败", tokens::COLOR_ERROR);
    }
    if diagnostics
        .candidates
        .iter()
        .all(|candidate| candidate.status == ComCandidateStatus::ClassNotRegistered)
    {
        return ("未检测到兼容 COM", tokens::COLOR_ERROR);
    }
    ("等待演示软件运行", tokens::COLOR_TEXT_SECONDARY)
}

/// 绘制稳定标签列和可换行值列组成的诊断行。
fn diagnostic_row(
    ui: &mut Ui,
    label: &str,
    value: &str,
    value_color: Color32,
    metrics: InterfaceMetrics,
) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(metrics.diagnostic_label_width, metrics.text_sm * 1.5),
            Layout::left_to_right(Align::Min),
            |ui| {
                ui.label(
                    egui::RichText::new(label)
                        .size(metrics.text_sm)
                        .color(tokens::COLOR_TEXT_SECONDARY),
                );
            },
        );
        ui.add_sized(
            [ui.available_width(), 0.0],
            Label::new(
                egui::RichText::new(value)
                    .size(metrics.text_sm)
                    .color(value_color),
            )
            .wrap(),
        );
    });
    ui.add_space(metrics.space_1);
}

#[derive(Debug, Clone, Copy)]
enum SettingsActionStyle {
    Neutral,
    Danger,
}

#[cfg(test)]
mod tests {
    use egui::{Context, RawInput, Rect, Shape};

    use super::*;

    const DPI_SCALES: [f32; 4] = [1.25, 1.5, 1.75, 2.0];
    const TOLERANCE: f32 = 0.001;

    /// 返回物理坐标到整数或半整数目标相位的最短环形距离。
    fn phase_error(physical: f32, expected_phase: f32) -> f32 {
        let fraction = physical.rem_euclid(1.0);
        if expected_phase == 0.0 {
            fraction.min(1.0 - fraction)
        } else {
            (fraction - expected_phase).abs()
        }
    }

    /// 验证顶部双按钮平分宽度并使用 100% 的 48pt 长条高度。
    #[test]
    fn settings_action_buttons_are_equal_width_and_full_size() {
        let context = Context::default();
        let mut rects = Vec::new();
        let _ = context.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(560.0, 640.0))),
                ..Default::default()
            },
            |ui| {
                tokens::apply_settings_widget_style(ui);
                ui.horizontal(|ui| {
                    let width =
                        equal_action_button_width(ui.available_width(), tokens::SETTINGS_METRICS);
                    rects.push(
                        settings_action_button(
                            ui,
                            "打开配置文件",
                            Icon::Folder,
                            SettingsActionStyle::Neutral,
                            width,
                            tokens::SETTINGS_METRICS,
                        )
                        .rect,
                    );
                    rects.push(
                        settings_action_button(
                            ui,
                            "退出软件",
                            Icon::Power,
                            SettingsActionStyle::Danger,
                            width,
                            tokens::SETTINGS_METRICS,
                        )
                        .rect,
                    );
                });
            },
        );

        assert_eq!(rects.len(), 2);
        assert!((rects[0].width() - rects[1].width()).abs() < f32::EPSILON);
        assert_eq!(rects[0].height(), 48.0);
        assert_eq!(rects[1].height(), 48.0);
        assert!(rects[1].left() > rects[0].right());
    }

    /// 验证设置长条按钮和 Folder 图标在四档 DPI 下输出物理像素对齐几何。
    #[test]
    fn settings_folder_action_uses_pixel_aligned_shapes() {
        for pixels_per_point in DPI_SCALES {
            let context = Context::default();
            context.set_pixels_per_point(pixels_per_point);
            let output = context.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(560.0, 640.0))),
                    ..Default::default()
                },
                |ui| {
                    let _ = settings_action_button(
                        ui,
                        "打开配置文件",
                        Icon::Folder,
                        SettingsActionStyle::Neutral,
                        240.0,
                        tokens::SETTINGS_METRICS,
                    );
                },
            );

            let button = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Rect(rect) if rect.stroke.width > 0.0 => Some(rect),
                    _ => None,
                })
                .expect("设置操作按钮应输出像素对齐边框");
            for edge in [
                button.rect.left(),
                button.rect.right(),
                button.rect.top(),
                button.rect.bottom(),
            ] {
                let physical = edge * pixels_per_point;
                assert!((physical - physical.round()).abs() < TOLERANCE);
            }

            let lines: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match shape.shape {
                    Shape::LineSegment { points, stroke } => Some((points, stroke)),
                    _ => None,
                })
                .collect();
            assert_eq!(lines.len(), 7);
            for (points, stroke) in lines {
                let width_pixels = (stroke.width * pixels_per_point).round() as u32;
                let expected_phase = if width_pixels.is_multiple_of(2) {
                    0.0
                } else {
                    0.5
                };
                if (points[0].y - points[1].y).abs() <= f32::EPSILON {
                    for x in [points[0].x, points[1].x] {
                        let physical = x * pixels_per_point;
                        assert!((physical - physical.round()).abs() < TOLERANCE);
                    }
                    let physical = points[0].y * pixels_per_point;
                    assert!(
                        phase_error(physical, expected_phase) < TOLERANCE,
                        "horizontal center phase mismatch: dpi={pixels_per_point}, points={points:?}, stroke={stroke:?}, physical={physical}"
                    );
                } else {
                    for y in [points[0].y, points[1].y] {
                        let physical = y * pixels_per_point;
                        assert!((physical - physical.round()).abs() < TOLERANCE);
                    }
                    let physical = points[0].x * pixels_per_point;
                    assert!(
                        phase_error(physical, expected_phase) < TOLERANCE,
                        "vertical center phase mismatch: dpi={pixels_per_point}, points={points:?}, stroke={stroke:?}, physical={physical}"
                    );
                }
            }
        }
    }
}
