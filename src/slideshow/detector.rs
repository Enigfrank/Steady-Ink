use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{RPC_E_CALL_REJECTED, RPC_E_SERVERCALL_RETRYLATER},
    System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    },
    UI::WindowsAndMessaging::{DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage},
};

use super::{
    PresentationApplication, SlidePage, SlideShowControlAction, SlideShowControlBackend,
    SlideShowKey,
    late_bound::{
        ActiveObjectError, ActiveSlideShowSnapshot, ComCandidate, application_is_visible,
        connect_active_object, control_active_slideshow, query_active_slideshow, same_com_identity,
        subscribe_application_events,
    },
    simulated_keys::send_simulated_control_key,
};

const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
const MESSAGE_PUMP_INTERVAL: Duration = Duration::from_millis(16);
const COM_QUERY_RETRY_DELAY: Duration = Duration::from_millis(50);
const COM_QUERY_MAX_ATTEMPTS: usize = 5;
const EMPTY_SNAPSHOT_CONFIRMATION_DELAY: Duration = Duration::from_millis(40);
const EMPTY_SNAPSHOT_CONFIRMATION_ATTEMPTS: usize = 3;

type WakeCallback = Arc<dyn Fn() + Send + Sync>;

const COM_CANDIDATES: [ComCandidate; 3] = [
    ComCandidate {
        application: PresentationApplication::PowerPoint,
        prog_id: "PowerPoint.Application",
    },
    ComCandidate {
        application: PresentationApplication::Wps,
        prog_id: "KWPP.Application",
    },
    ComCandidate {
        application: PresentationApplication::Wps,
        prog_id: "Ket.Application",
    },
];

/// 单个 ProgID 的 COM 可用性诊断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComCandidateDiagnostic {
    pub application: PresentationApplication,
    pub prog_id: String,
    pub status: ComCandidateStatus,
    pub detail: Option<String>,
}

/// COM 候选当前可用状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComCandidateStatus {
    Connected,
    InstalledNotRunning,
    ClassNotRegistered,
    ConnectionFailed,
    EventSubscriptionFailed,
}

/// PowerPoint/WPS 所有候选的诊断快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComDiagnostics {
    pub candidates: Vec<ComCandidateDiagnostic>,
}

/// detector 向应用核心发送的统一放映事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComDetectorEvent {
    Diagnostics(ComDiagnostics),
    SlideShowStarted {
        key: SlideShowKey,
        page: SlidePage,
    },
    SlideChanged {
        key: SlideShowKey,
        page: SlidePage,
    },
    SlideShowEnded {
        key: SlideShowKey,
    },
    ConnectionLost {
        key: Option<SlideShowKey>,
        detail: String,
    },
    ControlSucceeded {
        action: SlideShowControlAction,
        backend: SlideShowControlBackend,
    },
    ControlFailed {
        action: SlideShowControlAction,
        detail: String,
    },
}

/// UI 线程发送到 detector STA 的控制命令。
#[derive(Debug)]
enum DetectorCommand {
    Control {
        expected_key: SlideShowKey,
        action: SlideShowControlAction,
    },
    Resync,
}

/// 把 detector 事件送往运行时，并立即唤醒等待中的 winit 事件循环。
#[derive(Clone)]
struct DetectorEventEmitter {
    sender: mpsc::Sender<ComDetectorEvent>,
    wake: WakeCallback,
}

impl DetectorEventEmitter {
    /// 发送一个事件；运行时仍存在时同步触发一次轻量唤醒。
    fn emit(&self, event: ComDetectorEvent) {
        if self.sender.send(event).is_ok() {
            (self.wake)();
        }
    }
}

/// 独立 STA 线程中的 COM detector 句柄。
pub struct ComDetector {
    receiver: Receiver<ComDetectorEvent>,
    command_sender: mpsc::Sender<DetectorCommand>,
    stop_requested: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ComDetector {
    /// 启动 COM STA 线程；失败只会形成诊断事件，不阻塞 UI 渲染线程。
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (event_sender, receiver) = mpsc::channel();
        let emitter = DetectorEventEmitter {
            sender: event_sender,
            wake: Arc::new(wake),
        };
        let (command_sender, command_receiver) = mpsc::channel();
        let stop_requested = Arc::new(AtomicBool::new(false));
        let detector_stop = Arc::clone(&stop_requested);
        let thread = thread::Builder::new()
            .name("steady-ink-com-detector".to_owned())
            .spawn(move || detector_thread(detector_stop, emitter, command_receiver))
            .ok();
        Self {
            receiver,
            command_sender,
            stop_requested,
            thread,
        }
    }

