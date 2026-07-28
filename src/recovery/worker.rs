use std::{
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use crate::{app::AppState, error::AppError};

use super::format::RecoveryStore;

/// 后台保存线程返回给事件线程的非阻塞错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryEvent {
    Error(String),
}

/// worker 初始化完成后交给运行时的 manager、恢复状态和诊断。
pub struct RecoveryStartup {
    pub manager: RecoveryManager,
    pub recovered_state: Option<AppState>,
    pub diagnostic: Option<String>,
}

/// 退出 worker 时保留恢复文件或确认正常退出并清理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownMode {
    Preserve,
    Clean,
}

/// latest-state worker mailbox 的互斥状态。
#[derive(Default)]
struct WorkerState {
    latest: Option<(u64, AppState)>,
    submitted_generation: u64,
    processed_generation: u64,
    shutdown: Option<ShutdownMode>,
    stopped: bool,
    error: Option<String>,
}

/// 保存线程使用的单槽状态邮箱和 flush 确认条件变量。
#[derive(Default)]
struct WorkerMailbox {
    state: Mutex<WorkerState>,
    changed: Condvar,
}

impl WorkerMailbox {
    /// 用最新 AppState 覆盖未处理请求并立即唤醒后台线程。
    fn submit(&self, state: AppState) -> bool {
        let mut worker = self.state.lock().expect("恢复 worker 互斥量不应中毒");
        if worker.stopped || worker.shutdown.is_some() {
            return false;
        }
        worker.submitted_generation = worker.submitted_generation.wrapping_add(1);
        let generation = worker.submitted_generation;
        worker.latest = Some((generation, state));
        self.changed.notify_one();
        true
    }

    /// 等待一个最新状态或 shutdown 请求。
    fn wait_for_request(&self) -> (Option<(u64, AppState)>, Option<ShutdownMode>) {
        let mut worker = self.state.lock().expect("恢复 worker 互斥量不应中毒");
        while worker.latest.is_none() && worker.shutdown.is_none() {
            worker = self
                .changed
                .wait(worker)
                .expect("恢复 worker 互斥量不应中毒");
        }
        (worker.latest.take(), worker.shutdown)
    }

    /// 记录已落盘 generation 并唤醒 flush 等待者。
    fn mark_processed(&self, generation: u64) {
        let mut worker = self.state.lock().expect("恢复 worker 互斥量不应中毒");
        worker.processed_generation = worker.processed_generation.max(generation);
        self.changed.notify_all();
    }

    /// 记录不可继续的保存错误并停止接受新状态。
    fn mark_failed(&self, detail: String) {
        let mut worker = self.state.lock().expect("恢复 worker 互斥量不应中毒");
        worker.error = Some(detail);
        worker.stopped = true;
        self.changed.notify_all();
    }

    /// 标记 worker 已正常退出并唤醒全部等待者。
    fn mark_stopped(&self) {
        let mut worker = self.state.lock().expect("恢复 worker 互斥量不应中毒");
        worker.stopped = true;
        self.changed.notify_all();
    }

    /// 等待调用时已经提交的最新 generation 被处理。
    fn flush(&self) -> Result<(), String> {
        let mut worker = self.state.lock().expect("恢复 worker 互斥量不应中毒");
        let target = worker.submitted_generation;
        while worker.processed_generation < target && !worker.stopped {
            worker = self
                .changed
                .wait(worker)
                .expect("恢复 worker 互斥量不应中毒");
        }
        if let Some(error) = &worker.error {
            return Err(error.clone());
        }
        if worker.processed_generation < target {
            return Err("恢复 worker 在处理最新状态前退出".to_owned());
        }
        Ok(())
    }

    /// 请求 worker 在处理最后一个状态后按指定模式退出。
    fn request_shutdown(&self, mode: ShutdownMode) {
        let mut worker = self.state.lock().expect("恢复 worker 互斥量不应中毒");
        if !worker.stopped {
            worker.shutdown = Some(mode);
            self.changed.notify_one();
        }
    }

