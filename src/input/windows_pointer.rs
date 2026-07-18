use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::ScreenToClient,
    UI::{
        Input::Pointer::{
            GetPointerInfo, GetPointerTouchInfo, GetPointerType, POINTER_FLAG_CONFIDENCE,
            POINTER_FLAG_INRANGE, POINTER_INFO, POINTER_TOUCH_INFO,
        },
        WindowsAndMessaging::{
            MSG, POINTER_INPUT_TYPE, PT_PEN, PT_TOUCH, TOUCH_MASK_CONTACTAREA,
            WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERENTER, WM_POINTERLEAVE,
            WM_POINTERUP, WM_POINTERUPDATE,
        },
    },
};

use crate::ink::{CanvasPoint, EraseSample};

const PALM_CONFIRMATION_DELAY: Duration = Duration::from_millis(40);
const SINGLE_PALM_MIN_AREA: f32 = 4_096.0;
const SINGLE_PALM_MIN_MAJOR_AXIS: f32 = 72.0;
const CLUSTER_PALM_MIN_AREA: f32 = 6_400.0;
const CLUSTER_PALM_MIN_MAJOR_AXIS: f32 = 96.0;
const CLUSTER_PALM_MAX_MAJOR_AXIS: f32 = 320.0;
const MIN_CONTACT_RADIUS: f32 = 8.0;

/// 原生 Pointer Input hook 需要交给运行时的高层语义。
#[derive(Debug)]
pub enum WindowsPointerEvent {
    PenActivityChanged(bool),
    PalmCandidate {
        point: CanvasPoint,
    },
    PalmSupport {
        point: Option<CanvasPoint>,
    },
    PalmErase {
        phase: PalmErasePhase,
        sample: EraseSample,
    },
    CandidateRejected {
        points: Vec<CanvasPoint>,
        session_ended: bool,
    },
    CandidateCancelled {
        session_ended: bool,
    },
}

impl WindowsPointerEvent {
    /// 返回该原生事件可用于 UI 命中判断的物理像素位置。
    pub fn position(&self) -> Option<CanvasPoint> {
        match self {
            Self::PenActivityChanged(_) => None,
            Self::PalmCandidate { point } => Some(*point),
            Self::PalmSupport { point } => *point,
            Self::PalmErase { sample, .. } => Some(sample.center),
            Self::CandidateRejected { points, .. } => points.last().copied(),
            Self::CandidateCancelled { .. } => None,
        }
    }

    /// 返回该分类是否需要取消 egui 已经接收的临时触摸状态。
    pub const fn cancels_ui_pointer(&self) -> bool {
        matches!(
            self,
            Self::PalmCandidate { .. }
                | Self::PalmSupport { .. }
                | Self::PalmErase {
                    phase: PalmErasePhase::Begin,
                    ..
                }
        )
    }
}

/// 一次动态手掌擦除会话中的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PalmErasePhase {
    Begin,
    Move,
    End,
}

/// hook 对一条 Win32 消息的处理结果。
pub struct WindowsPointerDispatch {
    pub event: Option<WindowsPointerEvent>,
    pub swallow_winit: bool,
}

/// 保存在消息 hook 内的轻量 Pointer Input 分类器。
#[derive(Default)]
pub struct WindowsPointerTracker {
    pen_ids: HashSet<u32>,
    touches: HashMap<u32, TouchContact>,
    candidate_ids: HashSet<u32>,
    palm_ids: HashSet<u32>,
    palm_started: bool,
}

impl WindowsPointerTracker {
    /// 读取一条 WM_POINTER 消息并返回是否需要拦截 winit 及其手掌语义。
    pub fn capture_message(
        &mut self,
        raw_message: *const c_void,
    ) -> Option<WindowsPointerDispatch> {
        if raw_message.is_null() {
            return None;
        }
        // SAFETY: winit 的 with_msg_hook 保证指针在回调期间指向有效 MSG。
        let message = unsafe { &*raw_message.cast::<MSG>() };
        if !is_pointer_message(message.message) {
            return None;
        }

        let pointer_id = (message.wParam.0 & 0xffff) as u32;
        let pointer_type = pointer_type(pointer_id)
            .or_else(|| self.known_pointer_type(pointer_id))
            .unwrap_or_default();
        if pointer_type == PT_PEN {
            return Some(self.handle_pen_message(pointer_id, message));
        }
        if pointer_type != PT_TOUCH {
            return Some(WindowsPointerDispatch {
                event: None,
                swallow_winit: false,
            });
        }
        Some(self.handle_touch_message(pointer_id, message))
    }

