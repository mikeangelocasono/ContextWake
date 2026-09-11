param([string]$Binary = "target/release/ctxwake.exe")

$ErrorActionPreference = "Stop"
$binaryPath = [System.IO.Path]::GetFullPath($Binary)
if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "Build ContextWake first; binary not found at $binaryPath"
}

$tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$demoRoot = Join-Path $tempRoot "contextwake-deterministic-demo"
$resolvedDemo = [System.IO.Path]::GetFullPath($demoRoot)
if (-not $resolvedDemo.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "demo path escaped the operating-system temporary directory"
}
if (Test-Path -LiteralPath $resolvedDemo) {
    Remove-Item -LiteralPath $resolvedDemo -Recurse -Force
}

$repository = Join-Path $resolvedDemo "calculator"
$state = Join-Path $resolvedDemo "state"
New-Item -ItemType Directory -Path $repository | Out-Null

git -C $repository init --initial-branch=main
git -C $repository config user.name "ContextWake Demo"
git -C $repository config user.email "demo@example.invalid"
Set-Content -LiteralPath (Join-Path $repository "calculator.rs") -Value "pub fn add(a: i32, b: i32) -> i32 { a + b }" -Encoding utf8
Set-Content -LiteralPath (Join-Path $repository "README.md") -Value "# Calculator" -Encoding utf8
git -C $repository add calculator.rs README.md
git -C $repository commit -m "initial calculator"

Add-Content -LiteralPath (Join-Path $repository "calculator.rs") -Value "`npub fn subtract(a: i32, b: i32) -> i32 { a - b }"
git -C $repository add calculator.rs
Set-Content -LiteralPath (Join-Path $repository "division.rs") -Value "// pending division implementation" -Encoding utf8
Add-Content -LiteralPath (Join-Path $repository "README.md") -Value "`nDivision is pending."

$env:CONTEXTWAKE_HOME = $state
$env:CONTEXTWAKE_CODEX_BIN = $binaryPath
$env:CONTEXTWAKE_CLAUDE_BIN = $binaryPath

Write-Output "=== DETERMINISTIC PROVIDER FIXTURE: no live AI output ==="
& $binaryPath profile add Personal --agent codex --model-provider openai
& $binaryPath profile add Work --agent claude --model-provider anthropic --model sonnet
& $binaryPath workspace add $repository --trust
& $binaryPath checkpoint create --workspace $repository `
    --objective "Implement calculator division with division-by-zero handling" `
    --task "Add division and division-by-zero handling" `
    --completed "Addition and subtraction" `
    --decision "Errors return Result instead of panicking" `
    --pending "Division tests and documentation"
& $binaryPath profile use Work --handoff --workspace $repository --objective "Continue calculator division in Claude Code"
& $binaryPath handoff list
& $binaryPath status $repository

Write-Output "Disposable repository: $repository"
Write-Output "Isolated ContextWake state: $state"