    /// 返回 worker 退出时保留的错误。
    fn error(&self) -> Option<String> {
        self.state
            .lock()
            .expect("恢复 worker 互斥量不应中毒")
            .error
            .clone()
    }
}

/// 事件线程持有的后台恢复 worker 句柄。
pub struct RecoveryManager {
    mailbox: Arc<WorkerMailbox>,
    events: mpsc::Receiver<RecoveryEvent>,
    join: Option<JoinHandle<()>>,
}

impl RecoveryManager {
    /// 启动 worker，并等待它在后台完成 recovery 文件读取与校验。
    pub fn start(
        directory: PathBuf,
        wake_event_loop: impl Fn() + Send + 'static,
    ) -> Result<RecoveryStartup, AppError> {
        let mailbox = Arc::new(WorkerMailbox::default());
        let worker_mailbox = Arc::clone(&mailbox);
        let (events_tx, events) = mpsc::channel();
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name("steady-ink-recovery".to_owned())
            .spawn(move || {
                let opened = RecoveryStore::open(directory);
                match opened {
                    Ok((store, loaded)) => {
                        if startup_tx
                            .send(Ok((loaded.state, loaded.diagnostic)))
                            .is_ok()
                        {
                            run_worker(store, worker_mailbox, events_tx, &wake_event_loop);
                        }
                    }
                    Err(error) => {
                        let _ = startup_tx.send(Err(error.clone()));
                        worker_mailbox.mark_failed(error);
                    }
                }
            })
            .map_err(|error| AppError::Settings(format!("无法启动墨迹恢复线程: {error}")))?;
        let (recovered_state, diagnostic) = startup_rx
            .recv()
            .map_err(|_| AppError::Settings("墨迹恢复线程在初始化完成前退出".to_owned()))?
            .map_err(AppError::Settings)?;
        Ok(RecoveryStartup {
            manager: Self {
                mailbox,
                events,
                join: Some(join),
            },
            recovered_state,
            diagnostic,
        })
    }

    /// 无阻塞提交一份 cheap-clone AppState；旧的未处理状态会被覆盖。
    pub fn submit(&self, state: AppState) -> bool {
        self.mailbox.submit(state)
    }

    /// 等待调用前已提交的最新状态完成持久化，供正常退出和验收使用。
    pub fn flush(&self) -> Result<(), AppError> {
        self.mailbox.flush().map_err(AppError::Settings)
    }

    /// 非阻塞读取一个后台保存错误。
    pub fn try_recv_event(&self) -> Option<RecoveryEvent> {
        self.events.try_recv().ok()
    }

    /// 正常退出：处理最新状态、同步并清理活动 recovery 文件。
    pub fn shutdown_clean(&mut self) -> Result<(), AppError> {
        self.shutdown(ShutdownMode::Clean)
    }

    /// 异常退出路径：处理最新状态但保留 recovery 文件。
    pub fn shutdown_preserve(&mut self) -> Result<(), AppError> {
        self.shutdown(ShutdownMode::Preserve)
    }

    /// 请求指定 shutdown 模式、join worker 并传播保存或 panic 错误。
    fn shutdown(&mut self, mode: ShutdownMode) -> Result<(), AppError> {
        let Some(join) = self.join.take() else {
            return self
                .mailbox
                .error()
                .map_or(Ok(()), |error| Err(AppError::Settings(error)));
        };
        self.mailbox.request_shutdown(mode);
        join.join()
            .map_err(|_| AppError::Settings("墨迹恢复线程异常终止".to_owned()))?;
        self.mailbox
            .error()
            .map_or(Ok(()), |error| Err(AppError::Settings(error)))
    }
}

impl Drop for RecoveryManager {
    /// 未经正常退出确认时保留 recovery 文件，防止 panic 路径误清理。
    fn drop(&mut self) {
        let _ = self.shutdown_preserve();
    }
}

