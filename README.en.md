<p align="center">
  <img src="./assets/steady-ink-icon.svg" width="128" height="128" alt="Steady Ink icon">
</p>

<h1 align="center">Steady Ink</h1>

<p align="center">A Windows screen-annotation tool for classroom teaching</p>

<p align="center">
  <img src="https://img.shields.io/badge/status-early%20development-F59E0B" alt="Status: early development">
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-2563EB" alt="Platform: Windows 10/11">
  <img src="https://img.shields.io/badge/Rust-1.92%2B-111827?logo=rust&logoColor=white" alt="Rust 1.92 or later">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0--or--later-DC2626" alt="License: GPL-3.0-or-later"></a>
</p>

[中文](./README.md)

> [!WARNING]
> Steady Ink is still in early development. Use it with caution.

## Features

- Six pen colors, `4/6/8/16px` widths, optional natural tapering, three eraser sizes, undo, and clear.
- Windows Pointer pen, touch, palm rejection, and palm erasing input.
- PowerPoint and WPS presentation detection, navigation, and annotation integration.
- Local crash recovery, readable mode, machine-wide startup, and in-app restart.
- Optional live performance monitoring, slow-frame logs, and local JSON performance-data export.

## Status

| Area | Status |
| --- | --- |
| Ink annotation, smooth antialiasing, erasing, and settings | Implemented |
| Windows touch, palm rejection, and palm erasing | Implemented; target hardware tuning remains |
| GPU render thread, resource reuse, and crash recovery | Implemented; broader GPU and device validation remains |
| Performance monitoring and JSON export | Implemented; disabled by default |
| PowerPoint presentation integration | Basic validation completed |
| WPS presentation integration | Adapted; more real environments required |

## Installation

Download the Windows x64 Inno Setup installer and its SHA-256 file from [GitHub Releases](https://github.com/Enigfrank/Steady-Ink/releases).

- A fresh installation enables machine-wide startup by default.
- The desktop shortcut is selected by default; the Start Menu shortcut is optional and unselected by default.
- The completion page runs Steady Ink by default; silent installs never start the interactive app.
- An upgrade preserves the actual machine-wide startup state. Uninstall removes Steady Ink's startup value and selected shortcuts.

The full settings page exposes “Start with Windows for all users”. Changing it requests UAC; cancelling or failing leaves the previous state unchanged.

## Run from Source

Steady Ink requires 64-bit Windows 10/11, Rust 1.92+, the MSVC toolchain, and a Direct3D 12-capable GPU.

```powershell
git clone https://github.com/Enigfrank/Steady-Ink.git
Set-Location Steady-Ink
cargo run --release
```

The first `skia-safe` build may take some time. Set `RUST_LOG=debug` for detailed logs.

## Development

```powershell
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Core technologies: Rust, winit, egui, rust-skia, DirectComposition, D3D12, and Windows COM.

The source is organized into `app`, `window`, `render`, `ui`, `ink`, `input`, `slideshow`, `settings`, `recovery`, and `performance` feature modules.

## Scope and Privacy

Steady Ink currently supports Windows and a single monitor. It does not include any network features.

Ink state primarily remains in memory while drawing. To support recovery after an abnormal exit, the app writes compressed and validated recovery data to `%APPDATA%\Steady-Ink\recovery`, then removes active recovery files after a clean exit or restart.

Preferences are stored in `%APPDATA%\Steady-Ink\settings.toml`. Logs and performance data explicitly exported by the user stay under the same local application directory. The project contains no telemetry or online services; all of this data remains on the device.

## License

Steady Ink and its original project icon are licensed under [GPL-3.0](./LICENSE).