    /// 非阻塞读取一个 detector 事件。
    pub fn try_recv(&self) -> Result<ComDetectorEvent, TryRecvError> {
        self.receiver.try_recv()
    }

    /// 把带会话标识的控制请求发送到 COM STA；发送失败表示 detector 已退出。
    pub fn request_control(
        &self,
        expected_key: SlideShowKey,
        action: SlideShowControlAction,
    ) -> bool {
        self.command_sender
            .send(DetectorCommand::Control {
                expected_key,
                action,
            })
            .is_ok()
    }

    /// 请求 STA 立即查询活动放映，用于用户重新启用联动后的状态补获。
    pub fn request_resync(&self) -> bool {
        self.command_sender.send(DetectorCommand::Resync).is_ok()
    }
}

impl Drop for ComDetector {
    /// 请求 detector 退出并等待 STA 线程清理 connection point。
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 初始化 COM STA，循环连接活动演示应用并运行 connection point 消息泵。
fn detector_thread(
    stop_requested: Arc<AtomicBool>,
    emitter: DetectorEventEmitter,
    command_receiver: Receiver<DetectorCommand>,
) {
    // SAFETY: 该线程专用于 COM STA，初始化和反初始化严格成对。
    let initialize_result =
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok() };
    if let Err(error) = initialize_result {
        emitter.emit(ComDetectorEvent::Diagnostics(ComDiagnostics {
            candidates: vec![ComCandidateDiagnostic {
                application: PresentationApplication::PowerPoint,
                prog_id: "COM STA".to_owned(),
                status: ComCandidateStatus::ConnectionFailed,
                detail: Some(error.to_string()),
            }],
        }));
        return;
    }

    let mut last_diagnostics = None;
    let mut last_confirmed_snapshot = None;
    while !stop_requested.load(Ordering::Acquire) {
        match connect_first_available() {
            ConnectedCandidate::Connected {
                candidate,
                application,
                diagnostics,
            } => {
                send_diagnostics_if_changed(&emitter, &mut last_diagnostics, diagnostics);
                run_connected_session(
                    &stop_requested,
                    &emitter,
                    &command_receiver,
                    candidate,
                    application,
                    &mut last_confirmed_snapshot,
                );
            }
            ConnectedCandidate::Unavailable(diagnostics) => {
                send_diagnostics_if_changed(&emitter, &mut last_diagnostics, diagnostics);
                reject_pending_controls(
                    &command_receiver,
                    &emitter,
                    "COM 放映检测当前不可用，按键兜底保持禁用",
                );
                thread::sleep(RECONNECT_INTERVAL);
            }
        }
    }

    // SAFETY: 与本线程成功的 CoInitializeEx 成对调用。
    unsafe { CoUninitialize() };
}

/// 尝试所有已知 ProgID，并返回首个活动对象和完整候选诊断。
fn connect_first_available() -> ConnectedCandidate {
    let mut diagnostics = Vec::with_capacity(COM_CANDIDATES.len());
    for candidate in COM_CANDIDATES {
        match connect_active_object(candidate.prog_id) {
            Ok(application) => {
                match application_is_visible(&application) {
                    Ok(true) => {}
                    Ok(false) => {
                        diagnostics.push(candidate_diagnostic(
                            candidate,
                            ComCandidateStatus::InstalledNotRunning,
                            None,
                        ));
                        continue;
                    }
                    Err(error) => {
                        diagnostics.push(candidate_diagnostic(
                            candidate,
                            ComCandidateStatus::ConnectionFailed,
                            Some(format!("无法读取 Application.Visible: {error}")),
                        ));
                        continue;
                    }
                }
                diagnostics.push(ComCandidateDiagnostic {
                    application: candidate.application,
                    prog_id: candidate.prog_id.to_owned(),
                    status: ComCandidateStatus::Connected,
                    detail: None,
                });
                return ConnectedCandidate::Connected {
                    candidate,
                    application,
                    diagnostics: ComDiagnostics {
                        candidates: diagnostics,
                    },
                };
            }
            Err(ActiveObjectError::ClassNotRegistered) => diagnostics.push(candidate_diagnostic(
                candidate,
                ComCandidateStatus::ClassNotRegistered,
                None,
            )),
            Err(ActiveObjectError::NotRunning) => diagnostics.push(candidate_diagnostic(
                candidate,
                ComCandidateStatus::InstalledNotRunning,
                None,
            )),
            Err(ActiveObjectError::Other(detail)) => diagnostics.push(candidate_diagnostic(
                candidate,
                ComCandidateStatus::ConnectionFailed,
                Some(detail),
            )),
        }
    }
    ConnectedCandidate::Unavailable(ComDiagnostics {
        candidates: diagnostics,
    })
}

