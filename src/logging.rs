use std::{
    backtrace::Backtrace,
    fs::{self, OpenOptions},
    io::{self, Write},
    panic,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
use tracing_subscriber::{
    EnvFilter, fmt::MakeWriter, layer::SubscriberExt, util::SubscriberInitExt,
};

use crate::settings::SettingsStore;

const LOG_FILE_STEM: &str = "steady-ink";
const LOG_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// 初始化标准错误与本地日期命名的文件日志，并安装 panic 堆栈捕获。
pub(crate) fn initialize() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    match prepare_file_logging() {
        Ok(prepared) => {
            let stderr_layer = tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_timer(tracing_subscriber::fmt::time::OffsetTime::new(
                    local_offset,
                    Rfc3339,
                ))
                .with_writer(std::io::stderr);
            let file_layer = tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_timer(tracing_subscriber::fmt::time::OffsetTime::new(
                    local_offset,
                    Rfc3339,
                ))
                .with_writer(prepared.writer);
            if let Err(error) = tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .with(file_layer)
                .try_init()
            {
                eprintln!("Steady Ink 日志订阅器初始化失败: {error}");
            } else {
                for warning in prepared.cleanup_warnings {
                    tracing::warn!(warning = %warning, "日志清理失败");
                }
            }
        }
        Err(error) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_timer(tracing_subscriber::fmt::time::OffsetTime::new(
                    local_offset,
                    Rfc3339,
                ))
                .with_writer(std::io::stderr)
                .try_init();
            eprintln!("Steady Ink 文件日志初始化失败: {error}");
        }
    }
    install_panic_hook();
}

struct PreparedFileLogging {
    writer: DailyLogWriter,
    cleanup_warnings: Vec<String>,
}

/// 以本地日期命名、按次打开追加的文件 writer，保证日志无需等待后台缓冲刷新。
#[derive(Clone)]
struct DailyLogWriter {
    directory: PathBuf,
}

impl DailyLogWriter {
    /// 从已创建的日志目录构造按日 writer。
    const fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// 返回当前本地日期对应的稳定日志文件路径。
    fn file_path(&self) -> PathBuf {
        self.directory.join(current_log_file_name())
    }
}

impl<'a> MakeWriter<'a> for DailyLogWriter {
    type Writer = Box<dyn Write + Send>;

    /// 为每条 tracing 事件打开当天文件并追加，失败时退回无副作用 writer。
    fn make_writer(&'a self) -> Self::Writer {
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.file_path())
        {
            Ok(file) => Box::new(file),
            Err(error) => {
                eprintln!("Steady Ink 写入日志文件失败: {error}");
                Box::new(io::sink())
            }
        }
    }
}

/// 创建配置目录、清理旧文件并验证当天日志文件可以打开。
fn prepare_file_logging() -> Result<PreparedFileLogging, String> {
    let settings_store = SettingsStore::new().map_err(|error| error.to_string())?;
    let logs_directory = settings_store
        .ensure_logs_directory()
        .map_err(|error| error.to_string())?;
    let writer = DailyLogWriter::new(logs_directory);
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(writer.file_path())
        .map_err(|error| error.to_string())?;
    Ok(PreparedFileLogging {
        cleanup_warnings: cleanup_expired_logs(&writer.directory),
        writer,
    })
}

/// 返回上海本地日期的日志文件名，时间位于产品名与 `.log` 后缀之间。
fn current_log_file_name() -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    format!("{LOG_FILE_STEM}.{}.log", now.date())
}

/// 删除超过保留期的 Steady Ink 日志，并将单项失败交给调用方记录。
fn cleanup_expired_logs(directory: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("读取 {} 失败: {error}", directory.display()));
            return warnings;
        }
    };
    let now = SystemTime::now();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!("读取日志目录项失败: {error}"));
                continue;
            }
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_steady_ink_log_file(name) {
            continue;
        }
        let modified = match entry.metadata().and_then(|metadata| metadata.modified()) {
            Ok(modified) => modified,
            Err(error) => {
                warnings.push(format!("读取 {} 修改时间失败: {error}", path.display()));
                continue;
            }
        };
        let expired = now
            .duration_since(modified)
            .is_ok_and(|age| age > LOG_RETENTION);
        if expired && let Err(error) = fs::remove_file(&path) {
            warnings.push(format!("删除旧日志 {} 失败: {error}", path.display()));
        }
    }
    warnings
}

/// 判断文件名是否属于当前或早期命名规则下的 Steady Ink 日志。
fn is_steady_ink_log_file(name: &str) -> bool {
    (name.starts_with(&format!("{LOG_FILE_STEM}.")) && name.ends_with(".log"))
        || name.starts_with(&format!("{LOG_FILE_STEM}.log."))
}

/// 安装 panic hook，把 panic 上下文和完整 backtrace 送入现有 tracing 管道。
fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "<unknown>".to_owned());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<非字符串 panic payload>");
        let backtrace = Backtrace::force_capture();
        tracing::error!(
            thread = thread_name,
            location = %location,
            panic = %payload,
            backtrace = %backtrace,
            "Steady Ink 发生 panic"
        );
        previous(info);
    }));
}
