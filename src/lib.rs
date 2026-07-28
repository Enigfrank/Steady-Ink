pub mod app;
pub mod autostart;
pub mod error;
pub mod ink;
pub mod input;
mod logging;
pub mod recovery;
pub mod render;
pub mod settings;
pub mod slideshow;
pub mod ui;
pub mod window;

use error::AppError;
use std::backtrace::Backtrace;

/// 启动应用公共入口；窗口与渲染运行时将在该入口中组装。
pub fn run() -> Result<(), AppError> {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if let Some(result) = autostart::run_helper_if_requested(&arguments) {
        return result.map_err(|error| AppError::Autostart(error.to_string()));
    }

    logging::initialize();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Steady Ink 正在启动");
    let result = app::run();
    if let Err(error) = &result {
        let backtrace = Backtrace::force_capture();
        tracing::error!(
            %error,
            backtrace = %backtrace,
            "Steady Ink 运行失败"
        );
    }
    result
}
