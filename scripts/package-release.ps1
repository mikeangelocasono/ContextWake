param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9_-]+$')]
    [string]$Target,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9_-]+$')]
    [string]$Label,
    [string]$OutputDirectory = "dist"
)

$ErrorActionPreference = "Stop"

$metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
$package = $metadata.packages | Where-Object name -eq "contextwake"
if ($null -eq $package) {
    throw "contextwake package was not found in cargo metadata"
}

$version = $package.version
$executableName = if ($Target -like "*-windows-*") { "ctxwake.exe" } else { "ctxwake" }
$hostIsWindows = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
if ($hostIsWindows -and $Target -notlike "*-windows-*") {
    throw "Unix release archives must be packaged on a Unix host so executable permissions are preserved"
}
$binary = Join-Path "target/$Target/release" $executableName
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "release binary was not found at $binary"
}

$archiveRootName = "contextwake-v$version-$Label"
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
$stage = Join-Path $outputRoot $archiveRootName
$repositoryRoot = [System.IO.Path]::GetFullPath(".")
if ($outputRoot -eq $repositoryRoot) {
    throw "output directory must not be the repository root"
}
$repositoryPrefix = $repositoryRoot.TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
) + [System.IO.Path]::DirectorySeparatorChar
if (-not $outputRoot.StartsWith($repositoryPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "output directory must stay inside the repository"
}

New-Item -ItemType Directory -Force -Path $outputRoot | Out-Null
if (Test-Path -LiteralPath $stage) {
    Remove-Item -LiteralPath $stage -Recurse -Force
}
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item -LiteralPath $binary -Destination $stage
Copy-Item -LiteralPath "README.md" -Destination $stage
Copy-Item -LiteralPath "LICENSE" -Destination $stage

if ($Target -like "*-windows-*") {
    $artifact = Join-Path $outputRoot "$archiveRootName.zip"
    if (Test-Path -LiteralPath $artifact) {
        Remove-Item -LiteralPath $artifact -Force
    }
    Compress-Archive -LiteralPath $stage -DestinationPath $artifact -CompressionLevel Optimal
} else {
    $artifact = Join-Path $outputRoot "$archiveRootName.tar.gz"
    if (Test-Path -LiteralPath $artifact) {
        Remove-Item -LiteralPath $artifact -Force
    }
    chmod 755 (Join-Path $stage $executableName)
    if ($LASTEXITCODE -ne 0) {
        throw "chmod failed with exit code $LASTEXITCODE"
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
    "artifact=$artifact" | Out-File -FilePath $env:GITHUB_OUTPUT -Append -Encoding utf8
}
