<p align="center">
  <img src="./assets/steady-ink-icon.svg" width="128" height="128" alt="Steady Ink 项目图标">
</p>

<h1 align="center">Steady Ink</h1>

<p align="center">面向课堂教学的 Windows 屏幕批注工具</p>

<p align="center">
  <img src="https://img.shields.io/badge/status-early%20development-F59E0B" alt="项目状态：早期开发">
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-2563EB" alt="平台：Windows 10/11">
  <img src="https://img.shields.io/badge/Rust-1.92%2B-111827?logo=rust&logoColor=white" alt="Rust 1.92 或更高版本">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0--or--later-DC2626" alt="许可证：GPL-3.0-or-later"></a>
</p>

[English](./README.en.md)

> [!WARNING]
> Steady Ink 仍处于早期开发阶段。主要功能已经可用，但目标 4K 触摸设备和更多 Office/WPS 环境仍需验证。

## 功能

- 可拖动并吸附屏幕两侧的悬浮工具栏和透明批注层。
- 手指与触控笔书写、颜色和粗细选择、区域橡皮擦、清屏与撤销。
- 触控笔优先的防误触，以及无笔状态下的动态手掌橡皮擦。
- PowerPoint/WPS 放映检测、翻页控制、断线恢复和按放映位置保留墨迹。
- 快捷设置、完整设置、本地偏好保存和运行诊断。
- DirectComposition、D3D12 和 Skia GPU 合成；空闲时停止持续重绘。

## 当前状态

| 项目 | 状态 |
| --- | --- |
| 普通批注、工具栏和设置 | 已实现 |
| Windows 触控、防误触和手掌橡皮擦 | 已实现，目标触摸设备仍需调校 |
| PowerPoint 放映联动 | 已完成基础验证 |
| WPS 放映联动 | 已适配，仍需更多真实环境验证 |
| Intel 核显性能 | UHD Graphics 630、1080p 基线通过；4K 待验证 |
| Windows 安装包与正式发布 | 待完成 |

当前自动化测试共 72 项。Intel UHD Graphics 630 在 `1920 × 1080`、1000 条画笔 operation 和 200 次擦除 operation 下，input-to-display p95 为 `8.377ms`。

## 运行

需要 64 位 Windows 10/11、Rust 1.92+、MSVC 工具链和支持 Direct3D 12 的显卡。

```powershell
git clone https://github.com/Enigfrank/Steady-Ink.git
Set-Location Steady-Ink
cargo run --release
```

首次构建 `skia-safe` 可能需要较长时间。详细日志可通过 `RUST_LOG=debug` 启用。

## 性能验证

以下场景通过真实 DirectComposition/D3D12 路径执行 1000 条画笔和 200 次擦除，并在 p95 超过 `33ms`、使用 WARP 或非 Intel adapter 时返回失败：

```powershell
cargo build --release
$report = Join-Path $env:TEMP "steady-ink-gpu-benchmark.toml"
$env:STEADY_INK_GPU_BENCHMARK = "1"
$env:STEADY_INK_GPU_BENCHMARK_REPORT = $report
$process = Start-Process ".\target\release\steady-ink.exe" -Wait -PassThru
Get-Content -LiteralPath $report
if ($process.ExitCode -ne 0) { throw "GPU 压力场景未通过" }
```

报告会记录实际 adapter 和渲染尺寸，因此 1080p 结果不能替代 4K 验收。

## 开发

```powershell
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
```

主要技术栈：Rust、winit、egui、rust-skia、DirectComposition、D3D12 和 Windows COM。

代码按 `app`、`window`、`render`、`ui`、`ink`、`input`、`slideshow` 和 `settings` 功能模块组织。

## 范围与隐私

Steady Ink 当前仅支持 Windows 和单显示器，不包含云同步、账号、多人协作、墨迹持久化、截图保存或写回课件。

墨迹只保存在本次运行的内存中。用户偏好保存在 `%APPDATA%\Steady-Ink\settings.toml`，项目不包含遥测或在线服务。

## 许可证

Steady Ink 及项目原创图标使用 [GPL-3.0-or-later](./LICENSE) 许可证。
