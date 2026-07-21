use thiserror::Error;

/// 应用启动和运行期间可向顶层传播的错误。
#[derive(Debug, Error)]
pub enum AppError {
    #[error("窗口事件循环错误: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),

    #[error("窗口创建错误: {0}")]
    Window(#[from] winit::error::OsError),

    #[error("图形后端错误: {0}")]
    Graphics(String),

    #[error("设置读写错误: {0}")]
    Settings(String),

    #[error("系统级开机启动操作失败: {0}")]
    Autostart(String),
}
