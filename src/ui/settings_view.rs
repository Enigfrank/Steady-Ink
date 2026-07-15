use egui::{
    Align, Color32, CornerRadius, Frame, Layout, Margin, ScrollArea, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};

use super::{
    design_tokens as tokens,
    settings_controls::render_tool_preferences,
    toolbar::{UiCommand, UiViewState},
};
use crate::slideshow::{ComCandidateStatus, ComDiagnostics};

/// 绘制默认工具、联动开关、诊断和版本信息组成的设置界面。
pub fn render(ui: &mut Ui, view: UiViewState<'_>) -> Option<UiCommand> {
    ui.set_min_size(ui.available_size());
    Frame::new()
        .fill(tokens::COLOR_BACKGROUND)
        .stroke(Stroke::new(1.0, tokens::COLOR_BORDER))
        .corner_radius(CornerRadius::same(tokens::CARD_RADIUS))
        .inner_margin(Margin::same(tokens::SPACE_4 as i8))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let mut command = render_header(ui);
            ui.separator();

            ScrollArea::vertical().show(ui, |ui| {
                section_heading(ui, "默认批注工具");
                if command.is_none() {
                    command = render_tool_preferences(ui, view.tools);
                } else {
                    let _ = render_tool_preferences(ui, view.tools);
                }

                ui.separator();
                section_heading(ui, "演示联动");
                let mut integration_enabled = view.slideshow_integration_enabled;
                if ui
                    .add_sized(
                        [ui.available_width(), tokens::TOUCH_TARGET],
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
                    .size(tokens::TEXT_XS)
                    .color(tokens::COLOR_TEXT_SECONDARY),
                );

                ui.separator();
                section_heading(ui, "诊断");
                render_diagnostics(ui, view);

                ui.separator();
                section_heading(ui, "关于");
                diagnostic_row(ui, "版本", env!("CARGO_PKG_VERSION"), tokens::COLOR_TEXT_PRIMARY);
                diagnostic_row(
                    ui,
                    "应用",
                    "Steady Ink",
                    tokens::COLOR_TEXT_PRIMARY,
                );
            });
            command
        })
        .inner
}

/// 绘制设置标题和右上角仅使用熟悉 X 符号的关闭按钮。
fn render_header(ui: &mut Ui) -> Option<UiCommand> {
    let mut command = None;
    ui.horizontal(|ui| {
        ui.heading("设置");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if close_button(ui).clicked() {
                command = Some(UiCommand::CloseSettings);
            }
        });
    });
    command
}

/// 绘制 64pt 触摸命中区域的关闭图标按钮。
fn close_button(ui: &mut Ui) -> egui::Response {
    let size = Vec2::splat(tokens::TOUCH_TARGET);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if response.hovered() {
            tokens::COLOR_HOVER
        } else {
            tokens::COLOR_SURFACE
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(tokens::BUTTON_RADIUS), fill);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(tokens::BUTTON_RADIUS),
            Stroke::new(1.0, tokens::COLOR_BORDER),
            StrokeKind::Inside,
        );
        let half = tokens::ICON_SIZE / 2.0;
        ui.painter().line_segment(
            [
                rect.center() + egui::vec2(-half, -half),
                rect.center() + egui::vec2(half, half),
            ],
            Stroke::new(2.0, tokens::COLOR_TEXT_SECONDARY),
        );
        ui.painter().line_segment(
            [
                rect.center() + egui::vec2(half, -half),
                rect.center() + egui::vec2(-half, half),
            ],
            Stroke::new(2.0, tokens::COLOR_TEXT_SECONDARY),
        );
    }
    response.on_hover_text("关闭设置")
}

/// 绘制设置页各部分的 20pt 标题。
fn section_heading(ui: &mut Ui, title: &str) {
    ui.label(
        egui::RichText::new(title)
            .size(tokens::TEXT_LG)
            .strong()
            .color(tokens::COLOR_TEXT_PRIMARY),
    );
}

/// 绘制 COM、控制、GPU 和设置文件状态。
fn render_diagnostics(ui: &mut Ui, view: UiViewState<'_>) {
    let (com_status, com_color) =
        com_status(view.slideshow_integration_enabled, view.com_diagnostics);
    diagnostic_row(ui, "COM 检测", com_status, com_color);
    diagnostic_row(
        ui,
        "当前连接",
        view.slideshow_connection_error.unwrap_or("未报告连接中断"),
        if view.slideshow_connection_error.is_some() {
            tokens::COLOR_ERROR
        } else {
            tokens::COLOR_TEXT_PRIMARY
        },
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
    );
    diagnostic_row(
        ui,
        "图形设备",
        &view.gl_diagnostics.renderer,
        if view.gl_diagnostics.software_fallback {
            tokens::COLOR_ERROR
        } else {
            tokens::COLOR_TEXT_PRIMARY
        },
    );
    diagnostic_row(
        ui,
        "设置文件",
        &view.settings_path.display().to_string(),
        tokens::COLOR_TEXT_PRIMARY,
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
    );
}

/// 根据联动开关和候选诊断生成紧凑 COM 状态。
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

/// 绘制左右两列的诊断名称和值，并允许长路径自然换行。
fn diagnostic_row(ui: &mut Ui, label: &str, value: &str, value_color: Color32) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(tokens::DIAGNOSTIC_LABEL_WIDTH, 0.0),
            Layout::left_to_right(Align::Min),
            |ui| {
                ui.label(
                    egui::RichText::new(label)
                        .size(tokens::TEXT_SM)
                        .color(tokens::COLOR_TEXT_SECONDARY),
                );
            },
        );
        ui.label(
            egui::RichText::new(value)
                .size(tokens::TEXT_SM)
                .color(value_color),
        );
    });
}