    /// 根据已跟踪集合恢复离开消息无法再次查询到的指针类型。
    fn known_pointer_type(&self, pointer_id: u32) -> Option<POINTER_INPUT_TYPE> {
        if self.pen_ids.contains(&pointer_id) {
            Some(PT_PEN)
        } else if self.touches.contains_key(&pointer_id) {
            Some(PT_TOUCH)
        } else {
            None
        }
    }

    /// 更新触控笔靠近状态；触控笔消息仍交给 winit 处理实际书写。
    fn handle_pen_message(&mut self, pointer_id: u32, message: &MSG) -> WindowsPointerDispatch {
        let was_active = !self.pen_ids.is_empty();
        let in_range = pointer_info(pointer_id)
            .is_some_and(|info| has_pointer_flag(info, POINTER_FLAG_INRANGE));
        if matches!(message.message, WM_POINTERLEAVE | WM_POINTERCAPTURECHANGED) || !in_range {
            self.pen_ids.remove(&pointer_id);
        } else {
            self.pen_ids.insert(pointer_id);
        }
        let is_active = !self.pen_ids.is_empty();
        if is_active {
            self.candidate_ids.clear();
            self.palm_ids.clear();
            self.palm_started = false;
        }
        WindowsPointerDispatch {
            event: (was_active != is_active)
                .then_some(WindowsPointerEvent::PenActivityChanged(is_active)),
            swallow_winit: false,
        }
    }

