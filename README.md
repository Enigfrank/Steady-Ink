<p align="center">
  <img src="./assets/steady-ink-icon.svg" width="128" height="128" alt="Steady Ink 项目图标">
</p>

<h1 align="center">Steady Ink</h1>

<p align="center">
  面向课堂教学的 Windows 屏幕批注工具，在不修改原应用内容的前提下，提供低延迟书写与 PowerPoint/WPS 放映联动。
</p>

<p align="center">
  <img src="https://img.shields.io/badge/status-early%20development-F59E0B" alt="项目状态：早期开发">
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-2563EB" alt="平台：Windows 10/11">
  <img src="https://img.shields.io/badge/Rust-1.92%2B-111827?logo=rust&logoColor=white" alt="Rust 1.92 或更高版本">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0--or--later-DC2626" alt="许可证：GPL-3.0-or-later"></a>
</p>

> [!WARNING]
> Steady Ink 目前处于早期开发阶段，尚未提供面向日常教学使用的正式安装包。仓库中的代码主要用于验证透明窗口、GPU 墨迹、触摸交互和演示文稿联动方案。

## 项目简介

Steady Ink 希望让教师可以直接在课件、网页、教学软件或桌面内容上书写，而无需切换到独立白板，也不会把墨迹写回原文件。

项目专注于 Windows 4K 触摸教学设备，核心目标包括：

- 始终置顶、低遮挡的悬浮工具栏与透明批注层。
- 手指和触控笔书写、区域橡皮擦、清屏与撤销。
- 触控笔优先的防误触和无笔状态下的手掌橡皮擦。
- 通过 COM 可靠检测 PowerPoint/WPS 放映，并按放映位置保存会话内墨迹。
- 在普通 Intel 核显设备上保持低延迟，并在空闲时停止持续重绘。

## 当前状态

当前代码库已经具备可编译的技术原型，但完整 MVP 尚未完成。

| 模块 | 当前状态 |
| --- | --- |
| 透明置顶窗口与 WGL/OpenGL 上下文 | 已实现原型 |
| Skia GPU 墨迹与 egui 同帧合成 | 已实现原型，仍需目标设备验证 |
| 普通批注的画笔、区域橡皮擦、撤销和清屏 | 已实现基础流程 |
| 等待型事件循环与 OpenGL 驱动诊断 | 已实现 |
| 墨迹 operation、清屏撤销和逐页内存模型 | 已实现并包含单元测试 |
| PowerPoint/WPS late-bound COM 检测 | 已有底层实现，尚未接入桌面运行时并完成实机验证 |
| 设置界面与设置持久化 | 仅有数据模型 |
| Windows Pointer、防误触与手掌橡皮擦 | 尚未实现 |
| 放映工具栏、控制后端与完整逐页交互 | 尚未完成 |

## 技术架构

Steady Ink 使用 Rust 构建，并将窗口、UI、墨迹、输入与演示文稿联动按功能域拆分。

```text
winit event loop
└─ glutin-winit / WGL shared OpenGL context
   ├─ rust-skia GPU ink surface
   └─ egui + egui-winit + egui_glow UI

Windows Pointer Input ──> input router ──> ink document
PowerPoint / WPS COM ───> slideshow state ──> per-page ink store
```

主要技术选择：

- `winit`：窗口和等待型事件循环。
- `glutin-winit`：Windows OpenGL 上下文与透明窗口 surface。
- `rust-skia`：GPU 墨迹绘制与持久离屏缓存。
- `egui`、`egui-winit`、`egui_glow`：触摸友好的工具栏和界面合成。
- `windows`：Windows Pointer Input、COM 和 `SendInput` 等系统能力。

## 环境要求

- Windows 10 或 Windows 11，64 位。
- Rust 1.92 或更高版本，MSVC 工具链。
- Visual Studio Build Tools 2022，并安装“使用 C++ 的桌面开发”工作负载。
- PowerPoint 或支持 COM 自动化的 WPS 演示，仅在测试放映联动时需要。

项目当前仅支持 Windows；macOS 和 Linux 不在 MVP 范围内。

## 从源码运行

在 PowerShell 7 中执行：

```powershell
git clone https://github.com/Enigfrank/Steady-Ink.git
Set-Location Steady-Ink
cargo run --release
```

首次构建需要下载 Rust 依赖，`skia-safe` 的构建时间通常长于普通 Rust 项目。

需要查看更详细的运行日志时：

```powershell
$env:RUST_LOG = "debug"
cargo run
```

## 开发与检查

提交变更前请运行：

```powershell
cargo fmt --check
cargo check
cargo test
cargo clippy -- -D warnings
```

项目按功能域组织：

```text
src/
├─ app/         # 运行时组装与顶层状态机
├─ window/      # 透明窗口、WGL 和显示器几何
├─ render/      # Skia 与 egui 合成
├─ ui/          # 设计 token 和工具栏
├─ ink/         # 墨迹模型、撤销、逐页存储与 GPU 缓存
├─ input/       # 鼠标、触摸和后续 Pointer Input 路由
├─ slideshow/   # PowerPoint/WPS COM 检测与放映会话
└─ settings/    # 可持久化的用户偏好模型
```

## 路线图

- [x] Rust 项目骨架与功能域模块划分。
- [x] 透明 OpenGL、Skia 墨迹和 egui 合成原型。
- [x] 普通批注基础交互和可撤销墨迹模型。
- [x] 放映状态机、逐页墨迹模型与 COM 检测底层。
- [ ] 设置界面、快捷设置和本地偏好持久化。
- [ ] Windows Pointer Input、防误触和动态手掌橡皮擦。
- [ ] PowerPoint/WPS 事件接入、翻页控制与断线降级 UI。
- [ ] 4K 触摸屏与 Intel 核显性能验证。
- [ ] Windows 安装包、版本发布和升级说明。

## MVP 边界

第一版明确不包含：

- macOS、Linux、移动端和多显示器支持。
- 云同步、账号系统、多人协作或在线服务。
- 截图保存、墨迹文件保存或写回 PowerPoint/WPS 文件。
- 白板、形状识别、复杂手势、自定义色盘和主题系统。
- 在 COM 不可用时通过窗口标题、进程名或前台窗口猜测放映状态。

## 数据与隐私

- 墨迹按设计仅存在于当前内存会话，不写入课件或本地墨迹文件。
- 用户偏好将只保存在本地用户配置目录。
- 项目不依赖云服务，也不包含遥测或账号系统。

## 贡献

Issue 和 Pull Request 均欢迎。对于较大的功能或架构改动，建议先创建 Issue 说明教学场景、预期行为和验证方式，避免实现超出 MVP 范围的功能。

提交代码时请：

- 保持改动聚焦，并沿用现有按功能域拆分的小文件结构。
- 为新增函数添加简洁的函数级注释。
- 为状态机、墨迹语义和数据边界补充纯逻辑测试。
- 在涉及触摸、DPI、GPU 或 COM 时说明测试环境和仍需实机验证的部分。
- 确保格式化、编译、测试和 Clippy 检查通过。

报告触摸、渲染或放映联动问题时，请附上 Windows 版本、DPI 缩放、设备型号、OpenGL renderer、PowerPoint/WPS 版本和复现步骤。

## 许可证

Steady Ink 及本项目原创图标以 [GNU General Public License v3.0 or later](./LICENSE) 发布。
