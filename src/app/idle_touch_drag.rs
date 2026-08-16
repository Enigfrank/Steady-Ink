use winit::dpi::PhysicalPosition;

/// egui 确认非批注工具栏拖动后应选择的窗口移动路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IdleDragStart {
    ManualTouch {
        touch_id: u64,
        target_outer: PhysicalPosition<i32>,
    },
    NativeMouse,
    Ignored,
}

/// 一次触摸结束对非批注窗口拖动生命周期的影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IdleTouchEnd {
    Ignored,
    PendingCleared,
    FinishDrag { last_outer: PhysicalPosition<i32> },
}

/// egui 判定拖动前保存的第一根触摸及其物理屏幕起点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingTouch {
    id: u64,
    start_screen: PhysicalPosition<i32>,
    latest_screen: PhysicalPosition<i32>,
    start_outer: PhysicalPosition<i32>,
}

/// egui 已确认拖动后由运行时持续驱动的主触摸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManualTouch {
    id: u64,
    start_screen: PhysicalPosition<i32>,
    start_outer: PhysicalPosition<i32>,
    last_outer: PhysicalPosition<i32>,
}

/// 非批注工具栏窗口拖动的互斥阶段。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum IdleWindowDragPhase {
    #[default]
    Idle,
    PendingTouch(PendingTouch),
    NativeMouse,
    ManualTouch(ManualTouch),
}

/// 跟踪第一根触摸，并在 egui 确认拖动后输出物理窗口位置。
#[derive(Debug, Default)]
pub(super) struct IdleTouchDragState {
    phase: IdleWindowDragPhase,
}

impl IdleTouchDragState {
    /// 返回当前是否可以选择一根新的主触摸。
    pub(super) fn is_idle(&self) -> bool {
        matches!(self.phase, IdleWindowDragPhase::Idle)
    }

    /// 返回指定触摸是否是当前 pending 或 manual 主触摸。
    pub(super) fn tracks_touch(&self, touch_id: u64) -> bool {
        matches!(
            self.phase,
            IdleWindowDragPhase::PendingTouch(PendingTouch { id, .. })
                | IdleWindowDragPhase::ManualTouch(ManualTouch { id, .. })
                if id == touch_id
        )
    }

    /// 选择第一根触摸，并保存触摸开始时尚未移动的窗口位置。
    pub(super) fn start_touch(
        &mut self,
        touch_id: u64,
        screen_position: PhysicalPosition<i32>,
        window_outer: PhysicalPosition<i32>,
    ) -> bool {
        if !self.is_idle() {
            return false;
        }
        self.phase = IdleWindowDragPhase::PendingTouch(PendingTouch {
            id: touch_id,
            start_screen: screen_position,
            latest_screen: screen_position,
            start_outer: window_outer,
        });
        true
    }

    /// 更新主触摸位置；仅在 manual 阶段返回新的窗口 outer 位置。
    pub(super) fn move_touch(
        &mut self,
        touch_id: u64,
        screen_position: PhysicalPosition<i32>,
    ) -> Option<PhysicalPosition<i32>> {
        match &mut self.phase {
            IdleWindowDragPhase::PendingTouch(pending) if pending.id == touch_id => {
                pending.latest_screen = screen_position;
                None
            }
            IdleWindowDragPhase::ManualTouch(manual) if manual.id == touch_id => {
                let target = translated_outer_position(
                    manual.start_outer,
                    manual.start_screen,
                    screen_position,
                );
                manual.last_outer = target;
                Some(target)
            }
            _ => None,
        }
    }

