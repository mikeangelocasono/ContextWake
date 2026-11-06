<#
.SYNOPSIS
Regression tests for the Windows installer, uninstaller, release packaging, and release workflow hygiene.

.DESCRIPTION
Every test uses disposable state. Binaries go to a temporary directory, release files come from a temporary
local mirror (CONTEXTWAKE_TEST_RELEASE_DIR), and PATH changes go to a temporary key below
HKCU\Software\ContextWakeInstallerTests (CONTEXTWAKE_TEST_ENVIRONMENT_KEY). The real HKCU\Environment PATH
is only read, before and after the run, to prove that it was not modified.

.PARAMETER Archive
contextwake-v<version>-windows-x86_64.zip built by scripts/package-release.ps1. Tests that install need it.

.PARAMETER PowerShellPath
PowerShell that runs the scripts under test. Defaults to the PowerShell running this file, so running this
file with powershell.exe and with pwsh covers Windows PowerShell 5.1 and PowerShell 7.

.PARAMETER Only
Run only the tests whose names match this regular expression.

.EXAMPLE
cargo build --locked --release
./scripts/package-release.ps1 -Target x86_64-pc-windows-gnu -Label windows-x86_64 -BinaryDirectory target/release
./scripts/test-windows-installer.ps1 -Archive dist/contextwake-v0.1.0-alpha.3-windows-x86_64.zip
#>
[CmdletBinding()]
param(
    [string]$Archive = "",
    [string]$PowerShellPath = "",
    [string]$Only = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0
$ProgressPreference = "SilentlyContinue"

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "scripts/test-windows-installer.ps1 runs on Windows only"
}

$RepositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$Installer = Join-Path $PSScriptRoot "install.ps1"
$Uninstaller = Join-Path $PSScriptRoot "uninstall.ps1"
$RuntimeCheck = Join-Path $PSScriptRoot "check-windows-runtime.ps1"
$ChecksumWriter = Join-Path $PSScriptRoot "write-checksums.ps1"
$TestKeyRoot = "Software\ContextWakeInstallerTests"
$SampleDirectory = "C:\Users\Test User\AppData\Local\ContextWake\bin"
if ([string]::IsNullOrWhiteSpace($PowerShellPath)) {
    $PowerShellPath = (Get-Process -Id $PID).Path
}

$WorkRoot = Join-Path ([IO.Path]::GetTempPath()) ("contextwake-installer-tests-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $WorkRoot | Out-Null
# TEMP can be an 8.3 short path (GitHub's Windows runners use C:\Users\RUNNER~1\...). GetFullPath expands short
# names of existing directories, as the installer's own path resolution does, so expected paths match.
$WorkRoot = [IO.Path]::GetFullPath($WorkRoot)
$script:Results = @()
$script:CreatedKeys = @()
$script:Package = $null

# ---------------------------------------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------------------------------------

function Assert-True {
    param([bool]$Condition, [Parameter(Mandatory = $true)][string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

function Assert-Equal {
    param([AllowNull()][object]$Expected, [AllowNull()][object]$Actual, [Parameter(Mandatory = $true)][string]$Message)
    if (-not [object]::Equals($Expected, $Actual)) {
        throw "$Message`n  expected: [$Expected]`n  actual:   [$Actual]"
    }
}

function Assert-Throws {
    param([Parameter(Mandatory = $true)][scriptblock]$Body, [string]$Pattern = "", [Parameter(Mandatory = $true)][string]$Message)
    $failed = $false
    try {
        & $Body
    }
    catch {
        $failed = $true
        if ($Pattern -and $_.Exception.Message -notmatch $Pattern) {
            throw "$Message`n  error does not match '$Pattern': $($_.Exception.Message)"
        }
    }
    if (-not $failed) {
        throw "$Message`n  no error was raised"
    }
}

function Assert-Succeeded {
    param([Parameter(Mandatory = $true)][object]$Result, [Parameter(Mandatory = $true)][string]$Message)
    if ($Result.ExitCode -ne 0) {
        throw "$Message (exit code $($Result.ExitCode))`n$($Result.Output)"
    }
}

function Assert-Failed {
    param([Parameter(Mandatory = $true)][object]$Result, [string]$Pattern = "", [Parameter(Mandatory = $true)][string]$Message)
    if ($Result.ExitCode -eq 0) {
        throw "${Message}: the script succeeded but should have failed`n$($Result.Output)"
    }
    if ($Pattern -and $Result.Output -notmatch $Pattern) {
        throw "${Message}: output does not match '$Pattern'`n$($Result.Output)"
    }
}

function Assert-OutputContains {
    param([Parameter(Mandatory = $true)][object]$Result, [Parameter(Mandatory = $true)][string]$Text, [Parameter(Mandatory = $true)][string]$Message)
    if ($Result.Output.IndexOf($Text, [StringComparison]::Ordinal) -lt 0) {
        throw "${Message}: output does not contain '$Text'`n$($Result.Output)"
    }
}

function Test-Case {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [switch]$NeedsArchive,
        [Parameter(Mandatory = $true)][scriptblock]$Body
    )

    if ($Only -and $Name -notmatch $Only) {
        return
    }
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        if ($NeedsArchive -and $null -eq $script:Package) {
            throw "this test needs -Archive <contextwake-v<version>-windows-x86_64.zip>"
        }
        & $Body
        $script:Results += New-Object PSObject -Property @{ Name = $Name; Passed = $true }
        Write-Host ("PASS  {0} ({1:N1}s)" -f $Name, $watch.Elapsed.TotalSeconds)
    }
    catch {
        $script:Results += New-Object PSObject -Property @{ Name = $Name; Passed = $false }
        Write-Host ("FAIL  {0}" -f $Name)
        foreach ($line in ($_.Exception.Message -split "`n")) {
            Write-Host "      $line"
        }
    }
}

function Invoke-InScriptScope {
    # Loads a script's functions into a private scope (CONTEXTWAKE_INSTALLER_SOURCE_ONLY) and runs Body there.
    param(
        [Parameter(Mandatory = $true)][string]$Script,
        [Parameter(Mandatory = $true)][scriptblock]$Body,
        [object[]]$Arguments = @()
    )

    if (-not (Select-String -LiteralPath $Script -SimpleMatch 'CONTEXTWAKE_INSTALLER_SOURCE_ONLY' -Quiet)) {
        throw "$Script cannot be loaded without running it"
    }
    $previous = $env:CONTEXTWAKE_INSTALLER_SOURCE_ONLY
    $env:CONTEXTWAKE_INSTALLER_SOURCE_ONLY = "1"
    try {
        return & {
            param($scriptPath, $body, $bodyArguments)
            . $scriptPath
            & $body @bodyArguments
        } $Script $Body $Arguments
    }
    finally {
        $env:CONTEXTWAKE_INSTALLER_SOURCE_ONLY = $previous
    }
}

function Invoke-ChildPowerShell {
    # Runs PowerShell in a new process with extra environment variables and a working directory, and returns
    # its exit code and combined output.
    param(
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [hashtable]$Environment = @{},
        [string]$WorkingDirectory = $WorkRoot
    )

    $saved = @{}
    foreach ($name in @($Environment.Keys)) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        [Environment]::SetEnvironmentVariable($name, $Environment[$name], "Process")
    }
    Push-Location -LiteralPath $WorkingDirectory
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $output = @(& $PowerShellPath -NoProfile -NonInteractive -ExecutionPolicy Bypass @Arguments 2>&1 | ForEach-Object { "$_" })
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $previousPreference
        Pop-Location
        foreach ($name in @($saved.Keys)) {
            [Environment]::SetEnvironmentVariable($name, $saved[$name], "Process")
        }
    }
    return New-Object PSObject -Property @{ ExitCode = $exitCode; Output = ($output -join "`n") }
}

function Invoke-ChildCommand {
    # Runs a command in a new PowerShell process via -EncodedCommand (no quoting pitfalls). The process starts in
    # WorkingDirectory; a command that calls Set-Location then has a PowerShell location that differs from its
    # process working directory, as in an interactive Windows PowerShell session after 'cd'.
    param(
        [Parameter(Mandatory = $true)][string]$Command,
        [hashtable]$Environment = @{},
        [string]$WorkingDirectory = $WorkRoot
    )

    # With -EncodedCommand, powershell.exe renders warning records through its host as CLIXML for the parent
    # instead of text, even after *>&1, so turn every record into a string inside the child. Stop makes every
    # error, including statement-terminating ones such as parameter binding failures, end the child with a
    # non-zero exit code.
    $wrapped = "`$ErrorActionPreference = 'Stop'; `$ProgressPreference = 'SilentlyContinue'; & { " + $Command + " } *>&1 | ForEach-Object { `"`$_`" }"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($wrapped))
    return Invoke-ChildPowerShell -Arguments @("-EncodedCommand", $encoded) -Environment $Environment -WorkingDirectory $WorkingDirectory
}

