[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9_-]+$')]
    [string]$Target,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9_-]+$')]
    [string]$Label,
    # Archive directory; defaults to the repository's dist/. Relative paths resolve against the current
    # PowerShell location.
    [string]$OutputDirectory = "",
    # Directory holding the built binaries; defaults to target/<Target>/release (cargo build --target <Target>).
    # Pass target/release to package a host build made without --target.
    [string]$BinaryDirectory = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

# Resolve everything from the script location and the PowerShell location. .NET path APIs resolve relative
# paths against the process working directory, which Set-Location does not change in Windows PowerShell.
$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
function Resolve-LocationPath {
    param([Parameter(Mandatory = $true)][string]$Path)
    return [System.IO.Path]::GetFullPath($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path))
}

$metadataJson = cargo metadata --locked --no-deps --format-version 1 --manifest-path (Join-Path $repositoryRoot "Cargo.toml")
if ($LASTEXITCODE -ne 0) {
    throw "cargo metadata failed with exit code $LASTEXITCODE"
}
$metadata = (@($metadataJson) -join "`n") | ConvertFrom-Json
$package = $metadata.packages | Where-Object name -eq "contextwake"
if ($null -eq $package) {
    throw "contextwake package was not found in cargo metadata"
}

$version = $package.version
$isWindowsTarget = $Target -like "*-windows-*"
$executableNames = if ($isWindowsTarget) { @("ctx.exe", "ctxwake.exe") } else { @("ctx", "ctxwake") }
$hostIsWindows = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
if ($hostIsWindows -and -not $isWindowsTarget) {
    throw "Unix release archives must be packaged on a Unix host so executable permissions are preserved"
}
if ([string]::IsNullOrWhiteSpace($BinaryDirectory)) {
    $binaryRoot = Join-Path $repositoryRoot (Join-Path "target" (Join-Path $Target "release"))
}
else {
    $binaryRoot = Resolve-LocationPath -Path $BinaryDirectory
}
foreach ($executableName in $executableNames) {
    $binary = Join-Path $binaryRoot $executableName
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
        throw "release binary was not found at $binary"
    }
}

$archiveRootName = "contextwake-v$version-$Label"
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $outputRoot = Join-Path $repositoryRoot "dist"
}
else {
    $outputRoot = Resolve-LocationPath -Path $OutputDirectory
}
$outputRoot = $outputRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
$stage = Join-Path $outputRoot $archiveRootName
$repositoryRoot = $repositoryRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
$pathComparison = if ($hostIsWindows) { [System.StringComparison]::OrdinalIgnoreCase } else { [System.StringComparison]::Ordinal }
if ([string]::Equals($outputRoot, $repositoryRoot, $pathComparison)) {
    throw "output directory must not be the repository root"
}
$repositoryPrefix = $repositoryRoot + [System.IO.Path]::DirectorySeparatorChar
if (-not $outputRoot.StartsWith($repositoryPrefix, $pathComparison)) {
    throw "output directory must stay inside the repository: $outputRoot"
}

New-Item -ItemType Directory -Force -Path $outputRoot | Out-Null
if (Test-Path -LiteralPath $stage) {
    Remove-Item -LiteralPath $stage -Recurse -Force
}
New-Item -ItemType Directory -Path $stage | Out-Null
foreach ($executableName in $executableNames) {
    Copy-Item -LiteralPath (Join-Path $binaryRoot $executableName) -Destination $stage
}
Copy-Item -LiteralPath (Join-Path $repositoryRoot "README.md") -Destination $stage
Copy-Item -LiteralPath (Join-Path $repositoryRoot "LICENSE") -Destination $stage

if ($isWindowsTarget) {
    $artifact = Join-Path $outputRoot "$archiveRootName.zip"
    if (Test-Path -LiteralPath $artifact) {
        Remove-Item -LiteralPath $artifact -Force
    }
    # Write entries with '/' separators as the ZIP specification requires; Compress-Archive in Windows
    # PowerShell 5.1 writes '\', which other unzip tools turn into literal file names.
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $stream = [System.IO.File]::Open($artifact, [System.IO.FileMode]::CreateNew)
    try {
        $archive = New-Object System.IO.Compression.ZipArchive($stream, [System.IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($entryName in @($executableNames + @("LICENSE", "README.md"))) {
                $null = [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                    $archive,
                    (Join-Path $stage $entryName),
                    "$archiveRootName/$entryName",
                    [System.IO.Compression.CompressionLevel]::Optimal
                )
            }
        }
        finally {
            $archive.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
} else {
    $artifact = Join-Path $outputRoot "$archiveRootName.tar.gz"
    if (Test-Path -LiteralPath $artifact) {
        Remove-Item -LiteralPath $artifact -Force
    }
    foreach ($executableName in $executableNames) {
        chmod 755 (Join-Path $stage $executableName)
        if ($LASTEXITCODE -ne 0) {
            throw "chmod failed with exit code $LASTEXITCODE"
        }
    }
    tar -C $outputRoot -czf $artifact $archiveRootName
    if ($LASTEXITCODE -ne 0) {
        throw "tar failed with exit code $LASTEXITCODE"
    }
}

Remove-Item -LiteralPath $stage -Recurse -Force
$artifact = [System.IO.Path]::GetFullPath($artifact)
Write-Output "Created $artifact"
if ($env:GITHUB_OUTPUT) {
    [System.IO.File]::AppendAllText($env:GITHUB_OUTPUT, "artifact=$artifact`n", (New-Object System.Text.UTF8Encoding($false)))
}
