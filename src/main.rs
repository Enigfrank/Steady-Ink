#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// 启动 Steady Ink，并把启动错误写入标准错误流。
fn main() {
    if let Err(error) = steady_ink::run() {
        eprintln!("Steady Ink 启动失败: {error}");
        std::process::exit(1);
    }
}