function ConvertTo-PowerShellLiteral {
    param([AllowEmptyString()][string]$Value)
    return "'" + $Value.Replace("'", "''") + "'"
}

function New-TestDirectory {
    param([Parameter(Mandatory = $true)][string]$Name)
    $path = Join-Path $WorkRoot $Name
    New-Item -ItemType Directory -Path $path -Force | Out-Null
    return $path
}

function New-TestEnvironmentKey {
    # Creates a disposable stand-in for HKCU\Environment, optionally with a Path value of the given type.
    param([AllowNull()][object]$PathValue = $null, [Microsoft.Win32.RegistryValueKind]$Kind = [Microsoft.Win32.RegistryValueKind]::ExpandString)

    $keyPath = "$TestKeyRoot\" + [Guid]::NewGuid().ToString("N")
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($keyPath)
    try {
        if ($null -ne $PathValue) {
            $key.SetValue("Path", $PathValue, $Kind)
        }
    }
    finally {
        $key.Dispose()
    }
    $script:CreatedKeys += $keyPath
    return $keyPath
}

function Get-TestPathValue {
    param([Parameter(Mandatory = $true)][string]$KeyPath)

    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($KeyPath, $false)
    try {
        $value = $key.GetValue("Path", $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($null -eq $value) {
            return New-Object PSObject -Property @{ Exists = $false; Value = $null; Kind = $null }
        }
        return New-Object PSObject -Property @{ Exists = $true; Value = $value; Kind = $key.GetValueKind("Path") }
    }
    finally {
        $key.Dispose()
    }
}

function Remove-TestEnvironmentKeys {
    foreach ($keyPath in $script:CreatedKeys) {
        [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($keyPath, $false)
    }
    $root = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($TestKeyRoot, $false)
    if ($null -ne $root) {
        $empty = $root.SubKeyCount -eq 0 -and $root.ValueCount -eq 0
        $root.Dispose()
        if ($empty) {
            [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKey($TestKeyRoot, $false)
        }
    }
}

function Get-RealUserPathSnapshot {
    # Read-only tripwire: the tests must never change the real user PATH.
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $false)
    if ($null -eq $key) {
        return "<no HKCU\Environment key>"
    }
    try {
        $value = $key.GetValue("Path", $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($null -eq $value) {
            return "<no Path value>"
        }
        return "$($key.GetValueKind('Path'))|$value"
    }
    finally {
        $key.Dispose()
    }
}

function Get-FileSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Get-TransactionLeftovers {
    param([Parameter(Mandatory = $true)][string]$Directory)
    if (-not (Test-Path -LiteralPath $Directory)) {
        return @()
    }
    return @(Get-ChildItem -LiteralPath $Directory -Force -File | Where-Object { $_.Name -match '\.(new\.exe|backup)$' } | ForEach-Object { $_.Name })
}

function New-ZipArchive {
    # Writes a release-shaped zip: every entry below Root/, with '/' separators.
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][hashtable]$Files
    )

    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew)
    try {
        $zip = New-Object System.IO.Compression.ZipArchive($stream, [System.IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($name in @($Files.Keys | Sort-Object)) {
                $null = [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $Files[$name], "$Root/$name")
            }
        }
        finally {
            $zip.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

function New-ReleaseMirror {
    # Builds a local copy of the GitHub release layout for CONTEXTWAKE_TEST_RELEASE_DIR. SHA256SUMS is written
    # by scripts/write-checksums.ps1, exactly as the release workflow does.
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [string]$ArchiveSource = "",
        [hashtable]$Files = @{}
    )

    $mirror = New-TestDirectory -Name "mirror-$Name"
    $releaseDirectory = Join-Path $mirror "releases\download\$($script:Package.Tag)"
    New-Item -ItemType Directory -Path $releaseDirectory -Force | Out-Null
    $archivePath = Join-Path $releaseDirectory "$($script:Package.Root).zip"
    if ($ArchiveSource) {
        Copy-Item -LiteralPath $ArchiveSource -Destination $archivePath
    }
    else {
        New-ZipArchive -Path $archivePath -Root $script:Package.Root -Files $Files
    }
    $result = Invoke-ChildPowerShell -Arguments @("-File", $ChecksumWriter, "-Directory", $releaseDirectory)
    Assert-Succeeded $result "write-checksums.ps1 failed for mirror $Name"
    return $mirror
}

function Get-GoodMirror {
    if ($null -eq $script:GoodMirror) {
        $script:GoodMirror = New-ReleaseMirror -Name "good" -ArchiveSource $script:Package.Archive
    }
    return $script:GoodMirror
}

function Invoke-Installer {
    param(
        [Parameter(Mandatory = $true)][string]$Mirror,
        [Parameter(Mandatory = $true)][string]$InstallDirectory,
        [Parameter(Mandatory = $true)][string]$EnvironmentKey,
        [switch]$NoPathUpdate,
        [hashtable]$Environment = @{}
    )

    $arguments = @("-File", $Installer, "-Version", $script:Package.Tag, "-InstallDir", $InstallDirectory)
    if ($NoPathUpdate) {
        $arguments += "-NoPathUpdate"
    }
    $variables = @{ CONTEXTWAKE_TEST_RELEASE_DIR = $Mirror; CONTEXTWAKE_TEST_ENVIRONMENT_KEY = $EnvironmentKey }
    foreach ($name in @($Environment.Keys)) {
        $variables[$name] = $Environment[$name]
    }
    return Invoke-ChildPowerShell -Arguments $arguments -Environment $variables
}

function Invoke-Uninstaller {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDirectory,
        [Parameter(Mandatory = $true)][string]$EnvironmentKey
    )

    return Invoke-ChildPowerShell `
        -Arguments @("-File", $Uninstaller, "-InstallDir", $InstallDirectory) `
        -Environment @{ CONTEXTWAKE_TEST_ENVIRONMENT_KEY = $EnvironmentKey }
}

function Start-StandInProcess {
    # Replaces an installed binary with a copy of PING.EXE and runs it, so the file is a mapped, running image
    # exactly like a ctx.exe that is in use. (ctx itself has no long-running command that avoids probing the
    # real coding agents.)
    param([Parameter(Mandatory = $true)][string]$Binary)

    Remove-Item -LiteralPath $Binary -Force
    Copy-Item -LiteralPath (Join-Path $env:SystemRoot "System32\PING.EXE") -Destination $Binary
    $process = Start-Process -FilePath $Binary -ArgumentList @("-n", "120", "127.0.0.1") -WindowStyle Hidden -PassThru
    Start-Sleep -Milliseconds 500
    Assert-True (-not $process.HasExited) "the stand-in process for $Binary did not keep running"
    return $process
}

function Stop-StandInProcess {
    param([AllowNull()][object]$Process)
    if ($null -ne $Process -and -not $Process.HasExited) {
        $Process.Kill()
        $null = $Process.WaitForExit(10000)
        Start-Sleep -Milliseconds 300
    }
}

function New-SyntheticPe {
    # Writes a minimal PE32+ image whose import and delay-import tables name the given DLLs.
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [string[]]$Imports = @(),
        [string[]]$DelayImports = @()
    )

    $bytes = New-Object byte[] 0x800
    function Set-UInt16 { param([int]$Offset, [long]$Value) [BitConverter]::GetBytes([uint16]$Value).CopyTo($bytes, $Offset) }
    function Set-UInt32 { param([int]$Offset, [long]$Value) [BitConverter]::GetBytes([uint32]$Value).CopyTo($bytes, $Offset) }
    function Set-UInt64 { param([int]$Offset, [long]$Value) [BitConverter]::GetBytes([uint64]$Value).CopyTo($bytes, $Offset) }
    function ConvertTo-RawOffset { param([long]$Rva) return [int]($Rva - 0x1000 + 0x200) }

    $bytes[0] = 0x4D
    $bytes[1] = 0x5A
    Set-UInt32 0x3C 0x80
    $bytes[0x80] = 0x50
    $bytes[0x81] = 0x45
    $coffHeader = 0x84
    Set-UInt16 $coffHeader 0x8664
    Set-UInt16 ($coffHeader + 2) 1
    Set-UInt16 ($coffHeader + 16) 0xF0
    Set-UInt16 ($coffHeader + 18) 0x22
    $optionalHeader = $coffHeader + 20
    Set-UInt16 $optionalHeader 0x20B
    Set-UInt64 ($optionalHeader + 24) 0x140000000
    Set-UInt32 ($optionalHeader + 108) 16
    $dataDirectories = $optionalHeader + 112
    $section = $optionalHeader + 0xF0
    [Text.Encoding]::ASCII.GetBytes(".rdata").CopyTo($bytes, $section)
    Set-UInt32 ($section + 8) 0x600
    Set-UInt32 ($section + 12) 0x1000
    Set-UInt32 ($section + 16) 0x600
    Set-UInt32 ($section + 20) 0x200

    # Section layout by RVA: 0x1000 import descriptors, 0x1200 delay-import descriptors, 0x1400 DLL names.
    $nameRva = 0x1400
    if ($Imports.Count -gt 0) {
        Set-UInt32 ($dataDirectories + 8) 0x1000
        Set-UInt32 ($dataDirectories + 12) (20 * ($Imports.Count + 1))
        for ($index = 0; $index -lt $Imports.Count; $index++) {
            $descriptor = ConvertTo-RawOffset (0x1000 + (20 * $index))
            Set-UInt32 ($descriptor + 12) $nameRva
            Set-UInt32 ($descriptor + 16) 0x1580
            $name = [Text.Encoding]::ASCII.GetBytes($Imports[$index] + [char]0)
            $name.CopyTo($bytes, (ConvertTo-RawOffset $nameRva))
            $nameRva += $name.Length
        }
    }
    if ($DelayImports.Count -gt 0) {
        Set-UInt32 ($dataDirectories + (8 * 13)) 0x1200
        Set-UInt32 ($dataDirectories + (8 * 13) + 4) (32 * ($DelayImports.Count + 1))
        for ($index = 0; $index -lt $DelayImports.Count; $index++) {
            $descriptor = ConvertTo-RawOffset (0x1200 + (32 * $index))
            Set-UInt32 $descriptor 1
            Set-UInt32 ($descriptor + 4) $nameRva
            $name = [Text.Encoding]::ASCII.GetBytes($DelayImports[$index] + [char]0)
            $name.CopyTo($bytes, (ConvertTo-RawOffset $nameRva))
            $nameRva += $name.Length
        }
    }
    [IO.File]::WriteAllBytes($Path, $bytes)
}

function Get-WorkflowJobs {
    # Splits a workflow file into its jobs (two-space indented keys below 'jobs:') and returns name -> text.
    param([Parameter(Mandatory = $true)][string]$Text)

    $jobs = @{}
    $inJobs = $false
    $current = $null
    foreach ($line in ($Text -split "`r?`n")) {
        if ($line -match '^jobs:\s*$') {
            $inJobs = $true
            continue
        }
        if (-not $inJobs) {
            continue
        }
        if ($line -match '^\S') {
            break
        }
        if ($line -match '^  ([A-Za-z0-9_-]+):\s*$') {
            $current = $Matches[1]
            $jobs[$current] = ""
            continue
        }
        if ($null -ne $current) {
            $jobs[$current] += $line + "`n"
        }
    }
    return $jobs
}

# ---------------------------------------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------------------------------------

$savedVariables = @{}
foreach ($variable in @(Get-ChildItem Env: | Where-Object { $_.Name -like "CONTEXTWAKE_*" })) {
    $savedVariables[$variable.Name] = $variable.Value
    [Environment]::SetEnvironmentVariable($variable.Name, $null, "Process")
}
$realPathBefore = Get-RealUserPathSnapshot
$script:GoodMirror = $null

if ($Archive) {
    $archivePath = [IO.Path]::GetFullPath($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Archive))
    if (-not (Test-Path -LiteralPath $archivePath -PathType Leaf)) {
        throw "archive not found: $archivePath"
    }
    $archiveName = Split-Path -Leaf $archivePath
    if ($archiveName -notmatch '^contextwake-(v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?)-windows-x86_64\.zip$') {
        throw "archive name must look like contextwake-v<version>-windows-x86_64.zip: $archiveName"
    }
    $tag = $Matches[1]
    $payload = New-TestDirectory -Name "payload"
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory($archivePath, $payload)
    $root = "contextwake-$tag-windows-x86_64"
    $script:Package = New-Object PSObject -Property @{
        Archive = $archivePath
        Tag = $tag
        Version = $tag.Substring(1)
        Root = $root
        Ctx = Join-Path (Join-Path $payload $root) "ctx.exe"
        CtxWake = Join-Path (Join-Path $payload $root) "ctxwake.exe"
        License = Join-Path (Join-Path $payload $root) "LICENSE"
        Readme = Join-Path (Join-Path $payload $root) "README.md"
    }
}

