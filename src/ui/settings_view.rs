use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Label, Layout, Margin, Pos2, Response,
    ScrollArea, Sense, Stroke, Ui, Vec2,
};

use super::{
    design_tokens::{self as tokens, InterfaceMetrics},
    pixel_snap,
    settings_controls::{
        render_log_level_selector, render_palm_size_selector, render_tool_preferences,
    },
    toolbar::{Icon, UiCommand, UiViewState, paint_icon},
};
use crate::autostart::MachineAutostartState;
use crate::slideshow::{ComCandidateStatus, ComDiagnostics};

/// 绘制完整尺寸的默认工具、联动开关、诊断和版本设置界面。
pub fn render(ui: &mut Ui, view: UiViewState<'_>) -> Option<UiCommand> {
    let metrics = tokens::SETTINGS_METRICS;
    ui.set_min_size(ui.available_size());
    pixel_snap::show_pixel_aligned_frame(
        ui,
        tokens::material_frame(
            view.readable_mode,
            tokens::MaterialRole::Page,
            CornerRadius::same(metrics.card_radius),
            Margin::same(metrics.margin_space_4),
        ),
        |ui| {
            tokens::apply_settings_widget_style(ui, view.readable_mode);
            ui.set_min_size(ui.available_size());

            let mut command = render_header(ui, metrics, view.readable_mode);
            ui.add_space(metrics.space_3);
            let action_command = render_action_bar(ui, metrics, view.readable_mode);
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
                        render_tool_preferences(
                            ui,
                            view.tools,
                            tokens::SETTINGS_METRICS,
                            view.readable_mode,
                            true,
                        );
                    if command.is_none() {
                        command = preferences_command;
                    }

                    section_break(ui, metrics);
                    let display_command = render_display_settings(ui, view, metrics);
                    if command.is_none() {
                        command = display_command;
                    }

                    section_break(ui, metrics);
                    section_heading(ui, "触摸与手掌", metrics);
                    let palm_size_command =
                        render_palm_size_selector(ui, view.palm_size_preset, metrics);
                    if command.is_none() {
                        command = palm_size_command;
                    }

                    section_break(ui, metrics);
                    let autostart_command =
                        render_machine_autostart_setting(ui, view, metrics);
                    if command.is_none() {
                        command = autostart_command;
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
                    egui::CollapsingHeader::new("诊断与日志")
                        .default_open(false)
                        .show(ui, |ui| {
                            let log_level_command =
                                render_log_level_selector(ui, view.log_level, metrics);
                            if command.is_none() {
                                command = log_level_command;
                            }
                            ui.add_space(metrics.space_4);
                            let diagnostics_command = render_diagnostics(ui, view, metrics);
                            if command.is_none() {
                                command = diagnostics_command;
                            }
                        });

                    section_break(ui, metrics);
                    render_settings_footer(ui, metrics);
                    ui.add_space(metrics.space_2);
                });
            command
        },
    )
    .inner
}

/// 绘制设置标题和右上角关闭按钮。
fn render_header(ui: &mut Ui, metrics: InterfaceMetrics, readable_mode: bool) -> Option<UiCommand> {
    let mut command = None;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("设置")
                .size(metrics.text_xl)
                .strong()
                .color(tokens::COLOR_TEXT_PRIMARY),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if close_button(ui, metrics, readable_mode).clicked() {
                command = Some(UiCommand::CloseSettings);
            }
        });
    });
    command
}

/// 绘制同一行内两个等宽显示开关，并仅在异常时追加错误信息。
fn render_display_settings(
    ui: &mut Ui,
    view: UiViewState<'_>,
    metrics: InterfaceMetrics,
) -> Option<UiCommand> {
    section_heading(ui, "显示", metrics);
    let mut readable_mode = view.readable_mode;
    let mut performance_monitoring_enabled = view.performance_monitoring_enabled;
    let mut command = None;
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = metrics.touch_target;
        ui.horizontal(|ui| {
            let option_width = equal_row_item_width(ui.available_width(), 2, metrics.space_2);
            if ui
                .add_sized(
                    [option_width, metrics.touch_target],
                    egui::Checkbox::new(&mut readable_mode, "易读模式"),
                )
                .changed()
            {
                command = Some(UiCommand::SetReadableMode(readable_mode));
            }
            if ui
                .add_sized(
                    [option_width, metrics.touch_target],
                    egui::Checkbox::new(&mut performance_monitoring_enabled, "显示性能监控"),
                )
                .changed()
                && command.is_none()
            {
                command = Some(UiCommand::SetPerformanceMonitoringEnabled(
                    performance_monitoring_enabled,
                ));
            }
        });
    });
    if let Some(error) = view.ink_rendering_error {
        ui.label(
            egui::RichText::new(error)
                .size(metrics.text_xs)
                .color(tokens::COLOR_ERROR),
        );
    }
    command
}