/// 在后台持续合并 latest-state 请求并执行全部序列化和文件 I/O。
fn run_worker(
    mut store: RecoveryStore,
    mailbox: Arc<WorkerMailbox>,
    events: mpsc::Sender<RecoveryEvent>,
    wake_event_loop: &impl Fn(),
) {
    loop {
        let (request, shutdown) = mailbox.wait_for_request();
        if let Some((generation, state)) = request {
            if let Err(error) = store.persist(&state) {
                send_error(&events, error.clone(), wake_event_loop);
                mailbox.mark_failed(error);
                return;
            }
            mailbox.mark_processed(generation);
        }
        if let Some(mode) = shutdown {
            if mode == ShutdownMode::Clean
                && let Err(error) = store.clean_active_files()
            {
                send_error(&events, error.clone(), wake_event_loop);
                mailbox.mark_failed(error);
                return;
            }
            mailbox.mark_stopped();
            return;
        }
    }
}

/// 发送后台保存错误并唤醒等待型 winit 事件循环。
fn send_error(events: &mpsc::Sender<RecoveryEvent>, detail: String, wake_event_loop: &impl Fn()) {
    if events.send(RecoveryEvent::Error(detail)).is_ok() {
        wake_event_loop();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    use crate::{
        ink::{CanvasPoint, InkColor, PageKey, PenWidth},
        slideshow::{PresentationApplication, SlidePage, SlideShowKey, SlideShowSession},
    };

    use super::*;

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    /// 创建当前进程独占的临时恢复目录路径。
    fn test_directory(name: &str) -> PathBuf {
        let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "steady-ink-recovery-{name}-{}-{sequence}",
            std::process::id()
        ))
    }

    /// 启动不需要唤醒真实事件循环的测试 manager。
    fn start_test_manager(directory: PathBuf) -> RecoveryStartup {
        RecoveryManager::start(directory, || {}).expect("测试恢复 worker 应启动")
    }

    /// 向普通批注文档追加一条确定笔画。
    fn append_test_stroke(state: &mut AppState, offset: f32) {
        state
            .normal_document_mut()
            .append_draw_stroke(
                vec![
                    CanvasPoint::new(offset, offset),
                    CanvasPoint::new(offset + 8.0, offset + 8.0),
                ],
                InkColor::Red,
                PenWidth::Px4,
            )
            .expect("测试笔画应创建 operation");
    }

    /// 删除测试创建的精确临时目录。
    fn remove_test_directory(directory: &PathBuf) {
        if let Err(error) = fs::remove_dir_all(directory)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("清理测试目录 {} 失败: {error}", directory.display());
        }
    }

    /// 验证普通墨迹状态经后台保存和重新启动后完整恢复。
    #[test]
    fn background_worker_recovers_normal_document() {
        let directory = test_directory("normal");
        let startup = start_test_manager(directory.clone());
        let mut manager = startup.manager;
        let mut state = AppState::default();
        assert!(state.enter_normal_annotation());
        append_test_stroke(&mut state, 1.0);
        assert!(manager.submit(state.clone()));
        manager.flush().expect("状态应完成落盘");
        manager.shutdown_preserve().expect("测试应保留恢复文件");

        let recovered = start_test_manager(directory.clone());
        assert_eq!(recovered.recovered_state.as_ref(), Some(&state));
        let mut manager = recovered.manager;
        manager.shutdown_clean().expect("正常退出应清理文件");
        remove_test_directory(&directory);
    }

    /// 验证放映当前页与非活动页在增量重放后保持一致。
    #[test]
    fn recovery_round_trips_slideshow_pages() {
        let directory = test_directory("slideshow");
        let startup = start_test_manager(directory.clone());
        let mut manager = startup.manager;
        let key = SlideShowKey::new(PresentationApplication::PowerPoint, "deck", 42);
        let first = SlidePage::new(PageKey::new(1).expect("页键有效"), Some(101), Some(2));
        let second = SlidePage::new(PageKey::new(2).expect("页键有效"), Some(102), Some(2));
        let mut state = AppState::default();
        assert!(state.start_slideshow(SlideShowSession::new(key.clone(), first)));
        state
            .active_document_mut()
            .expect("放映应有活动文档")
            .append_draw_stroke(
                vec![CanvasPoint::new(1.0, 1.0)],
                InkColor::Blue,
                PenWidth::Px6,
            );
        assert!(state.change_slide(&key, second));
        state
            .active_document_mut()
            .expect("第二页应有活动文档")
            .append_draw_stroke(
                vec![CanvasPoint::new(2.0, 2.0)],
                InkColor::Green,
                PenWidth::Px8,
            );
        assert!(manager.submit(state.clone()));
        manager.flush().expect("放映状态应落盘");
        manager.shutdown_preserve().expect("测试应保留恢复文件");

        let recovered = start_test_manager(directory.clone());
        assert_eq!(recovered.recovered_state.as_ref(), Some(&state));
        let mut manager = recovered.manager;
        manager.shutdown_clean().expect("正常退出应清理文件");
        remove_test_directory(&directory);
    }

    /// 验证自动保存从提交到 durable flush 的延迟小于 500ms。
    #[test]
    fn autosave_latency_is_below_five_hundred_milliseconds() {
        let directory = test_directory("latency");
        let startup = start_test_manager(directory.clone());
        let mut manager = startup.manager;
        let mut state = AppState::default();
        assert!(state.enter_normal_annotation());
        append_test_stroke(&mut state, 1.0);

        let started = Instant::now();
        assert!(manager.submit(state));
        manager.flush().expect("自动保存应完成");
        assert!(started.elapsed() < Duration::from_millis(500));

        manager.shutdown_clean().expect("正常退出应清理文件");
        remove_test_directory(&directory);
    }

    /// 验证崩溃截断 journal 尾部不会影响前面全部有效记录。
    #[test]
    fn truncated_journal_tail_recovers_all_valid_records() {
        let directory = test_directory("truncated");
        let startup = start_test_manager(directory.clone());
        let mut manager = startup.manager;
        let mut state = AppState::default();
        assert!(state.enter_normal_annotation());
        append_test_stroke(&mut state, 1.0);
        assert!(manager.submit(state.clone()));
        manager.flush().expect("检查点应落盘");
        append_test_stroke(&mut state, 2.0);
        assert!(manager.submit(state.clone()));
        manager.flush().expect("增量应落盘");
        manager.shutdown_preserve().expect("测试应保留恢复文件");
        OpenOptions::new()
            .append(true)
            .open(directory.join("journal.sink"))
            .and_then(|mut file| file.write_all(&[8, 0, 0]))
            .expect("应追加模拟崩溃尾部");

        let recovered = start_test_manager(directory.clone());
        assert_eq!(recovered.recovered_state.as_ref(), Some(&state));
        assert!(
            recovered
                .diagnostic
                .as_deref()
                .is_some_and(|detail| detail.contains("不完整 journal 尾部"))
        );
        let mut manager = recovered.manager;
        manager.shutdown_clean().expect("正常退出应清理文件");
        remove_test_directory(&directory);
    }

    /// 验证完整但 CRC 损坏的 journal 被隔离且只恢复此前有效状态。
    #[test]
    fn corrupted_journal_is_quarantined_after_valid_prefix() {
        let directory = test_directory("corrupt-journal");
        let startup = start_test_manager(directory.clone());
        let mut manager = startup.manager;
        let mut checkpoint_state = AppState::default();
        assert!(checkpoint_state.enter_normal_annotation());
        append_test_stroke(&mut checkpoint_state, 1.0);
        assert!(manager.submit(checkpoint_state.clone()));
        manager.flush().expect("检查点应落盘");
        let mut latest_state = checkpoint_state.clone();
        append_test_stroke(&mut latest_state, 2.0);
        assert!(manager.submit(latest_state));
        manager.flush().expect("增量应落盘");
        manager.shutdown_preserve().expect("测试应保留恢复文件");

        let journal_path = directory.join("journal.sink");
        let mut journal = fs::read(&journal_path).expect("journal 应可读取");
        *journal.last_mut().expect("journal 应有完整记录") ^= 0x5a;
        fs::write(&journal_path, journal).expect("应写入模拟损坏");

        let recovered = start_test_manager(directory.clone());
        assert_eq!(recovered.recovered_state.as_ref(), Some(&checkpoint_state));
        assert!(
            recovered
                .diagnostic
                .as_deref()
                .is_some_and(|detail| detail.contains("journal 损坏"))
        );
        assert!(
            fs::read_dir(&directory)
                .expect("恢复目录应可枚举")
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains("corrupt-"))
        );
        let mut manager = recovered.manager;
        manager.shutdown_clean().expect("正常退出应清理活动文件");
        remove_test_directory(&directory);
    }

    /// 验证未知检查点版本不会进入 AppState，并被隔离供诊断。
    #[test]
    fn unknown_checkpoint_version_is_quarantined() {
        let directory = test_directory("unknown-version");
        let startup = start_test_manager(directory.clone());
        let mut manager = startup.manager;
        let mut state = AppState::default();
        assert!(state.enter_normal_annotation());
        append_test_stroke(&mut state, 1.0);
        assert!(manager.submit(state));
        manager.flush().expect("检查点应落盘");
        manager.shutdown_preserve().expect("测试应保留恢复文件");

        let checkpoint_path = directory.join("checkpoint.sink");
        let mut checkpoint = fs::read(&checkpoint_path).expect("检查点应可读取");
        checkpoint[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        fs::write(&checkpoint_path, checkpoint).expect("应写入未知版本");

        let recovered = start_test_manager(directory.clone());
        assert!(recovered.recovered_state.is_none());
        assert!(
            recovered
                .diagnostic
                .as_deref()
                .is_some_and(|detail| detail.contains("版本"))
        );
        let mut manager = recovered.manager;
        manager.shutdown_clean().expect("正常退出应清理活动文件");
        remove_test_directory(&directory);
    }

    /// 验证正常退出清理活动 recovery 文件而不删除目录本身。
    #[test]
    fn clean_shutdown_removes_active_recovery_files() {
        let directory = test_directory("clean");
        let startup = start_test_manager(directory.clone());
        let mut manager = startup.manager;
        let mut state = AppState::default();
        assert!(state.enter_normal_annotation());
        append_test_stroke(&mut state, 1.0);
        assert!(manager.submit(state));
        manager.shutdown_clean().expect("正常退出应清理文件");

        assert!(!directory.join("checkpoint.sink").exists());
        assert!(!directory.join("journal.sink").exists());
        remove_test_directory(&directory);
    }

    /// 验证连续增量的压缩落盘体积低于未压缩完整状态序列的两倍。
    #[test]
    fn incremental_storage_stays_below_double_full_snapshot_sequence() {
        let directory = test_directory("size");
        let startup = start_test_manager(directory.clone());
        let mut manager = startup.manager;
        let mut state = AppState::default();
        assert!(state.enter_normal_annotation());
        let mut full_snapshot_bytes = 0_u64;
        for index in 0..40 {
            append_test_stroke(&mut state, index as f32);
            full_snapshot_bytes += super::super::format::encode_state(&state)
                .expect("状态应序列化")
                .len() as u64;
            assert!(manager.submit(state.clone()));
            manager.flush().expect("每个增量应完成落盘");
        }
        manager.shutdown_preserve().expect("测试应保留恢复文件");
        let file_bytes = ["checkpoint.sink", "journal.sink"]
            .iter()
            .filter_map(|name| fs::metadata(directory.join(name)).ok())
            .map(|metadata| metadata.len())
            .sum::<u64>();
        assert!(file_bytes < full_snapshot_bytes * 2);

        let mut recovered = start_test_manager(directory.clone()).manager;
        recovered.shutdown_clean().expect("正常退出应清理文件");
        remove_test_directory(&directory);
    }
}