Write-Host "PowerShell under test: $PowerShellPath"
Write-Host "Work directory:        $WorkRoot"
if ($null -ne $script:Package) {
    Write-Host "Archive:               $($script:Package.Archive)"
}
Write-Host ""

# ---------------------------------------------------------------------------------------------------------
# Script structure and pure helpers
# ---------------------------------------------------------------------------------------------------------

Test-Case "install.ps1 and uninstall.ps1 share identical PATH and directory helpers" {
    $shared = @(
        "Test-FullyQualifiedPath", "ConvertFrom-VerbatimPath", "Resolve-InstallDirectory", "Remove-FileBestEffort",
        "Remove-StaleTransactionFiles", "ConvertTo-PathComparisonKey", "Test-SamePathEntry",
        "Get-UserEnvironmentKeyPath", "Get-UserPathValue", "Set-UserPathValue", "Publish-EnvironmentChange"
    )
    $definitions = @{}
    foreach ($file in @($Installer, $Uninstaller)) {
        $tokens = $null
        $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($file, [ref]$tokens, [ref]$errors)
        Assert-Equal 0 @($errors).Count "$file has parse errors"
        $functions = @{}
        foreach ($function in $ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $true)) {
            $functions[$function.Name] = $function.Extent.Text
        }
        $definitions[$file] = $functions
    }
    foreach ($name in $shared) {
        Assert-True $definitions[$Installer].ContainsKey($name) "install.ps1 does not define $name"
        Assert-True $definitions[$Uninstaller].ContainsKey($name) "uninstall.ps1 does not define $name"
        Assert-Equal $definitions[$Installer][$name] $definitions[$Uninstaller][$name] "$name differs between install.ps1 and uninstall.ps1"
    }
    foreach ($variable in @('$PathMarkerName', '$UserEnvironmentKeyPath', '$TestEnvironmentKeyPattern', '$TransactionFilePattern')) {
        $pattern = '(?m)^' + [Regex]::Escape($variable) + ' = .*$'
        $installLine = [Regex]::Match((Get-Content -Raw -LiteralPath $Installer), $pattern).Value
        $uninstallLine = [Regex]::Match((Get-Content -Raw -LiteralPath $Uninstaller), $pattern).Value
        Assert-True ($installLine.Length -gt 0) "install.ps1 does not define $variable"
        Assert-Equal $installLine $uninstallLine "$variable differs between install.ps1 and uninstall.ps1"
    }
}

Test-Case "PATH entries round-trip byte-for-byte through install and uninstall" {
    $originals = @(
        "",
        "%USERPROFILE%\AppData\Local\Microsoft\WindowsApps",
        "%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;%JAVA_HOME%\bin;C:\Program Files\Git\cmd",
        "C:\Tools;",
        ";;C:\Tools;;",
        ' C:\Spaced ; "C:\Program Files\Quoted" ;relative\dir',
        "C:\Tools;%CONTEXTWAKE_TEST_UNDEFINED%\bin"
    )
    $added = @(Invoke-InScriptScope -Script $Installer -Arguments @(, $originals) -Body {
            param($values)
            foreach ($value in $values) {
                Add-PathEntry -PathValue $value -Directory $SampleDirectory
            }
        })
    Assert-Equal $originals.Count $added.Count "Add-PathEntry result count"
    Assert-Equal $SampleDirectory $added[0] "an empty PATH becomes just the install directory"
    Assert-Equal ($originals[2] + ";" + $SampleDirectory) $added[2] "the entry is appended and %VAR% references stay unexpanded"
    Assert-Equal ("C:\Tools;" + $SampleDirectory + ";") $added[3] "a trailing separator is kept after the new entry"
    $restored = @(Invoke-InScriptScope -Script $Uninstaller -Arguments @(, $added) -Body {
            param($values)
            foreach ($value in $values) {
                Remove-PathEntry -PathValue $value -Directory $SampleDirectory
            }
        })
    for ($index = 0; $index -lt $originals.Count; $index++) {
        Assert-Equal $originals[$index] $restored[$index] "uninstall must restore PATH case $index exactly"
    }
    $edited = Invoke-InScriptScope -Script $Uninstaller -Body {
        @(
            (Remove-PathEntry -PathValue "C:\A;C:\USERS\TEST USER\APPDATA\LOCAL\CONTEXTWAKE\BIN\;C:\B" -Directory $SampleDirectory),
            (Remove-PathEntry -PathValue "$SampleDirectory;C:\A;$SampleDirectory" -Directory $SampleDirectory),
            (Remove-PathEntry -PathValue "$SampleDirectory;C:\A;$SampleDirectory" -Directory $SampleDirectory -All),
            (Remove-PathEntry -PathValue "C:\A;C:\B" -Directory $SampleDirectory)
        )
    }
    Assert-Equal "C:\A;C:\B" $edited[0] "an entry spelled differently is still removed"
    Assert-Equal "$SampleDirectory;C:\A" $edited[1] "only the last (installer-appended) duplicate is removed"
    Assert-Equal "C:\A" $edited[2] "-All removes every copy from the session PATH"
    Assert-Equal "C:\A;C:\B" $edited[3] "a PATH without the entry is returned unchanged"
}

