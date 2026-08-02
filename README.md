<p align="center">
  <img src="./assets/steady-ink-icon.svg" width="128" height="128" alt="Steady Ink 项目图标">
</p>

<h1 align="center">Steady Ink</h1>

<p align="center">一款简洁的 Windows 屏幕批注工具</p>

<p align="center">
  <img src="https://img.shields.io/badge/status-early%20development-F59E0B" alt="项目状态：早期开发">
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-2563EB" alt="平台：Windows 10/11">
  <img src="https://img.shields.io/badge/Rust-1.92%2B-111827?logo=rust&logoColor=white" alt="Rust 1.92 或更高版本">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0--or--later-DC2626" alt="许可证：GPL-3.0-or-later"></a>
</p>

[English Document](./README.en.md)

> [!WARNING]
> Steady Ink 仍处于早期开发阶段,请谨慎使用!

## 功能

- 六种画笔颜色、`4/6/8/16px` 画笔、可选自然笔锋、三档橡皮擦、撤销和清屏。
- 面向 Windows Pointer 的画笔、触控、防误触和手掌橡皮擦输入。
- PowerPoint 与 WPS 放映检测、翻页和批注联动。
- 本地崩溃恢复、易读模式、所有用户开机自启动和应用内重启。
- 可选实时性能监控、慢帧日志和本地 JSON 性能数据导出。


## 安装

从 [GitHub Releases](https://github.com/Enigfrank/Steady-Ink/releases) 下载唯一的 Windows x64 Inno Setup 安装包和 SHA-256 校验文件.

- 首次安装默认启用所有用户的开机自启动.
- 桌面快捷方式默认勾选，开始菜单快捷方式默认不勾选；两项都可在安装过程中调整.
- 安装完成页默认运行 Steady Ink；静默安装不会启动交互式应用.
- 覆盖升级保留实际的开机自启动状态，卸载会移除 Steady Ink 的系统级启动项和已创建的快捷方式.

完整设置页中的“为所有用户开机启动”开关会在修改时请求 UAC；取消或失败不会改变原状态.

## 从源代码运行

需要 64 位 Windows 10/11、Rust 1.92+、MSVC 工具链和支持 Direct3D 12 的显卡.

```powershell
git clone https://github.com/Enigfrank/Steady-Ink.git
Set-Location Steady-Ink
cargo run --release
```

首次构建 `skia-safe` 可能需要较长时间.详细日志可通过 `RUST_LOG=debug` 启用.

## 开发

```powershell
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo build --release
```

主要技术栈：Rust、winit、egui、rust-skia、DirectComposition、D3D12 和 Windows COM.

代码按 `app`、`window`、`render`、`ui`、`ink`、`input`、`slideshow`、`settings`、`recovery` 和 `performance` 功能模块组织.

## 范围与隐私

Steady Ink 当前仅支持 Windows 和单显示器，不包含任何联网功能.

绘制时的墨迹状态主要保存在内存中.为支持异常退出恢复，应用会把经过压缩和校验的恢复数据写入 `%APPDATA%\Steady-Ink\recovery`，并在正常退出或重启后清理活动恢复文件.

用户偏好保存在 `%APPDATA%\Steady-Ink\settings.toml`，日志和用户主动导出的性能数据保存在同一应用目录下.项目不包含遥测或在线服务，所有这些数据都留在本机.

## 许可证

Steady Ink 及项目原创图标使用 [GPL-3.0](./LICENSE) 许可证.
