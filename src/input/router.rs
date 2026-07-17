use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};

use crate::ink::{CanvasPoint, EraseSample};

use super::{PalmErasePhase, WindowsPointerEvent};

/// 画布输入路由向墨迹会话发送的统一指针动作。
#[derive(Debug, Clone, PartialEq)]
pub enum PointerAction {
    Begin(CanvasPoint),
    Move(CanvasPoint),
    End(CanvasPoint),
    BeginPalmErase(EraseSample),
    MovePalmErase(EraseSample),
    EndPalmErase(EraseSample),
    CommitBuffered(Vec<CanvasPoint>),
    Cancel,
}

/// 当前由通用 winit 事件驱动的基础鼠标/触摸路由器。
#[derive(Debug, Default)]
pub struct InputRouter {
    last_cursor_position: Option<CanvasPoint>,
    active_pointer: Option<ActivePointer>,
    pen_active: bool,
    palm_candidate_over_ui: Option<bool>,
    palm_erasing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivePointer {
    Mouse,
    Touch(u64),
}

impl InputRouter {
    /// 处理一个窗口事件，并在画布输入有效时返回统一指针动作。
    pub fn route(
        &mut self,
        event: &WindowEvent,
        ui_consumed: bool,
        canvas_enabled: bool,
    ) -> Option<PointerAction> {
        if !canvas_enabled {
            return self.cancel_active_pointer();
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = CanvasPoint::new(position.x as f32, position.y as f32);
                self.last_cursor_position = Some(point);
                (self.active_pointer == Some(ActivePointer::Mouse))
                    .then_some(PointerAction::Move(point))
            }
            WindowEvent::MouseInput { state, button, .. }
                if *button == MouseButton::Left && !self.has_active_touch() =>
            {
                self.route_mouse_button(*state, ui_consumed)
            }
            WindowEvent::Touch(touch) => {
                let point = CanvasPoint::new(touch.location.x as f32, touch.location.y as f32);
                self.route_touch(touch.id, touch.phase, point, ui_consumed)
            }
            WindowEvent::Focused(false) => self.cancel_active_pointer(),
            _ => None,
        }
    }

    /// 取消当前手势，供模式切换和窗口失焦时调用。
    pub fn cancel(&mut self) {
        self.active_pointer = None;
        self.palm_candidate_over_ui = None;
        self.palm_erasing = false;
    }

    /// 应用原生 Pointer Input 的笔活动和手掌分类结果。
    pub fn route_windows_pointer(
        &mut self,
        event: WindowsPointerEvent,
        ui_hit: bool,
        canvas_enabled: bool,
    ) -> Option<PointerAction> {
        match event {
            WindowsPointerEvent::PenActivityChanged(active) => {
                self.pen_active = active;
                if active
                    && (matches!(self.active_pointer, Some(ActivePointer::Touch(_)))
                        || self.palm_erasing)
                {
                    self.cancel_active_pointer()
                } else {
                    None
                }
            }
            WindowsPointerEvent::PalmCandidate { .. } => {
                self.palm_candidate_over_ui.get_or_insert(ui_hit);
                self.cancel_active_pointer()
            }
            WindowsPointerEvent::PalmSupport { .. } => {
                self.palm_candidate_over_ui = None;
                self.palm_erasing = false;
                self.cancel_active_pointer()
            }
            WindowsPointerEvent::CandidateRejected { points } => {
                let started_over_ui = self.palm_candidate_over_ui.take().unwrap_or(ui_hit);
                if canvas_enabled && !started_over_ui && !points.is_empty() {
                    Some(PointerAction::CommitBuffered(points))
                } else {
                    None
                }
            }
            WindowsPointerEvent::PalmErase { phase, sample } => {
                self.palm_candidate_over_ui = None;
                if !canvas_enabled {
                    self.palm_erasing = false;
                    return None;
                }
                match phase {
                    PalmErasePhase::Begin | PalmErasePhase::Move if !self.palm_erasing => {
                        if ui_hit {
                            None
                        } else {
                            self.palm_erasing = true;
                            Some(PointerAction::BeginPalmErase(sample))
                        }
                    }
                    PalmErasePhase::Begin => Some(PointerAction::MovePalmErase(sample)),
                    PalmErasePhase::Move => Some(PointerAction::MovePalmErase(sample)),
                    PalmErasePhase::End if self.palm_erasing => {
                        self.palm_erasing = false;
                        Some(PointerAction::EndPalmErase(sample))
                    }
                    PalmErasePhase::End => None,
                }
            }
        }
    }

    /// 将鼠标左键状态转换为画布手势动作。
    fn route_mouse_button(
        &mut self,
        state: ElementState,
        ui_consumed: bool,
    ) -> Option<PointerAction> {
        let point = self.last_cursor_position?;
        match state {
            ElementState::Pressed if !ui_consumed && self.active_pointer.is_none() => {
                self.active_pointer = Some(ActivePointer::Mouse);
                Some(PointerAction::Begin(point))
            }
            ElementState::Released if self.active_pointer == Some(ActivePointer::Mouse) => {
                self.active_pointer = None;
                Some(PointerAction::End(point))
            }
            _ => None,
        }
    }

