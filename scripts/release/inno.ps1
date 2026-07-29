function Resolve-InnoCompiler {
    <# Resolve Inno Setup's compiler without relying on a developer-specific path. #>
    param([string]$RequestedPath)

    $candidates = @()
    if (-not [string]::IsNullOrWhiteSpace($RequestedPath)) {
        $candidates += $RequestedPath
    }
    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        $candidates += $command.Source
    }
    if (-not [string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        $candidates += (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
    }
    $candidates += @(
        'C:\Program Files (x86)\Inno Setup 6\ISCC.exe',
        'C:\Program Files\Inno Setup 6\ISCC.exe'
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    throw 'ISCC.exe was not found. Install Inno Setup 6 or pass -IsccPath.'
}
