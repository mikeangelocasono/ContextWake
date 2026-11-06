[CmdletBinding()]
param(
    # Directory holding the release archives; defaults to the repository's dist/. Relative paths resolve
    # against the current PowerShell location.
    [string]$Directory = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

if ([string]::IsNullOrWhiteSpace($Directory)) {
    $root = [System.IO.Path]::GetFullPath((Join-Path (Join-Path $PSScriptRoot "..") "dist"))
}
else {
    $root = [System.IO.Path]::GetFullPath($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Directory))
}
if (-not (Test-Path -LiteralPath $root -PathType Container)) {
    throw "artifact directory does not exist: $root"
}

$manifest = Join-Path $root "SHA256SUMS"
$lines = @(
    Get-ChildItem -LiteralPath $root -File |
        Where-Object Name -ne "SHA256SUMS" |
        Sort-Object Name |
        ForEach-Object {
            $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant()
            "$hash  $($_.Name)"
        }
)
if ($lines.Count -eq 0) {
    throw "no release artifacts were found in $root"
}
# LF line endings on every platform: scripts/install.sh rejects CRLF manifests, and Set-Content would
# write CRLF on Windows.
[System.IO.File]::WriteAllText($manifest, (($lines -join "`n") + "`n"), (New-Object System.Text.ASCIIEncoding))

Write-Output "Created $manifest"
