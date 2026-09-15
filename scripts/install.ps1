[CmdletBinding()]
param(
    [string]$Version = $env:CONTEXTWAKE_VERSION,
    [string]$InstallDir = $env:CONTEXTWAKE_INSTALL_DIR,
    [switch]$NoPathUpdate
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

$Repository = "mikeangelocasono/ContextWake"
$RepositoryUrl = "https://github.com/$Repository"
$ReleaseFeedUrl = "$RepositoryUrl/releases.atom"
$DefaultReleaseVersion = "v0.1.0-alpha.2"
$PathMarkerName = ".contextwake-path-added"

function Write-Step {
    param([Parameter(Mandatory = $true)][string]$Message)
    Write-Host $Message
}

function Test-ReleaseVersion {
    param([Parameter(Mandatory = $true)][string]$Value)
    return $Value -match '^v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$'
}

function Assert-TrustedResponseUri {
    param([Parameter(Mandatory = $true)][Uri]$Uri)

    if ($Uri.Scheme -ne "https") {
        throw "download redirected to a non-HTTPS URL"
    }
    $hostName = $Uri.DnsSafeHost.ToLowerInvariant()
    if ($hostName -ne "github.com" -and
        $hostName -ne "api.github.com" -and
        -not $hostName.EndsWith(".githubusercontent.com")) {
        throw "download redirected to an unexpected host: $hostName"
    }
}

function Invoke-TrustedDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$OutFile
    )

    $parsedUri = [Uri]$Uri
    Assert-TrustedResponseUri -Uri $parsedUri
    $response = Invoke-WebRequest `
        -Uri $parsedUri `
        -OutFile $OutFile `
        -PassThru `
        -UseBasicParsing `
        -MaximumRedirection 5 `
        -TimeoutSec 300 `
        -Headers @{ "User-Agent" = "ContextWake-Installer" }
    $responseUriProperty = $response.BaseResponse.PSObject.Properties["ResponseUri"]
    if ($null -ne $responseUriProperty) {
        $finalUri = [Uri]$responseUriProperty.Value
    }
    else {
        $requestMessageProperty = $response.BaseResponse.PSObject.Properties["RequestMessage"]
        if ($null -eq $requestMessageProperty -or $null -eq $requestMessageProperty.Value.RequestUri) {
            throw "download response did not expose its final URL"
        }
        $finalUri = [Uri]$requestMessageProperty.Value.RequestUri
    }
    Assert-TrustedResponseUri -Uri $finalUri
}

function Try-TrustedDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$OutFile
    )

    try {
        Invoke-TrustedDownload -Uri $Uri -OutFile $OutFile
        return $true
    }
    catch {
        if (Test-Path -LiteralPath $OutFile) {
            Remove-Item -LiteralPath $OutFile -Force
        }
        return $false
    }
}

function Get-PublicReleaseTags {
    param([Parameter(Mandatory = $true)][string]$TemporaryDirectory)

    $releaseFile = Join-Path $TemporaryDirectory "releases.atom"
    Invoke-TrustedDownload -Uri $ReleaseFeedUrl -OutFile $releaseFile
    if ((Get-Item -LiteralPath $releaseFile).Length -gt 512KB) {
        throw "GitHub release metadata exceeded the 512 KiB safety limit"
    }
    $escapedRepository = [Regex]::Escape($RepositoryUrl)
    $pattern = "$escapedRepository/releases/tag/(v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?)"
    $content = Get-Content -Raw -Encoding UTF8 -LiteralPath $releaseFile
    $tags = @([Regex]::Matches($content, $pattern) | ForEach-Object { $_.Groups[1].Value } | Select-Object -Unique)
    if ($tags.Count -eq 0) {
        throw "GitHub returned malformed or empty release metadata"
    }
    return $tags
}

function Resolve-ReleaseVersion {
    param(
        [Parameter(Mandatory = $true)][string]$RequestedVersion,
        [Parameter(Mandatory = $true)][string]$AssetLabel,
        [Parameter(Mandatory = $true)][string]$TemporaryDirectory
    )

    if ($RequestedVersion -ne "latest") {
        if (-not (Test-ReleaseVersion -Value $RequestedVersion)) {
            throw "version must be 'latest' or a release tag such as v0.1.0-alpha.2"
        }
        return $RequestedVersion
    }

    $releaseTags = @()
    try {
        $releaseTags = @(Get-PublicReleaseTags -TemporaryDirectory $TemporaryDirectory)
    }
    catch {
        Write-Warning "Latest-release lookup failed; trying the installer's pinned known-good release."
    }
    if ($releaseTags -notcontains $DefaultReleaseVersion) {
        $releaseTags += $DefaultReleaseVersion
    }

    foreach ($tag in $releaseTags) {
        if (-not (Test-ReleaseVersion -Value $tag)) {
            continue
        }
        $archiveName = "contextwake-$tag-$AssetLabel.zip"
        $candidateManifest = Join-Path $TemporaryDirectory "SHA256SUMS.$tag"
        if (Try-TrustedDownload -Uri "$RepositoryUrl/releases/download/$tag/SHA256SUMS" -OutFile $candidateManifest) {
            try {
                $null = Get-ManifestChecksum -ManifestPath $candidateManifest -AssetName $archiveName
                return $tag
            }
            catch {
                continue
            }
        }
    }
    throw "no compatible public ContextWake release was found for Windows x86_64"
}

function Get-ManifestChecksum {
    param(
        [Parameter(Mandatory = $true)][string]$ManifestPath,
        [Parameter(Mandatory = $true)][string]$AssetName
    )

    $checksums = @()
    foreach ($line in (Get-Content -Encoding ASCII -LiteralPath $ManifestPath)) {
        if ($line -match '^([0-9A-Fa-f]{64})  ([A-Za-z0-9][A-Za-z0-9._-]*)$' -and
            $Matches[2] -ceq $AssetName) {
            $checksums += $Matches[1].ToLowerInvariant()
        }
    }
    if ($checksums.Count -ne 1) {
        throw "SHA256SUMS does not contain exactly one valid checksum for $AssetName"
    }
    return $checksums[0]
}

function Get-Sha256Hex {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    $hasher = [Security.Cryptography.SHA256]::Create()
    try {
        $digest = $hasher.ComputeHash($stream)
        return (($digest | ForEach-Object { $_.ToString("x2") }) -join '')
    }
    finally {
        $hasher.Dispose()
        $stream.Dispose()
    }
}

function Assert-SafeArchive {
    param(
        [Parameter(Mandatory = $true)][string]$ArchivePath,
        [Parameter(Mandatory = $true)][string]$ArchiveRoot
    )

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $expectedEntries = @(
        "$ArchiveRoot/ctx.exe",
        "$ArchiveRoot/ctxwake.exe",
        "$ArchiveRoot/LICENSE",
        "$ArchiveRoot/README.md"
    ) | Sort-Object

    $archive = [System.IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        if ($archive.Entries.Count -gt 16) {
            throw "release archive contains too many entries"
        }
        $actualEntries = @()
        [long]$expandedBytes = 0
        foreach ($entry in $archive.Entries) {
            $name = $entry.FullName.Replace('\', '/')
            $actualEntries += $name
            $expandedBytes += $entry.Length
            if ($entry.Length -gt 100MB) {
                throw "release archive entry exceeds the 100 MiB safety limit: $name"
            }
        }
        if ($expandedBytes -gt 200MB) {
            throw "release archive exceeds the 200 MiB expanded-size safety limit"
        }
        $differences = @(Compare-Object -ReferenceObject $expectedEntries -DifferenceObject ($actualEntries | Sort-Object))
        if ($differences.Count -ne 0) {
            throw "release archive contains an unexpected or missing entry"
        }
    }
    finally {
        $archive.Dispose()
    }
}

function Assert-RegularFile {
    param([Parameter(Mandatory = $true)][string]$Path)

    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "expected a regular file: $Path"
    }
}

function Assert-VersionOutput {
    param(
        [Parameter(Mandatory = $true)][string]$Binary,
        [Parameter(Mandatory = $true)][string]$CommandName,
        [Parameter(Mandatory = $true)][string]$ReleaseVersion
    )

    $expected = "$CommandName $($ReleaseVersion.Substring(1))"
    $output = (& $Binary --version 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $output -cne $expected) {
        throw "$CommandName verification failed; expected '$expected'"
    }
}

function Assert-SafeInstallDirectory {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not [IO.Path]::IsPathRooted($Path)) {
        throw "install directory must be an absolute path"
    }
    if (Test-Path -LiteralPath $Path) {
        $item = Get-Item -LiteralPath $Path -Force
        if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "install directory must be a real directory, not a file or reparse point"
        }
    }
}

function Install-Binaries {
    param(
        [Parameter(Mandatory = $true)][string]$PayloadDirectory,
        [Parameter(Mandatory = $true)][string]$DestinationDirectory,
        [Parameter(Mandatory = $true)][string]$ReleaseVersion
    )

    Assert-SafeInstallDirectory -Path $DestinationDirectory
    New-Item -ItemType Directory -Force -Path $DestinationDirectory | Out-Null
    Assert-SafeInstallDirectory -Path $DestinationDirectory

    $transaction = [Guid]::NewGuid().ToString("N")
    $names = @("ctx.exe", "ctxwake.exe")
    $backups = @{}
    $installed = @()

    try {
        foreach ($name in $names) {
            $destination = Join-Path $DestinationDirectory $name
            if (Test-Path -LiteralPath $destination) {
                Assert-RegularFile -Path $destination
            }
            $staged = Join-Path $DestinationDirectory ".$name.$transaction.new.exe"
            Copy-Item -LiteralPath (Join-Path $PayloadDirectory $name) -Destination $staged
            Assert-RegularFile -Path $staged
        }

        Assert-VersionOutput `
            -Binary (Join-Path $DestinationDirectory ".ctx.exe.$transaction.new.exe") `
            -CommandName "ctx" `
            -ReleaseVersion $ReleaseVersion

        try {
            foreach ($name in $names) {
                $destination = Join-Path $DestinationDirectory $name
                $staged = Join-Path $DestinationDirectory ".$name.$transaction.new.exe"
                if (Test-Path -LiteralPath $destination) {
                    $backup = Join-Path $DestinationDirectory ".$name.$transaction.backup"
                    Move-Item -LiteralPath $destination -Destination $backup
                    $backups[$name] = $backup
                }
                Move-Item -LiteralPath $staged -Destination $destination
                $installed += $name
            }

            Assert-VersionOutput -Binary (Join-Path $DestinationDirectory "ctx.exe") -CommandName "ctx" -ReleaseVersion $ReleaseVersion
            Assert-VersionOutput -Binary (Join-Path $DestinationDirectory "ctxwake.exe") -CommandName "ctxwake" -ReleaseVersion $ReleaseVersion
        }
        catch {
            foreach ($name in $installed) {
                $destination = Join-Path $DestinationDirectory $name
                if (Test-Path -LiteralPath $destination) {
                    Remove-Item -LiteralPath $destination -Force
                }
            }
            foreach ($name in $backups.Keys) {
                Move-Item -LiteralPath $backups[$name] -Destination (Join-Path $DestinationDirectory $name) -Force
            }
            throw
        }
    }
    finally {
        foreach ($name in $names) {
            foreach ($suffix in @("new.exe", "backup")) {
                $temporaryFile = Join-Path $DestinationDirectory ".$name.$transaction.$suffix"
                if (Test-Path -LiteralPath $temporaryFile) {
                    Remove-Item -LiteralPath $temporaryFile -Force
                }
            }
        }
    }
}

