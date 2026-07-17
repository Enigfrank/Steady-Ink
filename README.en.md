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
> Steady Ink is still in early development. Its main features are usable, but the target 4K touchscreen hardware and additional Office/WPS environments still require validation.

## Features

- A draggable floating toolbar and transparent annotation layer that snap to either screen edge.
- Finger and pen drawing, color and width selection, region erasing, clear, and undo.
- Pen-first palm rejection and dynamic palm erasing when no pen is active.
- PowerPoint/WPS presentation detection, navigation, reconnection, and per-position ink.
- Quick settings, full settings, local preferences, and runtime diagnostics.
- DirectComposition, D3D12, and Skia GPU composition with no continuous redraw while idle.

## Status

| Area | Status |
| --- | --- |
| Standard annotation, toolbars, and settings | Implemented |
| Windows touch, palm rejection, and palm erasing | Implemented; target hardware tuning remains |
| PowerPoint presentation integration | Basic validation completed |
| WPS presentation integration | Adapted; more real environments required |
| Intel integrated-GPU performance | Pending validation on target hardware |
| Windows installer and formal release | Pending |

## Run

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

The source is organized into `app`, `window`, `render`, `ui`, `ink`, `input`, `slideshow`, and `settings` feature modules.

## Scope and Privacy

Steady Ink currently supports Windows and a single monitor. It does not include cloud sync, accounts, collaboration, persistent ink, screenshot saving, or writing ink back to presentation files.

Ink remains in memory for the current run. Preferences are stored in `%APPDATA%\Steady-Ink\settings.toml`. The project contains no telemetry or online services.

## License

Steady Ink and its original project icon are licensed under [GPL-3.0-or-later](./LICENSE).
