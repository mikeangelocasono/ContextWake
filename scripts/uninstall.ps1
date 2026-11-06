[CmdletBinding()]
param(
    [string]$InstallDir = $env:CONTEXTWAKE_INSTALL_DIR,
    # Leave the user PATH unchanged, for example when its registry value cannot be edited safely. The
    # installer-owned entry and its marker stay, so a later uninstall without this switch removes both.
    [switch]$NoPathUpdate
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

# The helpers shared with scripts/install.ps1 must stay identical; scripts/test-windows-installer.ps1 checks it.
$PathMarkerName = ".contextwake-path-added"
$UserEnvironmentKeyPath = "Environment"
$TestEnvironmentKeyPattern = '^Software\\ContextWakeInstallerTests\\[0-9A-Za-z_-]{1,64}$'
$TransactionFilePattern = '^\.(ctx|ctxwake)\.exe\.[0-9a-f]{32}\.(new\.exe|backup)$'
# A previous binary that a failed update could not restore (install.ps1 keeps it under this name).
$PreviousBinaryPattern = '^\.(ctx|ctxwake)\.exe\.[0-9a-f]{32}\.previous\.exe$'

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

function Remove-PathEntry {
    # Removes the entry install.ps1 appended, together with one adjacent separator; -All removes every entry
    # naming Directory. All other characters of PathValue are kept unchanged.
    param(
        [AllowEmptyString()][string]$PathValue,
        [Parameter(Mandatory = $true)][string]$Directory,
        [switch]$All
    )

    $entries = New-Object System.Collections.Generic.List[string]
    $entries.AddRange([string[]]$PathValue.Split([char[]]@(';')))
    $targets = @()
    for ($index = $entries.Count - 1; $index -ge 0; $index--) {
        if (Test-SamePathEntry -Entry $entries[$index] -Directory $Directory) {
            $targets += $index
        }
    }
    if ($targets.Count -eq 0) {
        return $PathValue
    }
    if (-not $All) {
        # install.ps1 appends Directory verbatim, so the last entry with exactly that text (ignoring case) is
        # the installer's. Another spelling of the same directory, such as a %VARIABLE% form, is the user's own
        # and is removed only when no verbatim entry is left.
        $verbatim = @($targets | Where-Object { [string]::Equals($entries[$_], $Directory, [StringComparison]::OrdinalIgnoreCase) })
        if ($verbatim.Count -gt 0) {
            $targets = @($verbatim[0])
        }
        else {
            $targets = @($targets[0])
        }
    }
    # Indexes are in descending order, so removing one does not shift the others.
    foreach ($index in $targets) {
        $entries.RemoveAt($index)
    }
    return ($entries -join ';')
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

function Remove-InstalledBinary {
    # Deletes one installed binary. A running executable cannot be deleted on Windows but can be renamed, so
    # an in-use binary is renamed to a transaction backup that a later install or uninstall deletes.
    # Returns the backup file name when the binary could not be deleted yet.
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [Parameter(Mandatory = $true)][string]$Name
    )

    $binary = Join-Path $Directory $Name
    if (-not (Test-Path -LiteralPath $binary)) {
        return $null
    }
    $item = Get-Item -LiteralPath $binary -Force
    if ($item.PSIsContainer) {
        throw "refusing to remove a directory at the expected binary path: $binary"
    }
    if (Remove-FileBestEffort -Path $binary) {
        return $null
    }
    $backupName = ".$Name." + [Guid]::NewGuid().ToString("N") + ".backup"
    Move-Item -LiteralPath $binary -Destination (Join-Path $Directory $backupName)
    if (Remove-FileBestEffort -Path (Join-Path $Directory $backupName)) {
        return $null
    }
    return $backupName
}

function Invoke-ContextWakeUninstall {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw "scripts/uninstall.ps1 supports Windows only"
    }
    $directory = Resolve-InstallDirectory -Path $InstallDir
    if (Test-Path -LiteralPath $directory) {
        $installItem = Get-Item -LiteralPath $directory -Force
        if (-not $installItem.PSIsContainer -or ($installItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "install directory must be a real directory, not a file or reparse point"
        }
    }

    $pathMarker = Join-Path $directory $PathMarkerName
    $ownsPathEntry = Test-Path -LiteralPath $pathMarker -PathType Leaf
    $removeOwnedPath = $ownsPathEntry -and -not $NoPathUpdate
    if ($removeOwnedPath) {
        $keyPath = Get-UserEnvironmentKeyPath
        # Fail before deleting anything if the user PATH cannot be updated safely.
        $null = Get-UserPathValue -KeyPath $keyPath
    }

    $inUse = @(Remove-StaleTransactionFiles -Directory $directory)
    $inUse += @(Remove-StaleTransactionFiles -Directory $directory -Pattern $PreviousBinaryPattern)
    foreach ($name in @("ctx.exe", "ctxwake.exe")) {
        $leftover = Remove-InstalledBinary -Directory $directory -Name $name
        if ($null -ne $leftover) {
            $inUse += $leftover
        }
    }

    if ($removeOwnedPath) {
        $userPath = Get-UserPathValue -KeyPath $keyPath
        $newUserPath = Remove-PathEntry -PathValue $userPath.Value -Directory $directory
        if ($newUserPath -cne $userPath.Value) {
            Set-UserPathValue -KeyPath $keyPath -Value $newUserPath -Kind $userPath.Kind
            $null = Publish-EnvironmentChange -KeyPath $keyPath
        }
        $env:Path = Remove-PathEntry -PathValue $env:Path -Directory $directory -All
        Remove-Item -LiteralPath $pathMarker -Force
    }

    if (Test-Path -LiteralPath $directory -PathType Container) {
        $remaining = @(Get-ChildItem -Force -LiteralPath $directory)
        if ($remaining.Count -eq 0) {
            Remove-Item -LiteralPath $directory -Force
        }
    }

    Write-Host "ContextWake binaries were removed from $directory."
    if ($inUse.Count -gt 0) {
        Write-Warning ("A ctx or ctxwake process is still running, so these files could not be deleted yet: " +
            ($inUse -join ", ") + ". Close it and rerun the uninstaller to delete them and the empty install directory.")
    }
    if ($removeOwnedPath) {
        Write-Host "The installer-owned user PATH entry was removed."
    }
    elseif ($ownsPathEntry) {
        Write-Host "The user PATH was not changed (-NoPathUpdate). Rerun without -NoPathUpdate to remove the installer-owned entry and $directory."
    }
    Write-Host "Profiles, workspaces, sessions, checkpoints, handoffs, and configuration were preserved."
}

# CONTEXTWAKE_INSTALLER_SOURCE_ONLY=1 only defines the functions (used by scripts/test-windows-installer.ps1).
if ($env:CONTEXTWAKE_INSTALLER_SOURCE_ONLY -ne "1") {
    Invoke-ContextWakeUninstall
}
