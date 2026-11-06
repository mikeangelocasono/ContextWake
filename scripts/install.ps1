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
$DefaultReleaseVersion = "v0.1.0-alpha.3"
$PathMarkerName = ".contextwake-path-added"
$UserEnvironmentKeyPath = "Environment"
# Test hook CONTEXTWAKE_TEST_ENVIRONMENT_KEY may only redirect PATH updates to a disposable key below this one.
$TestEnvironmentKeyPattern = '^Software\\ContextWakeInstallerTests\\[0-9A-Za-z_-]{1,64}$'
# Staged and backup files of one install transaction: .<binary>.<32 hex digits>.new.exe / .backup
$TransactionFilePattern = '^\.(ctx|ctxwake)\.exe\.[0-9a-f]{32}\.(new\.exe|backup)$'

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

function Get-TestReleaseFile {
    # Maps a release URL to its copy in the local mirror named by the CONTEXTWAKE_TEST_RELEASE_DIR test hook,
    # which uses GitHub's layout (releases.atom, releases/download/<tag>/<asset>).
    param([Parameter(Mandatory = $true)][string]$Uri)

    $prefix = "$RepositoryUrl/"
    if (-not $Uri.StartsWith($prefix, [StringComparison]::Ordinal)) {
        throw "the test release mirror cannot serve $Uri"
    }
    $relative = $Uri.Substring($prefix.Length)
    if ($relative -cnotmatch '^(releases\.atom|releases/download/v[0-9A-Za-z.-]+/[A-Za-z0-9][A-Za-z0-9._-]*)$') {
        throw "the test release mirror cannot serve $Uri"
    }
    return Join-Path $env:CONTEXTWAKE_TEST_RELEASE_DIR ($relative.Replace('/', '\'))
}

function Invoke-TrustedDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$OutFile
    )

    $parsedUri = [Uri]$Uri
    Assert-TrustedResponseUri -Uri $parsedUri
    if (-not [string]::IsNullOrEmpty($env:CONTEXTWAKE_TEST_RELEASE_DIR)) {
        $mirrored = Get-TestReleaseFile -Uri $Uri
        if (-not (Test-Path -LiteralPath $mirrored -PathType Leaf)) {
            throw "the test release mirror has no file for $Uri"
        }
        Copy-Item -LiteralPath $mirrored -Destination $OutFile
        return
    }
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
            throw "version must be 'latest' or a release tag such as v0.1.0-alpha.3"
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
    # Native stderr must not become a terminating error before the exit code is known.
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $output = ((@(& $Binary --version 2>&1) | ForEach-Object { "$_" }) -join "`n").Trim()
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $previousPreference
    }
    if ($exitCode -ne 0 -or $output -cne $expected) {
        $hint = ""
        if ($exitCode -eq -1073741515) {
            $hint = " (0xC0000135 STATUS_DLL_NOT_FOUND: a DLL the binary needs is missing)"
        }
        throw "$CommandName verification failed: expected '$expected', but '$Binary --version' exited with code $exitCode$hint and printed '$output'"
    }
}

function Test-FullyQualifiedPath {
    # True for X:\... and \\server\share\... ; false for relative (bin, .\bin), drive-relative (C:bin) and
    # root-relative (\bin) paths, which Windows resolves against a current directory.
    param([AllowEmptyString()][string]$Path)

    return ($Path -match '^[A-Za-z]:[\\/]') -or ($Path -match '^[\\/]{2}[^\\/?.][^\\/]*[\\/][^\\/]')
}

