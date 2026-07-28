use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::ScreenToClient,
    System::Performance::QueryPerformanceFrequency,
    UI::{
        Input::Pointer::{
            GetPointerFrameTouchInfoHistory, GetPointerInfo, GetPointerInfoHistory,
            GetPointerTouchInfo, GetPointerType, POINTER_FLAG_CONFIDENCE, POINTER_FLAG_INCONTACT,
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
use crate::settings::PalmSizePreset;

use super::router::PointerSample;

const PALM_CONFIRMATION_DELAY: Duration = Duration::from_millis(40);
const SINGLE_PALM_MIN_AREA: f32 = 4_096.0;
const SINGLE_PALM_MIN_MAJOR_AXIS: f32 = 72.0;
const CLUSTER_PALM_MIN_AREA: f32 = 6_400.0;
const CLUSTER_PALM_MIN_MAJOR_AXIS: f32 = 96.0;
const CLUSTER_PALM_MAX_MAJOR_AXIS: f32 = 320.0;
const MIN_CONTACT_RADIUS: f32 = 8.0;
const MAX_TOUCH_HISTORY_ITEMS: usize = 4_096;

/// 在原生消息 hook 与应用设置之间同步手掌尺寸预设。
#[derive(Clone)]
pub struct SharedPalmSizePreset(Arc<AtomicU8>);

impl Default for SharedPalmSizePreset {
    /// 创建使用标准尺寸的共享预设。
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(palm_size_code(
            PalmSizePreset::Standard,
        ))))
    }
}

impl SharedPalmSizePreset {
    /// 原子更新下一条触摸消息使用的手掌尺寸预设。
    pub fn store(&self, preset: PalmSizePreset) {
        self.0.store(palm_size_code(preset), Ordering::Release);
    }

    /// 读取当前预设，并将未知编码安全退化为标准档。
    fn load(&self) -> PalmSizePreset {
        palm_size_from_code(self.0.load(Ordering::Acquire))
    }
}

/// 把手掌尺寸预设编码为共享原子的稳定小整数。
const fn palm_size_code(preset: PalmSizePreset) -> u8 {
    match preset {
        PalmSizePreset::Small => 0,
        PalmSizePreset::Standard => 1,
        PalmSizePreset::Large => 2,
    }
}

/// 把共享原子的整数恢复为手掌尺寸预设。
const fn palm_size_from_code(code: u8) -> PalmSizePreset {
    match code {
        0 => PalmSizePreset::Small,
        2 => PalmSizePreset::Large,
        _ => PalmSizePreset::Standard,
    }
}

/// 一档手掌预设对应的单接触与多接触联合分类阈值。
#[derive(Debug, Clone, Copy, PartialEq)]
struct PalmThresholds {
    single_min_area: f32,
    single_min_major_axis: f32,
    cluster_min_area: f32,
    cluster_min_major_axis: f32,
}

impl PalmThresholds {
    /// 按产品三档比例缩放当前标准阈值。
    fn for_preset(preset: PalmSizePreset) -> Self {
        let scale = match preset {
            PalmSizePreset::Small => 0.75,
            PalmSizePreset::Standard => 1.0,
            PalmSizePreset::Large => 1.25,
        };
        Self {
            single_min_area: SINGLE_PALM_MIN_AREA * scale,
            single_min_major_axis: SINGLE_PALM_MIN_MAJOR_AXIS * scale,
            cluster_min_area: CLUSTER_PALM_MIN_AREA * scale,
            cluster_min_major_axis: CLUSTER_PALM_MIN_MAJOR_AXIS * scale,
        }
    }
}

