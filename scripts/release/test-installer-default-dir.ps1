[CmdletBinding()]
param(
    [string]$IsccPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'inno.ps1')

function Assert-InstallerContract {
    <# Verify the production installer still exposes the reviewed directory and upgrade contract. #>
    param([string]$InstallerScript)

    $contents = Get-Content -LiteralPath $InstallerScript -Raw
    $requiredPatterns = @(
        'AppId=\{\{8B5B8F72-22A6-4E79-9BB7-4B5B5B3E9AA5\}',
        'DefaultDirName=\{code:GetDefaultInstallDir\}',
        'DisableDirPage=no',
        'UsePreviousAppDir=yes',
        'ArchitecturesAllowed=x64compatible',
        'ArchitecturesInstallIn64BitMode=x64compatible',
        'PrivilegesRequired=admin',
        'ValueData: """\{app\}\\steady-ink\.exe"""',
        'Name: "\{commondesktop\}\\\{#MyAppName\}"; Filename: "\{app\}\\steady-ink\.exe"',
        'Name: "\{commonprograms\}\\\{#MyAppName\}\\\{#MyAppName\}"; Filename: "\{app\}\\steady-ink\.exe"',
        'Filename: "\{app\}\\steady-ink\.exe"; Description: "\{cm:LaunchProgram,\{#MyAppName\}\}"'
    )
    foreach ($pattern in $requiredPatterns) {
        if ($contents -notmatch $pattern) {
            throw "Installer contract is missing pattern: $pattern"
        }
    }
}

function Remove-VerifiedTestDirectory {
    <# Remove only the GUID-named directory created under the system temporary root. #>
    param([string]$TestDirectory)

    if (-not (Test-Path -LiteralPath $TestDirectory)) {
        return
    }
    $temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $resolvedTestDirectory = [System.IO.Path]::GetFullPath($TestDirectory)
    $testDirectoryName = [System.IO.Path]::GetFileName($resolvedTestDirectory.TrimEnd([System.IO.Path]::DirectorySeparatorChar))
    $isVerifiedTestDirectory = $resolvedTestDirectory.StartsWith($temporaryRoot, [System.StringComparison]::OrdinalIgnoreCase) -and
        $testDirectoryName -match '^steady-ink-default-dir-[0-9a-f]{32}$'
    if (-not $isVerifiedTestDirectory) {
        throw "Refusing to remove an unverified test directory: $resolvedTestDirectory"
    }
    $cleanupDeadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        try {
            Remove-Item -LiteralPath $resolvedTestDirectory -Recurse -Force
            return
        }
        catch {
            $isTransientFileLock = $_.Exception -is [System.IO.IOException] -or
                $_.Exception -is [System.UnauthorizedAccessException]
            if (-not $isTransientFileLock -or [DateTime]::UtcNow -ge $cleanupDeadline) {
                throw
            }
            Start-Sleep -Milliseconds 100
        }
    } while (Test-Path -LiteralPath $resolvedTestDirectory)
}

function Assert-RecordedInstallDirectory {
    <# Verify the test installer recorded the expected normalized application directory. #>
    param(
        [string]$ResultFile,
        [string]$ExpectedDirectory,
        [string]$Scenario
    )

    $resultDeadline = [DateTime]::UtcNow.AddSeconds(5)
    while (-not (Test-Path -LiteralPath $ResultFile -PathType Leaf)) {
        if ([DateTime]::UtcNow -ge $resultDeadline) {
            throw "$Scenario did not record an installation directory."
        }
        Start-Sleep -Milliseconds 100
    }
    $actual = [System.IO.Path]::GetFullPath((Get-Content -LiteralPath $ResultFile -Raw).Trim())
    $expected = [System.IO.Path]::GetFullPath($ExpectedDirectory)
    if (-not $actual.Equals($expected, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "$Scenario expected '$expected', got '$actual'."
    }
}

function Wait-TestInstallerExit {
    <# Wait until the Inno launcher and its child release the compiled test executable. #>
    param([string]$InstallerPath)

    if (-not (Test-Path -LiteralPath $InstallerPath -PathType Leaf)) {
        return
    }
    $exitDeadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        try {
            $stream = [System.IO.File]::Open(
                $InstallerPath,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Read,
                [System.IO.FileShare]::None
            )
            $stream.Dispose()
            return
        }
        catch {
            $isTransientFileLock = $_.Exception -is [System.IO.IOException] -or
                $_.Exception -is [System.UnauthorizedAccessException]
            if (-not $isTransientFileLock -or [DateTime]::UtcNow -ge $exitDeadline) {
                throw
            }
            Start-Sleep -Milliseconds 100
        }
    } while ($true)
}

function Invoke-TestUninstallers {
    <# Silently remove isolated test installations from each controlled candidate directory. #>
    param([string[]]$InstallDirectories)

    foreach ($installDirectory in $InstallDirectories) {
        if (-not (Test-Path -LiteralPath $installDirectory -PathType Container)) {
            continue
        }
        $uninstallers = @(Get-ChildItem -LiteralPath $installDirectory -Filter 'unins*.exe' -File)
        foreach ($uninstaller in $uninstallers) {
            & $uninstaller.FullName '/VERYSILENT' '/SUPPRESSMSGBOXES' '/NORESTART'
            if ($LASTEXITCODE -ne 0) {
                throw "Test uninstaller failed with exit code $LASTEXITCODE."
            }
        }
    }
}