/// 在已连接对象上订阅事件，并通过事件后的状态差分生成统一语义。
fn run_connected_session(
    stop_requested: &AtomicBool,
    emitter: &DetectorEventEmitter,
    command_receiver: &Receiver<DetectorCommand>,
    candidate: ComCandidate,
    application: windows::Win32::System::Com::IDispatch,
    last_confirmed_snapshot: &mut Option<ActiveSlideShowSnapshot>,
) {
    let (raw_event_sender, raw_event_receiver) = mpsc::channel();
    let subscription = match subscribe_application_events(&application, raw_event_sender) {
        Ok(subscription) => subscription,
        Err(error) => {
            emitter.emit(ComDetectorEvent::Diagnostics(ComDiagnostics {
                candidates: vec![candidate_diagnostic(
                    candidate,
                    ComCandidateStatus::EventSubscriptionFailed,
                    Some(error.to_string()),
                )],
            }));
            thread::sleep(RECONNECT_INTERVAL);
            return;
        }
    };

    let current_snapshot = match query_active_slideshow_with_presence_confirmation(
        &application,
        candidate.application,
        last_confirmed_snapshot.as_ref(),
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            emitter.emit(ComDetectorEvent::ConnectionLost {
                key: last_confirmed_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.key.clone()),
                detail: error.to_string(),
            });
            return;
        }
    };
    emit_reconnected_snapshot(
        emitter,
        last_confirmed_snapshot.as_ref(),
        current_snapshot.as_ref(),
    );
    *last_confirmed_snapshot = current_snapshot;
    let mut next_health_check_at = Instant::now() + RECONNECT_INTERVAL;

    while !stop_requested.load(Ordering::Acquire) {
        pump_sta_messages();
        if !handle_pending_controls(
            command_receiver,
            emitter,
            candidate,
            &application,
            last_confirmed_snapshot,
        ) {
            break;
        }
        match raw_event_receiver.recv_timeout(MESSAGE_PUMP_INTERVAL) {
            Ok(dispid) => {
                tracing::debug!(
                    dispid,
                    candidate = candidate.prog_id,
                    "收到演示应用 COM 事件"
                );
                if !refresh_connected_snapshot(
                    emitter,
                    candidate,
                    &application,
                    last_confirmed_snapshot,
                ) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if Instant::now() >= next_health_check_at {
            if let Err(detail) = verify_active_object_identity(candidate, &application) {
                emitter.emit(ComDetectorEvent::ConnectionLost {
                    key: last_confirmed_snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.key.clone()),
                    detail,
                });
                break;
            }
            if !refresh_connected_snapshot(
                emitter,
                candidate,
                &application,
                last_confirmed_snapshot,
            ) {
                break;
            }
            next_health_check_at = Instant::now() + RECONNECT_INTERVAL;
        }
    }
    drop(subscription);
}

/// 验证 ROT 中当前活动对象仍与已订阅的 COM 对象具有相同身份。
fn verify_active_object_identity(
    candidate: ComCandidate,
    application: &windows::Win32::System::Com::IDispatch,
) -> Result<(), String> {
    let active_application =
        connect_active_object(candidate.prog_id).map_err(|error| match error {
            ActiveObjectError::ClassNotRegistered => {
                format!("{} COM 类已不可用", candidate.prog_id)
            }
            ActiveObjectError::NotRunning => format!("{} 活动 COM 对象已退出", candidate.prog_id),
            ActiveObjectError::Other(detail) => {
                format!("{} 活动 COM 对象查询失败: {detail}", candidate.prog_id)
            }
        })?;
    let is_same = same_com_identity(application, &active_application)
        .map_err(|error| format!("{} COM 对象身份比较失败: {error}", candidate.prog_id))?;
    if is_same {
        let visible = application_is_visible(&active_application)
            .map_err(|error| format!("{} 可见状态查询失败: {error}", candidate.prog_id))?;
        if visible {
            Ok(())
        } else {
            Err(format!("{} 应用窗口已经关闭", candidate.prog_id))
        }
    } else {
        Err(format!("{} 活动 COM 对象已经更换", candidate.prog_id))
    }
}

