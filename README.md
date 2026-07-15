<p align="center">
  <img src="./assets/steady-ink-icon.svg" width="128" height="128" alt="Steady Ink 项目图标">
</p>

<h1 align="center">Steady Ink</h1>

<p align="center">
  给课堂教学使用的 Windows 屏幕批注工具
</p>

<p align="center">
  <img src="https://img.shields.io/badge/status-early%20development-F59E0B" alt="项目状态：早期开发">
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-2563EB" alt="平台：Windows 10/11">
  <img src="https://img.shields.io/badge/Rust-1.92%2B-111827?logo=rust&logoColor=white" alt="Rust 1.92 或更高版本">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0--or--later-DC2626" alt="许可证：GPL-3.0-or-later"></a>
</p>

> [!WARNING]
> Steady Ink 目前处于早期开发阶段，尚未提供面向日常教学使用的正式安装包。主要功能已经可以在桌面程序中运行，但仍需在目标 4K 触摸教学设备上完成长期稳定性和性能验证。

## 项目简介

Steady Ink 主要为 Windows 4K 触摸教学设备设计，核心目标包括：

- 始终显示在课件上方、尽量不遮挡内容的悬浮工具栏和批注层。
- 手指和触控笔书写、区域橡皮擦、清屏与撤销。
- 触控笔优先的防误触和无笔状态下的手掌橡皮擦。
- 可靠识别 PowerPoint/WPS 是否正在放映，并按放映页保留本次使用中的墨迹。
- 在普通 Intel 核显设备上保持低延迟，并在空闲时停止持续重绘。

## 当前状态

当前代码库已经具备可运行的 Windows 原型，普通批注、设置、Windows 触控输入和 PowerPoint 放映联动都已接入同一套程序。完整的第一版仍需要目标设备和更多 Office/WPS 环境验证。

| 模块 | 当前状态 |
| --- | --- |
| 透明置顶窗口和 GPU 画面合成（DirectComposition/D3D12） | 已实现，并在 Intel UHD Graphics 630 上完成 1080p 基础运行验证 |
| 墨迹与界面同时绘制（Skia/egui） | 已实现，目标 4K 触摸设备仍需验证 |
| 普通批注、颜色/粗细选择、区域橡皮擦、撤销和清屏 | 已实现 |
| 悬浮工具栏、快捷设置、完整设置和本地设置保存 | 已实现 |
| Windows 触控输入、防误触与动态手掌橡皮擦 | 已接入程序并包含逻辑测试，仍需在目标触摸设备上调整参数 |
| PowerPoint 放映检测、控制和断线恢复 | 已接入程序并完成基础双页放映验证 |
| WPS 放映联动 | 已完成初步适配与诊断，仍需受支持 WPS 环境验证 |
| 放映工具栏、两侧翻页、收起动画和按页保留墨迹 | 已实现 |
| 低功耗等待、性能指标和显卡信息诊断 | 已实现 |

## 当前界面行为


- 非批注悬浮工具栏和普通批注工具栏都可拖动并吸附屏幕左侧或右侧；吸附只改变横坐标，保留并限制当前纵坐标。
- 普通批注模式的颜色和画笔粗细选择栏采用纵向布局。工具栏位于右侧时选择栏向左展开，位于左侧时向右展开，避免相互覆盖。
- 颜色固定为红、黄、蓝、绿、黑、白；画笔粗细固定为 4pt、8pt、16pt、24pt，默认 4pt。
- PowerPoint/WPS 放映模式的中央工具栏紧贴屏幕底边，左右翻页控件分别紧贴左下角和右下角，不保留额外安全距离。
- 界面尺寸统一为原设计的 81.648%，面板、按钮、弹层和边框表面使用 50% 不透明度；文字、图标、色样和墨迹保持完整不透明度。
- 设置页支持修改默认工具、启用或禁用 PowerPoint/WPS 联动、查看图形和放映连接诊断，以及正常退出软件。

## 技术实现

Steady Ink 使用 Rust 构建，代码按窗口、界面、墨迹、输入和 PowerPoint/WPS 联动等功能分开组织，便于维护。

```text
winit event loop
└─ DirectComposition / DXGI / D3D12
   └─ rust-skia GPU 绘图层
      ├─ 持续保留的墨迹层
      └─ egui + egui-winit 界面绘制

Windows 触控输入 ──> 输入分发 ──> 墨迹文档
PowerPoint / WPS 联动接口 ───> 放映状态 ──> 按页墨迹存储
```

主要技术选择：

- `winit`：负责窗口和按需等待的事件循环。
- DirectComposition / DXGI / D3D12：负责 Windows 透明窗口和 GPU 画面合成。
- `rust-skia`：负责 GPU 绘制墨迹、界面网格和窗口合成。
- `egui`、`egui-winit`：负责触摸友好的工具栏、布局和输入集成。
- `windows`：负责调用 Windows 触控输入、Office 联动接口（COM）和 `SendInput` 等系统能力。

