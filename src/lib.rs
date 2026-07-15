pub mod app;
pub mod error;
pub mod ink;
pub mod input;
pub mod render;
pub mod settings;
pub mod slideshow;
pub mod ui;
pub mod window;

use error::AppError;
use tracing_subscriber::EnvFilter;

/// 启动应用公共入口；窗口与渲染运行时将在该入口中组装。
pub fn run() -> Result<(), AppError> {
    initialize_tracing();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Steady Ink 正在启动");
    app::run()
}

/// 初始化日志订阅器，并允许通过 `RUST_LOG` 覆盖默认级别。
fn initialize_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