    /// 分类触摸接触，并在确认手掌时输出动态椭圆擦除采样。
    fn handle_touch_message(&mut self, pointer_id: u32, message: &MSG) -> WindowsPointerDispatch {
        if message.message == WM_POINTERLEAVE {
            return self.leave_touch(pointer_id);
        }

        let now = Instant::now();
        let touch = read_touch_contact(pointer_id, message.hwnd, now);
        if let Some(touch) = touch {
            match self.touches.entry(pointer_id) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().update(touch);
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(touch);
                }
            }
        }

        if !self.pen_ids.is_empty() {
            if matches!(message.message, WM_POINTERUP | WM_POINTERCAPTURECHANGED) {
                self.remove_touch(pointer_id);
            }
            return WindowsPointerDispatch {
                event: Some(WindowsPointerEvent::PalmSupport {
                    point: self.touches.get(&pointer_id).map(|touch| touch.point),
                }),
                swallow_winit: true,
            };
        }

        if matches!(message.message, WM_POINTERUP | WM_POINTERCAPTURECHANGED) {
            return self.finish_touch(pointer_id);
        }

        self.refresh_candidates();
        if self.candidate_ids.contains(&pointer_id) {
            if self.candidate_confirmed(now) {
                self.palm_ids.extend(self.candidate_ids.iter().copied());
                let Some(sample) = self.palm_sample() else {
                    return WindowsPointerDispatch {
                        event: Some(WindowsPointerEvent::PalmCandidate {
                            point: self
                                .touches
                                .get(&pointer_id)
                                .map_or(CanvasPoint::new(0.0, 0.0), |touch| touch.point),
                        }),
                        swallow_winit: true,
                    };
                };
                let phase = if self.palm_started {
                    PalmErasePhase::Move
                } else {
                    self.palm_started = true;
                    PalmErasePhase::Begin
                };
                return WindowsPointerDispatch {
                    event: Some(WindowsPointerEvent::PalmErase { phase, sample }),
                    swallow_winit: true,
                };
            }
            return WindowsPointerDispatch {
                event: Some(WindowsPointerEvent::PalmCandidate {
                    point: self
                        .touches
                        .get(&pointer_id)
                        .map_or(CanvasPoint::new(0.0, 0.0), |touch| touch.point),
                }),
                swallow_winit: true,
            };
        }

        WindowsPointerDispatch {
            event: None,
            swallow_winit: false,
        }
    }

    /// 完成普通、候选或已确认手掌接触。
    fn finish_touch(&mut self, pointer_id: u32) -> WindowsPointerDispatch {
        if self.palm_ids.contains(&pointer_id) {
            return self.finish_palm_touch(pointer_id);
        }

        if self.candidate_ids.contains(&pointer_id) {
            let points = self
                .touches
                .get(&pointer_id)
                .map(|touch| touch.points.clone())
                .unwrap_or_default();
            self.remove_touch(pointer_id);
            return WindowsPointerDispatch {
                event: Some(WindowsPointerEvent::CandidateRejected {
                    points,
                    session_ended: self.candidate_ids.is_empty(),
                }),
                swallow_winit: true,
            };
        }

        self.remove_touch(pointer_id);
        WindowsPointerDispatch {
            event: None,
            swallow_winit: false,
        }
    }

    /// 在指针离开窗口时终止 tracker 状态，避免把候选或手掌遗留到下一次接触。
    fn leave_touch(&mut self, pointer_id: u32) -> WindowsPointerDispatch {
        if self.palm_ids.contains(&pointer_id) {
            return self.finish_palm_touch(pointer_id);
        }

        if self.candidate_ids.contains(&pointer_id) {
            self.remove_touch(pointer_id);
            return WindowsPointerDispatch {
                event: Some(WindowsPointerEvent::CandidateCancelled {
                    session_ended: self.candidate_ids.is_empty(),
                }),
                swallow_winit: true,
            };
        }

        self.remove_touch(pointer_id);
        WindowsPointerDispatch {
            event: None,
            swallow_winit: false,
        }
    }

    /// 从已确认手掌移除一个接触，并在最后一个接触结束时提交擦除会话。
    fn finish_palm_touch(&mut self, pointer_id: u32) -> WindowsPointerDispatch {
        let sample = self.palm_sample();
        self.remove_touch(pointer_id);
        let ended = self.palm_ids.is_empty();
        if ended {
            self.palm_started = false;
            self.candidate_ids.clear();
        }
        WindowsPointerDispatch {
            event: sample.map(|sample| WindowsPointerEvent::PalmErase {
                phase: if ended {
                    PalmErasePhase::End
                } else {
                    PalmErasePhase::Move
                },
                sample,
            }),
            swallow_winit: true,
        }
    }

    /// 根据单接触面积、置信度和聚集多触点联合范围更新候选集合。
    fn refresh_candidates(&mut self) {
        for (pointer_id, touch) in &self.touches {
            if !touch.confident
                || (touch.geometry.area() >= SINGLE_PALM_MIN_AREA
                    && touch.geometry.major_axis() >= SINGLE_PALM_MIN_MAJOR_AXIS)
            {
                self.candidate_ids.insert(*pointer_id);
            }
        }

        if self.touches.len() >= 2 {
            let union = self
                .touches
                .values()
                .map(|touch| touch.geometry)
                .reduce(ContactGeometry::union);
            if let Some(union) = union {
                let major_axis = union.major_axis();
                if union.area() >= CLUSTER_PALM_MIN_AREA
                    && (CLUSTER_PALM_MIN_MAJOR_AXIS..=CLUSTER_PALM_MAX_MAJOR_AXIS)
                        .contains(&major_axis)
                {
                    self.candidate_ids.extend(self.touches.keys().copied());
                }
            }
        }
    }

    /// 返回最早候选接触是否已经超过短确认窗口。
    fn candidate_confirmed(&self, now: Instant) -> bool {
        self.candidate_ids
            .iter()
            .filter_map(|pointer_id| self.touches.get(pointer_id))
            .map(|touch| touch.started_at)
            .min()
            .is_some_and(|started_at| now.duration_since(started_at) >= PALM_CONFIRMATION_DELAY)
    }

    /// 把当前全部手掌贡献接触合并为一个动态旋转椭圆。
    fn palm_sample(&self) -> Option<EraseSample> {
        let mut contacts: Vec<_> = self
            .palm_ids
            .iter()
            .filter_map(|pointer_id| self.touches.get(pointer_id))
            .collect();
        contacts.sort_by(|left, right| {
            left.point
                .x
                .total_cmp(&right.point.x)
                .then_with(|| left.point.y.total_cmp(&right.point.y))
        });
        let geometry = contacts
            .iter()
            .map(|touch| touch.geometry)
            .reduce(ContactGeometry::union)?;
        let rotation_radians = if contacts.len() >= 2 {
            farthest_contact_rotation(&contacts)
        } else {
            contacts.first()?.geometry.rotation_radians
        };
        Some(EraseSample {
            center: geometry.center(),
            radius_x: (geometry.width() / 2.0).max(MIN_CONTACT_RADIUS),
            radius_y: (geometry.height() / 2.0).max(MIN_CONTACT_RADIUS),
            rotation_radians,
        })
    }

    /// 从所有跟踪集合中移除一个触摸标识。
    fn remove_touch(&mut self, pointer_id: u32) {
        self.touches.remove(&pointer_id);
        self.candidate_ids.remove(&pointer_id);
        self.palm_ids.remove(&pointer_id);
    }
}