Test-Case "PATH matching normalizes absolute entries and never matches relative ones" {
    $parent = New-TestDirectory -Name "match-parent"
    $directory = Join-Path $parent "bin"
    $previousVariable = $env:CONTEXTWAKE_TEST_PATH_PARENT
    $previousDirectory = [Environment]::CurrentDirectory
    $env:CONTEXTWAKE_TEST_PATH_PARENT = $parent
    try {
        # The process working directory is the install directory's parent, so a relative "bin" would
        # resolve to the install directory if it were treated as a path.
        [Environment]::CurrentDirectory = $parent
        $results = Invoke-InScriptScope -Script $Installer -Arguments @($directory, $parent) -Body {
            param($directory, $parent)
            $matching = @(
                $directory.ToUpperInvariant(),
                ($directory + "\"),
                ($directory.Replace("\", "/") + "/"),
                ('"' + $directory + '"'),
                "%CONTEXTWAKE_TEST_PATH_PARENT%\bin",
                ("\\?\" + $directory),
                ($parent + "\.\bin"),
                (" " + $directory + " ")
            )
            $notMatching = @(
                "bin", ".\bin", ".", "match-parent\bin", ($directory.Substring(0, 2) + "bin"),
                $directory.Substring(2), ($directory + "2"), $parent, "", "%CONTEXTWAKE_TEST_UNDEFINED%\bin"
            )
            foreach ($entry in $matching) {
                New-Object PSObject -Property @{ Entry = $entry; Expected = $true; Actual = (Test-PathContains -PathValue "C:\Other;$entry" -Directory $directory) }
            }
            foreach ($entry in $notMatching) {
                New-Object PSObject -Property @{ Entry = $entry; Expected = $false; Actual = (Test-PathContains -PathValue "C:\Other;$entry" -Directory $directory) }
            }
        }
    }
    finally {
        [Environment]::CurrentDirectory = $previousDirectory
        $env:CONTEXTWAKE_TEST_PATH_PARENT = $previousVariable
    }
    foreach ($result in @($results)) {
        Assert-Equal $result.Expected $result.Actual "Test-PathContains for entry [$($result.Entry)]"
    }
}

Test-Case "install and uninstall accept only absolute install directories" {
    $previousVariable = $env:CONTEXTWAKE_TEST_PATH_PARENT
    $env:CONTEXTWAKE_TEST_PATH_PARENT = "C:\Tools\Parent"
    try {
        foreach ($script in @($Installer, $Uninstaller)) {
            Invoke-InScriptScope -Script $script -Body {
                foreach ($relative in @("bin", ".\bin", "..\bin", "C:bin", "\bin", "/bin", "~\bin", "ContextWake\bin", "%CONTEXTWAKE_TEST_UNDEFINED%\bin")) {
                    Assert-Throws { Resolve-InstallDirectory -Path $relative } "absolute path" "relative install directory '$relative' must be rejected"
                }
                $accepted = @{
                    "C:\Tools\ContextWake" = "C:\Tools\ContextWake"
                    "C:/Tools/ContextWake/" = "C:\Tools\ContextWake"
                    "C:\Tools\.\ContextWake\..\ContextWake" = "C:\Tools\ContextWake"
                    "%CONTEXTWAKE_TEST_PATH_PARENT%\bin" = "C:\Tools\Parent\bin"
                    "\\server\share\ContextWake\" = "\\server\share\ContextWake"
                    "\\?\C:\Tools\ContextWake" = "C:\Tools\ContextWake"
                    "C:\" = "C:\"
                    " C:\Tools\ContextWake " = "C:\Tools\ContextWake"
                }
                foreach ($candidate in $accepted.Keys) {
                    Assert-Equal $accepted[$candidate] (Resolve-InstallDirectory -Path $candidate) "Resolve-InstallDirectory('$candidate')"
                }
                Assert-Equal (Join-Path $env:LOCALAPPDATA "ContextWake\bin") (Resolve-InstallDirectory -Path "") "the default install directory"
            }
        }
    }
    finally {
        $env:CONTEXTWAKE_TEST_PATH_PARENT = $previousVariable
    }
}

Test-Case "registry helpers read raw REG_EXPAND_SZ values and write back the same type" {
    $raw = "%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;%CONTEXTWAKE_TEST_UNDEFINED%\bin"
    $expandKey = New-TestEnvironmentKey -PathValue $raw -Kind ExpandString
    $stringKey = New-TestEnvironmentKey -PathValue "C:\Tools" -Kind String
    $emptyKey = New-TestEnvironmentKey
    $multiKey = New-TestEnvironmentKey -PathValue ([string[]]@("C:\A", "C:\B")) -Kind MultiString
    $dwordKey = New-TestEnvironmentKey -PathValue 1 -Kind DWord
    Invoke-InScriptScope -Script $Installer -Arguments @($expandKey, $stringKey, $emptyKey, $multiKey, $dwordKey, $raw) -Body {
        param($expandKey, $stringKey, $emptyKey, $multiKey, $dwordKey, $raw)
        $value = Get-UserPathValue -KeyPath $expandKey
        Assert-True $value.Exists "the REG_EXPAND_SZ value exists"
        Assert-Equal $raw $value.Value "Get-UserPathValue returns the unexpanded text"
        Assert-Equal ([Microsoft.Win32.RegistryValueKind]::ExpandString) $value.Kind "Get-UserPathValue reports REG_EXPAND_SZ"
        Set-UserPathValue -KeyPath $expandKey -Value ($raw + ";C:\New") -Kind $value.Kind

        $value = Get-UserPathValue -KeyPath $stringKey
        Assert-Equal ([Microsoft.Win32.RegistryValueKind]::String) $value.Kind "Get-UserPathValue reports REG_SZ"
        Set-UserPathValue -KeyPath $stringKey -Value "C:\Tools;C:\New" -Kind $value.Kind

        $value = Get-UserPathValue -KeyPath $emptyKey
        Assert-True (-not $value.Exists) "a missing Path value is reported as missing"
        Assert-Equal ([Microsoft.Win32.RegistryValueKind]::ExpandString) $value.Kind "a new Path value is created as REG_EXPAND_SZ"
        Set-UserPathValue -KeyPath $emptyKey -Value "C:\New" -Kind $value.Kind
        Set-UserPathValue -KeyPath $emptyKey -Value "" -Kind $value.Kind

        Assert-Throws { Get-UserPathValue -KeyPath $multiKey } "unsupported registry type" "REG_MULTI_SZ must be refused"
        Assert-Throws { Get-UserPathValue -KeyPath $dwordKey } "unsupported registry type" "REG_DWORD must be refused"
    }
    $after = Get-TestPathValue -KeyPath $expandKey
    Assert-Equal ($raw + ";C:\New") $after.Value "the raw REG_EXPAND_SZ text was written back"
    Assert-Equal ([Microsoft.Win32.RegistryValueKind]::ExpandString) $after.Kind "REG_EXPAND_SZ stays REG_EXPAND_SZ"
    $after = Get-TestPathValue -KeyPath $stringKey
    Assert-Equal "C:\Tools;C:\New" $after.Value "the REG_SZ text was written back"
    Assert-Equal ([Microsoft.Win32.RegistryValueKind]::String) $after.Kind "REG_SZ stays REG_SZ"
    Assert-True (-not (Get-TestPathValue -KeyPath $emptyKey).Exists) "writing an empty PATH removes the value"
}

Test-Case "the environment-key test hook only accepts disposable test keys" {
    $previous = $env:CONTEXTWAKE_TEST_ENVIRONMENT_KEY
    try {
        foreach ($script in @($Installer, $Uninstaller)) {
            Invoke-InScriptScope -Script $script -Body {
                $env:CONTEXTWAKE_TEST_ENVIRONMENT_KEY = $null
                Assert-Equal "Environment" (Get-UserEnvironmentKeyPath) "without the hook the real user environment key is used"
                foreach ($rejected in @("Environment", "Software\Microsoft\Windows", "Software\ContextWakeInstallerTests", "Software\ContextWakeInstallerTests\..\..\Environment", "Software\ContextWakeInstallerTests\a\b")) {
                    $env:CONTEXTWAKE_TEST_ENVIRONMENT_KEY = $rejected
                    Assert-Throws { Get-UserEnvironmentKeyPath } "ContextWakeInstallerTests" "test key '$rejected' must be rejected"
                }
                $env:CONTEXTWAKE_TEST_ENVIRONMENT_KEY = "Software\ContextWakeInstallerTests\abc_123-XYZ"
                Assert-Equal "Software\ContextWakeInstallerTests\abc_123-XYZ" (Get-UserEnvironmentKeyPath) "a disposable test key is accepted"
            }
        }
    }
    finally {
        $env:CONTEXTWAKE_TEST_ENVIRONMENT_KEY = $previous
    }
}

Test-Case "stale transaction files are deleted and unrelated files are kept" {
    $directory = New-TestDirectory -Name "stale-files"
    $guid = [Guid]::NewGuid().ToString("N")
    # NTFS names are case-insensitive, so the upper-case variant needs its own GUID to be a separate file.
    $upperGuid = "A" + [Guid]::NewGuid().ToString("N").ToUpperInvariant().Substring(1)
    $stale = @(".ctx.exe.$guid.backup", ".ctxwake.exe.$guid.new.exe", ".ctxwake.exe.$guid.backup")
    $kept = @("ctx.exe", "notes.txt", ".ctx.exe.123.backup", ".ctx.exe.$upperGuid.backup", ".other.exe.$guid.backup")
    foreach ($name in @($stale + $kept)) {
        Set-Content -LiteralPath (Join-Path $directory $name) -Value "x"
    }
    foreach ($script in @($Installer, $Uninstaller)) {
        $remaining = @(Invoke-InScriptScope -Script $script -Arguments @($directory) -Body {
                param($directory)
                Remove-StaleTransactionFiles -Directory $directory
            })
        Assert-Equal 0 $remaining.Count "no stale file should be reported as still in use"
    }
    foreach ($name in $stale) {
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $directory $name))) "$name should have been deleted"
    }
    foreach ($name in $kept) {
        Assert-True (Test-Path -LiteralPath (Join-Path $directory $name)) "$name must be kept"
    }
}

# ---------------------------------------------------------------------------------------------------------
# End-to-end installer and uninstaller runs against the local package
# ---------------------------------------------------------------------------------------------------------

Test-Case "install, reinstall, and uninstall keep a REG_EXPAND_SZ user PATH byte-for-byte" -NeedsArchive {
    $original = "%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;%CONTEXTWAKE_TEST_UNDEFINED%\bin;C:\Program Files\Example Tools"
    $key = New-TestEnvironmentKey -PathValue $original -Kind ExpandString
    $directory = Join-Path (New-TestDirectory -Name "Install Dir With Spaces\round-trip") "bin"
    $mirror = Get-GoodMirror

    $result = Invoke-Installer -Mirror $mirror -InstallDirectory $directory -EnvironmentKey $key
    Assert-Succeeded $result "fresh install"
    Assert-OutputContains $result "Updating user PATH... OK" "fresh install"
    Assert-True ($result.Output -notmatch "were not notified") "the WM_SETTINGCHANGE broadcast should succeed`n$($result.Output)"
    $value = Get-TestPathValue -KeyPath $key
    Assert-Equal ($original + ";" + $directory) $value.Value "install appends only the install directory and keeps %VAR% references"
    Assert-Equal ([Microsoft.Win32.RegistryValueKind]::ExpandString) $value.Kind "install keeps REG_EXPAND_SZ"
    Assert-True (Test-Path -LiteralPath (Join-Path $directory ".contextwake-path-added")) "install records PATH ownership"
    Assert-Equal (Get-FileSha256 $script:Package.Ctx) (Get-FileSha256 (Join-Path $directory "ctx.exe")) "installed ctx.exe matches the archive"
    Assert-Equal (Get-FileSha256 $script:Package.CtxWake) (Get-FileSha256 (Join-Path $directory "ctxwake.exe")) "installed ctxwake.exe matches the archive"
    Assert-Equal 0 @(Get-TransactionLeftovers -Directory $directory).Count "install leaves no staged or backup files"
    $version = (& (Join-Path $directory "ctx.exe") --version | Out-String).Trim()
    Assert-Equal "ctx $($script:Package.Version)" $version "the installed ctx.exe runs"

    $result = Invoke-Installer -Mirror $mirror -InstallDirectory $directory -EnvironmentKey $key
    Assert-Succeeded $result "reinstall"
    Assert-OutputContains $result "User PATH already contains the install directory." "reinstall"
    Assert-Equal ($original + ";" + $directory) (Get-TestPathValue -KeyPath $key).Value "reinstall does not touch PATH"
    Assert-Equal 0 @(Get-TransactionLeftovers -Directory $directory).Count "reinstall leaves no staged or backup files"

    $result = Invoke-Uninstaller -InstallDirectory $directory -EnvironmentKey $key
    Assert-Succeeded $result "uninstall"
    Assert-OutputContains $result "The installer-owned user PATH entry was removed." "uninstall"
    $value = Get-TestPathValue -KeyPath $key
    Assert-Equal $original $value.Value "uninstall restores the original raw PATH"
    Assert-Equal ([Microsoft.Win32.RegistryValueKind]::ExpandString) $value.Kind "uninstall keeps REG_EXPAND_SZ"
    Assert-True (-not (Test-Path -LiteralPath $directory)) "uninstall removes the empty install directory"
}

Test-Case "a REG_SZ PATH stays REG_SZ and a missing PATH value is created and removed" -NeedsArchive {
    $mirror = Get-GoodMirror
    $key = New-TestEnvironmentKey -PathValue "C:\Tools;C:\Other" -Kind String
    $directory = Join-Path (New-TestDirectory -Name "reg-sz") "bin"
    Assert-Succeeded (Invoke-Installer -Mirror $mirror -InstallDirectory $directory -EnvironmentKey $key) "install with REG_SZ PATH"
    $value = Get-TestPathValue -KeyPath $key
    Assert-Equal "C:\Tools;C:\Other;$directory" $value.Value "install appends to the REG_SZ PATH"
    Assert-Equal ([Microsoft.Win32.RegistryValueKind]::String) $value.Kind "REG_SZ stays REG_SZ"
    Assert-Succeeded (Invoke-Uninstaller -InstallDirectory $directory -EnvironmentKey $key) "uninstall with REG_SZ PATH"
    $value = Get-TestPathValue -KeyPath $key
    Assert-Equal "C:\Tools;C:\Other" $value.Value "uninstall restores the REG_SZ PATH"
    Assert-Equal ([Microsoft.Win32.RegistryValueKind]::String) $value.Kind "REG_SZ stays REG_SZ after uninstall"

    $key = New-TestEnvironmentKey
    $directory = Join-Path (New-TestDirectory -Name "no-path") "bin"
    Assert-Succeeded (Invoke-Installer -Mirror $mirror -InstallDirectory $directory -EnvironmentKey $key) "install without a PATH value"
    $value = Get-TestPathValue -KeyPath $key
    Assert-Equal $directory $value.Value "install creates PATH with only the install directory"
    Assert-Equal ([Microsoft.Win32.RegistryValueKind]::ExpandString) $value.Kind "a new PATH is REG_EXPAND_SZ"
    Assert-Succeeded (Invoke-Uninstaller -InstallDirectory $directory -EnvironmentKey $key) "uninstall of the created PATH"
    Assert-True (-not (Get-TestPathValue -KeyPath $key).Exists) "uninstall removes the PATH value it created"
}

Test-Case "an existing PATH entry for the directory is reused and left alone by uninstall" -NeedsArchive {
    $directory = Join-Path (New-TestDirectory -Name "preexisting") "bin"
    $original = "C:\Other;" + $directory.ToUpperInvariant() + "\"
    $key = New-TestEnvironmentKey -PathValue $original -Kind ExpandString
    $result = Invoke-Installer -Mirror (Get-GoodMirror) -InstallDirectory $directory -EnvironmentKey $key
    Assert-Succeeded $result "install with a pre-existing PATH entry"
    Assert-OutputContains $result "User PATH already contains the install directory." "install with a pre-existing PATH entry"
    Assert-Equal $original (Get-TestPathValue -KeyPath $key).Value "PATH is unchanged"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $directory ".contextwake-path-added"))) "a PATH entry the user owns is not claimed"
    $result = Invoke-Uninstaller -InstallDirectory $directory -EnvironmentKey $key
    Assert-Succeeded $result "uninstall"
    Assert-True ($result.Output -notmatch "PATH entry was removed") "uninstall must not report a PATH change`n$($result.Output)"
    Assert-Equal $original (Get-TestPathValue -KeyPath $key).Value "uninstall keeps the user's own PATH entry"
}

