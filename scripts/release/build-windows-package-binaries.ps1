param(
    [Parameter(Mandatory = $true)][string]$Target,
    [Parameter(Mandatory = $true)][string]$Profile,
    [Parameter(Mandatory = $true)][string]$ProfileDir,
    [Parameter(Mandatory = $true)][string[]]$TinyCargoArgs
)
$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
. (Join-Path $repoRoot "scripts/packaging/layout-tool.ps1")
$python = Resolve-LayoutPython
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
if (-not [System.IO.Path]::IsPathRooted($targetDir)) { $targetDir = Join-Path $repoRoot $targetDir }
$binaryDir = Join-Path $targetDir (Join-Path $Target $ProfileDir)
# Capture the full helper, tiny CLI and full daemon together, before the release
# tests and the reuse manifest bind those exact binaries. Never infer old logs.
$tool = Join-Path $repoRoot "scripts/packaging/notices.py"
$logs = Join-Path $binaryDir ("third-party-notices/build-" + [guid]::NewGuid().ToString("N"))
$cargoArgs = @("build", "--package", "rmux", "--locked", "--profile", $Profile, "--target", $Target)
function Capture([string]$Role, [string[]]$BuildArgs) {
    $arguments = @("capture", "--log", (Join-Path $logs "$Role.jsonl"), "--", "cargo") +
        $cargoArgs + $BuildArgs + @("--message-format=json-render-diagnostics")
    & $python $tool @arguments
    if ($LASTEXITCODE -ne 0) { throw "Windows $Role build failed" }
}
Capture "full" @("--bin", "rmux")
$original = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $binaryDir "rmux.exe")).Hash
Copy-Item -LiteralPath (Join-Path $binaryDir "rmux.exe") -Destination (Join-Path $binaryDir "rmux-full.exe") -Force
Capture "tiny" ($TinyCargoArgs + @("--bin", "rmux"))
Capture "daemon" @("--bin", "rmux-daemon")
if ((Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $binaryDir "rmux-full.exe")).Hash -ne $original) {
    throw "Windows full helper changed while capturing package binaries"
}
& $python $tool record --binary-dir $binaryDir `
    --log (Join-Path $logs "full.jsonl") --log (Join-Path $logs "tiny.jsonl") --log (Join-Path $logs "daemon.jsonl")
if ($LASTEXITCODE -ne 0) { throw "Windows notice generation failed" }