/// 查询已连接应用的可靠放映快照，发出差分事件并保留会话连续性。
fn refresh_connected_snapshot(
    emitter: &DetectorEventEmitter,
    candidate: ComCandidate,
    application: &windows::Win32::System::Com::IDispatch,
    previous_snapshot: &mut Option<ActiveSlideShowSnapshot>,
) -> bool {
    match query_active_slideshow_with_presence_confirmation(
        application,
        candidate.application,
        previous_snapshot.as_ref(),
    ) {
        Ok(current_snapshot) => {
            emit_snapshot_difference(
                emitter,
                previous_snapshot.as_ref(),
                current_snapshot.as_ref(),
            );
            *previous_snapshot = current_snapshot;
            true
        }
        Err(error) => {
            emitter.emit(ComDetectorEvent::ConnectionLost {
                key: previous_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.key.clone()),
                detail: error.to_string(),
            });
            false
        }
    }
}

/// 在 STA 线程中处理全部待执行控制，并在 COM 断线时请求退出连接循环。
fn handle_pending_controls(
    command_receiver: &Receiver<DetectorCommand>,
    emitter: &DetectorEventEmitter,
    candidate: ComCandidate,
    application: &windows::Win32::System::Com::IDispatch,
    previous_snapshot: &mut Option<ActiveSlideShowSnapshot>,
) -> bool {
    while let Ok(command) = command_receiver.try_recv() {
        let DetectorCommand::Control {
            expected_key,
            action,
        } = command
        else {
            let current_snapshot = match query_active_slideshow_with_presence_confirmation(
                application,
                candidate.application,
                previous_snapshot.as_ref(),
            ) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    emitter.emit(ComDetectorEvent::ConnectionLost {
                        key: previous_snapshot
                            .as_ref()
                            .map(|snapshot| snapshot.key.clone()),
                        detail: error.to_string(),
                    });
                    return false;
                }
            };
            if let Some(snapshot) = current_snapshot.as_ref() {
                emitter.emit(ComDetectorEvent::SlideShowStarted {
                    key: snapshot.key.clone(),
                    page: snapshot.page,
                });
            } else {
                emit_snapshot_difference(
                    emitter,
                    previous_snapshot.as_ref(),
                    current_snapshot.as_ref(),
                );
            }
            *previous_snapshot = current_snapshot;
            continue;
        };
        let current_snapshot =
            match query_active_slideshow_with_retry(application, candidate.application) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    emitter.emit(ComDetectorEvent::ConnectionLost {
                        key: previous_snapshot
                            .as_ref()
                            .map(|snapshot| snapshot.key.clone()),
                        detail: error.to_string(),
                    });
                    emitter.emit(ComDetectorEvent::ControlFailed {
                        action,
                        detail: "COM 状态复核失败，未执行按键兜底".to_owned(),
                    });
                    return false;
                }
            };

        if !update_snapshot_for_control(emitter, previous_snapshot, current_snapshot) {
            tracing::warn!(
                ?action,
                application = ?expected_key.application,
                window_id = expected_key.window_id,
                "控制前置复核得到瞬时空快照，已保留最后确认的放映会话"
            );
            emitter.emit(ComDetectorEvent::ControlFailed {
                action,
                detail: "COM 控制前置复核暂未找到活动放映，已保留当前批注会话".to_owned(),
            });
            continue;
        }

        let snapshot = previous_snapshot
            .as_ref()
            .expect("刚写入的放映快照必须存在");
        if snapshot.key != expected_key {
            emitter.emit(ComDetectorEvent::ControlFailed {
                action,
                detail: "控制请求对应的放映会话已经变化".to_owned(),
            });
            continue;
        }

        match control_active_slideshow(application, action) {
            Ok(()) => emitter.emit(ComDetectorEvent::ControlSucceeded {
                action,
                backend: SlideShowControlBackend::Com,
            }),
            Err(com_error) => match send_simulated_control_key(&snapshot.key, action) {
                Ok(()) => emitter.emit(ComDetectorEvent::ControlSucceeded {
                    action,
                    backend: SlideShowControlBackend::SimulatedKey,
                }),
                Err(simulated_error) => emitter.emit(ComDetectorEvent::ControlFailed {
                    action,
                    detail: format!("COM 控制失败: {com_error}; 按键兜底失败: {simulated_error}"),
                }),
            },
        }
    }
    true
}