function ConvertFrom-VerbatimPath {
    param([AllowEmptyString()][string]$Path)

    if ($Path.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) {
        return '\\' + $Path.Substring(8)
    }
    if ($Path.StartsWith('\\?\', [StringComparison]::Ordinal)) {
        return $Path.Substring(4)
    }
    return $Path
}

function Resolve-InstallDirectory {
    param([AllowEmptyString()][string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path)) {
        if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
            throw "LOCALAPPDATA is unavailable; provide an absolute -InstallDir"
        }
        $Path = Join-Path $env:LOCALAPPDATA "ContextWake\bin"
    }
    $candidate = ConvertFrom-VerbatimPath -Path ([Environment]::ExpandEnvironmentVariables($Path.Trim()))
    # Check before GetFullPath: it would silently resolve a relative path against the process working
    # directory, which Set-Location does not change.
    if (-not (Test-FullyQualifiedPath -Path $candidate)) {
        throw "install directory must be an absolute path such as C:\Tools\ContextWake, not '$Path'"
    }
    try {
        $fullPath = [IO.Path]::GetFullPath($candidate)
    }
    catch {
        throw "install directory is not a valid path: '$Path'"
    }
    if ($fullPath.Length -gt [IO.Path]::GetPathRoot($fullPath).Length) {
        $fullPath = $fullPath.TrimEnd('\', '/')
    }
    return $fullPath
}

function Assert-SafeInstallDirectory {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-FullyQualifiedPath -Path $Path)) {
        throw "install directory must be an absolute path"
    }
    if (Test-Path -LiteralPath $Path) {
        $item = Get-Item -LiteralPath $Path -Force
        if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "install directory must be a real directory, not a file or reparse point"
        }
    }
}

function Remove-FileBestEffort {
    param([Parameter(Mandatory = $true)][string]$Path)

    try {
        if (Test-Path -LiteralPath $Path) {
            Remove-Item -LiteralPath $Path -Force -ErrorAction Stop
        }
        return $true
    }
    catch {
        return $false
    }
}

function Remove-StaleTransactionFiles {
    # Windows can rename a running executable but cannot delete it, so the backup of a binary that was
    # still running during an earlier update stays behind. Delete those leftovers once they are free.
    # Returns the names of matching files that are still in use.
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [string]$Pattern = $TransactionFilePattern
    )

    $remaining = @()
    if (Test-Path -LiteralPath $Directory -PathType Container) {
        foreach ($item in @(Get-ChildItem -LiteralPath $Directory -Force -File)) {
            if ($item.Name -cmatch $Pattern -and -not (Remove-FileBestEffort -Path $item.FullName)) {
                $remaining += $item.Name
            }
        }
    }
    return $remaining
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
    $null = Remove-StaleTransactionFiles -Directory $DestinationDirectory

    $transaction = [Guid]::NewGuid().ToString("N")
    $names = @("ctx.exe", "ctxwake.exe")
    $backups = @{}
    $installed = @()
    $preserved = @{}

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
                    # Renaming works even while the old binary is running.
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
            $installError = $_
            foreach ($name in $installed) {
                $null = Remove-FileBestEffort -Path (Join-Path $DestinationDirectory $name)
            }
            foreach ($name in @($backups.Keys)) {
                try {
                    Move-Item -LiteralPath $backups[$name] -Destination (Join-Path $DestinationDirectory $name) -Force -ErrorAction Stop
                }
                catch {
                    # Never delete the only remaining copy of the previous binary. Every later install and
                    # uninstall deletes transaction files (.backup), so move it to a name outside that pattern.
                    $kept = $backups[$name]
                    $preserved[$kept] = $true
                    $previous = Join-Path $DestinationDirectory ".$name.$transaction.previous.exe"
                    try {
                        Move-Item -LiteralPath $kept -Destination $previous -ErrorAction Stop
                        $kept = $previous
                    }
                    catch {
                        # Keep the backup name; this run still does not delete it.
                    }
                    Write-Warning "Could not restore the previous $name; it is kept as $kept. Rename it to $name to use it again."
                }
            }
            throw $installError
        }
    }
    finally {
        $inUse = @()
        foreach ($name in $names) {
            foreach ($suffix in @("new.exe", "backup")) {
                $temporaryFile = Join-Path $DestinationDirectory ".$name.$transaction.$suffix"
                if (-not $preserved.ContainsKey($temporaryFile) -and -not (Remove-FileBestEffort -Path $temporaryFile)) {
                    $inUse += $temporaryFile
                }
            }
        }
        if ($inUse.Count -gt 0) {
            Write-Warning ("A previous ctx or ctxwake process is still running, so these old files could not be deleted yet: " +
                ($inUse -join ", ") + ". The next install or uninstall removes them after that process exits.")
        }
    }
}