function Normalize-PathEntry {
    param([Parameter(Mandatory = $true)][string]$Value)

    $trimmed = [Environment]::ExpandEnvironmentVariables($Value.Trim().Trim('"'))
    try {
        return [IO.Path]::GetFullPath($trimmed).TrimEnd('\', '/').ToLowerInvariant()
    }
    catch {
        return $trimmed.TrimEnd('\', '/').ToLowerInvariant()
    }
}

function Test-PathContains {
    param(
        [AllowEmptyString()][string]$PathValue,
        [Parameter(Mandatory = $true)][string]$Directory
    )

    $wanted = Normalize-PathEntry -Value $Directory
    foreach ($entry in @($PathValue -split ';')) {
        if (-not [string]::IsNullOrWhiteSpace($entry) -and
            (Normalize-PathEntry -Value $entry) -ceq $wanted) {
            return $true
        }
    }
    return $false
}

function Publish-EnvironmentChange {
    try {
        if ($null -eq ("ContextWakeInstaller.NativeMethods" -as [Type])) {
            Add-Type @'
using System;
using System.Runtime.InteropServices;
namespace ContextWakeInstaller {
    public static class NativeMethods {
        [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
        public static extern IntPtr SendMessageTimeout(
            IntPtr hWnd, uint message, UIntPtr wParam, string lParam,
            uint flags, uint timeout, out UIntPtr result);
    }
}
'@
        }
        $result = [UIntPtr]::Zero
        [void][ContextWakeInstaller.NativeMethods]::SendMessageTimeout(
            [IntPtr]0xffff,
            0x001A,
            [UIntPtr]::Zero,
            "Environment",
            0x0002,
            2000,
            [ref]$result
        )
    }
    catch {
        Write-Warning "PATH was saved, but other open applications may need to be restarted."
    }
}

function Add-UserPath {
    param([Parameter(Mandatory = $true)][string]$Directory)

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $marker = Join-Path $Directory $PathMarkerName
    if (-not (Test-PathContains -PathValue $userPath -Directory $Directory)) {
        $newUserPath = if ([string]::IsNullOrWhiteSpace($userPath)) {
            $Directory
        }
        else {
            $userPath + ';' + $Directory
        }
        Set-Content -LiteralPath $marker -Value "user-path" -Encoding ASCII
        try {
            [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
        }
        catch {
            Remove-Item -LiteralPath $marker -Force
            throw
        }
        Publish-EnvironmentChange
        Write-Step "Updating user PATH... OK"
    }
    else {
        Write-Step "User PATH already contains the install directory."
    }

    if (-not (Test-PathContains -PathValue $env:Path -Directory $Directory)) {
        $env:Path = $Directory + ';' + $env:Path
    }
}

function Invoke-ContextWakeInstall {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw "scripts/install.ps1 supports Windows only; use scripts/install.sh on Linux or macOS"
    }

    $nativeArchitecture = if (-not [string]::IsNullOrWhiteSpace($env:PROCESSOR_ARCHITEW6432)) {
        $env:PROCESSOR_ARCHITEW6432
    }
    else {
        $env:PROCESSOR_ARCHITECTURE
    }
    if (-not [string]::IsNullOrWhiteSpace($env:CONTEXTWAKE_TEST_ARCHITECTURE)) {
        $nativeArchitecture = $env:CONTEXTWAKE_TEST_ARCHITECTURE
    }
    if ($nativeArchitecture -notmatch '^(?i:AMD64)$') {
        throw "ContextWake does not currently publish a binary for Windows/$nativeArchitecture. Build from source: $RepositoryUrl#build-from-source"
    }

    if ([string]::IsNullOrWhiteSpace($Version)) {
        $Version = "latest"
    }
    if ([string]::IsNullOrWhiteSpace($InstallDir)) {
        if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
            throw "LOCALAPPDATA is unavailable; provide an absolute -InstallDir"
        }
        $InstallDir = Join-Path $env:LOCALAPPDATA "ContextWake\bin"
    }
    $InstallDir = [IO.Path]::GetFullPath([Environment]::ExpandEnvironmentVariables($InstallDir))

    if ([Enum]::GetNames([Net.SecurityProtocolType]) -contains "Tls12") {
        [Net.ServicePointManager]::SecurityProtocol =
            [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    }

    $temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) ("contextwake-install-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null
    try {
        $assetLabel = "windows-x86_64"
        $resolvedVersion = Resolve-ReleaseVersion `
            -RequestedVersion $Version `
            -AssetLabel $assetLabel `
            -TemporaryDirectory $temporaryDirectory
        $archiveName = "contextwake-$resolvedVersion-$assetLabel.zip"
        $archiveRoot = "contextwake-$resolvedVersion-$assetLabel"
        $releaseBase = "$RepositoryUrl/releases/download/$resolvedVersion"
        $archivePath = Join-Path $temporaryDirectory $archiveName
        $manifestPath = Join-Path $temporaryDirectory "SHA256SUMS"
        $extractDirectory = Join-Path $temporaryDirectory "extract"

        Write-Host "ContextWake Installer"
        Write-Host ""
        Write-Host "Platform: Windows x86_64"
        Write-Host "Version:  $resolvedVersion"
        Write-Host "Install:  $InstallDir"
        Write-Host ""

        Write-Step "Downloading release..."
        Invoke-TrustedDownload -Uri "$releaseBase/$archiveName" -OutFile $archivePath
        Invoke-TrustedDownload -Uri "$releaseBase/SHA256SUMS" -OutFile $manifestPath

        if ($env:CONTEXTWAKE_TEST_CORRUPT_ARCHIVE -eq "1") {
            [IO.File]::AppendAllText($archivePath, "corrupt")
        }

        Write-Step "Verifying SHA-256..."
        $expectedHash = Get-ManifestChecksum -ManifestPath $manifestPath -AssetName $archiveName
        $actualHash = Get-Sha256Hex -Path $archivePath
        if ($actualHash -cne $expectedHash) {
            throw "checksum mismatch for $archiveName. Expected: $expectedHash Actual: $actualHash. Installation aborted."
        }
        Write-Step "Verifying SHA-256... OK"

        Assert-SafeArchive -ArchivePath $archivePath -ArchiveRoot $archiveRoot
        New-Item -ItemType Directory -Path $extractDirectory | Out-Null
        [System.IO.Compression.ZipFile]::ExtractToDirectory($archivePath, $extractDirectory)
        $payloadDirectory = Join-Path $extractDirectory $archiveRoot
        foreach ($name in @("ctx.exe", "ctxwake.exe")) {
            Assert-RegularFile -Path (Join-Path $payloadDirectory $name)
        }

        Write-Step "Installing ctx and ctxwake..."
        Install-Binaries `
            -PayloadDirectory $payloadDirectory `
            -DestinationDirectory $InstallDir `
            -ReleaseVersion $resolvedVersion
        Write-Step "Installing ctx and ctxwake... OK"

        if (-not $NoPathUpdate -and $env:CONTEXTWAKE_NO_PATH_UPDATE -ne "1") {
            Add-UserPath -Directory $InstallDir
        }

        Write-Host ""
        Write-Host "ContextWake installed successfully."
        Write-Host ""
        Write-Host "Next:"
        Write-Host "  ctx doctor"
        Write-Host "  ctx"
    }
    finally {
        if (Test-Path -LiteralPath $temporaryDirectory) {
            Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
        }
    }
}

Invoke-ContextWakeInstall