/// 对 PowerPoint 忙碌期间的瞬时 RPC 拒绝执行有限重试，其他 COM 错误立即返回。
fn query_active_slideshow_with_retry(
    application: &windows::Win32::System::Com::IDispatch,
    application_kind: PresentationApplication,
) -> windows::core::Result<Option<ActiveSlideShowSnapshot>> {
    let mut attempt = 1;
    loop {
        match query_active_slideshow(application, application_kind) {
            Err(error)
                if is_transient_com_rejection(&error) && attempt < COM_QUERY_MAX_ATTEMPTS =>
            {
                tracing::debug!(
                    attempt,
                    max_attempts = COM_QUERY_MAX_ATTEMPTS,
                    hresult = ?error.code(),
                    "演示应用暂时拒绝 COM 状态查询，稍后重试"
                );
                pump_sta_messages();
                thread::sleep(COM_QUERY_RETRY_DELAY);
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// 在已有可靠会话时对首次空快照做有限存在性复核，避免瞬时 COM 空值结束会话。
fn query_active_slideshow_with_presence_confirmation(
    application: &windows::Win32::System::Com::IDispatch,
    application_kind: PresentationApplication,
    previous: Option<&ActiveSlideShowSnapshot>,
) -> windows::core::Result<Option<ActiveSlideShowSnapshot>> {
    let initial = query_active_slideshow_with_retry(application, application_kind)?;
    let confirmation = confirm_snapshot_after_initial(previous, initial, || {
        pump_sta_messages();
        thread::sleep(EMPTY_SNAPSHOT_CONFIRMATION_DELAY);
        query_active_slideshow_with_retry(application, application_kind)
    })?;
    if confirmation.empty_observations > 0 {
        if let Some(snapshot) = confirmation.snapshot.as_ref() {
            tracing::info!(
                empty_observations = confirmation.empty_observations,
                application = ?snapshot.key.application,
                window_id = snapshot.key.window_id,
                page = snapshot.page.key.show_position(),
                "瞬时空放映快照已恢复，抑制会话结束"
            );
        } else if let Some(previous) = previous {
            tracing::info!(
                empty_observations = confirmation.empty_observations,
                application = ?previous.key.application,
                window_id = previous.key.window_id,
                "连续空放映快照已确认，会话可以结束"
            );
        }
    }
    Ok(confirmation.snapshot)
}

/// 空快照存在性复核的纯状态结果，供 COM 路径和确定性测试共用。
#[derive(Debug, PartialEq, Eq)]
struct SnapshotPresenceConfirmation {
    snapshot: Option<ActiveSlideShowSnapshot>,
    empty_observations: usize,
}

/// 根据首次查询和后续有限序列确认空快照是否代表真实结束。
fn confirm_snapshot_after_initial<E>(
    previous: Option<&ActiveSlideShowSnapshot>,
    initial: Option<ActiveSlideShowSnapshot>,
    mut query_again: impl FnMut() -> Result<Option<ActiveSlideShowSnapshot>, E>,
) -> Result<SnapshotPresenceConfirmation, E> {
    if previous.is_none() || initial.is_some() {
        return Ok(SnapshotPresenceConfirmation {
            snapshot: initial,
            empty_observations: 0,
        });
    }

    let mut empty_observations = 1;
    while empty_observations < EMPTY_SNAPSHOT_CONFIRMATION_ATTEMPTS {
        match query_again()? {
            Some(snapshot) => {
                return Ok(SnapshotPresenceConfirmation {
                    snapshot: Some(snapshot),
                    empty_observations,
                });
            }
            None => empty_observations += 1,
        }
    }
    Ok(SnapshotPresenceConfirmation {
        snapshot: None,
        empty_observations,
    })
}

/// 返回一个 COM 错误是否属于 Office 忙碌时可安全短重试的 RPC 状态。
fn is_transient_com_rejection(error: &windows::core::Error) -> bool {
    matches!(
        error.code(),
        RPC_E_CALL_REJECTED | RPC_E_SERVERCALL_RETRYLATER
    )
}

/// COM 尚未建立可靠检测时拒绝积压控制，确保按键模拟不能单独工作。
fn reject_pending_controls(
    command_receiver: &Receiver<DetectorCommand>,
    emitter: &DetectorEventEmitter,
    detail: &str,
) {
    while let Ok(command) = command_receiver.try_recv() {
        if let DetectorCommand::Control { action, .. } = command {
            emitter.emit(ComDetectorEvent::ControlFailed {
                action,
                detail: detail.to_owned(),
            });
        }
    }
}

/// 控制前置复核仅在得到可靠快照时更新会话；空值必须保留最后确认状态。
fn update_snapshot_for_control(
    emitter: &DetectorEventEmitter,
    previous: &mut Option<ActiveSlideShowSnapshot>,
    current: Option<ActiveSlideShowSnapshot>,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    emit_snapshot_difference(emitter, previous.as_ref(), Some(&current));
    *previous = Some(current);
    true
}

/// 比较两个可靠 COM 快照并发出开始、翻页或结束事件。
fn emit_snapshot_difference(
    emitter: &DetectorEventEmitter,
    previous: Option<&ActiveSlideShowSnapshot>,
    current: Option<&ActiveSlideShowSnapshot>,
) {
    match (previous, current) {
        (None, Some(current)) => emitter.emit(ComDetectorEvent::SlideShowStarted {
            key: current.key.clone(),
            page: current.page,
        }),
        (Some(previous), None) => emitter.emit(ComDetectorEvent::SlideShowEnded {
            key: previous.key.clone(),
        }),
        (Some(previous), Some(current)) if previous.key != current.key => {
            emitter.emit(ComDetectorEvent::SlideShowEnded {
                key: previous.key.clone(),
            });
            emitter.emit(ComDetectorEvent::SlideShowStarted {
                key: current.key.clone(),
                page: current.page,
            });
        }
        (Some(previous), Some(current)) if previous.page != current.page => {
            emitter.emit(ComDetectorEvent::SlideChanged {
                key: current.key.clone(),
                page: current.page,
            });
        }
        _ => {}
    }
}

/// 在 connection point 重建后补发恢复、切换或结束事件，并保留会话连续性。
fn emit_reconnected_snapshot(
    emitter: &DetectorEventEmitter,
    previous: Option<&ActiveSlideShowSnapshot>,
    current: Option<&ActiveSlideShowSnapshot>,
) {
    match (previous, current) {
        (Some(previous), None) => emitter.emit(ComDetectorEvent::SlideShowEnded {
            key: previous.key.clone(),
        }),
        (Some(previous), Some(current)) if previous.key != current.key => {
            emitter.emit(ComDetectorEvent::SlideShowEnded {
                key: previous.key.clone(),
            });
            emitter.emit(ComDetectorEvent::SlideShowStarted {
                key: current.key.clone(),
                page: current.page,
            });
        }
        (_, Some(current)) => emitter.emit(ComDetectorEvent::SlideShowStarted {
            key: current.key.clone(),
            page: current.page,
        }),
        (None, None) => {}
    }
}

/// 处理 STA 消息队列，使 COM connection point 回调能够被调度。
fn pump_sta_messages() {
    let mut message = MSG::default();
    loop {
        // SAFETY: MSG 输出指针有效，当前线程只处理自己的消息队列。
        let has_message = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool();
        if !has_message {
            break;
        }
        // SAFETY: message 由 PeekMessageW 成功填充。
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// 为一个候选构造稳定的诊断记录。
fn candidate_diagnostic(
    candidate: ComCandidate,
    status: ComCandidateStatus,
    detail: Option<String>,
) -> ComCandidateDiagnostic {
    ComCandidateDiagnostic {
        application: candidate.application,
        prog_id: candidate.prog_id.to_owned(),
        status,
        detail,
    }
}

/// 仅在诊断内容变化时通知 UI，避免未运行 Office 时每秒刷日志。
fn send_diagnostics_if_changed(
    emitter: &DetectorEventEmitter,
    previous: &mut Option<ComDiagnostics>,
    current: ComDiagnostics,
) {
    if previous.as_ref() != Some(&current) {
        emitter.emit(ComDetectorEvent::Diagnostics(current.clone()));
        *previous = Some(current);
    }
}

enum ConnectedCandidate {
    Connected {
        candidate: ComCandidate,
        application: windows::Win32::System::Com::IDispatch,
        diagnostics: ComDiagnostics,
    },
    Unavailable(ComDiagnostics),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::PageKey;

    /// 创建纯快照序列测试使用的稳定放映快照。
    fn snapshot(window_id: i64, page: u32) -> ActiveSlideShowSnapshot {
        ActiveSlideShowSnapshot {
            key: SlideShowKey::new(PresentationApplication::PowerPoint, "deck", window_id),
            page: SlidePage::new(
                PageKey::new(page).expect("测试页键有效"),
                Some(i64::from(page)),
                Some(3),
            ),
        }
    }

    /// 创建可收集 detector 事件且不触发真实唤醒的测试 emitter。
    fn emitter_fixture() -> (DetectorEventEmitter, Receiver<ComDetectorEvent>) {
        let (sender, receiver) = mpsc::channel();
        (
            DetectorEventEmitter {
                sender,
                wake: Arc::new(|| {}),
            },
            receiver,
        )
    }

    /// 验证活动会话首次空快照后恢复同一快照时抑制结束。
    #[test]
    fn single_empty_then_recovery_preserves_session() {
        let previous = snapshot(1, 1);
        let recovered = previous.clone();
        let mut retries = vec![Ok::<_, ()>(Some(recovered.clone()))].into_iter();

        let confirmation = confirm_snapshot_after_initial(Some(&previous), None, || {
            retries.next().expect("只需一次复核")
        })
        .expect("测试复核不应失败");

        assert_eq!(confirmation.snapshot, Some(recovered));
        assert_eq!(confirmation.empty_observations, 1);
        let (emitter, receiver) = emitter_fixture();
        emit_snapshot_difference(&emitter, Some(&previous), confirmation.snapshot.as_ref());
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    /// 验证达到固定空快照次数后才确认结束活动会话。
    #[test]
    fn consecutive_empty_snapshots_confirm_end() {
        let previous = snapshot(1, 1);
        let mut retry_count = 0;

        let confirmation = confirm_snapshot_after_initial(Some(&previous), None, || {
            retry_count += 1;
            Ok::<_, ()>(None)
        })
        .expect("测试复核不应失败");

        assert_eq!(confirmation.snapshot, None);
        assert_eq!(
            confirmation.empty_observations,
            EMPTY_SNAPSHOT_CONFIRMATION_ATTEMPTS
        );
        assert_eq!(retry_count, EMPTY_SNAPSHOT_CONFIRMATION_ATTEMPTS - 1);
    }

    /// 验证控制前置复核为空时保留最后确认快照且不产生结束事件。
    #[test]
    fn empty_control_preflight_keeps_last_confirmed_snapshot() {
        let confirmed = snapshot(1, 1);
        let mut previous = Some(confirmed.clone());
        let (emitter, receiver) = emitter_fixture();

        assert!(!update_snapshot_for_control(&emitter, &mut previous, None));
        assert_eq!(previous, Some(confirmed));
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    /// 验证空快照后出现另一会话时生成旧会话结束和新会话开始事件。
    #[test]
    fn empty_then_different_key_switches_sessions() {
        let previous = snapshot(1, 1);
        let current = snapshot(2, 1);
        let confirmation = confirm_snapshot_after_initial(Some(&previous), None, || {
            Ok::<_, ()>(Some(current.clone()))
        })
        .expect("测试复核不应失败");
        let (emitter, receiver) = emitter_fixture();

        emit_snapshot_difference(&emitter, Some(&previous), confirmation.snapshot.as_ref());

        assert!(matches!(
            receiver.recv().expect("应结束旧会话"),
            ComDetectorEvent::SlideShowEnded { key } if key == previous.key
        ));
        assert!(matches!(
            receiver.recv().expect("应开始新会话"),
            ComDetectorEvent::SlideShowStarted { key, .. } if key == current.key
        ));
    }
}
