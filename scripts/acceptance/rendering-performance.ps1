param(
    [ValidateSet('All', 'Batch', 'Gpu')]
    [string]$Scenario = 'All',
    [int]$StrokeCount = 1000,
    [int]$ResizeIterations = 8,
    [int]$EraserIterations = 6,
    [int]$IdleMemoryWaitSeconds = 2,
    [string]$OutputPath = 'target/acceptance-results/rendering-performance.json'
)

$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class RenderingAcceptanceNative
{
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct POINT
    {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool ClientToScreen(IntPtr hWnd, ref POINT point);

    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll")]
    public static extern bool GetCursorPos(out POINT point);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);

    [DllImport("user32.dll")]
    public static extern uint GetDpiForWindow(IntPtr hWnd);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool SetWindowPos(
        IntPtr hWnd,
        IntPtr hWndInsertAfter,
        int x,
        int y,
        int width,
        int height,
        uint flags
    );

    [DllImport("user32.dll")]
    public static extern IntPtr GetDC(IntPtr hWnd);

    [DllImport("gdi32.dll")]
    public static extern uint GetPixel(IntPtr hdc, int x, int y);

    [DllImport("user32.dll")]
    public static extern int ReleaseDC(IntPtr hWnd, IntPtr hdc);

    [DllImport("dwmapi.dll")]
    public static extern int DwmFlush();
}
'@

$script:MouseLeftDown = 0x0002
$script:MouseLeftUp = 0x0004
$script:LogName = "steady-ink.$((Get-Date).ToString('yyyy-MM-dd')).log"
$script:OriginalCursor = New-Object RenderingAcceptanceNative+POINT
[void][RenderingAcceptanceNative]::GetCursorPos([ref]$script:OriginalCursor)

# 返回窗口当前客户区物理像素尺寸。
function Get-ClientSize {
    param([IntPtr]$WindowHandle)

    $rect = New-Object RenderingAcceptanceNative+RECT
    if (-not [RenderingAcceptanceNative]::GetClientRect($WindowHandle, [ref]$rect)) {
        throw "无法读取窗口客户区，Win32 错误码：$([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    [pscustomobject]@{
        Width = $rect.Right - $rect.Left
        Height = $rect.Bottom - $rect.Top
    }
}

