use std::time::Instant;

use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};

use crate::ink::{CanvasPoint, EraseSample};

use super::{PalmErasePhase, PenPhase, WindowsPointerEvent};

/// 画布输入路由向墨迹会话发送的统一指针动作。
#[derive(Debug, Clone, PartialEq)]
pub enum PointerAction {
    Begin(PointerSample),
    Move(PointerSample),
    End(PointerSample),
    BeginBatch(Vec<PointerSample>),
    MoveBatch(Vec<PointerSample>),
    EndBatch(Vec<PointerSample>),
    BeginPalmErase(EraseSample),
    MovePalmErase(EraseSample),
    EndPalmErase(EraseSample),
    CommitBuffered(Vec<PointerSample>),
    Cancel,
}

/// 一个供输入路由和笔画构建共用的带单调时间采样。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerSample {
    pub point: CanvasPoint,
    pub timestamp_micros: u64,
}

impl PointerSample {
    /// 创建一个包含相对单调微秒时间的指针采样。
    pub const fn new(point: CanvasPoint, timestamp_micros: u64) -> Self {
        Self {
            point,
            timestamp_micros,
        }
    }
}

/// 当前由通用 winit 事件驱动的基础鼠标/触摸路由器。
#[derive(Debug)]
pub struct InputRouter {
    last_cursor_position: Option<CanvasPoint>,
    clock_start: Instant,
    active_pointer: Option<ActivePointer>,
    pen_active: bool,
    palm_candidate_over_ui: Option<bool>,
    palm_erase_blocked: bool,
    palm_erasing: bool,
    native_pen_blocked: bool,
    native_pen_drawing: bool,
}