透明呈现不再依赖普通 WGL 默认 framebuffer、`WS_EX_LAYERED` color key 或全屏 CPU 位图上传。当前后端使用 `DXGI_FORMAT_B8G8R8A8_UNORM`、flip sequential 和 `DXGI_ALPHA_MODE_PREMULTIPLIED`，以避免 Intel 核显环境中 WGL 全屏透明窗口被 DWM 合成为黑色的问题。

## 环境要求

- Windows 10 或 Windows 11，64 位。
- Rust 1.92 或更高版本，MSVC 工具链。
- Visual Studio Build Tools 2022，并安装“使用 C++ 的桌面开发”工作负载。
- 支持 Direct3D 12 和 DirectComposition 的显卡及驱动；如果使用 WARP 软件渲染，只能用于功能测试，不能作为性能测试结果。
- PowerPoint 或支持自动化控制的 WPS 演示，仅在测试放映联动时需要。

目前只支持 Windows，暂不支持 macOS 和 Linux。

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

需要采集帧耗时、输入到显示延迟、重绘原因和完整画布重建次数时：

```powershell
$env:STEADY_INK_METRICS = "1"
cargo run --release
```

性能指标默认关闭；启用后每五秒输出一次报告，不会因此启动持续重绘。

## 开发与检查

提交变更前请运行：

```powershell
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
```

当前自动化测试包含 42 项逻辑和布局测试，覆盖状态流转、清屏撤销、逐页墨迹、手掌分类、放映连接恢复、选择栏方向、侧边吸附和放映工具栏边缘定位。

项目按功能组织：

```text
src/
├─ app/         # 程序组装与顶层状态管理
├─ window/      # 透明窗口、DirectComposition/D3D12 和屏幕位置尺寸
├─ render/      # Skia 与 egui 合成
├─ ui/          # 设计变量和工具栏
├─ ink/         # 墨迹数据、撤销、逐页存储与 GPU 缓存
├─ input/       # 鼠标、触摸、Windows 触控输入和手掌识别
├─ slideshow/   # PowerPoint/WPS 放映检测与会话管理
└─ settings/    # 用户设置与本地 TOML 存储
```

## 路线图

- [x] Rust 项目骨架与按功能拆分的模块。
- [x] DirectComposition 透明交换链、Skia 墨迹和 egui 合成原型。
- [x] 普通批注交互、工具选择和可撤销墨迹模型。
- [x] 设置界面、快捷设置、本地设置保存和退出软件操作。
- [x] Windows 触控输入、防误触和动态手掌橡皮擦运行路径。
- [x] PowerPoint/WPS 放映联动、翻页控制和断线降级界面。
- [x] 放映状态管理、按页保留墨迹、底部工具栏和两侧翻页控件。
- [ ] 在目标触摸设备上完成手掌识别阈值和触控笔悬停验证。
- [ ] 完成 WPS 真实放映事件、控制和重连兼容性验证。
- [ ] 4K 触摸屏与 Intel 核显性能验证。
- [ ] Windows 安装包、版本发布和升级说明。

## 第一版暂不包含

第一版明确不包含：

- macOS、Linux、移动端和多显示器支持。
- 云同步、账号系统、多人协作或在线服务。
- 截图保存、墨迹文件保存或写回 PowerPoint/WPS 文件。
- 白板、形状识别、复杂手势、自定义色盘和主题系统。
- 在放映连接不可用时通过窗口标题、进程名或前台窗口猜测放映状态。

## 数据与隐私

- 墨迹只保留在本次打开软件期间，不写入课件或本地墨迹文件。
- 用户设置保存在 `%APPDATA%\Steady-Ink\settings.toml`，只包含默认工具和演示联动开关。
- 项目不依赖云服务，也不包含遥测或账号系统。

## 贡献

欢迎提交问题反馈和代码改进建议。对于较大的功能或架构改动，建议先发起一条说明教学场景、预期行为和验证方式的讨论，避免实现超出第一版计划的功能。

提交代码时请：

- 保持改动聚焦，并沿用现有按功能拆分的小文件结构。
- 为新增函数添加简洁的函数级注释。
- 为状态管理、墨迹行为和数据边界补充纯逻辑测试。
- 在涉及触摸、显示缩放、GPU 或 PowerPoint/WPS 联动时说明测试环境和仍需实机验证的部分。
- 确保格式化、编译、测试和 Clippy 检查通过。

报告触摸、画面或放映联动问题时，请附上 Windows 版本、显示缩放比例、设备型号、显卡型号和驱动信息、PowerPoint/WPS 版本和复现步骤。

## 许可证

Steady Ink 及本项目原创图标以 [GNU General Public License v3.0 or later](./LICENSE) 发布。