/// 绘制配置目录、重启和退出组成的顶部等宽操作栏。
fn render_action_bar(
    ui: &mut Ui,
    metrics: InterfaceMetrics,
    readable_mode: bool,
) -> Option<UiCommand> {
    let mut command = None;
    ui.horizontal(|ui| {
        let button_width = equal_row_item_width(
            ui.available_width(),
            SETTINGS_ACTIONS.len(),
            metrics.space_2,
        );
        for action in SETTINGS_ACTIONS {
            if settings_action_button(
                ui,
                action.label,
                action.icon,
                action.style,
                button_width,
                metrics,
                readable_mode,
            )
            .clicked()
                && command.is_none()
            {
                command = Some(action.command);
            }
        }
    });
    command
}

/// 计算固定数量控件扣除统一间距后平分一行的稳定宽度。
fn equal_row_item_width(available_width: f32, item_count: usize, gap: f32) -> f32 {
    if item_count == 0 {
        return 0.0;
    }
    let gap_width = item_count.saturating_sub(1) as f32 * gap;
    ((available_width - gap_width) / item_count as f32).max(0.0)
}

/// 绘制图标和文字横向排列的设置页长条操作按钮。
fn settings_action_button(
    ui: &mut Ui,
    label: &str,
    icon: Icon,
    style: SettingsActionStyle,
    width: f32,
    metrics: InterfaceMetrics,
    readable_mode: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, metrics.action_button_height),
        Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let (fill, default_border) =
        tokens::button_colors(readable_mode, false, &response, ui.is_enabled());
    let (foreground, border) = match style {
        SettingsActionStyle::Neutral => (tokens::COLOR_TEXT_PRIMARY, default_border),
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
fn close_button(ui: &mut Ui, metrics: InterfaceMetrics, readable_mode: bool) -> Response {
    let size = Vec2::splat(metrics.action_button_height);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let (fill, border) =
            tokens::button_colors(readable_mode, false, &response, ui.is_enabled());
        pixel_snap::paint_pixel_aligned_rect(
            ui,
            rect,
            CornerRadius::same(metrics.button_radius),
            fill,
            Stroke::new(1.0, border),
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

/// 绘制只存在于完整设置页的所有用户自启动开关和诊断。
fn render_machine_autostart_setting(
    ui: &mut Ui,
    view: UiViewState<'_>,
    metrics: InterfaceMetrics,
) -> Option<UiCommand> {
    section_heading(ui, "启动", metrics);
    let Some(state) = view.machine_autostart_state else {
        let mut disabled = false;
        ui.scope(|ui| {
            ui.spacing_mut().interact_size.y = metrics.touch_target;
            ui.horizontal(|ui| {
                ui.add_enabled(
                    false,
                    egui::Checkbox::new(&mut disabled, "为所有用户开机启动"),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("无法读取")
                            .size(metrics.text_xs)
                            .color(tokens::COLOR_ERROR),
                    );
                });
            });
        });
        ui.add_space(metrics.space_1);
        ui.label(
            egui::RichText::new(
                view.machine_autostart_error
                    .unwrap_or("无法读取系统级启动状态，请重新打开设置后重试。"),
            )
            .size(metrics.text_xs)
            .color(tokens::COLOR_ERROR),
        );
        return None;
    };

    let mut enabled = state.enabled();
    let status_color = if matches!(state, MachineAutostartState::EnabledPathMismatch) {
        tokens::COLOR_ERROR
    } else {
        tokens::COLOR_TEXT_SECONDARY
    };
    let changed = ui
        .scope(|ui| {
            ui.spacing_mut().interact_size.y = metrics.touch_target;
            ui.horizontal(|ui| {
                let changed = ui
                    .add(egui::Checkbox::new(&mut enabled, "为所有用户开机启动"))
                    .changed();
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(state.label())
                            .size(metrics.text_xs)
                            .color(status_color),
                    );
                });
                changed
            })
            .inner
        })
        .inner;
    if let Some(error) = view.machine_autostart_error {
        ui.add_space(metrics.space_1);
        ui.label(
            egui::RichText::new(error)
                .size(metrics.text_xs)
                .color(tokens::COLOR_ERROR),
        );
    }
    changed.then_some(UiCommand::SetMachineAutostart(enabled))
}

/// 绘制由 Cargo 包元数据生成的设置页版权信息。
fn render_settings_footer(ui: &mut Ui, metrics: InterfaceMetrics) {
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.label(
            egui::RichText::new(settings_footer_text())
                .size(metrics.text_xs)
                .color(tokens::COLOR_TEXT_TERTIARY),
        );
    });
}

