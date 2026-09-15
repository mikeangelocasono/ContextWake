[CmdletBinding()]
param(
    [string]$InstallDir = $env:CONTEXTWAKE_INSTALL_DIR
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

function Remove-OwnedPathEntry {
    param(
        [AllowEmptyString()][string]$PathValue,
        [Parameter(Mandatory = $true)][string]$Directory
    )

    if ([string]::IsNullOrEmpty($PathValue)) {
        return $PathValue
    }
    $pattern = "(?i)(^|;)" + [Regex]::Escape($Directory) + "(?=;|$)"
    $match = [Regex]::Match($PathValue, $pattern)
    if (-not $match.Success) {
        return $PathValue
    }
    $removeLength = $match.Length
    if ($match.Index -eq 0 -and
        $PathValue.Length -gt $removeLength -and
        $PathValue[$removeLength] -eq ';') {
        $removeLength += 1
    }
    return $PathValue.Remove($match.Index, $removeLength)
}

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "scripts/uninstall.ps1 supports Windows only"
}
if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw "LOCALAPPDATA is unavailable; provide an absolute -InstallDir"
    }
    $InstallDir = Join-Path $env:LOCALAPPDATA "ContextWake\bin"
}
$InstallDir = [IO.Path]::GetFullPath([Environment]::ExpandEnvironmentVariables($InstallDir))
if (-not [IO.Path]::IsPathRooted($InstallDir)) {
    throw "install directory must be an absolute path"
}
if (Test-Path -LiteralPath $InstallDir) {
    $installItem = Get-Item -LiteralPath $InstallDir -Force
    if (-not $installItem.PSIsContainer -or ($installItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "install directory must be a real directory, not a file or reparse point"
    }
}

$pathMarker = Join-Path $InstallDir ".contextwake-path-added"
$removeOwnedPath = Test-Path -LiteralPath $pathMarker -PathType Leaf
foreach ($name in @("ctx.exe", "ctxwake.exe")) {
    $binary = Join-Path $InstallDir $name
    if (Test-Path -LiteralPath $binary) {
        $item = Get-Item -LiteralPath $binary -Force
        if ($item.PSIsContainer) {
            throw "refusing to remove a directory at the expected binary path: $binary"
        }
        Remove-Item -LiteralPath $binary -Force
    }
}

if ($removeOwnedPath) {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    [Environment]::SetEnvironmentVariable(
        "Path",
        (Remove-OwnedPathEntry -PathValue $userPath -Directory $InstallDir),
        "User"
    )
    $env:Path = Remove-OwnedPathEntry -PathValue $env:Path -Directory $InstallDir
    Remove-Item -LiteralPath $pathMarker -Force
}

if (Test-Path -LiteralPath $InstallDir -PathType Container) {
    $remaining = @(Get-ChildItem -Force -LiteralPath $InstallDir)
    if ($remaining.Count -eq 0) {
        Remove-Item -LiteralPath $InstallDir -Force
    }
}

Write-Host "ContextWake binaries were removed."
if ($removeOwnedPath) {
    Write-Host "The installer-owned user PATH entry was removed."
}
Write-Host "Profiles, workspaces, sessions, checkpoints, handoffs, and configuration were preserved."