Test-Case "relative install directories are rejected before anything is written" -NeedsArchive {
    $probe = New-TestDirectory -Name "relative-probe"
    $key = New-TestEnvironmentKey -PathValue "C:\Other" -Kind ExpandString
    $mirror = Get-GoodMirror
    # A root-relative path (\...) resolves against the current drive; this one points into the work directory.
    $rootRelative = $WorkRoot.Substring(2) + "\root-relative"
    foreach ($relative in @("bin", ".\bin", "..\relative-escape", "C:bin", $rootRelative)) {
        $result = Invoke-ChildPowerShell `
            -Arguments @("-File", $Installer, "-Version", $script:Package.Tag, "-InstallDir", $relative) `
            -Environment @{ CONTEXTWAKE_TEST_RELEASE_DIR = $mirror; CONTEXTWAKE_TEST_ENVIRONMENT_KEY = $key } `
            -WorkingDirectory $probe
        Assert-Failed $result "absolute path" "install -InstallDir '$relative'"
    }
    $result = Invoke-ChildPowerShell `
        -Arguments @("-File", $Installer, "-Version", $script:Package.Tag) `
        -Environment @{ CONTEXTWAKE_TEST_RELEASE_DIR = $mirror; CONTEXTWAKE_TEST_ENVIRONMENT_KEY = $key; CONTEXTWAKE_INSTALL_DIR = "bin" } `
        -WorkingDirectory $probe
    Assert-Failed $result "absolute path" "install with CONTEXTWAKE_INSTALL_DIR=bin"
    $result = Invoke-ChildPowerShell `
        -Arguments @("-File", $Uninstaller, "-InstallDir", "bin") `
        -Environment @{ CONTEXTWAKE_TEST_ENVIRONMENT_KEY = $key } `
        -WorkingDirectory $probe
    Assert-Failed $result "absolute path" "uninstall -InstallDir bin"
    $semicolon = Join-Path $WorkRoot "semi;colon\bin"
    $result = Invoke-Installer -Mirror $mirror -InstallDirectory $semicolon -EnvironmentKey $key
    Assert-Failed $result "cannot be added to PATH" "install into a directory containing ';'"
    Assert-Equal 0 @(Get-ChildItem -LiteralPath $probe -Force).Count "nothing was created below the working directory"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $WorkRoot "relative-escape"))) "nothing was created next to the working directory"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $WorkRoot "root-relative"))) "nothing was created for the root-relative path"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $WorkRoot "semi;colon"))) "nothing was created for the rejected ';' directory"
    Assert-Equal "C:\Other" (Get-TestPathValue -KeyPath $key).Value "PATH is unchanged"
}

Test-Case "a binary that fails verification after the swap is rolled back" -NeedsArchive {
    $directory = Join-Path (New-TestDirectory -Name "rollback") "bin"
    $key = New-TestEnvironmentKey -PathValue "C:\Other" -Kind ExpandString
    Assert-Succeeded (Invoke-Installer -Mirror (Get-GoodMirror) -InstallDirectory $directory -EnvironmentKey $key -NoPathUpdate) "initial install"
    $ctxBefore = Get-FileSha256 (Join-Path $directory "ctx.exe")
    $ctxWakeBefore = Get-FileSha256 (Join-Path $directory "ctxwake.exe")

    # The new ctx.exe still prints the right version (appended bytes do not change how it runs) but differs
    # from the installed one; the new ctxwake.exe fails verification, which happens only after the swap.
    $badFiles = New-TestDirectory -Name "bad-payload"
    $modifiedCtx = Join-Path $badFiles "ctx.exe"
    Copy-Item -LiteralPath $script:Package.Ctx -Destination $modifiedCtx
    [IO.File]::AppendAllText($modifiedCtx, "contextwake-rollback-test")
    $brokenCtxWake = Join-Path $badFiles "ctxwake.exe"
    Copy-Item -LiteralPath (Join-Path $env:SystemRoot "System32\whoami.exe") -Destination $brokenCtxWake
    $mirror = New-ReleaseMirror -Name "rollback" -Files @{
        "ctx.exe" = $modifiedCtx
        "ctxwake.exe" = $brokenCtxWake
        "LICENSE" = $script:Package.License
        "README.md" = $script:Package.Readme
    }

    $result = Invoke-Installer -Mirror $mirror -InstallDirectory $directory -EnvironmentKey $key -NoPathUpdate
    Assert-Failed $result "ctxwake verification failed" "update with a broken ctxwake.exe"
    Assert-OutputContains $result "exited with code" "the verification error reports the exit code"
    Assert-Equal $ctxBefore (Get-FileSha256 (Join-Path $directory "ctx.exe")) "the previous ctx.exe is restored"
    Assert-Equal $ctxWakeBefore (Get-FileSha256 (Join-Path $directory "ctxwake.exe")) "the previous ctxwake.exe is restored"
    Assert-Equal 0 @(Get-TransactionLeftovers -Directory $directory).Count "rollback leaves no staged or backup files"
    Assert-Equal "C:\Other" (Get-TestPathValue -KeyPath $key).Value "PATH is unchanged"
}

Test-Case "a rollback that cannot restore a binary keeps its backup and reports the original error" -NeedsArchive {
    $directory = New-TestDirectory -Name "rollback-keep\bin"
    $payload = New-TestDirectory -Name "rollback-keep-payload"
    Copy-Item -LiteralPath $script:Package.Ctx -Destination (Join-Path $payload "ctx.exe")
    Copy-Item -LiteralPath (Join-Path $env:SystemRoot "System32\whoami.exe") -Destination (Join-Path $payload "ctxwake.exe")
    # Stand-ins for the previous install; they are only renamed, never run.
    Set-Content -LiteralPath (Join-Path $directory "ctx.exe") -Value "previous ctx"
    Set-Content -LiteralPath (Join-Path $directory "ctxwake.exe") -Value "previous ctxwake"
    $outcome = Invoke-InScriptScope -Script $Installer -Arguments @($payload, $directory, $script:Package.Tag) -Body {
        param($payload, $directory, $tag)
        # Shadow Move-Item so that restoring the ctx.exe backup fails, as it would if the file were locked.
        function Move-Item {
            [CmdletBinding()]
            param([string]$LiteralPath, [string]$Destination, [switch]$Force)
            if ($LiteralPath -like "*\.ctx.exe.*.backup" -and $Destination -like "*\ctx.exe") {
                throw "simulated restore failure"
            }
            Microsoft.PowerShell.Management\Move-Item -LiteralPath $LiteralPath -Destination $Destination -Force:$Force
        }
        $warnings = @()
        $message = ""
        try {
            Install-Binaries -PayloadDirectory $payload -DestinationDirectory $directory -ReleaseVersion $tag -WarningVariable +warnings -WarningAction SilentlyContinue
        }
        catch {
            $message = $_.Exception.Message
        }
        New-Object PSObject -Property @{ Message = $message; Warnings = (@($warnings | ForEach-Object { "$_" }) -join "`n") }
    }
    Assert-True ($outcome.Message -match "ctxwake verification failed") "the original verification error is reported, not the restore failure: [$($outcome.Message)]"
    Assert-True ($outcome.Warnings -match "Could not restore the previous ctx\.exe") "the failed restore is reported: [$($outcome.Warnings)]"
    $kept = @(Get-ChildItem -LiteralPath $directory -Force -File | Where-Object { $_.Name -like ".ctx.exe.*.backup" })
    Assert-Equal 1 $kept.Count "the backup of the previous ctx.exe is kept"
    Assert-Equal "previous ctx" (Get-Content -LiteralPath $kept[0].FullName -Raw).Trim() "the kept backup holds the previous ctx.exe"
    Assert-Equal "previous ctxwake" (Get-Content -LiteralPath (Join-Path $directory "ctxwake.exe") -Raw).Trim() "the previous ctxwake.exe is restored"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $directory "ctx.exe"))) "the rejected new ctx.exe is removed"
}