/// 返回不重复维护版本、作者和许可证的版权文案。
fn settings_footer_text() -> String {
    format!(
        "Steady Ink v{} · © 2026 {} · {}",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_AUTHORS"),
        env!("CARGO_PKG_LICENSE")
    )
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

/// 绘制运行诊断、性能摘要和性能快照导出操作。
fn render_diagnostics(
    ui: &mut Ui,
    view: UiViewState<'_>,
    metrics: InterfaceMetrics,
) -> Option<UiCommand> {
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
        "墨迹渲染",
        view.ink_rendering_error.unwrap_or("正常"),
        if view.ink_rendering_error.is_some() {
            tokens::COLOR_ERROR
        } else {
            tokens::COLOR_TEXT_PRIMARY
        },
        metrics,
    );
    let (autostart_status, autostart_color) = match view.machine_autostart_state {
        Some(state) => (
            state.label(),
            if matches!(state, MachineAutostartState::EnabledPathMismatch) {
                tokens::COLOR_ERROR
            } else {
                tokens::COLOR_TEXT_PRIMARY
            },
        ),
        None => ("无法读取", tokens::COLOR_ERROR),
    };
    diagnostic_row(ui, "系统自启动", autostart_status, autostart_color, metrics);
    let performance_summary = if view.performance_snapshot.frame_count() == 0 {
        "等待样本".to_owned()
    } else {
        format!(
            "{:.1} FPS / {:.2} ms p95",
            view.performance_snapshot.fps(),
            view.performance_snapshot.p95_frame_time_ms()
        )
    };
    diagnostic_row(
        ui,
        "渲染性能",
        &performance_summary,
        tokens::COLOR_TEXT_PRIMARY,
        metrics,
    );
    let gpu_resources = format!(
        "{:.1} MiB（应用自有估算）",
        view.performance_snapshot.managed_gpu_mebibytes()
    );
    diagnostic_row(
        ui,
        "GPU 渲染资源",
        &gpu_resources,
        tokens::COLOR_TEXT_PRIMARY,
        metrics,
    );
    if let Some(status) = view.performance_export_status {
        diagnostic_row(
            ui,
            "性能导出",
            status,
            if view.performance_export_failed {
                tokens::COLOR_ERROR
            } else {
                tokens::COLOR_TEXT_PRIMARY
            },
            metrics,
        );
    }
    ui.add_space(metrics.space_4);
    let button_width = ui.available_width();
    settings_action_button(
        ui,
        "导出性能数据",
        Icon::Download,
        SettingsActionStyle::Neutral,
        button_width,
        metrics,
        view.readable_mode,
    )
    .clicked()
    .then_some(UiCommand::ExportPerformanceData)
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

#[derive(Debug, Clone, Copy)]
struct SettingsAction {
    label: &'static str,
    icon: Icon,
    style: SettingsActionStyle,
    command: UiCommand,
}

const SETTINGS_ACTIONS: [SettingsAction; 3] = [
    SettingsAction {
        label: "打开配置文件",
        icon: Icon::Folder,
        style: SettingsActionStyle::Neutral,
        command: UiCommand::OpenSettingsDirectory,
    },
    SettingsAction {
        label: "重启应用",
        icon: Icon::Restart,
        style: SettingsActionStyle::Neutral,
        command: UiCommand::RestartApplication,
    },
    SettingsAction {
        label: "退出应用",
        icon: Icon::Power,
        style: SettingsActionStyle::Danger,
        command: UiCommand::ExitApplication,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证顶部操作栏的顺序和命令映射保持稳定。
    #[test]
    fn settings_actions_have_expected_order_and_commands() {
        let labels = SETTINGS_ACTIONS.map(|action| action.label);
        let commands = SETTINGS_ACTIONS.map(|action| action.command);

        assert_eq!(labels, ["打开配置文件", "重启应用", "退出应用"]);
        assert_eq!(
            commands,
            [
                UiCommand::OpenSettingsDirectory,
                UiCommand::RestartApplication,
                UiCommand::ExitApplication,
            ]
        );
    }

    /// 验证三个操作按钮扣除两段网格间距后恰好填满可用宽度。
    #[test]
    fn three_action_buttons_fill_available_width() {
        let available_width = 528.0;
        let gap = tokens::SETTINGS_METRICS.space_2;
        let width = equal_row_item_width(available_width, SETTINGS_ACTIONS.len(), gap);

        assert_eq!(
            width * SETTINGS_ACTIONS.len() as f32 + gap * 2.0,
            available_width
        );
    }

    /// 验证设置选项行使用完整触摸高度且双列仍精确填满可用宽度。
    #[test]
    fn setting_option_rows_use_full_touch_target() {
        let metrics = tokens::SETTINGS_METRICS;
        let option_width = equal_row_item_width(528.0, 2, metrics.space_2);

        assert_eq!(metrics.action_button_height, 48.0);
        assert_eq!(metrics.touch_target, 64.0);
        assert!(metrics.touch_target > metrics.action_button_height);
        assert_eq!(option_width * 2.0 + metrics.space_2, 528.0);
    }

    /// 验证页尾从 Cargo 元数据生成已确认的完整版权文案。
    #[test]
    fn footer_uses_package_metadata() {
        assert_eq!(
            settings_footer_text(),
            format!(
                "Steady Ink v{} · © 2026 {} · {}",
                env!("CARGO_PKG_VERSION"),
                env!("CARGO_PKG_AUTHORS"),
                env!("CARGO_PKG_LICENSE")
            )
        );
    }
}