    /// 响应 egui 的既有拖动命令，并选择 manual touch 或原生 mouse 路径。
    pub(super) fn promote_or_start_mouse(&mut self) -> IdleDragStart {
        let previous = std::mem::take(&mut self.phase);
        match previous {
            IdleWindowDragPhase::Idle => {
                self.phase = IdleWindowDragPhase::NativeMouse;
                IdleDragStart::NativeMouse
            }
            IdleWindowDragPhase::PendingTouch(pending) => {
                let target = translated_outer_position(
                    pending.start_outer,
                    pending.start_screen,
                    pending.latest_screen,
                );
                self.phase = IdleWindowDragPhase::ManualTouch(ManualTouch {
                    id: pending.id,
                    start_screen: pending.start_screen,
                    start_outer: pending.start_outer,
                    last_outer: target,
                });
                IdleDragStart::ManualTouch {
                    touch_id: pending.id,
                    target_outer: target,
                }
            }
            active => {
                self.phase = active;
                IdleDragStart::Ignored
            }
        }
    }

    /// 结束匹配触摸；只有已提升的 manual drag 请求执行一次吸附。
    pub(super) fn end_touch(&mut self, touch_id: u64) -> IdleTouchEnd {
        let previous = std::mem::take(&mut self.phase);
        match previous {
            IdleWindowDragPhase::PendingTouch(pending) if pending.id == touch_id => {
                IdleTouchEnd::PendingCleared
            }
            IdleWindowDragPhase::ManualTouch(manual) if manual.id == touch_id => {
                IdleTouchEnd::FinishDrag {
                    last_outer: manual.last_outer,
                }
            }
            active => {
                self.phase = active;
                IdleTouchEnd::Ignored
            }
        }
    }

    /// 结束原生鼠标拖动，并报告是否需要执行现有吸附。
    pub(super) fn end_mouse(&mut self) -> bool {
        if matches!(self.phase, IdleWindowDragPhase::NativeMouse) {
            self.phase = IdleWindowDragPhase::Idle;
            true
        } else {
            false
        }
    }

    /// 无吸附地清理 pending 或 active 状态，用于失焦、模式和几何切换。
    pub(super) fn clear(&mut self) -> bool {
        !matches!(std::mem::take(&mut self.phase), IdleWindowDragPhase::Idle)
    }
}

/// 以物理屏幕位移平移触摸开始时的物理窗口 outer 位置。
fn translated_outer_position(
    start_outer: PhysicalPosition<i32>,
    start_screen: PhysicalPosition<i32>,
    current_screen: PhysicalPosition<i32>,
) -> PhysicalPosition<i32> {
    PhysicalPosition::new(
        translated_axis(start_outer.x, start_screen.x, current_screen.x),
        translated_axis(start_outer.y, start_screen.y, current_screen.y),
    )
}

