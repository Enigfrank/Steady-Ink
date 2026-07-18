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
    palm_erase_blocked: bool,
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
            return self.cancel_all_input();
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
        self.clear_palm_session();
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
                if active {
                    self.cancel_all_input()
                } else {
                    None
                }
            }
            WindowsPointerEvent::PalmCandidate { .. } => {
                if self.palm_candidate_over_ui.is_none() {
                    self.palm_candidate_over_ui = Some(ui_hit);
                    self.palm_erase_blocked = false;
                }
                self.cancel_active_pointer()
            }
            WindowsPointerEvent::PalmSupport { .. } => self.cancel_all_input(),
            WindowsPointerEvent::CandidateRejected {
                points,
                session_ended,
            } => {
                let started_over_ui = self.palm_candidate_over_ui.unwrap_or(ui_hit);
                if session_ended {
                    self.palm_candidate_over_ui = None;
                    self.palm_erase_blocked = false;
                    self.palm_erasing = false;
                }
                if canvas_enabled && !started_over_ui && !points.is_empty() {
                    Some(PointerAction::CommitBuffered(points))
                } else {
                    None
                }
            }
            WindowsPointerEvent::CandidateCancelled { session_ended } => {
                if session_ended {
                    self.cancel_all_input()
                } else {
                    None
                }
            }
            WindowsPointerEvent::PalmErase { phase, sample } => {
                if !canvas_enabled {
                    return self.cancel_all_input();
                }
                let started_over_ui = self.palm_candidate_over_ui.unwrap_or(ui_hit);
                match phase {
                    PalmErasePhase::Begin | PalmErasePhase::Move if self.palm_erase_blocked => None,
                    PalmErasePhase::Begin | PalmErasePhase::Move if !self.palm_erasing => {
                        if started_over_ui {
                            self.palm_erase_blocked = true;
                            None
                        } else {
                            self.palm_erasing = true;
                            Some(PointerAction::BeginPalmErase(sample))
                        }
                    }
                    PalmErasePhase::Begin => Some(PointerAction::MovePalmErase(sample)),
                    PalmErasePhase::Move => Some(PointerAction::MovePalmErase(sample)),
                    PalmErasePhase::End => {
                        self.palm_candidate_over_ui = None;
                        self.palm_erase_blocked = false;
                        if self.palm_erasing {
                            self.palm_erasing = false;
                            Some(PointerAction::EndPalmErase(sample))
                        } else {
                            None
                        }
                    }
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

    /// 清理当前指针和所有手掌会话状态，并在需要时取消运行时手势。
    fn cancel_all_input(&mut self) -> Option<PointerAction> {
        let had_active_input = self.active_pointer.take().is_some() || self.palm_erasing;
        self.clear_palm_session();
        had_active_input.then_some(PointerAction::Cancel)
    }

    /// 丢弃候选起点、阻止擦除和已开始擦除的会话标记。
    fn clear_palm_session(&mut self) {
        self.palm_candidate_over_ui = None;
        self.palm_erase_blocked = false;
        self.palm_erasing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建用于路由状态测试的稳定手掌采样。
    fn palm_sample() -> EraseSample {
        EraseSample::circle(CanvasPoint::new(32.0, 48.0), 48.0)
    }

    /// 验证从 UI 起始的手掌在移动到画布后仍不会启动擦除。
    #[test]
    fn palm_started_over_ui_stays_blocked() {
        let sample = palm_sample();
        let mut router = InputRouter::default();

        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmCandidate {
                    point: sample.center,
                },
                true,
                true,
            ),
            None
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::Begin,
                    sample,
                },
                false,
                true,
            ),
            None
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::Move,
                    sample,
                },
                false,
                true,
            ),
            None
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::End,
                    sample,
                },
                false,
                true,
            ),
            None
        );
    }

    /// 验证从画布起始的手掌跨过 UI 时继续产生擦除动作。
    #[test]
    fn palm_started_over_canvas_ignores_later_ui_hit() {
        let sample = palm_sample();
        let mut router = InputRouter::default();

        router.route_windows_pointer(
            WindowsPointerEvent::PalmCandidate {
                point: sample.center,
            },
            false,
            true,
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::Begin,
                    sample,
                },
                true,
                true,
            ),
            Some(PointerAction::BeginPalmErase(sample))
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::End,
                    sample,
                },
                true,
                true,
            ),
            Some(PointerAction::EndPalmErase(sample))
        );
    }

    /// 验证异常取消不会把 UI 候选恢复为普通笔画，且会清理旧起点状态。
    #[test]
    fn cancelled_candidate_drops_ui_state() {
        let sample = palm_sample();
        let mut router = InputRouter::default();

        router.route_windows_pointer(
            WindowsPointerEvent::PalmCandidate {
                point: sample.center,
            },
            true,
            true,
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::CandidateCancelled {
                    session_ended: true,
                },
                false,
                true,
            ),
            None
        );
        router.route_windows_pointer(
            WindowsPointerEvent::PalmCandidate {
                point: sample.center,
            },
            false,
            true,
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::Begin,
                    sample,
                },
                true,
                true,
            ),
            Some(PointerAction::BeginPalmErase(sample))
        );
    }

    /// 验证候选簇尚未结束时保留最初的 UI 起点判断。
    #[test]
    fn unfinished_candidate_session_keeps_original_ui_origin() {
        let sample = palm_sample();
        let mut router = InputRouter::default();

        router.route_windows_pointer(
            WindowsPointerEvent::PalmCandidate {
                point: sample.center,
            },
            true,
            true,
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::CandidateRejected {
                    points: vec![sample.center],
                    session_ended: false,
                },
                false,
                true,
            ),
            None
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::Begin,
                    sample,
                },
                false,
                true,
            ),
            None
        );
    }

    /// 验证画布候选正常结束时提交缓冲笔画并清理会话起点。
    #[test]
    fn rejected_canvas_candidate_commits_and_ends_session() {
        let sample = palm_sample();
        let mut router = InputRouter::default();

        router.route_windows_pointer(
            WindowsPointerEvent::PalmCandidate {
                point: sample.center,
            },
            false,
            true,
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::CandidateRejected {
                    points: vec![sample.center],
                    session_ended: true,
                },
                true,
                true,
            ),
            Some(PointerAction::CommitBuffered(vec![sample.center]))
        );
        router.route_windows_pointer(
            WindowsPointerEvent::PalmCandidate {
                point: sample.center,
            },
            true,
            true,
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::Begin,
                    sample,
                },
                false,
                true,
            ),
            None
        );
    }

    /// 验证触控笔进入范围会清理候选 UI 起点，防止状态泄漏到下一会话。
    #[test]
    fn pen_activity_clears_palm_session_state() {
        let sample = palm_sample();
        let mut router = InputRouter::default();

        router.route_windows_pointer(
            WindowsPointerEvent::PalmCandidate {
                point: sample.center,
            },
            true,
            true,
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PenActivityChanged(true),
                false,
                true
            ),
            None
        );
        router.route_windows_pointer(
            WindowsPointerEvent::PalmCandidate {
                point: sample.center,
            },
            false,
            true,
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::PalmErase {
                    phase: PalmErasePhase::Begin,
                    sample,
                },
                true,
                true,
            ),
            Some(PointerAction::BeginPalmErase(sample))
        );
    }
}