function Assert-NoTestUninstallEntry {
    <# Verify the isolated installer removed every HKCU uninstall entry for the test directory. #>
    param([string]$TestDirectory)

    $registryRoots = @(
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKCU:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall'
    )
    $registryDeadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        $matchingEntries = @()
        foreach ($registryRoot in $registryRoots) {
            if (-not (Test-Path -LiteralPath $registryRoot)) {
                continue
            }
            $testEntries = Get-ChildItem -LiteralPath $registryRoot | Where-Object {
                $_.PSChildName -like 'steady-ink-default-dir-test-*'
            }
            $matchingEntries += @($testEntries | Where-Object {
                $properties = Get-ItemProperty -LiteralPath $_.PSPath
                $installLocationProperty = $properties.PSObject.Properties['InstallLocation']
                $installLocation = if ($null -eq $installLocationProperty) {
                    ''
                }
                else {
                    [string]$installLocationProperty.Value
                }
                -not [string]::IsNullOrWhiteSpace($installLocation) -and
                    [System.IO.Path]::GetFullPath($installLocation).StartsWith(
                        $TestDirectory,
                        [System.StringComparison]::OrdinalIgnoreCase
                    )
            })
        }
        if ($matchingEntries.Count -eq 0) {
            return
        }
        if ([DateTime]::UtcNow -ge $registryDeadline) {
            throw 'The isolated test installer left an HKCU uninstall entry behind.'
        }
        Start-Sleep -Milliseconds 100
    } while ($true)
}

function Remove-StaleTestUninstallEntries {
    <# Remove only orphaned uninstall entries created by earlier isolated test runs. #>
    $registryRoots = @(
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKCU:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall'
    )
    $temporaryPrefix = Join-Path ([System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())) 'steady-ink-default-dir-'
    foreach ($registryRoot in $registryRoots) {
        if (-not (Test-Path -LiteralPath $registryRoot)) {
            continue
        }
        foreach ($entry in Get-ChildItem -LiteralPath $registryRoot) {
            if ($entry.PSChildName -notlike 'steady-ink-default-dir-test-*') {
                continue
            }
            $properties = Get-ItemProperty -LiteralPath $entry.PSPath
            $displayNameProperty = $properties.PSObject.Properties['DisplayName']
            $installLocationProperty = $properties.PSObject.Properties['InstallLocation']
            if ($null -eq $displayNameProperty -or $null -eq $installLocationProperty) {
                continue
            }
            $installLocation = [System.IO.Path]::GetFullPath([string]$installLocationProperty.Value)
            $isVerifiedTestEntry = [string]$displayNameProperty.Value -eq 'Steady Ink Default Directory Test version 1.0.0' -and
                $installLocation.StartsWith($temporaryPrefix, [System.StringComparison]::OrdinalIgnoreCase) -and
                -not (Test-Path -LiteralPath $installLocation)
            if ($isVerifiedTestEntry) {
                Remove-Item -LiteralPath $entry.PSPath -Force
            }
        }
    }
}

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$installerScript = Join-Path $repositoryRoot 'packaging\windows\steady-ink.iss'
$testScript = Join-Path $repositoryRoot 'packaging\windows\default-dir-test.iss'
$compilerPath = Resolve-InnoCompiler -RequestedPath $IsccPath
$testId = [guid]::NewGuid().ToString('N')
$testDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "steady-ink-default-dir-$testId"
$selectedInstallDirectory = Join-Path $testDirectory 'selected-install'
$fallbackInstallDirectory = Join-Path $testDirectory 'fallback-install'
$resultFile = Join-Path $testDirectory 'selected-directory.txt'
$testAppId = "steady-ink-default-dir-test-$testId"
$testInstaller = Join-Path $testDirectory 'Steady-Ink-Default-Dir-Test.exe'

Remove-StaleTestUninstallEntries

try {
    Assert-InstallerContract -InstallerScript $installerScript
    New-Item -ItemType Directory -Path $testDirectory | Out-Null

    & $compilerPath "/DOutputDir=$testDirectory" "/DTestAppId=$testAppId" "/DFallbackDir=$fallbackInstallDirectory" "/DResultFile=$resultFile" $testScript
    if ($LASTEXITCODE -ne 0) {
        throw 'Default-directory test installer compilation failed.'
    }

    if (-not (Test-Path -LiteralPath $testInstaller -PathType Leaf)) {
        throw "Inno Setup did not create the expected test installer: $testInstaller"
    }
    & $testInstaller '/VERYSILENT' '/SUPPRESSMSGBOXES' '/NORESTART' "/DIR=$selectedInstallDirectory"
    if ($LASTEXITCODE -ne 0) {
        throw "Initial directory-selection test failed with exit code $LASTEXITCODE."
    }
    Assert-RecordedInstallDirectory -ResultFile $resultFile -ExpectedDirectory $selectedInstallDirectory -Scenario 'Initial directory selection'

    Remove-Item -LiteralPath $resultFile -Force
    & $testInstaller '/VERYSILENT' '/SUPPRESSMSGBOXES' '/NORESTART'
    if ($LASTEXITCODE -ne 0) {
        throw "Upgrade directory-retention test failed with exit code $LASTEXITCODE."
    }
    Assert-RecordedInstallDirectory -ResultFile $resultFile -ExpectedDirectory $selectedInstallDirectory -Scenario 'Upgrade directory retention'
    Write-Output 'Installer default-directory and upgrade-retention contracts passed.'
}
finally {
    try {
        Wait-TestInstallerExit -InstallerPath $testInstaller
        Invoke-TestUninstallers -InstallDirectories @($fallbackInstallDirectory, $selectedInstallDirectory)
        Assert-NoTestUninstallEntry -TestDirectory $testDirectory
    }
    finally {
        Remove-VerifiedTestDirectory -TestDirectory $testDirectory
    }
}
