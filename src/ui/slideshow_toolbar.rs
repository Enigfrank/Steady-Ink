use egui::{
    Align2, Area, Context, CornerRadius, FontId, Frame, Id, Margin, Order, Pos2, Rect, RectAlign,
    Sense, Stroke, StrokeKind, Ui, Vec2,
};

use super::{
    design_tokens as tokens,
    settings_controls::SelectorOrientation,
    toolbar::{Icon, UiCommand, UiViewState, icon_button, render_ink_tool_buttons},
};
use crate::{app::AppMode, window::DockSide};

const BODY_BUTTON_COUNT: f32 = 8.0;

/// 绘制放映态双侧翻页组、底部胶囊工具栏和可选退出确认框。
pub fn render(context: &Context, view: UiViewState<'_>) -> Option<UiCommand> {
    let mut command = None;
    keep_first(
        &mut command,
        render_navigation_group(context, DockSide::Left, view),
    );
    keep_first(
        &mut command,
        render_navigation_group(context, DockSide::Right, view),
    );
    keep_first(&mut command, render_bottom_toolbar(context, view));

    if view.mode == AppMode::SlideShowConnectionLost {
        if view.dismiss_slideshow_confirmation {
            keep_first(&mut command, render_dismiss_confirmation(context));
        } else {
            render_connection_status(context);
        }
    }

    command
}

/// 在目标尚无命令时保留第一个离散交互，避免同一帧后绘制区域覆盖先前点击。
fn keep_first(target: &mut Option<UiCommand>, candidate: Option<UiCommand>) {
    if target.is_none() {
        *target = candidate;
    }
}

/// 在屏幕指定一侧绘制完整的上一页、可选页码和下一页控件组。
fn render_navigation_group(
    context: &Context,
    side: DockSide,
    view: UiViewState<'_>,
) -> Option<UiCommand> {
    let (id, anchor, offset) = navigation_placement(side);

    Area::new(id)
        .anchor(anchor, offset)
        .order(Order::Foreground)
        .show(context, |ui| {
            Frame::new()
                .fill(tokens::COLOR_BACKGROUND)
                .stroke(Stroke::new(1.0, tokens::COLOR_BORDER))
                .corner_radius(CornerRadius::same(tokens::CAPSULE_RADIUS))
                .inner_margin(Margin::same(tokens::MARGIN_SPACE_2))
                .show(ui, |ui| {
                    ui.add_enabled_ui(view.slideshow_controls_enabled, |ui| {
                        ui.horizontal(|ui| {
                            let mut command = None;
                            if icon_button(ui, "上一页", Icon::Previous, false, None).clicked() {
                                command = Some(UiCommand::PreviousSlide);
                            }
                            if let Some((current, total)) = view.slide_page_numbers {
                                render_page_number(ui, current, total);
                            }
                            if icon_button(ui, "下一页", Icon::Next, false, None).clicked() {
                                command = Some(UiCommand::NextSlide);
                            }
                            command
                        })
                        .inner
                    })
                    .inner
                })
                .inner
        })
        .inner
}

/// 返回左右翻页组在对应屏幕下角的稳定锚点，并让控件外框紧贴屏幕边缘。
fn navigation_placement(side: DockSide) -> (Id, Align2, Vec2) {
    match side {
        DockSide::Left => (
            Id::new("slideshow_navigation_left"),
            Align2::LEFT_BOTTOM,
            Vec2::ZERO,
        ),
        DockSide::Right => (
            Id::new("slideshow_navigation_right"),
            Align2::RIGHT_BOTTOM,
            Vec2::ZERO,
        ),
    }
}

/// 绘制仅在页码可靠时出现的页码区域，不为未知页码预留空白宽度。
fn render_page_number(ui: &mut Ui, current: u32, total: u32) {
    let size = Vec2::new(tokens::PAGE_NUMBER_WIDTH, tokens::TOUCH_TARGET);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }

    ui.painter().rect_filled(
        rect,
        CornerRadius::same(tokens::BUTTON_RADIUS),
        tokens::COLOR_SURFACE,
    );
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(tokens::BUTTON_RADIUS),
        Stroke::new(1.0, tokens::COLOR_BORDER),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        format!("{current}/{total}"),
        FontId::proportional(tokens::TEXT_BASE),
        if ui.is_enabled() {
            tokens::COLOR_TEXT_PRIMARY
        } else {
            tokens::COLOR_TEXT_TERTIARY
        },
    );
}

