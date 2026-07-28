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
                    section_heading(ui, "显示", metrics);
                    let mut readable_mode = view.readable_mode;
                    if ui
                        .add_sized(
                            [ui.available_width(), metrics.touch_target],
                            egui::Checkbox::new(&mut readable_mode, "易读模式"),
                        )
                        .changed()
                        && command.is_none()
                    {
                        command = Some(UiCommand::SetReadableMode(readable_mode));
                    }
                    let mut performance_monitoring_enabled =
                        view.performance_monitoring_enabled;
                    if ui
                        .add_sized(
                            [ui.available_width(), metrics.touch_target],
                            egui::Checkbox::new(
                                &mut performance_monitoring_enabled,
                                "显示性能监控",
                            ),
                        )
                        .changed()
                        && command.is_none()
                    {
                        command = Some(UiCommand::SetPerformanceMonitoringEnabled(
                            performance_monitoring_enabled,
                        ));
                    }
                    if let Some(error) = view.ink_rendering_error {
                        ui.label(
                            egui::RichText::new(error)
                                .size(metrics.text_xs)
                                .color(tokens::COLOR_ERROR),
                        );
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
                            ui.add_space(metrics.space_4);
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
                        });

                    section_break(ui, metrics);
                    section_heading(ui, "应用操作", metrics);
                    let action_command = render_action_bar(ui, metrics, view.readable_mode);
                    if command.is_none() {
                        command = action_command;
                    }
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

/// 绘制配置目录和退出软件组成的顶部等宽操作栏。
fn render_action_bar(
    ui: &mut Ui,
    metrics: InterfaceMetrics,
    readable_mode: bool,
) -> Option<UiCommand> {
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
            readable_mode,
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
            readable_mode,
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
        ui.add_enabled(
            false,
            egui::Checkbox::new(&mut disabled, "为所有用户开机启动"),
        );
        ui.label(
            egui::RichText::new("无法读取系统级启动状态，请重新打开设置后重试。")
                .size(metrics.text_xs)
                .color(tokens::COLOR_ERROR),
        );
        if let Some(error) = view.machine_autostart_error {
            ui.label(
                egui::RichText::new(error)
                    .size(metrics.text_xs)
                    .color(tokens::COLOR_ERROR),
            );
        }
        return None;
    };

    let mut enabled = state.enabled();
    let changed = ui
        .add_sized(
            [ui.available_width(), metrics.touch_target],
            egui::Checkbox::new(&mut enabled, "为所有用户开机启动"),
        )
        .changed();
    ui.label(
        egui::RichText::new(state.label())
            .size(metrics.text_xs)
            .color(
                if matches!(state, MachineAutostartState::EnabledPathMismatch) {
                    tokens::COLOR_ERROR
                } else {
                    tokens::COLOR_TEXT_SECONDARY
                },
            ),
    );
    if let Some(error) = view.machine_autostart_error {
        ui.label(
            egui::RichText::new(error)
                .size(metrics.text_xs)
                .color(tokens::COLOR_ERROR),
        );
    }
    changed.then_some(UiCommand::SetMachineAutostart(enabled))
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
        "墨迹恢复",
        view.recovery_error.unwrap_or("正常"),
        if view.recovery_error.is_some() {
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
    diagnostic_row(
        ui,
        "自启动诊断",
        view.machine_autostart_error.unwrap_or("正常"),
        if view.machine_autostart_error.is_some() {
            tokens::COLOR_ERROR
        } else {
            tokens::COLOR_TEXT_PRIMARY
        },
        metrics,
    );
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
