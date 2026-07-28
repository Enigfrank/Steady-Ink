param(
    [int]$InputSamples = 160,
    [string]$OutputPath = 'target/acceptance-results/egui-final.json'
)

$ErrorActionPreference = 'Stop'

# 调用共享的真实窗口驱动，只执行 egui retained frame 对照场景。
& (Join-Path $PSScriptRoot 'rendering-performance.ps1') `
    -Scenario Egui `
    -EguiInputSamples $InputSamples `
    -OutputPath $OutputPath