/// 原生 Pointer Input hook 需要交给运行时的高层语义。
#[derive(Debug)]
pub enum WindowsPointerEvent {
    PenActivityChanged(bool),
    Pen {
        phase: PenPhase,
        points: Vec<PointerSample>,
    },
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
        points: Vec<PointerSample>,
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
            Self::Pen { points, .. } => points.last().map(|sample| sample.point),
            Self::PalmCandidate { point } => Some(*point),
            Self::PalmSupport { point } => *point,
            Self::PalmErase { sample, .. } => Some(sample.center),
            Self::CandidateRejected { points, .. } => points.last().map(|sample| sample.point),
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

/// 一次原生触控笔接触中的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenPhase {
    Begin,
    Move,
    End,
    Cancel,
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
pub struct WindowsPointerTracker {
    pen_ids: HashSet<u32>,
    pen_contact_ids: HashSet<u32>,
    pointer_history: Vec<POINTER_INFO>,
    touch_history: Vec<POINTER_TOUCH_INFO>,
    clock_start: Instant,
    qpc_frequency: Option<f64>,
    pen_time_sources: HashMap<u32, PenTimeState>,
    touches: HashMap<u32, TouchContact>,
    candidate_ids: HashSet<u32>,
    palm_ids: HashSet<u32>,
    palm_started: bool,
    palm_size_preset: SharedPalmSizePreset,
}

impl Default for WindowsPointerTracker {
    /// 初始化原生 Pointer 跟踪器和可用的 QPC 频率缓存。
    fn default() -> Self {
        Self {
            pen_ids: HashSet::new(),
            pen_contact_ids: HashSet::new(),
            pointer_history: Vec::new(),
            touch_history: Vec::new(),
            clock_start: Instant::now(),
            qpc_frequency: query_qpc_frequency(),
            pen_time_sources: HashMap::new(),
            touches: HashMap::new(),
            candidate_ids: HashSet::new(),
            palm_ids: HashSet::new(),
            palm_started: false,
            palm_size_preset: SharedPalmSizePreset::default(),
        }
    }
}

/// 记录一条原生笔接触固定使用的时间源及其单调结果。
struct PenTimeState {
    source: PenTimeSource,
    last_micros: u64,
}

/// 原生笔时间源优先级：QPC、dwTime，最后退化到到达时钟。
enum PenTimeSource {
    Qpc {
        frequency: f64,
    },
    DwTime {
        last_raw: Option<u32>,
        unwrapped_millis: u64,
    },
    Arrival,
}

/// 查询当前系统可用的 QPC 频率，并在失败时交给后续时间源退化处理。
fn query_qpc_frequency() -> Option<f64> {
    let mut frequency = 0_i64;
    // SAFETY: Windows API 只写入调用者提供的有效频率输出位置。
    unsafe {
        QueryPerformanceFrequency(&mut frequency).ok()?;
    }
    (frequency > 0).then_some(frequency as f64)
}

/// 将 QPC 计数换算为不会因整数乘法溢出的微秒值。
fn qpc_timestamp_micros(counter: u64, frequency: f64) -> Option<u64> {
    if !frequency.is_finite() || frequency <= 0.0 || counter == 0 {
        return None;
    }
    let micros = counter as f64 * 1_000_000.0 / frequency;
    micros.is_finite().then_some(micros.max(0.0) as u64)
}

impl WindowsPointerTracker {
    /// 创建使用应用共享手掌尺寸预设的原生 Pointer 跟踪器。
    pub fn with_palm_size_preset(palm_size_preset: SharedPalmSizePreset) -> Self {
        Self {
            palm_size_preset,
            ..Self::default()
        }
    }

    /// 返回 hook 当前是否观察到正在接触屏幕的原生触控笔。
    pub fn pen_contact_active(&self) -> bool {
        !self.pen_contact_ids.is_empty()
    }

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