function ConvertTo-PathComparisonKey {
    # Returns a normalized form of an absolute PATH entry, or $null for an entry that is empty or relative
    # (a relative entry depends on the current directory and never names the install directory).
    param([AllowEmptyString()][string]$Value)

    $text = [Environment]::ExpandEnvironmentVariables($Value.Trim().Trim('"').Trim())
    $text = ConvertFrom-VerbatimPath -Path $text
    if (-not (Test-FullyQualifiedPath -Path $text)) {
        return $null
    }
    try {
        $text = [IO.Path]::GetFullPath($text)
    }
    catch {
        # Keep the expanded text when the entry is not a valid path.
    }
    return $text.TrimEnd('\', '/')
}

function Test-SamePathEntry {
    param(
        [AllowEmptyString()][string]$Entry,
        [Parameter(Mandatory = $true)][string]$Directory
    )

    $entryKey = ConvertTo-PathComparisonKey -Value $Entry
    $directoryKey = ConvertTo-PathComparisonKey -Value $Directory
    return ($null -ne $entryKey) -and ($null -ne $directoryKey) -and
        [string]::Equals($entryKey, $directoryKey, [StringComparison]::OrdinalIgnoreCase)
}

function Test-PathContains {
    param(
        [AllowEmptyString()][string]$PathValue,
        [Parameter(Mandatory = $true)][string]$Directory
    )

    foreach ($entry in $PathValue.Split([char[]]@(';'))) {
        if (Test-SamePathEntry -Entry $entry -Directory $Directory) {
            return $true
        }
    }
    return $false
}

function Add-PathEntry {
    # Appends Directory as one new entry. Every existing character of PathValue, including %VAR%
    # references, quotes, spacing and empty entries, is kept unchanged so uninstall can restore it exactly.
    param(
        [AllowEmptyString()][string]$PathValue,
        [Parameter(Mandatory = $true)][string]$Directory
    )

    if ($PathValue.Length -eq 0) {
        return $Directory
    }
    if ($PathValue.EndsWith(';')) {
        return $PathValue + $Directory + ';'
    }
    return $PathValue + ';' + $Directory
}

function Get-UserEnvironmentKeyPath {
    $override = $env:CONTEXTWAKE_TEST_ENVIRONMENT_KEY
    if ([string]::IsNullOrEmpty($override)) {
        return $UserEnvironmentKeyPath
    }
    if ($override -notmatch $TestEnvironmentKeyPattern) {
        throw "CONTEXTWAKE_TEST_ENVIRONMENT_KEY must name a key below HKCU\Software\ContextWakeInstallerTests"
    }
    return $override
}

function Get-UserPathValue {
    # Reads HKCU\<KeyPath>\Path without expanding %VAR% references and reports its registry type.
    param([Parameter(Mandatory = $true)][string]$KeyPath)

    $missing = New-Object PSObject -Property @{
        Exists = $false
        Value = ""
        Kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
    }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($KeyPath, $false)
    if ($null -eq $key) {
        return $missing
    }
    try {
        $value = $key.GetValue("Path", $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($null -eq $value) {
            return $missing
        }
        $kind = $key.GetValueKind("Path")
        if ($kind -ne [Microsoft.Win32.RegistryValueKind]::ExpandString -and
            $kind -ne [Microsoft.Win32.RegistryValueKind]::String) {
            throw "the user PATH (HKCU\$KeyPath\Path) has the unsupported registry type $kind; change it to REG_EXPAND_SZ or REG_SZ, or rerun with -NoPathUpdate to leave PATH unchanged"
        }
        return New-Object PSObject -Property @{
            Exists = $true
            Value = [string]$value
            Kind = $kind
        }
    }
    finally {
        $key.Dispose()
    }
}

function Set-UserPathValue {
    # Writes the whole raw value with its original registry type (REG_EXPAND_SZ stays REG_EXPAND_SZ);
    # an empty value removes Path again.
    param(
        [Parameter(Mandatory = $true)][string]$KeyPath,
        [AllowEmptyString()][string]$Value,
        [Parameter(Mandatory = $true)][Microsoft.Win32.RegistryValueKind]$Kind
    )

    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($KeyPath)
    if ($null -eq $key) {
        throw "could not open HKCU\$KeyPath for writing"
    }
    try {
        if ($Value.Length -eq 0) {
            $key.DeleteValue("Path", $false)
        }
        else {
            $key.SetValue("Path", $Value, $Kind)
        }
    }
    finally {
        $key.Dispose()
    }
}

function Publish-EnvironmentChange {
    # A registry write alone notifies nobody. Broadcasting WM_SETTINGCHANGE("Environment") makes Explorer
    # reload the user environment, so terminals opened afterwards see the new PATH without signing out.
    param([Parameter(Mandatory = $true)][string]$KeyPath)

    try {
        if ($null -eq ("ContextWakeInstaller.NativeMethods" -as [Type])) {
            Add-Type @'
using System;
using System.Runtime.InteropServices;
namespace ContextWakeInstaller {
    public static class NativeMethods {
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern IntPtr SendMessageTimeout(
            IntPtr hWnd, uint message, UIntPtr wParam, string lParam,
            uint flags, uint timeout, out UIntPtr result);
    }
}
'@
        }
        $result = [UIntPtr]::Zero
        $sent = [ContextWakeInstaller.NativeMethods]::SendMessageTimeout(
            [IntPtr]0xffff,
            0x001A,
            [UIntPtr]::Zero,
            "Environment",
            0x0002,
            2000,
            [ref]$result
        )
        if ($sent -ne [IntPtr]::Zero) {
            return $true
        }
    }
    catch {
        # Add-Type can be unavailable (for example when compilation is blocked); use the fallback.
    }
    if ($KeyPath -eq $UserEnvironmentKeyPath) {
        try {
            # Deleting a user variable that does not exist changes nothing, but .NET still broadcasts
            # WM_SETTINGCHANGE("Environment") afterwards.
            [Environment]::SetEnvironmentVariable("CONTEXTWAKE_INSTALLER_NOTIFY_" + [Guid]::NewGuid().ToString("N"), $null, "User")
            return $true
        }
        catch {
            # Fall through to the warning.
        }
    }
    Write-Warning "PATH was saved, but running programs were not notified. If a new terminal cannot find ctx, sign out and back in."
    return $false
}

function Add-UserPath {
    param([Parameter(Mandatory = $true)][string]$Directory)

    $keyPath = Get-UserEnvironmentKeyPath
    $userPath = Get-UserPathValue -KeyPath $keyPath
    $marker = Join-Path $Directory $PathMarkerName
    if (Test-PathContains -PathValue $userPath.Value -Directory $Directory) {
        Write-Step "User PATH already contains the install directory."
    }
    else {
        $newUserPath = Add-PathEntry -PathValue $userPath.Value -Directory $Directory
        Set-Content -LiteralPath $marker -Value "user-path" -Encoding ASCII
        try {
            Set-UserPathValue -KeyPath $keyPath -Value $newUserPath -Kind $userPath.Kind
        }
        catch {
            Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue
            throw
        }
        $null = Publish-EnvironmentChange -KeyPath $keyPath
        Write-Step "Updating user PATH... OK"
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
    $InstallDir = Resolve-InstallDirectory -Path $InstallDir
    $updatePath = -not $NoPathUpdate -and $env:CONTEXTWAKE_NO_PATH_UPDATE -ne "1"
    if ($updatePath) {
        if ($InstallDir.Contains(';')) {
            throw "an install directory containing ';' cannot be added to PATH; choose another directory or pass -NoPathUpdate"
        }
        # Fail before downloading anything if the user PATH cannot be updated safely.
        $null = Get-UserPathValue -KeyPath (Get-UserEnvironmentKeyPath)
    }
    if (-not [string]::IsNullOrEmpty($env:CONTEXTWAKE_TEST_RELEASE_DIR)) {
        Write-Warning "CONTEXTWAKE_TEST_RELEASE_DIR is set; release files are read from $env:CONTEXTWAKE_TEST_RELEASE_DIR instead of GitHub."
    }

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

        if ($updatePath) {
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

# CONTEXTWAKE_INSTALLER_SOURCE_ONLY=1 only defines the functions (used by scripts/test-windows-installer.ps1).
if ($env:CONTEXTWAKE_INSTALLER_SOURCE_ONLY -ne "1") {
    Invoke-ContextWakeInstall
}
