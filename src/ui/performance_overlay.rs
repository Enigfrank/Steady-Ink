use egui::{Align, Align2, CornerRadius, FontId, Layout, Sense, Shape, Stroke, Vec2};

use super::{design_tokens as tokens, pixel_snap};
use crate::performance::{PerformanceSnapshot, SLOW_FRAME_THRESHOLD};

/// 在可书写全屏画布顶部居中绘制不参与交互的性能快照。
pub(super) fn render(context: &egui::Context, snapshot: PerformanceSnapshot, readable_mode: bool) {
    egui::Area::new(egui::Id::new("performance_overlay"))
        .anchor(
            Align2::CENTER_TOP,
            Vec2::new(0.0, tokens::PERFORMANCE_OVERLAY_MARGIN),
        )
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(context, |ui| {
            pixel_snap::show_pixel_aligned_frame(
                ui,
                tokens::material_frame(
                    readable_mode,
                    tokens::MaterialRole::Floating,
                    CornerRadius::same(tokens::CARD_RADIUS),
                    egui::Margin::same(tokens::MARGIN_SPACE_2),
                ),
                |ui| {
                    ui.set_width(tokens::PERFORMANCE_OVERLAY_WIDTH);
                    ui.label(
                        egui::RichText::new("性能")
                            .font(FontId::proportional(tokens::TEXT_SM))
                            .strong()
                            .color(tokens::COLOR_TEXT_PRIMARY),
                    );
                    ui.add_space(tokens::SPACE_1);
                    render_frame_chart(ui, snapshot);
                    ui.add_space(tokens::SPACE_2);
                    metric_row(ui, "FPS", format!("{:.1}", snapshot.fps()));
                    metric_row(
                        ui,
                        "帧耗时",
                        format!(
                            "{:.2} / {:.2} ms",
                            snapshot.last_frame_time_ms(),
                            snapshot.p95_frame_time_ms()
                        ),
                    );
                    metric_row(
                        ui,
                        "输入延迟",
                        if snapshot.input_sample_count() == 0 {
                            "暂无样本".to_owned()
                        } else {
                            format!("{:.2} ms", snapshot.p95_input_latency_ms())
                        },
                    );
                    metric_row(ui, "可见笔画", snapshot.visible_strokes().to_string());
                    metric_row(
                        ui,
                        "画面 G/S/P",
                        format!(
                            "{}/{}/{}",
                            snapshot.generated_frames(),
                            snapshot.submitted_frames(),
                            snapshot.presented_frames()
                        ),
                    );
                    metric_row(
                        ui,
                        "丢弃 / 替换",
                        format!(
                            "{} / {}",
                            snapshot.discarded_frames(),
                            snapshot.mailbox_replacements()
                        ),
                    );
                    metric_row(
                        ui,
                        "活动点 / 增量",
                        format!(
                            "{} / {}",
                            snapshot.active_samples(),
                            snapshot.incremental_primitives()
                        ),
                    );
                    metric_row(ui, "活动回退", snapshot.full_active_fallbacks().to_string());
                    metric_row(
                        ui,
                        "缓存重建",
                        format!(
                            "{} / {}",
                            snapshot.region_rebuild_count(),
                            snapshot.full_rebuild_count()
                        ),
                    );
                    metric_row(
                        ui,
                        "GPU 资源",
                        format!("{:.1} MiB", snapshot.managed_gpu_mebibytes()),
                    );
                },
            );
        });
}

/// 绘制最近帧耗时折线，并以 33ms 作为最小纵轴范围。
fn render_frame_chart(ui: &mut egui::Ui, snapshot: PerformanceSnapshot) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(
            tokens::PERFORMANCE_OVERLAY_WIDTH,
            tokens::PERFORMANCE_CHART_HEIGHT,
        ),
        Sense::hover(),
    );
    let samples = snapshot.frame_times_ms();
    if samples.len() < 2 {
        return;
    }
    let slow_frame_ms = SLOW_FRAME_THRESHOLD.as_secs_f32() * 1_000.0;
    let chart_max = samples.iter().copied().fold(slow_frame_ms, f32::max);
    let last_index = (samples.len() - 1) as f32;
    let points = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let x = rect.left() + rect.width() * index as f32 / last_index;
            let normalized = (*sample / chart_max).clamp(0.0, 1.0);
            egui::pos2(x, rect.bottom() - rect.height() * normalized)
        })
        .collect();
    ui.painter().add(Shape::line(
        points,
        Stroke::new(tokens::TOOL_METRICS.points(2.0), tokens::COLOR_PRIMARY),
    ));
}

/// 绘制固定宽度的性能指标名称和值。
fn metric_row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(label)
                .size(tokens::TEXT_SM)
                .color(tokens::COLOR_TEXT_SECONDARY),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .size(tokens::TEXT_SM)
                    .color(tokens::COLOR_TEXT_PRIMARY),
            );
        });
    });
}