    /// 更新触控笔靠近和接触状态，并输出原生批量书写采样。
    fn handle_pen_message(&mut self, pointer_id: u32, message: &MSG) -> WindowsPointerDispatch {
        let was_active = !self.pen_ids.is_empty();
        let pointer_info = pointer_info(pointer_id);
        let in_range =
            pointer_info.is_some_and(|info| has_pointer_flag(info, POINTER_FLAG_INRANGE));
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
        let was_in_contact = self.pen_contact_ids.contains(&pointer_id);
        let is_in_contact =
            pointer_info.is_some_and(|info| has_pointer_flag(info, POINTER_FLAG_INCONTACT));
        let phase = pen_phase_for_message(message.message, was_in_contact, is_in_contact);
        match phase {
            Some(PenPhase::Begin | PenPhase::Move) => {
                self.pen_contact_ids.insert(pointer_id);
            }
            Some(PenPhase::End | PenPhase::Cancel) => {
                self.pen_contact_ids.remove(&pointer_id);
            }
            None => {}
        }
        if phase == Some(PenPhase::Begin) {
            self.begin_pen_time_source(pointer_id, pointer_info);
        }
        let event = phase.map(|phase| WindowsPointerEvent::Pen {
            phase,
            points: if phase == PenPhase::Cancel {
                Vec::new()
            } else {
                self.read_pen_points(pointer_id, message.hwnd, pointer_info)
            },
        });
        if matches!(phase, Some(PenPhase::End | PenPhase::Cancel)) {
            self.pen_time_sources.remove(&pointer_id);
        }
        WindowsPointerDispatch {
            event: event.or_else(|| {
                (was_active != is_active)
                    .then_some(WindowsPointerEvent::PenActivityChanged(is_active))
            }),
            swallow_winit: false,
        }
    }

    /// 读取当前触控笔消息的时间正序位置历史，并在失败时退化为当前点。
    fn read_pen_points(
        &mut self,
        pointer_id: u32,
        window: HWND,
        current: Option<POINTER_INFO>,
    ) -> Vec<PointerSample> {
        let Some(current) = current else {
            return Vec::new();
        };
        let history_count = current.historyCount.max(1);
        self.pointer_history
            .resize(history_count as usize, POINTER_INFO::default());
        let mut entries_count = history_count;
        let history_read = unsafe {
            GetPointerInfoHistory(
                pointer_id,
                &mut entries_count,
                Some(self.pointer_history.as_mut_ptr()),
            )
        }
        .is_ok();
        if history_read {
            let count = (entries_count as usize).min(self.pointer_history.len());
            let history_entries: Vec<_> =
                chronological_pointer_history(&self.pointer_history, count)
                    .copied()
                    .collect();
            let points: Vec<_> = history_entries
                .iter()
                .filter_map(|info| {
                    let point = screen_to_client(window, info.ptPixelLocation)?;
                    Some(PointerSample::new(
                        point,
                        self.pen_timestamp(pointer_id, info),
                    ))
                })
                .collect();
            if !points.is_empty() {
                return points;
            }
        }
        screen_to_client(window, current.ptPixelLocation)
            .map(|point| PointerSample::new(point, self.pen_timestamp(pointer_id, &current)))
            .into_iter()
            .collect()
    }

    /// 为一条新原生笔接触固定其可用的时间源。
    fn begin_pen_time_source(&mut self, pointer_id: u32, current: Option<POINTER_INFO>) {
        let source = current
            .filter(|info| info.PerformanceCount > 0)
            .and(self.qpc_frequency)
            .map(|frequency| PenTimeSource::Qpc { frequency })
            .or_else(|| {
                current
                    .filter(|info| info.dwTime > 0)
                    .map(|_| PenTimeSource::DwTime {
                        last_raw: None,
                        unwrapped_millis: 0,
                    })
            })
            .unwrap_or(PenTimeSource::Arrival);
        self.pen_time_sources.insert(
            pointer_id,
            PenTimeState {
                source,
                last_micros: 0,
            },
        );
    }

    /// 把一条原生笔 Pointer 信息转换为固定时间源下的单调微秒。
    fn pen_timestamp(&mut self, pointer_id: u32, info: &POINTER_INFO) -> u64 {
        if !self.pen_time_sources.contains_key(&pointer_id) {
            self.begin_pen_time_source(pointer_id, Some(*info));
        }
        let arrival_micros = self.arrival_timestamp_micros();
        let Some(state) = self.pen_time_sources.get_mut(&pointer_id) else {
            return arrival_micros;
        };
        let timestamp = match &mut state.source {
            PenTimeSource::Qpc { frequency } if info.PerformanceCount > 0 => {
                qpc_timestamp_micros(info.PerformanceCount, *frequency).unwrap_or(arrival_micros)
            }
            PenTimeSource::DwTime {
                last_raw,
                unwrapped_millis,
            } if info.dwTime > 0 => {
                let raw = info.dwTime;
                if let Some(previous) = *last_raw {
                    let delta = raw.wrapping_sub(previous);
                    if delta <= u32::MAX / 2 {
                        *unwrapped_millis = unwrapped_millis.saturating_add(delta as u64);
                    }
                } else {
                    *unwrapped_millis = raw as u64;
                }
                *last_raw = Some(raw);
                unwrapped_millis.saturating_mul(1_000)
            }
            _ => arrival_micros,
        };
        state.last_micros = timestamp.max(state.last_micros);
        state.last_micros
    }

