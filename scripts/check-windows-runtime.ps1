[CmdletBinding()]
param(
    # Windows executables to inspect, for example target/x86_64-pc-windows-msvc/release/ctx.exe.
    [Parameter(Mandatory = $true)][string[]]$Path
)

# Fails when an executable imports a runtime DLL that does not ship with Windows.
# The Visual C++ runtime (VCRUNTIME140.dll, MSVCP140.dll, ...) is installed only by
# the Visual C++ Redistributable, and MinGW runtime DLLs only by GCC toolchains, so
# a release binary that imports one cannot start on a clean Windows system.
# The import table is parsed from the file; the executable is never run.

$ErrorActionPreference = "Stop"
Set-StrictMode -Version 2.0

$NonSystemRuntimePattern = '^(vcruntime[0-9]+(_[0-9]+)?d?|msvcp[0-9]+(_[a-z0-9_]+)?d?|ucrtbased|concrt[0-9]+d?|vccorlib[0-9]+d?|vcomp[0-9]+d?|libgcc_s_[a-z0-9_-]+|libstdc\+\+-[0-9]+|libwinpthread-[0-9]+)\.dll$'

function Get-PeImportedDllNames {
    param([Parameter(Mandatory = $true)][string]$File)

    $bytes = [IO.File]::ReadAllBytes($File)

    function Assert-Range {
        param([long]$Offset, [long]$Length)
        if ($Offset -lt 0 -or $Length -lt 0 -or ($Offset + $Length) -gt $bytes.Length) {
            throw "$File is not a valid PE file (offset $Offset is outside the file)"
        }
    }
    function Read-UInt16 { param([long]$Offset) Assert-Range $Offset 2; return [long][BitConverter]::ToUInt16($bytes, [int]$Offset) }
    function Read-UInt32 { param([long]$Offset) Assert-Range $Offset 4; return [long][BitConverter]::ToUInt32($bytes, [int]$Offset) }
    function Read-UInt64 { param([long]$Offset) Assert-Range $Offset 8; return [decimal][BitConverter]::ToUInt64($bytes, [int]$Offset) }

    if ($bytes.Length -lt 64 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
        throw "$File is not a valid PE file (missing MZ header)"
    }
    $peOffset = Read-UInt32 0x3C
    Assert-Range $peOffset 24
    if ($bytes[$peOffset] -ne 0x50 -or $bytes[$peOffset + 1] -ne 0x45 -or $bytes[$peOffset + 2] -ne 0 -or $bytes[$peOffset + 3] -ne 0) {
        throw "$File is not a valid PE file (missing PE signature)"
    }
    $coffHeader = $peOffset + 4
    $sectionCount = Read-UInt16 ($coffHeader + 2)
    $optionalHeaderSize = Read-UInt16 ($coffHeader + 16)
    $optionalHeader = $coffHeader + 20
    $magic = Read-UInt16 $optionalHeader
    if ($magic -eq 0x20B) {
        $imageBase = Read-UInt64 ($optionalHeader + 24)
        $dataDirectories = $optionalHeader + 112
    }
    elseif ($magic -eq 0x10B) {
        $imageBase = [decimal](Read-UInt32 ($optionalHeader + 28))
        $dataDirectories = $optionalHeader + 96
    }
    else {
        throw "$File is not a valid PE file (unknown optional header magic $magic)"
    }
    $directoryCount = Read-UInt32 ($dataDirectories - 4)

    $sections = @()
    $sectionTable = $optionalHeader + $optionalHeaderSize
    for ($index = 0; $index -lt $sectionCount; $index++) {
        $entry = $sectionTable + (40 * $index)
        $sections += New-Object PSObject -Property @{
            VirtualSize = Read-UInt32 ($entry + 8)
            VirtualAddress = Read-UInt32 ($entry + 12)
            RawSize = Read-UInt32 ($entry + 16)
            RawPointer = Read-UInt32 ($entry + 20)
        }
    }

    function ConvertTo-FileOffset {
        param([long]$Rva)
        foreach ($section in $sections) {
            $span = [Math]::Max($section.VirtualSize, $section.RawSize)
            if ($Rva -ge $section.VirtualAddress -and $Rva -lt ($section.VirtualAddress + $span)) {
                return $Rva - $section.VirtualAddress + $section.RawPointer
            }
        }
        throw "$File is not a valid PE file (RVA $Rva is outside every section)"
    }
    function Read-AsciiName {
        param([long]$Rva)
        $start = ConvertTo-FileOffset $Rva
        $end = $start
        while ($end -lt $bytes.Length -and $bytes[$end] -ne 0 -and ($end - $start) -lt 512) {
            $end++
        }
        return [Text.Encoding]::ASCII.GetString($bytes, [int]$start, [int]($end - $start))
    }

    $names = @()
    if ($directoryCount -gt 1) {
        $importRva = Read-UInt32 ($dataDirectories + 8)
        if ($importRva -ne 0) {
            $descriptor = ConvertTo-FileOffset $importRva
            for ($index = 0; $index -lt 4096; $index++) {
                $nameRva = Read-UInt32 ($descriptor + 12)
                if ($nameRva -eq 0) {
                    break
                }
                $names += Read-AsciiName $nameRva
                $descriptor += 20
            }
        }
    }
    if ($directoryCount -gt 13) {
        $delayRva = Read-UInt32 ($dataDirectories + (8 * 13))
        if ($delayRva -ne 0) {
            $descriptor = ConvertTo-FileOffset $delayRva
            for ($index = 0; $index -lt 4096; $index++) {
                $attributes = Read-UInt32 $descriptor
                $nameRva = Read-UInt32 ($descriptor + 4)
                if ($nameRva -eq 0) {
                    break
                }
                if (($attributes -band 1) -eq 0) {
                    # Version-1 delay descriptors store virtual addresses instead of RVAs.
                    $nameRva = [long]([decimal]$nameRva - $imageBase)
                }
                $names += Read-AsciiName $nameRva
                $descriptor += 32
            }
        }
    }
    return $names
}

$failures = @()
foreach ($item in $Path) {
    $file = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($item)
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        throw "executable not found: $file"
    }
    $imports = @(Get-PeImportedDllNames -File $file)
    $nonSystem = @($imports | Where-Object { $_ -match $NonSystemRuntimePattern })
    Write-Host "$file imports: $($imports -join ', ')"
    if ($nonSystem.Count -gt 0) {
        $failures += "$file imports $($nonSystem -join ', ')"
    }
}
if ($failures.Count -gt 0) {
    throw ("Windows executables must not depend on runtime DLLs that are not part of Windows. " +
        ($failures -join '; ') +
        ". Build x86_64-pc-windows-msvc with -C target-feature=+crt-static (.cargo/config.toml) and make sure RUSTFLAGS does not replace it.")
}
Write-Host "OK: no Visual C++ or MinGW runtime DLL imports."