impl Default for InputRouter {
    /// 以创建路由器的单调时刻作为鼠标和触摸的时间基准。
    fn default() -> Self {
        Self {
            last_cursor_position: None,
            clock_start: Instant::now(),
            active_pointer: None,
            pen_active: false,
            palm_candidate_over_ui: None,
            palm_erase_blocked: false,
            palm_erasing: false,
            native_pen_blocked: false,
            native_pen_drawing: false,
        }
    }
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
        promoted_pen_contact: bool,
    ) -> Option<PointerAction> {
        if !canvas_enabled {
            return self.cancel_all_input();
        }
        if promoted_pen_contact
            && matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput {
                        button: MouseButton::Left,
                        ..
                    }
            )
        {
            return None;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = CanvasPoint::new(position.x as f32, position.y as f32);
                self.last_cursor_position = Some(point);
                (self.active_pointer == Some(ActivePointer::Mouse))
                    .then_some(PointerAction::Move(self.sample(point)))
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
        self.clear_pen_session();
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
            WindowsPointerEvent::Pen { phase, points } => {
                self.pen_active = true;
                self.clear_palm_session();
                self.route_pen(phase, points, ui_hit, canvas_enabled)
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

    /// 将原生触控笔批量采样转换为单次画布手势动作。
    fn route_pen(
        &mut self,
        phase: PenPhase,
        points: Vec<PointerSample>,
        ui_hit: bool,
        canvas_enabled: bool,
    ) -> Option<PointerAction> {
        if !canvas_enabled {
            return self.cancel_all_input();
        }
        match phase {
            PenPhase::Begin => {
                let cancelled_other = self.active_pointer.take().is_some() || self.palm_erasing;
                self.palm_erasing = false;
                self.native_pen_blocked = ui_hit || points.is_empty();
                self.native_pen_drawing = !self.native_pen_blocked;
                if self.native_pen_drawing {
                    Some(PointerAction::BeginBatch(points))
                } else {
                    cancelled_other.then_some(PointerAction::Cancel)
                }
            }
            PenPhase::Move if self.native_pen_drawing && !points.is_empty() => {
                Some(PointerAction::MoveBatch(points))
            }
            PenPhase::Move => None,
            PenPhase::End => {
                let was_drawing = self.native_pen_drawing;
                self.clear_pen_session();
                was_drawing.then_some(PointerAction::EndBatch(points))
            }
            PenPhase::Cancel => {
                let was_drawing = self.native_pen_drawing;
                self.clear_pen_session();
                was_drawing.then_some(PointerAction::Cancel)
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
                Some(PointerAction::Begin(self.sample(point)))
            }
            ElementState::Released if self.active_pointer == Some(ActivePointer::Mouse) => {
                self.active_pointer = None;
                Some(PointerAction::End(self.sample(point)))
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
                Some(PointerAction::Begin(self.sample(point)))
            }
            TouchPhase::Moved if self.active_pointer == Some(pointer) => {
                Some(PointerAction::Move(self.sample(point)))
            }
            TouchPhase::Ended if self.active_pointer == Some(pointer) => {
                self.active_pointer = None;
                Some(PointerAction::End(self.sample(point)))
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

    /// 将一个普通 winit 点转换为路由器时间基准下的采样。
    fn sample(&self, point: CanvasPoint) -> PointerSample {
        PointerSample::new(point, self.clock_start.elapsed().as_micros() as u64)
    }

    /// 在存在活动指针时生成一次取消动作。
    fn cancel_active_pointer(&mut self) -> Option<PointerAction> {
        let had_active_input =
            self.active_pointer.take().is_some() || self.palm_erasing || self.native_pen_drawing;
        self.palm_erasing = false;
        self.clear_pen_session();
        had_active_input.then_some(PointerAction::Cancel)
    }

    /// 清理当前指针和所有手掌会话状态，并在需要时取消运行时手势。
    fn cancel_all_input(&mut self) -> Option<PointerAction> {
        let had_active_input =
            self.active_pointer.take().is_some() || self.palm_erasing || self.native_pen_drawing;
        self.clear_palm_session();
        self.clear_pen_session();
        had_active_input.then_some(PointerAction::Cancel)
    }

    /// 丢弃候选起点、阻止擦除和已开始擦除的会话标记。
    fn clear_palm_session(&mut self) {
        self.palm_candidate_over_ui = None;
        self.palm_erase_blocked = false;
        self.palm_erasing = false;
    }

    /// 清理原生触控笔接触的阻止和绘制标记。
    fn clear_pen_session(&mut self) {
        self.native_pen_blocked = false;
        self.native_pen_drawing = false;
    }
}

#[cfg(test)]
mod tests {
    use winit::{dpi::PhysicalPosition, event::DeviceId};

    use super::*;

    /// 创建用于路由状态测试的稳定手掌采样。
    fn palm_sample() -> EraseSample {
        EraseSample::circle(CanvasPoint::new(32.0, 48.0), 48.0)
    }

    /// 创建用于原生触控笔路由测试的批量位置。
    fn pen_points() -> Vec<PointerSample> {
        vec![
            PointerSample::new(CanvasPoint::new(12.0, 16.0), 100),
            PointerSample::new(CanvasPoint::new(20.0, 24.0), 200),
        ]
    }

    /// 创建用于候选触摸回放断言的固定时间采样。
    fn buffered_sample(point: CanvasPoint) -> PointerSample {
        PointerSample::new(point, 100)
    }

    /// 验证原生笔接触期间提升的左键和移动事件不会生成重复画布动作。
    #[test]
    fn promoted_pen_mouse_events_are_suppressed() {
        let device_id = DeviceId::dummy();
        let mut router = InputRouter::default();
        let pressed = WindowEvent::MouseInput {
            device_id,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let moved = WindowEvent::CursorMoved {
            device_id,
            position: PhysicalPosition::new(24.0, 32.0),
        };

        assert_eq!(router.route(&pressed, false, true, true), None);
        assert_eq!(router.route(&moved, false, true, true), None);
        assert_eq!(router.active_pointer, None);
        assert_eq!(router.last_cursor_position, None);
    }

    /// 验证画布起始的原生笔批次持续绘制，并允许抬起批次为空。
    #[test]
    fn native_pen_routes_batches_until_end() {
        let mut router = InputRouter::default();

        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::Pen {
                    phase: PenPhase::Begin,
                    points: pen_points(),
                },
                false,
                true,
            ),
            Some(PointerAction::BeginBatch(pen_points()))
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::Pen {
                    phase: PenPhase::Move,
                    points: pen_points(),
                },
                true,
                true,
            ),
            Some(PointerAction::MoveBatch(pen_points()))
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::Pen {
                    phase: PenPhase::End,
                    points: Vec::new(),
                },
                true,
                true,
            ),
            Some(PointerAction::EndBatch(Vec::new()))
        );
    }

    /// 验证画布在原生笔接触期间禁用时取消活动笔迹而不是继续追加。
    #[test]
    fn disabled_canvas_cancels_native_pen_session() {
        let mut router = InputRouter::default();
        router.route_windows_pointer(
            WindowsPointerEvent::Pen {
                phase: PenPhase::Begin,
                points: pen_points(),
            },
            false,
            true,
        );

        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::Pen {
                    phase: PenPhase::Move,
                    points: pen_points(),
                },
                false,
                false,
            ),
            Some(PointerAction::Cancel)
        );
        assert!(!router.native_pen_drawing);
    }

    /// 验证从 UI 起始的原生笔接触整段不会生成画布动作。
    #[test]
    fn native_pen_started_over_ui_stays_blocked() {
        let mut router = InputRouter::default();

        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::Pen {
                    phase: PenPhase::Begin,
                    points: pen_points(),
                },
                true,
                true,
            ),
            None
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::Pen {
                    phase: PenPhase::Move,
                    points: pen_points(),
                },
                false,
                true,
            ),
            None
        );
        assert_eq!(
            router.route_windows_pointer(
                WindowsPointerEvent::Pen {
                    phase: PenPhase::End,
                    points: pen_points(),
                },
                false,
                true,
            ),
            None
        );
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
                    points: vec![buffered_sample(sample.center)],
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
                    points: vec![buffered_sample(sample.center)],
                    session_ended: true,
                },
                true,
                true,
            ),
            Some(PointerAction::CommitBuffered(vec![buffered_sample(
                sample.center,
            )]))
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