/// 绘制固定收缩按钮和从其右侧滑入、滑出的工具栏主体。
fn render_bottom_toolbar(context: &Context, view: UiViewState<'_>) -> Option<UiCommand> {
    let screen = context.content_rect();
    let body_width = toolbar_body_width();
    let toggle_width = toolbar_outer_height();
    let overlap = tokens::SPACE_2;
    let full_width = toggle_width + body_width - overlap;
    let toggle_left = screen.center().x - full_width / 2.0;
    let toolbar_top = bottom_toolbar_top(screen);
    let body_origin_x = toggle_left + toggle_width - overlap;
    let expanded = view.mode != AppMode::SlideShowAnnotatingCollapsed;
    let progress = context.animate_bool_with_time(
        Id::new("slideshow_toolbar_expanded_animation"),
        expanded,
        tokens::SLIDESHOW_TOOLBAR_ANIMATION_SECONDS,
    );

    let mut command = render_toolbar_body(
        context,
        view,
        Pos2::new(body_origin_x, toolbar_top),
        body_width,
        progress,
    );
    keep_first(
        &mut command,
        render_toolbar_toggle(context, view, Pos2::new(toggle_left, toolbar_top), expanded),
    );
    command
}

/// 绘制随动画向固定收缩按钮方向平移并裁剪的工具栏主体。
fn render_toolbar_body(
    context: &Context,
    view: UiViewState<'_>,
    origin: Pos2,
    body_width: f32,
    progress: f32,
) -> Option<UiCommand> {
    if progress <= f32::EPSILON {
        return None;
    }

    let animated_left = origin.x - body_width * (1.0 - progress);
    let animated_right = origin.x + body_width * progress;
    let clip_rect = Rect::from_min_max(
        origin,
        Pos2::new(animated_right, origin.y + toolbar_outer_height()),
    );
    let body_interactive = progress >= 1.0 - f32::EPSILON;

    Area::new("slideshow_toolbar_body".into())
        .fixed_pos(Pos2::new(animated_left, origin.y))
        .order(Order::Middle)
        .interactable(body_interactive)
        .show(context, |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
            Frame::new()
                .fill(tokens::COLOR_BACKGROUND)
                .stroke(Stroke::new(1.0, tokens::COLOR_BORDER))
                .corner_radius(CornerRadius::same(tokens::CAPSULE_RADIUS))
                .inner_margin(Margin::same(tokens::MARGIN_SPACE_2))
                .show(ui, |ui| {
                    ui.set_min_width(toolbar_body_content_width());
                    ui.set_max_width(toolbar_body_content_width());
                    ui.horizontal(|ui| {
                        let interaction = render_ink_tool_buttons(
                            ui,
                            view.tools,
                            RectAlign::TOP_START,
                            SelectorOrientation::Horizontal,
                        );
                        let mut command = interaction.command;
                        let (exit_label, exit_enabled, requested_command) =
                            if view.mode == AppMode::SlideShowConnectionLost {
                                ("退出批注", true, UiCommand::RequestDismissSlideshow)
                            } else {
                                (
                                    "退出放映",
                                    view.slideshow_controls_enabled,
                                    UiCommand::ExitSlideShow,
                                )
                            };
                        let exit_clicked = ui
                            .push_id("slideshow_exit_action", |ui| {
                                ui.add_enabled_ui(exit_enabled, |ui| {
                                    icon_button(ui, exit_label, Icon::Exit, false, None).clicked()
                                })
                                .inner
                            })
                            .inner;
                        let exit_command = if exit_clicked {
                            Some(requested_command)
                        } else {
                            None
                        };
                        keep_first(&mut command, exit_command);
                        command
                    })
                    .inner
                })
                .inner
        })
        .inner
}

/// 绘制位置始终不变的收缩或展开按钮；连接中断时禁止再次收缩。
fn render_toolbar_toggle(
    context: &Context,
    view: UiViewState<'_>,
    position: Pos2,
    expanded: bool,
) -> Option<UiCommand> {
    Area::new("slideshow_toolbar_toggle".into())
        .fixed_pos(position)
        .order(Order::Foreground)
        .show(context, |ui| {
            Frame::new()
                .fill(tokens::COLOR_BACKGROUND)
                .stroke(Stroke::new(1.0, tokens::COLOR_BORDER))
                .corner_radius(CornerRadius::same(tokens::CAPSULE_RADIUS))
                .inner_margin(Margin::same(tokens::MARGIN_SPACE_2))
                .show(ui, |ui| {
                    ui.add_enabled_ui(view.mode != AppMode::SlideShowConnectionLost, |ui| {
                        let (label, icon) = if expanded {
                            ("收缩", Icon::Collapse)
                        } else {
                            ("展开", Icon::Expand)
                        };
                        icon_button(ui, label, icon, false, None).clicked()
                    })
                    .inner
                    .then_some(UiCommand::ToggleSlideshowToolbar)
                })
                .inner
        })
        .inner
}

