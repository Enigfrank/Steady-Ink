[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Tag,
    [string]$IsccPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-RepositoryRoot {
    <# Return the repository root relative to this script. #>
    return (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
}

function Invoke-CargoMetadata {
    <# Read Cargo.toml metadata so every package name is derived from Cargo. #>
    param([string]$RepositoryRoot)

    $manifest = Join-Path $RepositoryRoot 'Cargo.toml'
    $json = (& cargo metadata --manifest-path $manifest --no-deps --format-version 1 --locked 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed: $json"
    }
    return ($json | ConvertFrom-Json)
}

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

function Invoke-ReleasePackage {
    <# Build and package exactly the two supported release assets. #>
    param(
        [string]$RepositoryRoot,
        [string]$TagName,
        [string]$RequestedCompilerPath
    )

    $metadata = Invoke-CargoMetadata -RepositoryRoot $RepositoryRoot
    if ($metadata.packages.Count -ne 1) {
        throw 'Expected exactly one Cargo package.'
    }
    $version = [string]$metadata.packages[0].version
    $expectedTag = "v$version"
    if ($TagName -notmatch '^v\d+\.\d+\.\d+$' -or $TagName -ne $expectedTag) {
        throw "Tag '$TagName' must exactly match Cargo version '$expectedTag'."
    }
    $compilerPath = Resolve-InnoCompiler -RequestedPath $RequestedCompilerPath

    & cargo build --manifest-path (Join-Path $RepositoryRoot 'Cargo.toml') --release --locked
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo build --release --locked failed.'
    }

    $executable = Join-Path $RepositoryRoot 'target\release\steady-ink.exe'
    $installerScript = Join-Path $RepositoryRoot 'packaging\windows\steady-ink.iss'
    $chineseMessages = Join-Path $RepositoryRoot 'packaging\windows\Languages\ChineseSimplified.isl'
    $icon = Join-Path $RepositoryRoot 'assets\steady-ink-icon.ico'
    foreach ($input in @($executable, $installerScript, $chineseMessages, $icon)) {
        if (-not (Test-Path -LiteralPath $input -PathType Leaf)) {
            throw "Required packaging input is missing: $input"
        }
    }

    $dist = Join-Path $RepositoryRoot 'dist'
    if (Test-Path -LiteralPath $dist) {
        Remove-Item -LiteralPath $dist -Recurse -Force
    }
    New-Item -ItemType Directory -Path $dist -Force | Out-Null

    & $compilerPath "/DMyAppVersion=$version" "/DSourceDir=$RepositoryRoot" "/DOutputDir=$dist" $installerScript
    if ($LASTEXITCODE -ne 0) {
        throw 'Inno Setup compilation failed.'
    }

    $setup = Join-Path $dist "Steady-Ink-$version-Setup.exe"
    if (-not (Test-Path -LiteralPath $setup -PathType Leaf)) {
        throw "Inno Setup did not create the expected installer: $setup"
    }
    $hash = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
    $checksum = "$hash  $(Split-Path -Leaf $setup)"
    $checksumPath = "$setup.sha256"
    [System.IO.File]::WriteAllText($checksumPath, "$checksum`r`n", [System.Text.Encoding]::ASCII)

    $assets = @(Get-ChildItem -LiteralPath $dist -File)
    if ($assets.Count -ne 2 -or ($assets.Name -notcontains (Split-Path -Leaf $setup)) -or ($assets.Name -notcontains (Split-Path -Leaf $checksumPath))) {
        throw 'The dist directory must contain only the installer and its SHA-256 file.'
    }
    Write-Output "Created $setup"
    Write-Output "Created $checksumPath"
}

$root = Get-RepositoryRoot
Invoke-ReleasePackage -RepositoryRoot $root -TagName $Tag -RequestedCompilerPath $IsccPath
