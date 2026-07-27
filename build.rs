use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const WINDOW_ICON_SIZE: u32 = 256;

fn main() {
    println!("cargo:rerun-if-changed=assets/steady-ink-icon.png");
    println!("cargo:rerun-if-changed=assets/steady-ink-icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=ProgramFiles");
    println!("cargo:rerun-if-env-changed=ProgramW6432");

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let png_path = manifest_dir.join("assets/steady-ink-icon.png");
    let ico_path = manifest_dir.join("assets/steady-ink-icon.ico");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));

    write_window_icon(&png_path, &out_dir);
    if cfg!(windows) {
        compile_windows_resources(&ico_path, &out_dir);
    }
}

/// 从项目 PNG 生成固定尺寸的 RGBA 窗口图标，避免运行时读取或解码资源。
fn write_window_icon(png_path: &Path, out_dir: &Path) {
    let image = image::open(png_path)
        .unwrap_or_else(|error| panic!("无法读取窗口图标 {}: {error}", png_path.display()));
    if image.width() != 512 || image.height() != 512 {
        panic!(
            "窗口图标必须为 512 x 512，实际为 {} x {}",
            image.width(),
            image.height()
        );
    }
    let resized = image.resize_exact(
        WINDOW_ICON_SIZE,
        WINDOW_ICON_SIZE,
        image::imageops::FilterType::Lanczos3,
    );
    let rgba = resized.to_rgba8();
    let output = out_dir.join("steady-ink-window.rgba");
    fs::write(&output, rgba.as_raw())
        .unwrap_or_else(|error| panic!("无法写入窗口图标 {}: {error}", output.display()));
}

/// 使用 Windows 资源编译器把 ICO 和版本信息嵌入 PE 文件。
fn compile_windows_resources(ico_path: &Path, out_dir: &Path) {
    if !ico_path.is_file() {
        panic!("Windows 图标资源不存在: {}", ico_path.display());
    }
    let Some(compiler) = find_resource_compiler() else {
        if env::var_os("CI").is_some() {
            panic!("CI 中未找到 rc.exe 或 llvm-rc.exe，无法嵌入 Windows 图标资源");
        }
        println!("cargo:warning=未找到 rc.exe 或 llvm-rc.exe，本地构建跳过 PE 图标资源");
        return;
    };

    let resource_script = out_dir.join("steady-ink.rc");
    let resource_file = out_dir.join("steady-ink.res");
    fs::write(&resource_script, resource_script_contents(ico_path))
        .unwrap_or_else(|error| panic!("无法写入 Windows 资源脚本: {error}"));
    let output = Command::new(&compiler)
        .args(["/nologo", "/fo"])
        .arg(&resource_file)
        .arg(&resource_script)
        .output()
        .unwrap_or_else(|error| panic!("无法运行资源编译器 {}: {error}", compiler.display()));
    if !output.status.success() {
        panic!(
            "Windows 资源编译失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    println!("cargo:rustc-link-arg={}", resource_file.display());
}

/// 返回当前环境中可用的 Windows 资源编译器。
fn find_resource_compiler() -> Option<PathBuf> {
    let llvm_candidates = ["ProgramW6432", "ProgramFiles"]
        .into_iter()
        .filter_map(env::var_os)
        .map(PathBuf::from)
        .map(|root| root.join("LLVM").join("bin").join("llvm-rc.exe"));
    let mut candidates = env::var_os("RC")
        .into_iter()
        .map(PathBuf::from)
        .chain([PathBuf::from("rc.exe"), PathBuf::from("llvm-rc.exe")])
        .chain(llvm_candidates);
    candidates.find(|candidate| {
        Command::new(candidate)
            .args(["/?"])
            .output()
            .is_ok_and(|output| output.status.success() || !output.stderr.is_empty())
    })
}

/// 构造包含图标和 Cargo 版本信息的 .rc 文本。
fn resource_script_contents(ico_path: &Path) -> String {
    let icon_path = ico_path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let version = env::var("CARGO_PKG_VERSION").expect("package version");
    let numeric_version = version.split('-').next().unwrap_or(&version);
    let parts = numeric_version
        .split('.')
        .map(|part| part.parse::<u16>().expect("numeric package version"))
        .chain(std::iter::repeat(0))
        .take(4)
        .collect::<Vec<_>>();
    let version_tuple = format!("{}, {}, {}, {}", parts[0], parts[1], parts[2], parts[3]);
    format!(
        r#"#define IDI_STEADY_INK 101
IDI_STEADY_INK ICON "{icon_path}"

1 VERSIONINFO
 FILEVERSION {version_tuple}
 PRODUCTVERSION {version_tuple}
 FILEFLAGSMASK 0x3fL
 FILEFLAGS 0x0L
 FILEOS 0x40004L
 FILETYPE 0x1L
 FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Enigfrank\\0"
      VALUE "FileDescription", "Steady Ink\\0"
      VALUE "FileVersion", "{version}\\0"
      VALUE "InternalName", "steady-ink\\0"
      VALUE "LegalCopyright", "Copyright (C) 2026 Enigfrank\\0"
      VALUE "OriginalFilename", "steady-ink.exe\\0"
      VALUE "ProductName", "Steady Ink\\0"
      VALUE "ProductVersion", "{version}\\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#
    )
}