/// 在更宽整数中计算一个物理坐标轴，并夹紧到 Win32 坐标范围。
fn translated_axis(start_outer: i32, start_screen: i32, current_screen: i32) -> i32 {
    let translated = i64::from(start_outer) + i64::from(current_screen) - i64::from(start_screen);
    translated.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证第一根触摸进入 pending，第二根触摸不能替换主 ID。
    #[test]
    fn start_selects_one_primary_touch() {
        let mut state = IdleTouchDragState::default();
        assert!(state.start_touch(7, point(400, 300), point(1_800, 200)));
        assert!(!state.start_touch(8, point(500, 400), point(1_800, 200)));
        assert!(state.tracks_touch(7));
        assert!(!state.tracks_touch(8));
    }

    /// 验证提升立即包含 egui 阈值前已经累计的物理位移。
    #[test]
    fn promotion_applies_accumulated_touch_delta() {
        let mut state = pending_state();
        assert_eq!(state.move_touch(7, point(436, 324)), None);
        assert_eq!(
            state.promote_or_start_mouse(),
            IdleDragStart::ManualTouch {
                touch_id: 7,
                target_outer: point(1_836, 224),
            }
        );
    }

    /// 验证 manual 阶段只按主触摸的物理屏幕位移更新窗口。
    #[test]
    fn movement_ignores_secondary_touch() {
        let mut state = pending_state();
        let _ = state.promote_or_start_mouse();
        assert_eq!(state.move_touch(8, point(900, 900)), None);
        assert_eq!(
            state.move_touch(7, point(445, 280)),
            Some(point(1_845, 180))
        );
        assert_eq!(
            state.move_touch(7, point(470, 350)),
            Some(point(1_870, 250))
        );
    }

    /// 验证未提升的轻触只清理 pending，不请求窗口吸附。
    #[test]
    fn tap_below_drag_threshold_does_not_finish_drag() {
        let mut state = pending_state();
        assert_eq!(state.end_touch(7), IdleTouchEnd::PendingCleared);
        assert!(state.is_idle());
    }

    /// 验证 manual End 只由主触摸触发一次吸附并携带最后有效位置。
    #[test]
    fn matching_end_finishes_once() {
        let mut state = pending_state();
        let _ = state.promote_or_start_mouse();
        assert_eq!(state.end_touch(8), IdleTouchEnd::Ignored);
        assert_eq!(
            state.move_touch(7, point(430, 320)),
            Some(point(1_830, 220))
        );
        assert_eq!(
            state.end_touch(7),
            IdleTouchEnd::FinishDrag {
                last_outer: point(1_830, 220)
            }
        );
        assert_eq!(state.end_touch(7), IdleTouchEnd::Ignored);
    }

    /// 验证 Cancel 与 End 共享同一匹配触摸结束语义。
    #[test]
    fn matching_cancel_finishes_manual_drag() {
        let mut state = pending_state();
        let _ = state.promote_or_start_mouse();
        assert_eq!(
            state.end_touch(7),
            IdleTouchEnd::FinishDrag {
                last_outer: point(1_800, 200)
            }
        );
        assert!(state.is_idle());
    }

    /// 验证失焦可无吸附清理尚未超过拖动阈值的触摸。
    #[test]
    fn focus_loss_cleanup_clears_pending_touch() {
        let mut pending = pending_state();
        assert!(pending.clear());
        assert!(!pending.clear());
    }

    /// 验证模式或窗口几何切换可无吸附清理已提升的触摸拖动。
    #[test]
    fn mode_or_geometry_cleanup_clears_active_touch() {
        let mut active = pending_state();
        let _ = active.promote_or_start_mouse();
        assert!(active.clear());
        assert!(active.is_idle());
    }

    /// 验证没有活动触摸时仍选择 winit 原生鼠标拖动路径。
    #[test]
    fn command_without_touch_uses_native_mouse_fallback() {
        let mut state = IdleTouchDragState::default();
        assert_eq!(state.promote_or_start_mouse(), IdleDragStart::NativeMouse);
        assert!(state.end_mouse());
        assert!(!state.end_mouse());
    }

    /// 验证 100%、200% 和 fractional DPI 下都只使用输入的物理像素差值。
    #[test]
    fn physical_delta_is_not_scaled_again() {
        for scale in [1.0_f32, 1.5, 2.0] {
            let start = point((200.0 * scale) as i32, (100.0 * scale) as i32);
            let delta = point((24.0 * scale) as i32, (-12.0 * scale) as i32);
            let current = point(start.x + delta.x, start.y + delta.y);
            assert_eq!(
                translated_outer_position(point(1_000, 300), start, current),
                point(1_000 + delta.x, 300 + delta.y)
            );
        }
    }

    /// 验证极端物理坐标不会溢出，而是夹紧到 Win32 可表示范围。
    #[test]
    fn physical_delta_clamps_to_i32_range() {
        assert_eq!(
            translated_outer_position(
                point(i32::MAX, i32::MIN),
                point(i32::MIN, i32::MAX),
                point(i32::MAX, i32::MIN),
            ),
            point(i32::MAX, i32::MIN)
        );
    }

    /// 创建各测试共享的 pending 主触摸状态。
    fn pending_state() -> IdleTouchDragState {
        let mut state = IdleTouchDragState::default();
        assert!(state.start_touch(7, point(400, 300), point(1_800, 200)));
        state
    }

    /// 简化物理位置测试数据的构造。
    const fn point(x: i32, y: i32) -> PhysicalPosition<i32> {
        PhysicalPosition::new(x, y)
    }
}
