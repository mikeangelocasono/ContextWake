param([string]$Directory = "dist")

$ErrorActionPreference = "Stop"
$root = [System.IO.Path]::GetFullPath($Directory)
if (-not (Test-Path -LiteralPath $root -PathType Container)) {
    throw "artifact directory does not exist: $root"
}

$manifest = Join-Path $root "SHA256SUMS"
Get-ChildItem -LiteralPath $root -File |
    Where-Object Name -ne "SHA256SUMS" |
    Sort-Object Name |
    ForEach-Object {
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant()
        "$hash  $($_.Name)"
    } | Set-Content -LiteralPath $manifest -Encoding ascii

Write-Output "Created $manifest"