/// 返回接触中心最远点对的稳定椭圆方向，范围归一化到 `[0, PI)`。
fn farthest_contact_rotation(contacts: &[&TouchContact]) -> f32 {
    let mut farthest_distance_squared = 0.0;
    let mut rotation_radians = contacts
        .first()
        .map_or(0.0, |contact| contact.geometry.rotation_radians);
    for (index, left) in contacts.iter().enumerate() {
        for right in &contacts[index + 1..] {
            let delta_x = right.point.x - left.point.x;
            let delta_y = right.point.y - left.point.y;
            let distance_squared = delta_x.mul_add(delta_x, delta_y * delta_y);
            if distance_squared > farthest_distance_squared {
                farthest_distance_squared = distance_squared;
                rotation_radians = delta_y.atan2(delta_x).rem_euclid(std::f32::consts::PI);
            }
        }
    }
    rotation_radians
}

/// 原生 Pointer Input 中的一个活动触摸接触。
struct TouchContact {
    point: CanvasPoint,
    geometry: ContactGeometry,
    confident: bool,
    started_at: Instant,
    points: Vec<CanvasPoint>,
}

impl TouchContact {
    /// 使用第一个原生采样创建接触记录。
    fn new(
        point: CanvasPoint,
        geometry: ContactGeometry,
        confident: bool,
        started_at: Instant,
    ) -> Self {
        Self {
            point,
            geometry,
            confident,
            started_at,
            points: vec![point],
        }
    }

    /// 更新接触几何和去重后的轨迹点。
    fn update(&mut self, next: Self) {
        self.point = next.point;
        self.geometry = next.geometry;
        self.confident = next.confident;
        if self.points.last().is_none_or(|last| {
            let delta_x = last.x - next.point.x;
            let delta_y = last.y - next.point.y;
            delta_x.mul_add(delta_x, delta_y * delta_y) >= 0.25
        }) {
            self.points.push(next.point);
        }
    }
}

/// 物理像素中的接触包围矩形及设备报告方向。
#[derive(Clone, Copy)]
struct ContactGeometry {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    rotation_radians: f32,
}

impl ContactGeometry {
    /// 返回两个接触包围矩形的联合区域。
    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
            rotation_radians: self.rotation_radians,
        }
    }

    /// 返回接触矩形中心。
    fn center(self) -> CanvasPoint {
        CanvasPoint::new(
            (self.left + self.right) / 2.0,
            (self.top + self.bottom) / 2.0,
        )
    }

    /// 返回接触宽度。
    fn width(self) -> f32 {
        (self.right - self.left).abs()
    }

    /// 返回接触高度。
    fn height(self) -> f32 {
        (self.bottom - self.top).abs()
    }

    /// 返回接触面积。
    fn area(self) -> f32 {
        self.width() * self.height()
    }

    /// 返回接触长轴尺寸。
    fn major_axis(self) -> f32 {
        self.width().max(self.height())
    }
}

/// 从 Windows Pointer API 读取触摸位置、接触区域和置信度。
fn read_touch_contact(pointer_id: u32, window: HWND, now: Instant) -> Option<TouchContact> {
    let mut touch_info = POINTER_TOUCH_INFO::default();
    // SAFETY: 输出结构在调用期间有效，pointer_id 来自当前 WM_POINTER 消息。
    unsafe { GetPointerTouchInfo(pointer_id, &mut touch_info) }.ok()?;
    let point = screen_to_client(window, touch_info.pointerInfo.ptPixelLocation)?;
    let geometry = if touch_info.touchMask & TOUCH_MASK_CONTACTAREA != 0 {
        let top_left = screen_to_client(
            window,
            POINT {
                x: touch_info.rcContact.left,
                y: touch_info.rcContact.top,
            },
        )?;
        let bottom_right = screen_to_client(
            window,
            POINT {
                x: touch_info.rcContact.right,
                y: touch_info.rcContact.bottom,
            },
        )?;
        ContactGeometry {
            left: top_left.x,
            top: top_left.y,
            right: bottom_right.x,
            bottom: bottom_right.y,
            rotation_radians: (touch_info.orientation as f32).to_radians(),
        }
    } else {
        ContactGeometry {
            left: point.x - MIN_CONTACT_RADIUS,
            top: point.y - MIN_CONTACT_RADIUS,
            right: point.x + MIN_CONTACT_RADIUS,
            bottom: point.y + MIN_CONTACT_RADIUS,
            rotation_radians: 0.0,
        }
    };
    Some(TouchContact::new(
        point,
        geometry,
        has_pointer_flag(touch_info.pointerInfo, POINTER_FLAG_CONFIDENCE),
        now,
    ))
}

/// 查询一个 Pointer Input 标识的设备类型。
fn pointer_type(pointer_id: u32) -> Option<POINTER_INPUT_TYPE> {
    let mut pointer_type = POINTER_INPUT_TYPE::default();
    // SAFETY: 输出值在调用期间有效，pointer_id 来自当前消息。
    unsafe { GetPointerType(pointer_id, &mut pointer_type) }
        .ok()
        .map(|()| pointer_type)
}