    /// 将单个 winit 触摸接触转换为画布手势动作。
    fn route_touch(
        &mut self,
        touch_id: u64,
        phase: TouchPhase,
        point: CanvasPoint,
        ui_consumed: bool,
    ) -> Option<PointerAction> {
        let pointer = ActivePointer::Touch(touch_id);
        match phase {
            TouchPhase::Started if !ui_consumed && self.active_pointer.is_none() => {
                self.active_pointer = Some(pointer);
                Some(PointerAction::Begin(point))
            }
            TouchPhase::Moved if self.active_pointer == Some(pointer) => {
                Some(PointerAction::Move(point))
            }
            TouchPhase::Ended if self.active_pointer == Some(pointer) => {
                self.active_pointer = None;
                Some(PointerAction::End(point))
            }
            TouchPhase::Cancelled if self.active_pointer == Some(pointer) => {
                self.active_pointer = None;
                Some(PointerAction::Cancel)
            }
            _ => None,
        }
    }

    /// 返回当前是否已有触摸接触占用画布输入。
    fn has_active_touch(&self) -> bool {
        matches!(self.active_pointer, Some(ActivePointer::Touch(_)))
    }

    /// 在存在活动指针时生成一次取消动作。
    fn cancel_active_pointer(&mut self) -> Option<PointerAction> {
        let had_active_input = self.active_pointer.take().is_some() || self.palm_erasing;
        self.palm_erasing = false;
        had_active_input.then_some(PointerAction::Cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建输入路由测试使用的动态擦除采样。
    fn erase_sample(x: f32, y: f32) -> EraseSample {
        EraseSample {
            center: CanvasPoint::new(x, y),
            radius_x: 40.0,
            radius_y: 24.0,
            rotation_radians: 0.25,
        }
    }

    /// 验证画布候选被否决后会按原轨迹提交，而不会丢失普通触摸。
    #[test]
    fn rejected_canvas_candidate_commits_buffered_points() {
        let mut router = InputRouter::default();
        let points = vec![CanvasPoint::new(10.0, 20.0), CanvasPoint::new(30.0, 40.0)];

        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmCandidate { point: points[0] },
                false,
                true,
            ),
            None
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::CandidateRejected {
                    points: points.clone(),
                },
                false,
                true,
            ),
            Some(PointerAction::CommitBuffered(points))
        );
    }

    /// 验证从 UI 开始的候选被否决后不会回放为墨迹或 UI 点击。
    #[test]
    fn rejected_ui_candidate_is_not_replayed() {
        let mut router = InputRouter::default();
        let point = CanvasPoint::new(10.0, 20.0);

        router.route_windows_pointer(WindowsPointerEvent::PalmCandidate { point }, true, true);
        let action = router.route_windows_pointer(
            WindowsPointerEvent::CandidateRejected {
                points: vec![point],
            },
            false,
            true,
        );

        assert_eq!(action, None);
    }

    /// 验证触控笔活跃时的掌托会取消已有触摸且不会启动手掌擦除。
    #[test]
    fn palm_support_cancels_touch_without_erasing() {
        let mut router = InputRouter {
            active_pointer: Some(ActivePointer::Touch(7)),
            ..InputRouter::default()
        };

        let action = router.route_windows_pointer(
            WindowsPointerEvent::PalmSupport {
                point: Some(CanvasPoint::new(20.0, 30.0)),
            },
            false,
            true,
        );

        assert_eq!(action, Some(PointerAction::Cancel));
        assert_eq!(router.active_pointer, None);
        assert!(!router.palm_erasing);
    }

    /// 验证手掌擦除阶段被转换为一个连续的 Begin、Move、End 动作序列。
    #[test]
    fn palm_erase_routes_complete_phase_sequence() {
        let mut router = InputRouter::default();
        let begin = erase_sample(10.0, 20.0);
        let moved = erase_sample(30.0, 40.0);
        let end = erase_sample(50.0, 60.0);

        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::Begin,
                    sample: begin,
                },
                false,
                true,
            ),
            Some(PointerAction::BeginPalmErase(begin))
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::Move,
                    sample: moved,
                },
                false,
                true,
            ),
            Some(PointerAction::MovePalmErase(moved))
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::End,
                    sample: end,
                },
                false,
                true,
            ),
            Some(PointerAction::EndPalmErase(end))
        );
        assert!(!router.palm_erasing);
    }

    /// 验证手掌从 UI 上开始时不会创建擦除会话。
    #[test]
    fn palm_erase_begin_over_ui_is_suppressed() {
        let mut router = InputRouter::default();

        let action = router.route_windows_pointer(
            WindowsPointerEvent::PalmErase {
                phase: PalmErasePhase::Begin,
                sample: erase_sample(10.0, 20.0),
            },
            true,
            true,
        );

        assert_eq!(action, None);
        assert!(!router.palm_erasing);
    }
}
