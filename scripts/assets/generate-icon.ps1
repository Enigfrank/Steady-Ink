[CmdletBinding()]
param(
    [string]$SourcePath = (Join-Path $PSScriptRoot '..\..\assets\steady-ink-icon.png'),
    [string]$OutputPath = (Join-Path $PSScriptRoot '..\..\assets\steady-ink-icon.ico')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

function Convert-BitmapToPngBytes {
    <# Convert one resized bitmap to an alpha-preserving PNG payload. #>
    param([System.Drawing.Bitmap]$Bitmap)

    $stream = [System.IO.MemoryStream]::new()
    try {
        $Bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        return ,([byte[]]$stream.ToArray())
    }
    finally {
        $stream.Dispose()
    }
}

function New-IconFrame {
    <# Render a source image at one Windows icon size with transparent edges. #>
    param(
        [System.Drawing.Image]$Source,
        [int]$Size
    )

    $bitmap = [System.Drawing.Bitmap]::new(
        $Size,
        $Size,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    try {
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
            $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
            $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
            $graphics.Clear([System.Drawing.Color]::Transparent)
            $graphics.DrawImage($Source, [System.Drawing.Rectangle]::new(0, 0, $Size, $Size))
        }
        finally {
            $graphics.Dispose()
        }
        return ,([byte[]](Convert-BitmapToPngBytes $bitmap))
    }
    finally {
        $bitmap.Dispose()
    }
}

function Write-MultiResolutionIcon {
    <# Write PNG-compressed frames into a Windows ICO container. #>
    param(
        [hashtable[]]$Frames,
        [string]$Path
    )

    $directoryBytes = 6 + (16 * $Frames.Count)
    $offset = $directoryBytes
    $stream = [System.IO.FileStream]::new($Path, [System.IO.FileMode]::Create)
    try {
        $writer = [System.IO.BinaryWriter]::new($stream)
        try {
            $writer.Write([uint16]0)
            $writer.Write([uint16]1)
            $writer.Write([uint16]$Frames.Count)
            foreach ($frame in $Frames) {
                $sizeByte = if ($frame.Size -eq 256) { 0 } else { [byte]$frame.Size }
                $writer.Write([byte]$sizeByte)
                $writer.Write([byte]$sizeByte)
                $writer.Write([byte]0)
                $writer.Write([byte]0)
                $writer.Write([uint16]1)
                $writer.Write([uint16]32)
                $writer.Write([uint32]$frame.Bytes.Length)
                $writer.Write([uint32]$offset)
                $offset += $frame.Bytes.Length
            }
            foreach ($frame in $Frames) {
                $writer.Write([byte[]]$frame.Bytes)
            }
        }
        finally {
            $writer.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

$source = [System.Drawing.Image]::FromFile((Resolve-Path -LiteralPath $SourcePath))
try {
    if ($source.Width -ne 512 -or $source.Height -ne 512) {
        throw "Icon source must be 512 x 512, got $($source.Width) x $($source.Height)."
    }

    $sizes = @(16, 24, 32, 48, 64, 128, 256)
    $frames = foreach ($size in $sizes) {
        @{ Size = $size; Bytes = (New-IconFrame -Source $source -Size $size) }
    }
    $parent = Split-Path -Parent $OutputPath
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
    Write-MultiResolutionIcon -Frames $frames -Path $OutputPath
}
finally {
    $source.Dispose()
}

Write-Output "Generated $OutputPath"