# 把一个客户区坐标转换为屏幕物理像素坐标。
function ConvertTo-ScreenPoint {
    param(
        [IntPtr]$WindowHandle,
        [int]$X,
        [int]$Y
    )

    $point = New-Object RenderingAcceptanceNative+POINT
    $point.X = $X
    $point.Y = $Y
    if (-not [RenderingAcceptanceNative]::ClientToScreen($WindowHandle, [ref]$point)) {
        throw "客户区坐标转换失败，Win32 错误码：$([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    $point
}

# 向 egui 控件发送一次真实鼠标点击。
function Invoke-ClientClick {
    param(
        [IntPtr]$WindowHandle,
        [int]$X,
        [int]$Y
    )

    $point = ConvertTo-ScreenPoint $WindowHandle $X $Y
    [void][RenderingAcceptanceNative]::SetCursorPos($point.X, $point.Y)
    Start-Sleep -Milliseconds 30
    [RenderingAcceptanceNative]::mouse_event(
        $script:MouseLeftDown,
        0,
        0,
        0,
        [UIntPtr]::Zero
    )
    Start-Sleep -Milliseconds 20
    [RenderingAcceptanceNative]::mouse_event(
        $script:MouseLeftUp,
        0,
        0,
        0,
        [UIntPtr]::Zero
    )
}

# 通过真实鼠标输入生成一条两点笔画或擦除轨迹。
function Invoke-ClientStroke {
    param(
        [IntPtr]$WindowHandle,
        [int]$StartX,
        [int]$StartY,
        [int]$EndX,
        [int]$EndY,
        [int]$DelayMilliseconds = 2
    )

    $start = ConvertTo-ScreenPoint $WindowHandle $StartX $StartY
    $end = ConvertTo-ScreenPoint $WindowHandle $EndX $EndY
    [void][RenderingAcceptanceNative]::SetCursorPos($start.X, $start.Y)
    Start-Sleep -Milliseconds $DelayMilliseconds
    [RenderingAcceptanceNative]::mouse_event(
        $script:MouseLeftDown,
        0,
        0,
        0,
        [UIntPtr]::Zero
    )
    [void][RenderingAcceptanceNative]::SetCursorPos($end.X, $end.Y)
    Start-Sleep -Milliseconds $DelayMilliseconds
    [RenderingAcceptanceNative]::mouse_event(
        $script:MouseLeftUp,
        0,
        0,
        0,
        [UIntPtr]::Zero
    )
}

# 读取指定客户区位置最终合成到桌面的颜色值。
function Get-ClientPixel {
    param(
        [IntPtr]$WindowHandle,
        [int]$X,
        [int]$Y
    )

    $dwmResult = [RenderingAcceptanceNative]::DwmFlush()
    if ($dwmResult -ne 0) {
        throw "等待桌面合成失败，HRESULT：0x$($dwmResult.ToString('X8'))"
    }
    Start-Sleep -Milliseconds 20
    $point = ConvertTo-ScreenPoint $WindowHandle $X $Y
    $screenDc = [RenderingAcceptanceNative]::GetDC([IntPtr]::Zero)
    try {
        [RenderingAcceptanceNative]::GetPixel($screenDc, $point.X, $point.Y)
    }
    finally {
        [void][RenderingAcceptanceNative]::ReleaseDC([IntPtr]::Zero, $screenDc)
    }
}

# 等待应用创建可响应的主窗口。
function Wait-MainWindow {
    param(
        [System.Diagnostics.Process]$Process,
        [int]$TimeoutSeconds = 15
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Milliseconds 100
        $Process.Refresh()
    } while (
        $Process.MainWindowHandle -eq 0 -and
        -not $Process.HasExited -and
        [DateTime]::UtcNow -lt $deadline
    )
    if ($Process.HasExited -or $Process.MainWindowHandle -eq 0) {
        throw "验收程序未在 ${TimeoutSeconds}s 内创建主窗口"
    }
    [void][RenderingAcceptanceNative]::SetForegroundWindow($Process.MainWindowHandle)
    $Process.MainWindowHandle
}

# 等待窗口达到或离开批注模式对应的尺寸范围。
function Wait-WindowMode {
    param(
        [IntPtr]$WindowHandle,
        [ValidateSet('Annotation', 'Idle')]
        [string]$Mode,
        [int]$TimeoutSeconds = 15
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $size = Get-ClientSize $WindowHandle
        $matches = if ($Mode -eq 'Annotation') {
            $size.Width -ge 800 -and $size.Height -ge 600
        }
        else {
            $size.Width -lt 500 -and $size.Height -lt 500
        }
        if ($matches) {
            return $size
        }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "窗口未在 ${TimeoutSeconds}s 内进入 $Mode 模式，最终尺寸为 $($size.Width)x$($size.Height)"
}

# 点击悬浮工具栏首个按钮并进入全屏批注。
function Enter-Annotation {
    param([IntPtr]$WindowHandle)

    $size = Get-ClientSize $WindowHandle
    $scale = [RenderingAcceptanceNative]::GetDpiForWindow($WindowHandle) / 96.0
    Invoke-ClientClick $WindowHandle ([int]($size.Width / 2)) ([int](32.0 * $scale))
    Wait-WindowMode $WindowHandle Annotation
}

# 根据当前窗口 DPI 返回普通批注工具栏指定按钮中心。
function Get-ToolbarPoint {
    param(
        [IntPtr]$WindowHandle,
        [ValidateSet('Eraser', 'Undo', 'Clear', 'Exit')]
        [string]$Action
    )

    $size = Get-ClientSize $WindowHandle
    $scale = [RenderingAcceptanceNative]::GetDpiForWindow($WindowHandle) / 96.0
    $logicalHeight = $size.Height / $scale
    $toolbarHeight = 486.4
    $toolbarTop = ($logicalHeight - $toolbarHeight) / 2.0
    $relativeY = switch ($Action) {
        'Eraser' { 211.2 }
        'Undo' { 332.8 }
        'Clear' { 390.4 }
        'Exit' { 454.4 }
    }
    [pscustomobject]@{
        X = [int]($size.Width - 51.2 * $scale)
        Y = [int](($toolbarTop + $relativeY) * $scale)
    }
}

# 点击普通批注工具栏中的指定命令。
function Invoke-ToolbarAction {
    param(
        [IntPtr]$WindowHandle,
        [ValidateSet('Eraser', 'Undo', 'Clear', 'Exit')]
        [string]$Action
    )

    $point = Get-ToolbarPoint $WindowHandle $Action
    Invoke-ClientClick $WindowHandle $point.X $point.Y
}

# 返回日志文件当前行数，文件不存在时返回零。
function Get-LogLineCount {
    param([string]$LogPath)

    if (-not (Test-Path -LiteralPath $LogPath)) {
        return 0
    }
    @(Get-Content -LiteralPath $LogPath).Count
}

# 返回从指定行号以后追加的日志行。
function Get-NewLogLines {
    param(
        [string]$LogPath,
        [int]$StartLine
    )

    if (-not (Test-Path -LiteralPath $LogPath)) {
        return @()
    }
    @(Get-Content -LiteralPath $LogPath | Select-Object -Skip $StartLine)
}

# 等待新增日志中出现目标正则并返回第一条匹配行。
function Wait-LogLine {
    param(
        [string]$LogPath,
        [int]$StartLine,
        [string]$Pattern,
        [int]$TimeoutSeconds = 30
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $match = Get-NewLogLines $LogPath $StartLine |
            Where-Object { $_ -match $Pattern } |
            Select-Object -First 1
        if ($null -ne $match) {
            return [string]$match
        }
        Start-Sleep -Milliseconds 25
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "日志未在 ${TimeoutSeconds}s 内出现：$Pattern"
}

# 等待第一类日志出现，并返回它之后的第一条第二类日志。
function Wait-LogSequence {
    param(
        [string]$LogPath,
        [int]$StartLine,
        [string]$FirstPattern,
        [string]$SecondPattern,
        [int]$TimeoutSeconds = 30
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $lines = @(Get-NewLogLines $LogPath $StartLine)
        for ($firstIndex = 0; $firstIndex -lt $lines.Count; $firstIndex++) {
            if ($lines[$firstIndex] -notmatch $FirstPattern) {
                continue
            }
            for ($secondIndex = $firstIndex + 1; $secondIndex -lt $lines.Count; $secondIndex++) {
                if ($lines[$secondIndex] -match $SecondPattern) {
                    return [pscustomobject]@{
                        First = [string]$lines[$firstIndex]
                        Second = [string]$lines[$secondIndex]
                    }
                }
            }
        }
        Start-Sleep -Milliseconds 25
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "日志未在 ${TimeoutSeconds}s 内依次出现：$FirstPattern -> $SecondPattern"
}

# 等待增量日志累计处理指定数量的文档操作。
function Wait-OperationTotal {
    param(
        [string]$LogPath,
        [int]$StartLine,
        [int]$ExpectedOperations,
        [int]$TimeoutSeconds = 90
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $total = 0
        foreach ($line in Get-NewLogLines $LogPath $StartLine) {
            if (
                $line -match '(?:验收基线增量墨迹渲染完成|增量墨迹渲染完成)' -and
                $line -match 'operations=(\d+)'
            ) {
                $total += [int]$Matches[1]
            }
        }
        if ($total -ge $ExpectedOperations) {
            return $total
        }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "增量日志只处理了 $total/$ExpectedOperations 个操作"
}

# 从结构化 tracing 文本中解析一个数值字段。
function Get-LogNumber {
    param(
        [string]$Line,
        [string]$Name
    )

    if ($Line -notmatch "(?:^|\s)$([regex]::Escape($Name))=([0-9.]+)") {
        throw "日志缺少字段 $Name：$Line"
    }
    [double]$Matches[1]
}

# 返回数值集合的中位数。
function Get-Median {
    param([double[]]$Values)

    if ($Values.Count -eq 0) {
        throw '无法计算空集合的中位数'
    }
    $sorted = @($Values | Sort-Object)
    $middle = [int]($sorted.Count / 2)
    if ($sorted.Count % 2 -eq 1) {
        return [double]$sorted[$middle]
    }
    ([double]$sorted[$middle - 1] + [double]$sorted[$middle]) / 2.0
}

# 多次采样并返回进程工作集与私有内存中位数。
function Get-ProcessMemorySnapshot {
    param([System.Diagnostics.Process]$Process)

    $workingSets = @()
    $privateSizes = @()
    for ($index = 0; $index -lt 5; $index++) {
        $Process.Refresh()
        $workingSets += $Process.WorkingSet64 / 1MB
        $privateSizes += $Process.PrivateMemorySize64 / 1MB
        Start-Sleep -Milliseconds 150
    }
    [pscustomobject]@{
        WorkingSetMB = [math]::Round((Get-Median $workingSets), 2)
        PrivateMemoryMB = [math]::Round((Get-Median $privateSizes), 2)
    }
}

# 调整无边框批注窗口并保持其位于主屏左上角。
function Set-AcceptanceWindowSize {
    param(
        [IntPtr]$WindowHandle,
        [int]$Width,
        [int]$Height
    )

    $flags = 0x0004 -bor 0x0010
    if (-not [RenderingAcceptanceNative]::SetWindowPos(
        $WindowHandle,
        [IntPtr]::Zero,
        0,
        0,
        $Width,
        $Height,
        $flags
    )) {
        throw "调整验收窗口失败，Win32 错误码：$([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
}

# 创建带隔离设置与日志目录的 release 验收进程。
function Start-AcceptanceProcess {
    param(
        [string]$Executable,
        [string]$Label
    )

    $runDirectory = Join-Path 'target/acceptance-runs' $Label
    if (Test-Path -LiteralPath $runDirectory) {
        Remove-Item -LiteralPath $runDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Force $runDirectory | Out-Null
    $process = Start-Process -FilePath $Executable -PassThru -Environment @{
        STEADY_INK_ACCEPTANCE_DIR = (Resolve-Path $runDirectory).Path
        RUST_LOG = 'steady_ink=debug'
    }
    $windowHandle = Wait-MainWindow $process
    [pscustomobject]@{
        Process = $process
        WindowHandle = $windowHandle
        RunDirectory = (Resolve-Path $runDirectory).Path
        LogPath = Join-Path $runDirectory "logs/$script:LogName"
    }
}

# 关闭验收进程并恢复运行前的鼠标位置。
function Stop-AcceptanceProcess {
    param([System.Diagnostics.Process]$Process)

    [void][RenderingAcceptanceNative]::SetCursorPos(
        $script:OriginalCursor.X,
        $script:OriginalCursor.Y
    )
    if (-not $Process.HasExited) {
        Stop-Process -Id $Process.Id
    }
}

# 运行一次批量绘制或优化前基线的完整真实窗口场景。
function Invoke-BatchAcceptanceCase {
    param(
        [string]$Executable,
        [string]$Label
    )

    $session = Start-AcceptanceProcess $Executable $Label
    $process = $session.Process
    $windowHandle = $session.WindowHandle
    $logPath = $session.LogPath
    try {
        $idleMemory = Get-ProcessMemorySnapshot $process
        $annotationSize = Enter-Annotation $windowHandle
        Start-Sleep -Milliseconds 500
        $backgroundPixel = Get-ClientPixel $windowHandle 58 80
        $strokeLogStart = Get-LogLineCount $logPath

        for ($index = 0; $index -lt $StrokeCount; $index++) {
            $column = $index % 40
            $row = [int]($index / 40)
            $startX = 50 + $column * 35
            $startY = 80 + $row * 30
            Invoke-ClientStroke $windowHandle $startX $startY ($startX + 16) $startY
        }
        $processedOperations = Wait-OperationTotal $logPath $strokeLogStart $StrokeCount
        Start-Sleep -Milliseconds 500
        $inkPixel = Get-ClientPixel $windowHandle 58 80
        $inkMemory = Get-ProcessMemorySnapshot $process

        $rebuildMicros = @()
        $frameMicros = @()
        for ($index = 0; $index -lt $ResizeIterations; $index++) {
            $phaseStart = Get-LogLineCount $logPath
            if ($index % 2 -eq 0) {
                Set-AcceptanceWindowSize $windowHandle 1600 900
            }
            else {
                Set-AcceptanceWindowSize $windowHandle $annotationSize.Width $annotationSize.Height
            }
            $renderSequence = Wait-LogSequence `
                $logPath `
                $phaseStart `
                "(?:验收窗口 resize 完成|(?:验收基线增量墨迹渲染完成|增量墨迹渲染完成).*operations=$StrokeCount\b)" `
                '验收合成帧完成'
            $renderLine = Get-NewLogLines $logPath $phaseStart |
                Where-Object {
                    $_ -match "(?:验收基线增量墨迹渲染完成|增量墨迹渲染完成).*operations=$StrokeCount\b"
                } |
                Select-Object -Last 1
            $frameLine = $renderSequence.Second
            $rebuildMicros += if ($null -eq $renderLine) {
                0.0
            }
            else {
                Get-LogNumber ([string]$renderLine) 'elapsed_micros'
            }
            $frameMicros += Get-LogNumber $frameLine 'elapsed_micros'
        }

        Invoke-ToolbarAction $windowHandle Eraser
        Start-Sleep -Milliseconds 200
        $eraserCommitMicros = @()
        $eraserCommitFrameMicros = @()
        $eraserUndoMicros = @()
        $eraserUndoFrameMicros = @()
        $erasedPixel = $null
        for ($index = 0; $index -lt $EraserIterations; $index++) {
            $lineY = 80 + $index * 30
            $phaseStart = Get-LogLineCount $logPath
            Invoke-ClientStroke $windowHandle 40 $lineY 80 $lineY -DelayMilliseconds 30
            $commitSequence = Wait-LogSequence `
                $logPath `
                $phaseStart `
                '(?:验收基线增量墨迹渲染完成|增量墨迹渲染完成).*operations=1\b' `
                '验收合成帧完成'
            $eraserCommitMicros += Get-LogNumber $commitSequence.First 'elapsed_micros'
            $eraserCommitFrameMicros += Get-LogNumber $commitSequence.Second 'elapsed_micros'
            if ($index -eq 0) {
                $erasedPixel = Get-ClientPixel $windowHandle 58 80
            }

            $undoStart = Get-LogLineCount $logPath
            Invoke-ToolbarAction $windowHandle Undo
            $undoSequence = Wait-LogSequence `
                $logPath `
                $undoStart `
                '(?:验收基线局部墨迹重建完成|局部墨迹重建完成)' `
                '验收合成帧完成'
            $eraserUndoMicros += Get-LogNumber $undoSequence.First 'elapsed_micros'
            $eraserUndoFrameMicros += Get-LogNumber $undoSequence.Second 'elapsed_micros'
            Start-Sleep -Milliseconds 100
        }
        $undoPixel = Get-ClientPixel $windowHandle 58 80

        $clearStart = Get-LogLineCount $logPath
        Invoke-ToolbarAction $windowHandle Clear
        [void](Wait-LogSequence `
            $logPath `
            $clearStart `
            '(?:验收基线增量墨迹渲染完成|增量墨迹渲染完成).*operations=1\b' `
            '验收合成帧完成')
        $clearPixel = Get-ClientPixel $windowHandle 58 80

        $undoClearStart = Get-LogLineCount $logPath
        Invoke-ToolbarAction $windowHandle Undo
        [void](Wait-LogSequence `
            $logPath `
            $undoClearStart `
            '(?:验收基线全量墨迹重建完成|全量墨迹重建完成)' `
            '验收合成帧完成')
        $undoClearPixel = Get-ClientPixel $windowHandle 58 80

        Invoke-ToolbarAction $windowHandle Exit
        $idleSize = Wait-WindowMode $windowHandle Idle
        Start-Sleep -Milliseconds 500
        $returnedIdleMemory = Get-ProcessMemorySnapshot $process

        [pscustomobject]@{
            Label = $Label
            ProcessedOperations = $processedOperations
            RebuildMedianMicros = [math]::Round((Get-Median $rebuildMicros), 2)
            FrameMedianMicros = [math]::Round((Get-Median $frameMicros), 2)
            EraserCommitMedianMicros = [math]::Round((Get-Median $eraserCommitMicros), 2)
            EraserCommitFrameMedianMicros = [math]::Round((Get-Median $eraserCommitFrameMicros), 2)
            EraserUndoMedianMicros = [math]::Round((Get-Median $eraserUndoMicros), 2)
            EraserUndoFrameMedianMicros = [math]::Round((Get-Median $eraserUndoFrameMicros), 2)
            IdleMemory = $idleMemory
            InkMemory = $inkMemory
            ReturnedIdleMemory = $returnedIdleMemory
            AnnotationSize = $annotationSize
            IdleSize = $idleSize
            Pixels = [pscustomobject]@{
                Background = $backgroundPixel
                Ink = $inkPixel
                Erased = $erasedPixel
                Undo = $undoPixel
                Clear = $clearPixel
                UndoClear = $undoClearPixel
            }
            VisualChecks = [pscustomobject]@{
                DrawChangedPixel = $inkPixel -ne $backgroundPixel
                EraserRestoredBackground = $erasedPixel -eq $backgroundPixel
                UndoRestoredInk = $undoPixel -eq $inkPixel
                ClearRestoredBackground = $clearPixel -eq $backgroundPixel
                UndoClearRestoredInk = $undoClearPixel -eq $inkPixel
            }
            Responding = $process.Responding
        }
    }
    finally {
        Stop-AcceptanceProcess $process
    }
}

# 运行一次 GPU 资源生命周期、resize 和模式内存场景。
function Invoke-GpuAcceptanceCase {
    param(
        [string]$Executable,
        [string]$Label
    )

    $session = Start-AcceptanceProcess $Executable $Label
    $process = $session.Process
    $windowHandle = $session.WindowHandle
    $logPath = $session.LogPath
    try {
        Start-Sleep -Milliseconds 500
        $idleMemory = Get-ProcessMemorySnapshot $process
        $annotationSize = Enter-Annotation $windowHandle
        Start-Sleep -Milliseconds 700
        $annotationMemory = Get-ProcessMemorySnapshot $process

        $initialStart = Get-LogLineCount $logPath
        Invoke-ClientStroke $windowHandle 200 200 240 200
        [void](Wait-OperationTotal $logPath $initialStart 1)

        $resizeMicros = @()
        for ($index = 0; $index -lt $ResizeIterations; $index++) {
            $phaseStart = Get-LogLineCount $logPath
            if ($index % 2 -eq 0) {
                Set-AcceptanceWindowSize $windowHandle 1600 900
            }
            else {
                Set-AcceptanceWindowSize $windowHandle $annotationSize.Width $annotationSize.Height
            }
            $resizeSequence = Wait-LogSequence `
                $logPath `
                $phaseStart `
                '验收窗口 resize 完成' `
                '验收合成帧完成'
            $resizeMicros += Get-LogNumber $resizeSequence.First 'elapsed_micros'

            $strokeStart = Get-LogLineCount $logPath
            Invoke-ClientStroke $windowHandle (220 + $index * 20) 240 (240 + $index * 20) 240
            [void](Wait-OperationTotal $logPath $strokeStart 1)
        }
        $activeMemory = Get-ProcessMemorySnapshot $process

        $poolLines = Get-NewLogLines $logPath 0 |
            Where-Object { $_ -match '预览资源池状态' -and $_ -match 'hit_rate=' }
        $poolHitRate = $null
        if ($poolLines.Count -gt 0) {
            $poolHitRate = Get-LogNumber ([string]$poolLines[-1]) 'hit_rate'
        }

        Invoke-ToolbarAction $windowHandle Exit
        $idleSize = Wait-WindowMode $windowHandle Idle
        Start-Sleep -Seconds $IdleMemoryWaitSeconds
        $returnedIdleMemory = Get-ProcessMemorySnapshot $process
        $privateReduction = if ($activeMemory.PrivateMemoryMB -gt 0) {
            100.0 * ($activeMemory.PrivateMemoryMB - $returnedIdleMemory.PrivateMemoryMB) /
                $activeMemory.PrivateMemoryMB
        }
        else {
            0.0
        }

        [pscustomobject]@{
            Label = $Label
            ResizeMedianMicros = [math]::Round((Get-Median $resizeMicros), 2)
            IdleMemory = $idleMemory
            AnnotationMemory = $annotationMemory
            ActiveMemory = $activeMemory
            ReturnedIdleMemory = $returnedIdleMemory
            PrivateMemoryReductionPercent = [math]::Round($privateReduction, 2)
            PoolHitRate = $poolHitRate
            AnnotationSize = $annotationSize
            IdleSize = $idleSize
            Responding = $process.Responding
        }
    }
    finally {
        Stop-AcceptanceProcess $process
    }
}

# 计算从基线到优化版本的百分比下降。
function Get-ReductionPercent {
    param(
        [double]$Baseline,
        [double]$Optimized
    )

    if ($Baseline -le 0) {
        return 0.0
    }
    [math]::Round(100.0 * ($Baseline - $Optimized) / $Baseline, 2)
}

$result = [ordered]@{
    Timestamp = (Get-Date).ToString('o')
    Machine = $env:COMPUTERNAME
    StrokeCount = $StrokeCount
    ResizeIterations = $ResizeIterations
    EraserIterations = $EraserIterations
}

try {
    if ($Scenario -in @('All', 'Batch')) {
        $batchBaseline = Invoke-BatchAcceptanceCase `
            'target/acceptance-bin/prebatch-perf.exe' `
            'batch-baseline'
        $batchOptimized = Invoke-BatchAcceptanceCase `
            'target/acceptance-bin/batch-perf.exe' `
            'batch-optimized'
        $batchMemoryIncrease = if ($batchBaseline.InkMemory.PrivateMemoryMB -gt 0) {
            100.0 * (
                $batchOptimized.InkMemory.PrivateMemoryMB -
                $batchBaseline.InkMemory.PrivateMemoryMB
            ) / $batchBaseline.InkMemory.PrivateMemoryMB
        }
        else {
            0.0
        }
        $result.Batch = [ordered]@{
            Baseline = $batchBaseline
            Optimized = $batchOptimized
            RebuildReductionPercent = Get-ReductionPercent `
                $batchBaseline.RebuildMedianMicros `
                $batchOptimized.RebuildMedianMicros
            FrameReductionPercent = Get-ReductionPercent `
                $batchBaseline.FrameMedianMicros `
                $batchOptimized.FrameMedianMicros
            EraserCommitReductionPercent = Get-ReductionPercent `
                $batchBaseline.EraserCommitMedianMicros `
                $batchOptimized.EraserCommitMedianMicros
            EraserCommitFrameReductionPercent = Get-ReductionPercent `
                $batchBaseline.EraserCommitFrameMedianMicros `
                $batchOptimized.EraserCommitFrameMedianMicros
            EraserUndoReductionPercent = Get-ReductionPercent `
                $batchBaseline.EraserUndoMedianMicros `
                $batchOptimized.EraserUndoMedianMicros
            EraserUndoFrameReductionPercent = Get-ReductionPercent `
                $batchBaseline.EraserUndoFrameMedianMicros `
                $batchOptimized.EraserUndoFrameMedianMicros
            MemoryIncreasePercent = [math]::Round($batchMemoryIncrease, 2)
        }
    }

    if ($Scenario -in @('All', 'Gpu')) {
        $gpuBaseline = Invoke-GpuAcceptanceCase `
            'target/acceptance-bin/gpu-baseline-instrumented.exe' `
            'gpu-baseline'
        $gpuOptimized = Invoke-GpuAcceptanceCase `
            'target/acceptance-bin/gpu-optimized-instrumented.exe' `
            'gpu-optimized'
        $result.Gpu = [ordered]@{
            Baseline = $gpuBaseline
            Optimized = $gpuOptimized
            ResizeReductionPercent = Get-ReductionPercent `
                $gpuBaseline.ResizeMedianMicros `
                $gpuOptimized.ResizeMedianMicros
        }
    }
}
finally {
    [void][RenderingAcceptanceNative]::SetCursorPos(
        $script:OriginalCursor.X,
        $script:OriginalCursor.Y
    )
}

$resolvedOutput = [IO.Path]::GetFullPath((Join-Path (Get-Location) $OutputPath))
$outputDirectory = Split-Path -Parent $resolvedOutput
New-Item -ItemType Directory -Force $outputDirectory | Out-Null
$result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $resolvedOutput -Encoding utf8
$result | ConvertTo-Json -Depth 8