Test-Case "updating while ctx.exe is running succeeds and a later uninstall cleans up" -NeedsArchive {
    $directory = Join-Path (New-TestDirectory -Name "update-running") "bin"
    $key = New-TestEnvironmentKey -PathValue "C:\Other" -Kind ExpandString
    $mirror = Get-GoodMirror
    Assert-Succeeded (Invoke-Installer -Mirror $mirror -InstallDirectory $directory -EnvironmentKey $key -NoPathUpdate) "initial install"
    $process = Start-StandInProcess -Binary (Join-Path $directory "ctx.exe")
    try {
        $result = Invoke-Installer -Mirror $mirror -InstallDirectory $directory -EnvironmentKey $key -NoPathUpdate
        Assert-Succeeded $result "update while the old ctx.exe is running"
        Assert-OutputContains $result "ContextWake installed successfully." "update while running"
        Assert-OutputContains $result "still running" "update while running warns about the old file"
        Assert-Equal (Get-FileSha256 $script:Package.Ctx) (Get-FileSha256 (Join-Path $directory "ctx.exe")) "the new ctx.exe is installed"
        Assert-Equal 1 @(Get-TransactionLeftovers -Directory $directory).Count "only the in-use backup is left behind"
    }
    finally {
        Stop-StandInProcess $process
    }
    $result = Invoke-Uninstaller -InstallDirectory $directory -EnvironmentKey $key
    Assert-Succeeded $result "uninstall after the old process exited"
    Assert-True (-not (Test-Path -LiteralPath $directory)) "uninstall removes the leftover backup and the directory"
}

