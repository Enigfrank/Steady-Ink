use egui::{
    Align2, Area, Context, CornerRadius, FontId, Id, Margin, Order, Pos2, Rect, RectAlign,
    Response, Sense, Stroke, Ui, Vec2,
};

use super::{
    design_tokens as tokens, pixel_snap,
    settings_controls::SelectorOrientation,
    toolbar::{
        Icon, UiCommand, UiFrameOutput, UiViewState, icon_button, paint_icon,
        render_ink_tool_buttons,
    },
};
use crate::{app::AppMode, window::DockSide};

const BODY_BUTTON_COUNT: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct SlideshowToolbarState {
    toggle_position: Pos2,
    session_generation: u64,
    viewport_size: Vec2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpansionDirection {
    Left,
    Right,
}

#[derive(Debug, Default)]
struct ToggleInteraction {
    command: Option<UiCommand>,
    drag_delta: Vec2,
}

#[derive(Debug, Clone, Copy)]
struct ToolbarPlacement {
    origin: Pos2,
    body_width: f32,
    direction: ExpansionDirection,
}

/// 绘制放映态控件，并返回命令和本帧实际交互区域。
pub fn render(context: &Context, view: UiViewState<'_>) -> UiFrameOutput {
    let mut command = None;
    let mut hit_regions = Vec::new();
    keep_first(
        &mut command,
        render_navigation_group(context, DockSide::Left, view, &mut hit_regions),
    );
    keep_first(
        &mut command,
        render_navigation_group(context, DockSide::Right, view, &mut hit_regions),
    );
    keep_first(
        &mut command,
        render_bottom_toolbar(context, view, &mut hit_regions),
    );

    if view.mode == AppMode::SlideShowConnectionLost {
        if view.dismiss_slideshow_confirmation {
            keep_first(
                &mut command,
                render_dismiss_confirmation(context, view.readable_mode, &mut hit_regions),
            );
        } else {
            render_connection_status(context, view.readable_mode, &mut hit_regions);
        }
    }

    UiFrameOutput {
        command,
        slideshow_hit_regions: hit_regions,
    }
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
    hit_regions: &mut Vec<Rect>,
) -> Option<UiCommand> {
    let (id, anchor, offset) = navigation_placement(side);

    let response = Area::new(id)
        .anchor(anchor, offset)
        .order(Order::Foreground)
        .show(context, |ui| {
            pixel_snap::show_pixel_aligned_frame(
                ui,
                tokens::material_frame(
                    view.readable_mode,
                    tokens::MaterialRole::Floating,
                    CornerRadius::same(tokens::CAPSULE_RADIUS),
                    Margin::same(tokens::MARGIN_SPACE_2),
                ),
                |ui| {
                    ui.add_enabled_ui(view.slideshow_controls_enabled, |ui| {
                        ui.horizontal(|ui| {
                            let mut command = None;
                            if icon_only_button(ui, Icon::Previous, "上一页", view.readable_mode)
                                .clicked()
                            {
                                command = Some(UiCommand::PreviousSlide);
                            }
                            if let Some((current, total)) = view.slide_page_numbers {
                                render_page_number(ui, current, total, view.readable_mode);
                            }
                            if icon_only_button(ui, Icon::Next, "下一页", view.readable_mode)
                                .clicked()
                            {
                                command = Some(UiCommand::NextSlide);
                            }
                            command
                        })
                        .inner
                    })
                    .inner
                },
            )
            .inner
        });
    hit_regions.push(response.response.rect);
    response.inner
}

/// 绘制仅含居中图标、无文字说明的方形按钮，悬停时显示文字提示。
fn icon_only_button(ui: &mut Ui, icon: Icon, tooltip: &str, readable_mode: bool) -> Response {
    let size = Vec2::splat(tokens::TOUCH_TARGET);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let enabled = ui.is_enabled();
        let (fill, border) = tokens::button_colors(readable_mode, false, &response, enabled);
        pixel_snap::paint_pixel_aligned_rect(
            ui,
            rect,
            CornerRadius::same(tokens::BUTTON_RADIUS),
            fill,
            Stroke::new(1.0, border),
        );
        let foreground = if enabled {
            tokens::COLOR_TEXT_SECONDARY
        } else {
            tokens::COLOR_TEXT_TERTIARY
        };
        paint_icon(
            ui,
            rect.center(),
            icon,
            None,
            foreground,
            foreground,
            tokens::TOOL_METRICS,
        );
    }
    response.on_hover_text(tooltip)
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
fn render_page_number(ui: &mut Ui, current: u32, total: u32, readable_mode: bool) {
    let size = Vec2::new(tokens::PAGE_NUMBER_WIDTH, tokens::TOUCH_TARGET);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let palette = tokens::material_palette(readable_mode, tokens::MaterialRole::Control);
    pixel_snap::paint_pixel_aligned_rect(
        ui,
        rect,
        CornerRadius::same(tokens::BUTTON_RADIUS),
        palette.selected,
        Stroke::new(1.0, palette.selected_border),
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
fn render_bottom_toolbar(
    context: &Context,
    view: UiViewState<'_>,
    hit_regions: &mut Vec<Rect>,
) -> Option<UiCommand> {
    let screen = context.content_rect();
    let body_width = toolbar_body_width();
    let toggle_width = toolbar_outer_height();
    let overlap = tokens::SPACE_2;
    let full_width = toggle_width + body_width - overlap;
    let session_generation = view.slideshow_session_generation.unwrap_or_default();
    let state_id = Id::new("slideshow_toolbar_state");
    let mut state = context.data_mut(|data| {
        data.get_temp::<SlideshowToolbarState>(state_id)
            .filter(|state| state.session_generation == session_generation)
            .unwrap_or_else(|| SlideshowToolbarState {
                toggle_position: Pos2::new(
                    screen.center().x - full_width / 2.0,
                    bottom_toolbar_top(screen),
                ),
                session_generation,
                viewport_size: screen.size(),
            })
    });
    if state.viewport_size != screen.size() {
        state.toggle_position =
            constrain_toggle_position(state.toggle_position, screen, Vec2::splat(toggle_width));
    }
    state.viewport_size = screen.size();
    let direction = expansion_direction(
        screen,
        state.toggle_position,
        body_width,
        toggle_width,
        overlap,
    );
    let placement = ToolbarPlacement {
        origin: match direction {
            ExpansionDirection::Right => Pos2::new(
                state.toggle_position.x + toggle_width - overlap,
                state.toggle_position.y,
            ),
            ExpansionDirection::Left => Pos2::new(
                state.toggle_position.x - body_width + overlap,
                state.toggle_position.y,
            ),
        },
        body_width,
        direction,
    };
    let expanded = view.mode != AppMode::SlideShowAnnotatingCollapsed;
    let progress = context.animate_bool_with_time(
        Id::new("slideshow_toolbar_expanded_animation"),
        expanded,
        tokens::SLIDESHOW_TOOLBAR_ANIMATION_SECONDS,
    );

    let mut command = render_toolbar_body(context, view, placement, progress, hit_regions);
    let toggle_interaction =
        render_toolbar_toggle(context, view, state.toggle_position, expanded, hit_regions);
    state.toggle_position += toggle_interaction.drag_delta;
    state.toggle_position =
        constrain_toggle_position(state.toggle_position, screen, Vec2::splat(toggle_width));
    keep_first(&mut command, toggle_interaction.command);
    context.data_mut(|data| data.insert_temp(state_id, state));
    command
}

/// 绘制随动画向固定收缩按钮方向平移并裁剪的工具栏主体。
fn render_toolbar_body(
    context: &Context,
    view: UiViewState<'_>,
    placement: ToolbarPlacement,
    progress: f32,
    hit_regions: &mut Vec<Rect>,
) -> Option<UiCommand> {
    if progress <= f32::EPSILON {
        return None;
    }

    let (animated_left, clip_rect) = match placement.direction {
        ExpansionDirection::Right => (
            placement.origin.x - placement.body_width * (1.0 - progress),
            Rect::from_min_max(
                placement.origin,
                Pos2::new(
                    placement.origin.x + placement.body_width * progress,
                    placement.origin.y + toolbar_outer_height(),
                ),
            ),
        ),
        ExpansionDirection::Left => (
            placement.origin.x + placement.body_width * (1.0 - progress),
            Rect::from_min_max(
                placement.origin,
                Pos2::new(
                    placement.origin.x + placement.body_width,
                    placement.origin.y + toolbar_outer_height(),
                ),
            ),
        ),
    };
    let response = Area::new("slideshow_toolbar_body".into())
        .fixed_pos(Pos2::new(animated_left, placement.origin.y))
        .order(Order::Middle)
        .interactable(true)
        .show(context, |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(clip_rect));
            pixel_snap::show_pixel_aligned_frame(
                ui,
                tokens::material_frame(
                    view.readable_mode,
                    tokens::MaterialRole::Floating,
                    CornerRadius::same(tokens::CAPSULE_RADIUS),
                    Margin::same(tokens::MARGIN_SPACE_2),
                ),
                |ui| {
                    ui.set_min_width(toolbar_body_content_width());
                    ui.set_max_width(toolbar_body_content_width());
                    ui.horizontal(|ui| {
                        let interaction = render_ink_tool_buttons(
                            ui,
                            view.tools,
                            Some(view.slideshow_input_mode),
                            RectAlign::TOP_START,
                            SelectorOrientation::Horizontal,
                            view.readable_mode,
                        );
                        hit_regions.extend(interaction.popup_rects);
                        let mut command = interaction.command;
                        ui.add_space(tokens::SPACE_2);
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
                                    icon_button(
                                        ui,
                                        exit_label,
                                        Icon::Exit,
                                        false,
                                        None,
                                        view.readable_mode,
                                    )
                                    .clicked()
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
                },
            )
            .inner
        });
    let visible_rect = response.response.rect.intersect(clip_rect);
    if visible_rect.width() > 0.0 && visible_rect.height() > 0.0 {
        hit_regions.push(visible_rect);
    }
    response.inner
}

/// 绘制位置始终不变的收缩或展开按钮；连接中断时禁止再次收缩。
fn render_toolbar_toggle(
    context: &Context,
    view: UiViewState<'_>,
    position: Pos2,
    expanded: bool,
    hit_regions: &mut Vec<Rect>,
) -> ToggleInteraction {
    let response = Area::new("slideshow_toolbar_toggle".into())
        .fixed_pos(position)
        .order(Order::Foreground)
        .show(context, |ui| {
            pixel_snap::show_pixel_aligned_frame(
                ui,
                tokens::material_frame(
                    view.readable_mode,
                    tokens::MaterialRole::Floating,
                    CornerRadius::same(tokens::CAPSULE_RADIUS),
                    Margin::same(tokens::MARGIN_SPACE_2),
                ),
                |ui| {
                    let response = ui
                        .add_enabled_ui(view.mode != AppMode::SlideShowConnectionLost, |ui| {
                            let (label, icon) = if expanded {
                                ("收缩", Icon::Collapse)
                            } else {
                                ("展开", Icon::Expand)
                            };
                            icon_button(ui, label, icon, false, None, view.readable_mode)
                        })
                        .inner;
                    ToggleInteraction {
                        command: response
                            .clicked()
                            .then_some(UiCommand::ToggleSlideshowToolbar),
                        drag_delta: response.drag_delta(),
                    }
                },
            )
            .inner
        });
    hit_regions.push(response.response.rect);
    response.inner
}

/// 约束收缩/展开按钮完整留在当前可见视口内。
fn constrain_toggle_position(position: Pos2, screen: Rect, toggle_size: Vec2) -> Pos2 {
    Pos2::new(
        position.x.clamp(
            screen.left(),
            (screen.right() - toggle_size.x).max(screen.left()),
        ),
        position.y.clamp(
            screen.top(),
            (screen.bottom() - toggle_size.y).max(screen.top()),
        ),
    )
}

/// 根据按钮两侧空间选择工具栏主体展开方向，优先保持向右展开。
fn expansion_direction(
    screen: Rect,
    toggle_position: Pos2,
    body_width: f32,
    toggle_width: f32,
    overlap: f32,
) -> ExpansionDirection {
    let required_width = body_width - overlap;
    let right_space = screen.right() - (toggle_position.x + toggle_width - overlap);
    let left_space = toggle_position.x + overlap - screen.left();
    if right_space >= required_width || right_space >= left_space {
        ExpansionDirection::Right
    } else {
        ExpansionDirection::Left
    }
}

/// 在断线降级态工具栏上方显示简短状态，不占用底部工具按钮宽度。
fn render_connection_status(context: &Context, readable_mode: bool, hit_regions: &mut Vec<Rect>) {
    let response = Area::new("slideshow_connection_status".into())
        .anchor(
            Align2::CENTER_BOTTOM,
            Vec2::new(0.0, -(toolbar_outer_height() + tokens::SPACE_2)),
        )
        .order(Order::Foreground)
        .interactable(false)
        .show(context, |ui| {
            pixel_snap::show_pixel_aligned_frame(
                ui,
                tokens::material_frame(
                    readable_mode,
                    tokens::MaterialRole::Popover,
                    CornerRadius::same(tokens::CARD_RADIUS),
                    Margin::symmetric(tokens::MARGIN_SPACE_4, tokens::MARGIN_SPACE_2),
                )
                .stroke(Stroke::new(1.0, tokens::COLOR_ERROR_SURFACE)),
                |ui| {
                    ui.label(
                        egui::RichText::new("演示连接中断")
                            .size(tokens::TEXT_SM)
                            .color(tokens::COLOR_ERROR),
                    );
                },
            );
        });
    hit_regions.push(response.response.rect);
}

/// 绘制退出本地批注的紧凑确认框，确认不会调用 COM 或发送模拟按键。
fn render_dismiss_confirmation(
    context: &Context,
    readable_mode: bool,
    hit_regions: &mut Vec<Rect>,
) -> Option<UiCommand> {
    let response = Area::new("slideshow_dismiss_confirmation".into())
        .anchor(
            Align2::CENTER_BOTTOM,
            Vec2::new(0.0, -(toolbar_outer_height() + tokens::SPACE_2)),
        )
        .order(Order::Foreground)
        .show(context, |ui| {
            pixel_snap::show_pixel_aligned_frame(
                ui,
                tokens::material_frame(
                    readable_mode,
                    tokens::MaterialRole::Popover,
                    CornerRadius::same(tokens::CARD_RADIUS),
                    Margin::same(tokens::MARGIN_SPACE_2),
                ),
                |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("退出批注并清空本次放映墨迹？")
                                .size(tokens::TEXT_SM)
                                .color(tokens::COLOR_TEXT_PRIMARY),
                        );
                        ui.horizontal(|ui| {
                            if icon_button(ui, "取消", Icon::Cancel, false, None, readable_mode)
                                .clicked()
                            {
                                return Some(UiCommand::CancelDismissSlideshow);
                            }
                            if icon_button(ui, "确认", Icon::Confirm, false, None, readable_mode)
                                .clicked()
                            {
                                return Some(UiCommand::ConfirmDismissSlideshow);
                            }
                            None
                        })
                        .inner
                    })
                    .inner
                },
            )
            .inner
        });
    hit_regions.push(response.response.rect);
    response.inner
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
    const GROUP_GAP_COUNT: f32 = 3.0;
    BODY_BUTTON_COUNT * tokens::TOUCH_TARGET
        + (BODY_BUTTON_COUNT - 1.0 + GROUP_GAP_COUNT) * tokens::SPACE_2
}

/// 返回包含左右内边距的底部工具栏主体宽度。
const fn toolbar_body_width() -> f32 {
    toolbar_body_content_width() + tokens::SPACE_2 * 2.0
}