/// 查询通用 Pointer Input 信息。
fn pointer_info(pointer_id: u32) -> Option<POINTER_INFO> {
    let mut pointer_info = POINTER_INFO::default();
    // SAFETY: 输出结构在调用期间有效，pointer_id 来自当前消息。
    unsafe { GetPointerInfo(pointer_id, &mut pointer_info) }
        .ok()
        .map(|()| pointer_info)
}

/// 把 Pointer API 的屏幕物理像素坐标转换为当前窗口客户区物理像素。
fn screen_to_client(window: HWND, mut point: POINT) -> Option<CanvasPoint> {
    // SAFETY: HWND 和 POINT 均来自当前线程正在分发的窗口消息。
    unsafe { ScreenToClient(window, &mut point) }
        .as_bool()
        .then_some(CanvasPoint::new(point.x as f32, point.y as f32))
}

/// 判断 POINTER_INFO 是否包含指定标志。
const fn has_pointer_flag(
    pointer_info: POINTER_INFO,
    flag: windows::Win32::UI::Input::Pointer::POINTER_FLAGS,
) -> bool {
    pointer_info.pointerFlags.0 & flag.0 != 0
}

/// 返回消息是否属于本模块需要观察的 Pointer Input 范围。
const fn is_pointer_message(message: u32) -> bool {
    matches!(
        message,
        WM_POINTERDOWN
            | WM_POINTERUPDATE
            | WM_POINTERUP
            | WM_POINTERENTER
            | WM_POINTERLEAVE
            | WM_POINTERCAPTURECHANGED
    )
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// 创建不依赖 Windows API 的已跟踪触摸接触。
    fn touch_contact() -> TouchContact {
        TouchContact::new(
            CanvasPoint::new(100.0, 120.0),
            ContactGeometry {
                left: 80.0,
                top: 90.0,
                right: 120.0,
                bottom: 150.0,
                rotation_radians: 0.0,
            },
            true,
            Instant::now(),
        )
    }

    /// 验证候选手掌离开窗口时只清理状态，不提交缓冲普通笔画。
    #[test]
    fn leaving_candidate_cancels_without_buffered_stroke() {
        let mut tracker = WindowsPointerTracker::default();
        tracker.touches.insert(7, touch_contact());
        tracker.candidate_ids.insert(7);

        let dispatch = tracker.leave_touch(7);

        assert!(matches!(
            dispatch.event,
            Some(WindowsPointerEvent::CandidateCancelled {
                session_ended: true
            })
        ));
        assert!(dispatch.swallow_winit);
        assert!(tracker.touches.is_empty());
        assert!(tracker.candidate_ids.is_empty());
        assert!(tracker.palm_ids.is_empty());
    }

    /// 验证候选簇仅在最后一个触点离开后才结束路由会话。
    #[test]
    fn leaving_one_candidate_preserves_the_remaining_session() {
        let mut tracker = WindowsPointerTracker::default();
        tracker.touches.insert(7, touch_contact());
        tracker.touches.insert(8, touch_contact());
        tracker.candidate_ids.extend([7, 8]);

        let first_dispatch = tracker.leave_touch(7);
        let second_dispatch = tracker.leave_touch(8);

        assert!(matches!(
            first_dispatch.event,
            Some(WindowsPointerEvent::CandidateCancelled {
                session_ended: false
            })
        ));
        assert!(matches!(
            second_dispatch.event,
            Some(WindowsPointerEvent::CandidateCancelled {
                session_ended: true
            })
        ));
        assert!(tracker.touches.is_empty());
        assert!(tracker.candidate_ids.is_empty());
    }

    /// 验证最后一个已确认手掌离开窗口时发出擦除结束并清理所有集合。
    #[test]
    fn leaving_last_palm_ends_erase_session() {
        let mut tracker = WindowsPointerTracker::default();
        tracker.touches.insert(7, touch_contact());
        tracker.candidate_ids.insert(7);
        tracker.palm_ids.insert(7);
        tracker.palm_started = true;

        let dispatch = tracker.leave_touch(7);

        assert!(matches!(
            dispatch.event,
            Some(WindowsPointerEvent::PalmErase {
                phase: PalmErasePhase::End,
                ..
            })
        ));
        assert!(dispatch.swallow_winit);
        assert!(tracker.touches.is_empty());
        assert!(tracker.candidate_ids.is_empty());
        assert!(tracker.palm_ids.is_empty());
        assert!(!tracker.palm_started);
    }
}