Test-Case "uninstalling while ctx.exe is running removes the PATH entry and finishes on rerun" -NeedsArchive {
    $directory = Join-Path (New-TestDirectory -Name "uninstall-running") "bin"
    $key = New-TestEnvironmentKey -PathValue "C:\Other" -Kind ExpandString
    Assert-Succeeded (Invoke-Installer -Mirror (Get-GoodMirror) -InstallDirectory $directory -EnvironmentKey $key) "install"
    $process = Start-StandInProcess -Binary (Join-Path $directory "ctx.exe")
    try {
        $result = Invoke-Uninstaller -InstallDirectory $directory -EnvironmentKey $key
        Assert-Succeeded $result "uninstall while ctx.exe is running"
        Assert-OutputContains $result "still running" "uninstall while running warns about the in-use file"
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $directory "ctx.exe"))) "ctx.exe is no longer reachable"
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $directory "ctxwake.exe"))) "ctxwake.exe is removed"
        Assert-Equal "C:\Other" (Get-TestPathValue -KeyPath $key).Value "the PATH entry is removed"
    }
    finally {
        Stop-StandInProcess $process
    }
    Assert-Succeeded (Invoke-Uninstaller -InstallDirectory $directory -EnvironmentKey $key) "second uninstall"
    Assert-True (-not (Test-Path -LiteralPath $directory)) "the second uninstall removes the leftover and the directory"
}

Test-Case "the README scriptblock invocation works for install and uninstall" -NeedsArchive {
    $directory = Join-Path (New-TestDirectory -Name "scriptblock form") "bin"
    $key = New-TestEnvironmentKey -PathValue "C:\Other" -Kind ExpandString
    $quotedDirectory = ConvertTo-PowerShellLiteral $directory
    foreach ($step in @(
            @{ Script = $Installer; Arguments = "-Version '$($script:Package.Tag)' -InstallDir $quotedDirectory -NoPathUpdate" },
            @{ Script = $Uninstaller; Arguments = "-InstallDir $quotedDirectory" })) {
        $command = "& ([scriptblock]::Create((Get-Content -Raw -LiteralPath $(ConvertTo-PowerShellLiteral $step.Script)))) $($step.Arguments)"
        $result = Invoke-ChildCommand -Command $command -Environment @{
            CONTEXTWAKE_TEST_RELEASE_DIR = (Get-GoodMirror)
            CONTEXTWAKE_TEST_ENVIRONMENT_KEY = $key
        }
        Assert-Succeeded $result "scriptblock invocation of $(Split-Path -Leaf $step.Script)"
        if ($step.Script -eq $Installer) {
            Assert-True (Test-Path -LiteralPath (Join-Path $directory "ctx.exe")) "the scriptblock install created ctx.exe"
        }
    }
    Assert-True (-not (Test-Path -LiteralPath $directory)) "the scriptblock uninstall removed the directory"
    Assert-Equal "C:\Other" (Get-TestPathValue -KeyPath $key).Value "PATH is unchanged with -NoPathUpdate"
}

Test-Case "checksum mismatches and unexpected archive entries are rejected" -NeedsArchive {
    $key = New-TestEnvironmentKey -PathValue "C:\Other" -Kind ExpandString
    $directory = Join-Path $WorkRoot "rejected\bin"
    $result = Invoke-Installer -Mirror (Get-GoodMirror) -InstallDirectory $directory -EnvironmentKey $key -Environment @{ CONTEXTWAKE_TEST_CORRUPT_ARCHIVE = "1" }
    Assert-Failed $result "checksum mismatch" "corrupted archive"
    $extra = Join-Path (New-TestDirectory -Name "extra-entry") "payload.dll"
    Set-Content -LiteralPath $extra -Value "unexpected"
    $mirror = New-ReleaseMirror -Name "extra-entry" -Files @{
        "ctx.exe" = $script:Package.Ctx
        "ctxwake.exe" = $script:Package.CtxWake
        "LICENSE" = $script:Package.License
        "README.md" = $script:Package.Readme
        "payload.dll" = $extra
    }
    $result = Invoke-Installer -Mirror $mirror -InstallDirectory $directory -EnvironmentKey $key
    Assert-Failed $result "unexpected or missing entry" "archive with an extra entry"
    Assert-True (-not (Test-Path -LiteralPath $directory)) "nothing was installed"
    Assert-Equal "C:\Other" (Get-TestPathValue -KeyPath $key).Value "PATH is unchanged"
}

# ---------------------------------------------------------------------------------------------------------
# Release tooling
# ---------------------------------------------------------------------------------------------------------

Test-Case "write-checksums.ps1 writes sorted LF-terminated SHA-256 lines" {
    $directory = New-TestDirectory -Name "checksums"
    $processDirectory = New-TestDirectory -Name "checksums-process-directory"
    Set-Content -LiteralPath (Join-Path $directory "b-archive.zip") -Value "b"
    Set-Content -LiteralPath (Join-Path $directory "a-archive.tar.gz") -Value "a"
    $command = "Set-Location -LiteralPath $(ConvertTo-PowerShellLiteral $WorkRoot); & $(ConvertTo-PowerShellLiteral $ChecksumWriter) -Directory checksums"
    $result = Invoke-ChildCommand -Command $command -WorkingDirectory $processDirectory
    Assert-Succeeded $result "write-checksums.ps1 with a directory relative to the PowerShell location"
    Assert-Equal 0 @(Get-ChildItem -LiteralPath $processDirectory -Force).Count "nothing is resolved against the process working directory"
    $bytes = [IO.File]::ReadAllBytes((Join-Path $directory "SHA256SUMS"))
    Assert-True ($bytes -notcontains 13) "SHA256SUMS must not contain CR characters"
    Assert-Equal 10 ([int]$bytes[$bytes.Length - 1]) "SHA256SUMS ends with a newline"
    $lines = @([Text.Encoding]::ASCII.GetString($bytes).TrimEnd("`n").Split("`n"))
    Assert-Equal 2 $lines.Count "one line per artifact"
    Assert-Equal ((Get-FileSha256 (Join-Path $directory "a-archive.tar.gz")) + "  a-archive.tar.gz") $lines[0] "first line"
    Assert-Equal ((Get-FileSha256 (Join-Path $directory "b-archive.zip")) + "  b-archive.zip") $lines[1] "second line"
}

Test-Case "package-release.ps1 resolves paths from its own and the PowerShell location and writes '/' entries" -NeedsArchive {
    if ($null -eq (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "cargo must be on PATH: package-release.ps1 reads the version from cargo metadata"
    }
    $location = New-TestDirectory -Name "package-location"
    $processDirectory = New-TestDirectory -Name "package-process-directory"
    $binaries = New-TestDirectory -Name "package-location\built"
    Copy-Item -LiteralPath $script:Package.Ctx -Destination (Join-Path $binaries "ctx.exe")
    Copy-Item -LiteralPath $script:Package.CtxWake -Destination (Join-Path $binaries "ctxwake.exe")
    $label = "installer-test-" + [Guid]::NewGuid().ToString("N").Substring(0, 8)
    $packager = Join-Path $PSScriptRoot "package-release.ps1"
    $distribution = Join-Path $RepositoryRoot "dist"
    $prefix = "Set-Location -LiteralPath $(ConvertTo-PowerShellLiteral $location); & $(ConvertTo-PowerShellLiteral $packager) -Target x86_64-pc-windows-msvc -Label $label -BinaryDirectory built"
    try {
        $result = Invoke-ChildCommand -Command $prefix -WorkingDirectory $processDirectory
        Assert-Succeeded $result "package-release.ps1 started from another directory"
        $created = @(Get-ChildItem -LiteralPath $distribution -Filter "contextwake-v*-$label.zip" -File)
        Assert-Equal 1 $created.Count "the archive is written to the repository's dist directory"
        $root = $created[0].BaseName
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [System.IO.Compression.ZipFile]::OpenRead($created[0].FullName)
        try {
            $entries = @($zip.Entries | ForEach-Object { $_.FullName } | Sort-Object)
        }
        finally {
            $zip.Dispose()
        }
        $expected = @(@("ctx.exe", "ctxwake.exe", "LICENSE", "README.md") | ForEach-Object { "$root/$_" } | Sort-Object)
        Assert-Equal ($expected -join "|") ($entries -join "|") "archive entries use '/' and contain exactly the release files"
        Assert-Equal 0 @(Get-ChildItem -LiteralPath $processDirectory -Force).Count "nothing is written below the process working directory"
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $location "dist"))) "the default output directory is the repository's dist"

        $result = Invoke-ChildCommand -Command "$prefix -OutputDirectory out" -WorkingDirectory $processDirectory
        Assert-Failed $result "must stay inside the repository" "an output directory outside the repository"
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $location "out"))) "nothing is created outside the repository"
    }
    finally {
        Get-ChildItem -LiteralPath $distribution -Filter "contextwake-v*-$label*" -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force
    }
}