/// 在断线降级态工具栏上方显示简短状态，不占用底部工具按钮宽度。
fn render_connection_status(context: &Context) {
    Area::new("slideshow_connection_status".into())
        .anchor(
            Align2::CENTER_BOTTOM,
            Vec2::new(0.0, -(toolbar_outer_height() + tokens::SPACE_2)),
        )
        .order(Order::Foreground)
        .interactable(false)
        .show(context, |ui| {
            Frame::new()
                .fill(tokens::COLOR_SURFACE)
                .stroke(Stroke::new(1.0, tokens::COLOR_ERROR_SURFACE))
                .corner_radius(CornerRadius::same(tokens::CARD_RADIUS))
                .inner_margin(Margin::symmetric(
                    tokens::MARGIN_SPACE_4,
                    tokens::MARGIN_SPACE_2,
                ))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("演示连接中断")
                            .size(tokens::TEXT_SM)
                            .color(tokens::COLOR_ERROR),
                    );
                });
        });
}

/// 绘制退出本地批注的紧凑确认框，确认不会调用 COM 或发送模拟按键。
fn render_dismiss_confirmation(context: &Context) -> Option<UiCommand> {
    Area::new("slideshow_dismiss_confirmation".into())
        .anchor(
            Align2::CENTER_BOTTOM,
            Vec2::new(0.0, -(toolbar_outer_height() + tokens::SPACE_2)),
        )
        .order(Order::Foreground)
        .show(context, |ui| {
            Frame::new()
                .fill(tokens::COLOR_SURFACE)
                .stroke(Stroke::new(1.0, tokens::COLOR_BORDER))
                .corner_radius(CornerRadius::same(tokens::CARD_RADIUS))
                .inner_margin(Margin::same(tokens::MARGIN_SPACE_2))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("退出批注并清空本次放映墨迹？")
                                .size(tokens::TEXT_SM)
                                .color(tokens::COLOR_TEXT_PRIMARY),
                        );
                        ui.horizontal(|ui| {
                            if icon_button(ui, "取消", Icon::Cancel, false, None).clicked() {
                                return Some(UiCommand::CancelDismissSlideshow);
                            }
                            if icon_button(ui, "确认", Icon::Confirm, false, None).clicked() {
                                return Some(UiCommand::ConfirmDismissSlideshow);
                            }
                            None
                        })
                        .inner
                    })
                    .inner
                })
                .inner
        })
        .inner
}

/// 返回底部胶囊工具栏的固定外层高度。
const fn toolbar_outer_height() -> f32 {
    tokens::TOUCH_TARGET + tokens::SPACE_2 * 2.0
}

/// 返回底部胶囊工具栏紧贴屏幕底边时的顶部坐标。
fn bottom_toolbar_top(screen: Rect) -> f32 {
    screen.bottom() - toolbar_outer_height()
}

/// 返回八个功能按钮及间距占用的固定内容宽度。
const fn toolbar_body_content_width() -> f32 {
    BODY_BUTTON_COUNT * tokens::TOUCH_TARGET + (BODY_BUTTON_COUNT - 1.0) * tokens::SPACE_2
}

/// 返回包含左右内边距的底部工具栏主体宽度。
const fn toolbar_body_width() -> f32 {
    toolbar_body_content_width() + tokens::SPACE_2 * 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证双侧翻页组固定在左右下角，并同时紧贴横向和纵向屏幕边缘。
    #[test]
    fn navigation_groups_use_bottom_corner_anchors() {
        let (_, left_anchor, left_offset) = navigation_placement(DockSide::Left);
        let (_, right_anchor, right_offset) = navigation_placement(DockSide::Right);

        assert_eq!(left_anchor, Align2::LEFT_BOTTOM);
        assert_eq!(right_anchor, Align2::RIGHT_BOTTOM);
        assert_eq!(left_offset, Vec2::ZERO);
        assert_eq!(right_offset, Vec2::ZERO);
    }

    /// 验证放映态中央工具栏的外框底边与屏幕底边完全重合。
    #[test]
    fn bottom_toolbar_touches_screen_bottom() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_920.0, 1_080.0));

        assert_eq!(
            bottom_toolbar_top(screen) + toolbar_outer_height(),
            screen.bottom()
        );
    }
}