    /// 返回 tracker 初始化后的单调到达时间，作为原生时间源的最后退化路径。
    fn arrival_timestamp_micros(&self) -> u64 {
        self.clock_start.elapsed().as_micros() as u64
    }

    /// 分类触摸接触，并在确认手掌时输出动态椭圆擦除采样。
    fn handle_touch_message(&mut self, pointer_id: u32, message: &MSG) -> WindowsPointerDispatch {
        if message.message == WM_POINTERLEAVE {
            return self.leave_touch(pointer_id);
        }

        let now = Instant::now();
        self.update_touch_frame(pointer_id, message.hwnd, now);

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

    /// 批量读取同一触摸帧的历史接触，并在失败时退化到当前消息接触。
    fn update_touch_frame(&mut self, pointer_id: u32, window: HWND, now: Instant) {
        let arrival_timestamp_micros = self.arrival_timestamp_micros();
        let mut entries_count = 0_u32;
        let mut pointer_count = 0_u32;
        // SAFETY: 空缓冲调用只查询系统为当前 Pointer 帧报告的两个维度。
        let _ = unsafe {
            GetPointerFrameTouchInfoHistory(
                pointer_id,
                &mut entries_count,
                &mut pointer_count,
                None,
            )
        };
        let total_items = (entries_count as usize).checked_mul(pointer_count as usize);
        if let Some(total_items) = total_items.filter(|count| {
            entries_count > 0 && pointer_count > 0 && *count <= MAX_TOUCH_HISTORY_ITEMS
        }) {
            self.touch_history
                .resize(total_items, POINTER_TOUCH_INFO::default());
            let mut read_entries = entries_count;
            let mut read_pointers = pointer_count;
            // SAFETY: 缓冲区按查询得到的 entries * pointers 大小分配，并在调用期间有效。
            let read_succeeded = unsafe {
                GetPointerFrameTouchInfoHistory(
                    pointer_id,
                    &mut read_entries,
                    &mut read_pointers,
                    Some(self.touch_history.as_mut_ptr()),
                )
            }
            .is_ok();
            if read_succeeded {
                let mut updated = false;
                for info in chronological_touch_history(
                    &self.touch_history,
                    read_entries as usize,
                    read_pointers as usize,
                ) {
                    let timestamp_micros = touch_timestamp_micros(
                        &info.pointerInfo,
                        self.qpc_frequency,
                        arrival_timestamp_micros,
                    );
                    if let Some(touch) =
                        touch_contact_from_info(info, window, now, timestamp_micros)
                    {
                        update_touch_contact(&mut self.touches, info.pointerInfo.pointerId, touch);
                        updated = true;
                    }
                }
                if updated {
                    return;
                }
            }
        }

        if let Some(touch) = read_touch_contact(
            pointer_id,
            window,
            now,
            arrival_timestamp_micros,
            self.qpc_frequency,
        ) {
            update_touch_contact(&mut self.touches, pointer_id, touch);
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
        let thresholds = PalmThresholds::for_preset(self.palm_size_preset.load());
        for (pointer_id, touch) in &self.touches {
            if !touch.confident
                || (touch.geometry.area() >= thresholds.single_min_area
                    && touch.geometry.major_axis() >= thresholds.single_min_major_axis)
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
                if union.area() >= thresholds.cluster_min_area
                    && (thresholds.cluster_min_major_axis..=CLUSTER_PALM_MAX_MAJOR_AXIS)
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

/// 将 Win32 笔消息和接触标记归一化为单次接触阶段。
const fn pen_phase_for_message(
    message: u32,
    was_in_contact: bool,
    is_in_contact: bool,
) -> Option<PenPhase> {
    match message {
        WM_POINTERDOWN => Some(PenPhase::Begin),
        WM_POINTERUPDATE if is_in_contact => Some(PenPhase::Move),
        WM_POINTERUPDATE if was_in_contact => Some(PenPhase::Cancel),
        WM_POINTERUP => Some(PenPhase::End),
        WM_POINTERLEAVE | WM_POINTERCAPTURECHANGED if was_in_contact => Some(PenPhase::Cancel),
        _ => None,
    }
}

/// 按时间正序访问 Windows 以新到旧顺序返回的已初始化历史条目。
fn chronological_pointer_history(
    history: &[POINTER_INFO],
    count: usize,
) -> impl Iterator<Item = &POINTER_INFO> {
    history[..count.min(history.len())].iter().rev()
}

/// 按时间正序遍历 Windows 以新帧到旧帧排列的触摸历史矩阵。
fn chronological_touch_history(
    history: &[POINTER_TOUCH_INFO],
    entries_count: usize,
    pointer_count: usize,
) -> impl Iterator<Item = &POINTER_TOUCH_INFO> {
    let available_entries = if pointer_count == 0 {
        0
    } else {
        entries_count.min(history.len() / pointer_count)
    };
    (0..available_entries).rev().flat_map(move |entry| {
        let start = entry * pointer_count;
        history[start..start + pointer_count].iter()
    })
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
    last_frame_id: u32,
    last_timestamp_micros: u64,
    points: Vec<PointerSample>,
}

impl TouchContact {
    /// 使用第一个原生采样创建接触记录。
    fn new(
        point: CanvasPoint,
        geometry: ContactGeometry,
        confident: bool,
        started_at: Instant,
        frame_id: u32,
        timestamp_micros: u64,
    ) -> Self {
        Self {
            point,
            geometry,
            confident,
            started_at,
            last_frame_id: frame_id,
            last_timestamp_micros: timestamp_micros,
            points: vec![PointerSample::new(point, timestamp_micros)],
        }
    }

    /// 更新接触几何和去重后的轨迹点。
    fn update(&mut self, next: Self) {
        if !touch_frame_is_newer(
            next.last_frame_id,
            self.last_frame_id,
            next.last_timestamp_micros,
            self.last_timestamp_micros,
        ) {
            return;
        }
        self.point = next.point;
        self.geometry = next.geometry;
        self.confident = next.confident;
        self.last_frame_id = next.last_frame_id;
        self.last_timestamp_micros = next.last_timestamp_micros;
        if self.points.last().is_none_or(|last| {
            let delta_x = last.point.x - next.point.x;
            let delta_y = last.point.y - next.point.y;
            delta_x.mul_add(delta_x, delta_y * delta_y) >= 0.25
        }) {
            self.points.push(PointerSample::new(
                next.point,
                next.points[0].timestamp_micros,
            ));
        }
    }
}

/// 判断触摸帧是否更新，并在驱动不提供 frameId 时退化到时间戳。
fn touch_frame_is_newer(
    next_frame_id: u32,
    current_frame_id: u32,
    next_timestamp_micros: u64,
    current_timestamp_micros: u64,
) -> bool {
    if next_frame_id == 0 || current_frame_id == 0 {
        return next_timestamp_micros > current_timestamp_micros;
    }
    let delta = next_frame_id.wrapping_sub(current_frame_id);
    delta != 0 && delta <= u32::MAX / 2
}

/// 将一条触摸接触插入跟踪表，或按帧顺序更新已有接触。
fn update_touch_contact(
    touches: &mut HashMap<u32, TouchContact>,
    pointer_id: u32,
    touch: TouchContact,
) {
    match touches.entry(pointer_id) {
        std::collections::hash_map::Entry::Occupied(mut entry) => entry.get_mut().update(touch),
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(touch);
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
fn read_touch_contact(
    pointer_id: u32,
    window: HWND,
    now: Instant,
    timestamp_micros: u64,
    qpc_frequency: Option<f64>,
) -> Option<TouchContact> {
    let mut touch_info = POINTER_TOUCH_INFO::default();
    // SAFETY: 输出结构在调用期间有效，pointer_id 来自当前 WM_POINTER 消息。
    unsafe { GetPointerTouchInfo(pointer_id, &mut touch_info) }.ok()?;
    let timestamp_micros =
        touch_timestamp_micros(&touch_info.pointerInfo, qpc_frequency, timestamp_micros);
    touch_contact_from_info(&touch_info, window, now, timestamp_micros)
}

/// 将一条已初始化的 Windows 触摸信息转换为物理像素接触记录。
fn touch_contact_from_info(
    touch_info: &POINTER_TOUCH_INFO,
    window: HWND,
    now: Instant,
    timestamp_micros: u64,
) -> Option<TouchContact> {
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
        touch_info.pointerInfo.frameId,
        timestamp_micros,
    ))
}

/// 从触摸 Pointer 信息选择 QPC、毫秒时间或到达时间作为去重时间戳。
fn touch_timestamp_micros(
    info: &POINTER_INFO,
    qpc_frequency: Option<f64>,
    arrival_timestamp_micros: u64,
) -> u64 {
    qpc_frequency
        .and_then(|frequency| qpc_timestamp_micros(info.PerformanceCount, frequency))
        .or_else(|| (info.dwTime > 0).then_some(info.dwTime as u64 * 1_000))
        .unwrap_or(arrival_timestamp_micros)
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
            1,
            100,
        )
    }

    /// 验证接触标记在更新消息中消失时立即取消旧笔会话。
    #[test]
    fn pen_update_without_contact_cancels_active_session() {
        assert_eq!(
            pen_phase_for_message(WM_POINTERUPDATE, true, false),
            Some(PenPhase::Cancel)
        );
        assert_eq!(pen_phase_for_message(WM_POINTERUPDATE, false, false), None);
    }

    /// 验证历史条目转为时间正序，并把系统返回数量限制在已初始化缓冲区内。
    #[test]
    fn pointer_history_is_chronological_and_bounded() {
        let history = [
            POINTER_INFO {
                frameId: 3,
                ..POINTER_INFO::default()
            },
            POINTER_INFO {
                frameId: 2,
                ..POINTER_INFO::default()
            },
            POINTER_INFO {
                frameId: 1,
                ..POINTER_INFO::default()
            },
        ];

        let frame_ids: Vec<_> = chronological_pointer_history(&history, usize::MAX)
            .map(|info| info.frameId)
            .collect();

        assert_eq!(frame_ids, vec![1, 2, 3]);
    }

    /// 验证触摸历史只反转帧顺序，并保留每帧内的接触顺序。
    #[test]
    fn touch_history_is_chronological_and_bounded() {
        let history = [
            POINTER_TOUCH_INFO {
                pointerInfo: POINTER_INFO {
                    frameId: 3,
                    pointerId: 31,
                    ..POINTER_INFO::default()
                },
                ..POINTER_TOUCH_INFO::default()
            },
            POINTER_TOUCH_INFO {
                pointerInfo: POINTER_INFO {
                    frameId: 3,
                    pointerId: 32,
                    ..POINTER_INFO::default()
                },
                ..POINTER_TOUCH_INFO::default()
            },
            POINTER_TOUCH_INFO {
                pointerInfo: POINTER_INFO {
                    frameId: 2,
                    pointerId: 21,
                    ..POINTER_INFO::default()
                },
                ..POINTER_TOUCH_INFO::default()
            },
            POINTER_TOUCH_INFO {
                pointerInfo: POINTER_INFO {
                    frameId: 2,
                    pointerId: 22,
                    ..POINTER_INFO::default()
                },
                ..POINTER_TOUCH_INFO::default()
            },
        ];

        let identifiers: Vec<_> = chronological_touch_history(&history, usize::MAX, 2)
            .map(|info| (info.pointerInfo.frameId, info.pointerInfo.pointerId))
            .collect();

        assert_eq!(identifiers, vec![(2, 21), (2, 22), (3, 31), (3, 32)]);
        assert_eq!(chronological_touch_history(&history, 2, 0).count(), 0);
    }

    /// 验证相邻消息重复返回同一时间戳时不会覆盖几何或追加缓冲点。
    #[test]
    fn touch_contact_ignores_duplicate_history_samples() {
        let mut contact = touch_contact();
        let duplicate = TouchContact::new(
            CanvasPoint::new(140.0, 160.0),
            ContactGeometry {
                left: 120.0,
                top: 130.0,
                right: 160.0,
                bottom: 190.0,
                rotation_radians: 0.0,
            },
            true,
            Instant::now(),
            1,
            100,
        );

        contact.update(duplicate);

        assert_eq!(contact.point, CanvasPoint::new(100.0, 120.0));
        assert_eq!(contact.points.len(), 1);
    }

    /// 验证同一毫秒内 frameId 递增的有效触摸不会被时间戳去重误删。
    #[test]
    fn touch_contact_accepts_a_new_frame_with_the_same_timestamp() {
        let mut contact = touch_contact();
        let next = TouchContact::new(
            CanvasPoint::new(104.0, 124.0),
            ContactGeometry {
                left: 84.0,
                top: 94.0,
                right: 124.0,
                bottom: 154.0,
                rotation_radians: 0.0,
            },
            true,
            Instant::now(),
            2,
            100,
        );

        contact.update(next);

        assert_eq!(contact.point, CanvasPoint::new(104.0, 124.0));
        assert_eq!(contact.points.len(), 2);
    }

    /// 验证三档阈值严格递增，并保留当前标准档数值。
    #[test]
    fn palm_size_thresholds_are_ordered() {
        let small = PalmThresholds::for_preset(PalmSizePreset::Small);
        let standard = PalmThresholds::for_preset(PalmSizePreset::Standard);
        let large = PalmThresholds::for_preset(PalmSizePreset::Large);

        assert!(small.single_min_area < standard.single_min_area);
        assert!(standard.single_min_area < large.single_min_area);
        assert!(small.single_min_major_axis < standard.single_min_major_axis);
        assert!(standard.single_min_major_axis < large.single_min_major_axis);
        assert!(small.cluster_min_area < standard.cluster_min_area);
        assert!(standard.cluster_min_area < large.cluster_min_area);
        assert!(small.cluster_min_major_axis < standard.cluster_min_major_axis);
        assert!(standard.cluster_min_major_axis < large.cluster_min_major_axis);
        assert_eq!(standard.single_min_area, SINGLE_PALM_MIN_AREA);
    }

    /// 验证小档可识别低于标准阈值的单接触，而标准档保持保守。
    #[test]
    fn small_preset_classifies_a_smaller_single_contact() {
        let shared = SharedPalmSizePreset::default();
        let mut tracker = WindowsPointerTracker::with_palm_size_preset(shared.clone());
        tracker.touches.insert(
            7,
            TouchContact::new(
                CanvasPoint::new(30.0, 30.0),
                ContactGeometry {
                    left: 0.0,
                    top: 0.0,
                    right: 60.0,
                    bottom: 60.0,
                    rotation_radians: 0.0,
                },
                true,
                Instant::now(),
                1,
                100,
            ),
        );

        tracker.refresh_candidates();
        assert!(!tracker.candidate_ids.contains(&7));

        shared.store(PalmSizePreset::Small);
        tracker.refresh_candidates();
        assert!(tracker.candidate_ids.contains(&7));
    }

    /// 验证小档可识别低于标准联合阈值的双触点簇。
    #[test]
    fn small_preset_classifies_a_smaller_contact_cluster() {
        let shared = SharedPalmSizePreset::default();
        let mut tracker = WindowsPointerTracker::with_palm_size_preset(shared.clone());
        for (pointer_id, left) in [(7, 0.0), (8, 50.0)] {
            tracker.touches.insert(
                pointer_id,
                TouchContact::new(
                    CanvasPoint::new(left + 15.0, 35.0),
                    ContactGeometry {
                        left,
                        top: 0.0,
                        right: left + 30.0,
                        bottom: 70.0,
                        rotation_radians: 0.0,
                    },
                    true,
                    Instant::now(),
                    1,
                    100,
                ),
            );
        }

        tracker.refresh_candidates();
        assert!(tracker.candidate_ids.is_empty());

        shared.store(PalmSizePreset::Small);
        tracker.refresh_candidates();
        assert_eq!(tracker.candidate_ids, HashSet::from([7, 8]));
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