Test-Case "check-windows-runtime.ps1 rejects Visual C++ and MinGW runtime imports" {
    $directory = New-TestDirectory -Name "pe-files"
    $cases = @(
        @{ Name = "vcruntime.exe"; Imports = @("KERNEL32.dll", "VCRUNTIME140.dll"); Delay = @(); Fails = $true; Pattern = "VCRUNTIME140\.dll" },
        @{ Name = "delay-msvcp.exe"; Imports = @("KERNEL32.dll"); Delay = @("MSVCP140.dll"); Fails = $true; Pattern = "MSVCP140\.dll" },
        @{ Name = "mingw.exe"; Imports = @("KERNEL32.dll", "libgcc_s_seh-1.dll"); Delay = @(); Fails = $true; Pattern = "libgcc_s_seh-1\.dll" },
        @{ Name = "system.exe"; Imports = @("KERNEL32.dll", "msvcrt.dll", "api-ms-win-crt-runtime-l1-1-0.dll", "ucrtbase.dll"); Delay = @("USER32.dll"); Fails = $false; Pattern = "" }
    )
    foreach ($case in $cases) {
        $file = Join-Path $directory $case.Name
        New-SyntheticPe -Path $file -Imports $case.Imports -DelayImports $case.Delay
        $result = Invoke-ChildPowerShell -Arguments @("-File", $RuntimeCheck, "-Path", $file)
        if ($case.Fails) {
            Assert-Failed $result $case.Pattern "runtime check of $($case.Name)"
        }
        else {
            Assert-Succeeded $result "runtime check of $($case.Name)"
            foreach ($dll in @($case.Imports + $case.Delay)) {
                Assert-OutputContains $result $dll "the runtime check lists every import of $($case.Name)"
            }
        }
    }
    $notPe = Join-Path $directory "not-pe.exe"
    Set-Content -LiteralPath $notPe -Value "not a portable executable"
    Assert-Failed (Invoke-ChildPowerShell -Arguments @("-File", $RuntimeCheck, "-Path", $notPe)) "not a valid PE file" "runtime check of a text file"
}

Test-Case "packaged ctx.exe and ctxwake.exe import only DLLs that ship with Windows" -NeedsArchive {
    foreach ($binary in @($script:Package.Ctx, $script:Package.CtxWake)) {
        $result = Invoke-ChildPowerShell -Arguments @("-File", $RuntimeCheck, "-Path", $binary)
        Assert-Succeeded $result "runtime check of $binary"
    }
}

Test-Case "every workflow action is pinned to a full commit SHA" {
    $workflows = @(Get-ChildItem -LiteralPath (Join-Path $RepositoryRoot ".github\workflows") -File | Where-Object { $_.Extension -in @(".yml", ".yaml") })
    Assert-True ($workflows.Count -gt 0) "no workflows found"
    $count = 0
    foreach ($workflow in $workflows) {
        $lineNumber = 0
        foreach ($line in (Get-Content -LiteralPath $workflow.FullName)) {
            $lineNumber++
            if ($line -notmatch '^\s*(?:-\s+)?uses:\s*(\S+)(.*)$') {
                continue
            }
            $reference = $Matches[1].Trim('"', "'")
            $comment = $Matches[2]
            $count++
            if ($reference.StartsWith("./")) {
                continue
            }
            Assert-True ($reference -cmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(/[^@\s]+)?@[0-9a-f]{40}$') "$($workflow.Name):$lineNumber '$reference' is not pinned to a full commit SHA"
            Assert-True ($comment -match '^\s+#\s*\S+') "$($workflow.Name):$lineNumber pinned action needs a '# <tag or branch>' comment"
        }
    }
    Assert-True ($count -gt 0) "no 'uses:' lines were found"
}

Test-Case "the release workflow publishes only gated, cache-free builds" {
    $text = Get-Content -Raw -LiteralPath (Join-Path $RepositoryRoot ".github\workflows\release.yml")
    Assert-True ($text -notmatch 'rust-cache|actions/cache') "release builds must not restore a build cache"
    $jobs = Get-WorkflowJobs -Text $text
    foreach ($job in @("build", "checksums", "publish")) {
        Assert-True $jobs.ContainsKey($job) "release.yml has no '$job' job"
    }
    $build = $jobs["build"]
    $release = $build.IndexOf("cargo build --locked --release", [StringComparison]::Ordinal)
    Assert-True ($release -ge 0) "the build job does not run 'cargo build --locked --release'"
    foreach ($gate in @("cargo fmt --all -- --check", "cargo clippy --locked", "cargo test --locked")) {
        $position = $build.IndexOf($gate, [StringComparison]::Ordinal)
        Assert-True ($position -ge 0 -and $position -lt $release) "the build job must run '$gate' before the release build"
    }
    foreach ($check in @("check-windows-runtime.ps1", "test-windows-installer.ps1", "persist-credentials: false")) {
        Assert-True ($build.Contains($check)) "the build job must run or set '$check'"
    }
    Assert-True ($jobs["checksums"] -match '(?m)^\s+needs:\s*\[?\s*build\b') "checksums must need build"
    Assert-True ($jobs["publish"] -match '(?m)^\s+needs:.*\bchecksums\b') "publish must need checksums"
    Assert-True ($jobs["publish"] -match '(?m)^\s+needs:.*\bbuild\b') "publish must need build"
    Assert-True ($jobs["checksums"].Contains("write-checksums.ps1")) "checksums must be written by scripts/write-checksums.ps1"
}

Test-Case "MSVC builds link the C runtime statically and other targets are unaffected" {
    $config = Join-Path $RepositoryRoot ".cargo\config.toml"
    Assert-True (Test-Path -LiteralPath $config -PathType Leaf) ".cargo/config.toml is missing"
    $sections = @{}
    $current = ""
    foreach ($line in (Get-Content -LiteralPath $config)) {
        $trimmed = $line.Trim()
        if ($trimmed.StartsWith("#") -or $trimmed.Length -eq 0) {
            continue
        }
        if ($trimmed -match '^\[(.+)\]$') {
            $current = $Matches[1]
            $sections[$current] = @()
            continue
        }
        $sections[$current] += $trimmed
    }
    Assert-True $sections.ContainsKey("target.x86_64-pc-windows-msvc") "no [target.x86_64-pc-windows-msvc] section"
    Assert-True (@($sections["target.x86_64-pc-windows-msvc"]) -contains 'rustflags = ["-C", "target-feature=+crt-static"]') "the MSVC target must use -C target-feature=+crt-static"
    foreach ($name in @($sections.Keys)) {
        if ($name -ne "target.x86_64-pc-windows-msvc") {
            Assert-True (-not ((@($sections[$name]) -join "`n") -match 'crt-static')) "[$name] must not change the C runtime of other targets"
        }
    }
}

# ---------------------------------------------------------------------------------------------------------
# Teardown
# ---------------------------------------------------------------------------------------------------------

try {
    Remove-TestEnvironmentKeys
}
catch {
    Write-Warning "Could not delete the temporary registry keys: $_"
}
$realPathAfter = Get-RealUserPathSnapshot
$tripwirePassed = [string]::Equals($realPathBefore, $realPathAfter, [StringComparison]::Ordinal)
$script:Results += New-Object PSObject -Property @{ Name = "the real HKCU\Environment PATH was not modified"; Passed = $tripwirePassed }
Write-Host ("{0}  the real HKCU\Environment PATH was not modified" -f $(if ($tripwirePassed) { "PASS" } else { "FAIL" }))

foreach ($name in @($savedVariables.Keys)) {
    [Environment]::SetEnvironmentVariable($name, $savedVariables[$name], "Process")
}
try {
    Remove-Item -LiteralPath $WorkRoot -Recurse -Force
}
catch {
    Write-Warning "Could not delete $WorkRoot`: $_"
}

$failed = @($script:Results | Where-Object { -not $_.Passed })
Write-Host ""
Write-Host ("{0} passed, {1} failed" -f (@($script:Results).Count - $failed.Count), $failed.Count)
if ($failed.Count -gt 0) {
    throw "$($failed.Count) installer test(s) failed: $(($failed | ForEach-Object { $_.Name }) -join '; ')"
}
