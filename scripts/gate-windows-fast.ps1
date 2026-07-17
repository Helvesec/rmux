param(
    [string]$Target = "x86_64-pc-windows-msvc",
    [int]$TestThreads = [Environment]::ProcessorCount,
    [switch]$SkipDoc,
    [switch]$SourceGates,
    [switch]$InstallNextest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Error "error: $Message"
    exit 1
}

function Run-Step([string]$Label, [scriptblock]$Command) {
    Write-Host ""
    Write-Host "[gate] $Label"
    $start = Get-Date
    & $Command
    if ($LASTEXITCODE -ne 0) {
        $elapsed = [int]((Get-Date) - $start).TotalSeconds
        Fail "$Label failed after ${elapsed}s"
    }
    $elapsed = [int]((Get-Date) - $start).TotalSeconds
    Write-Host "[gate] PASS $Label (${elapsed}s)"
}

function Has-Command([string]$Name) {
    $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

function Has-Nextest {
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        & cargo nextest --version *> $null
        return $LASTEXITCODE -eq 0
    } catch {
        return $false
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
}

if ($TestThreads -lt 1) {
    Fail "-TestThreads must be greater than zero"
}

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot
$assertCargoFilter = Join-Path $PSScriptRoot "assert-cargo-filter-nonempty.ps1"

Remove-Item Env:\RUST_TEST_THREADS -ErrorAction SilentlyContinue
$env:RUST_MIN_STACK = "8388608"
if (-not $env:CARGO_INCREMENTAL) {
    $env:CARGO_INCREMENTAL = "1"
}
if (-not $env:CARGO_BUILD_JOBS) {
    $env:CARGO_BUILD_JOBS = [string][Environment]::ProcessorCount
}
if (-not $env:CARGO_TERM_COLOR) {
    $env:CARGO_TERM_COLOR = "always"
}

if ($InstallNextest -and -not (Has-Nextest)) {
    Run-Step "install cargo-nextest" { cargo install cargo-nextest --locked }
}

Write-Host "[gate] target=$Target"
Write-Host "[gate] test_threads=$TestThreads"
Write-Host "[gate] rust_min_stack=$env:RUST_MIN_STACK"
Write-Host "[gate] cargo_build_jobs=$env:CARGO_BUILD_JOBS"
Write-Host "[gate] cargo_incremental=$env:CARGO_INCREMENTAL"

Run-Step "cargo fmt" { cargo fmt --all --check }
Run-Step "cargo clippy" { cargo clippy --workspace --all-targets --locked --target $Target -- -D warnings }
Run-Step "cargo filter nonempty rmux-client output writer" {
    & $assertCargoFilter 1 -- test -p rmux-client --locked output_writer_failure_wakes
}
Run-Step "cargo filter nonempty rmux-client blocked output" {
    & $assertCargoFilter 1 -- test -p rmux-client --locked blocked_console_output_does_not_block_input_forwarding
}
Run-Step "cargo filter nonempty windows prompt overlay chain" {
    & $assertCargoFilter 2 -- test -p rmux --locked --test windows_prompt_overlay_chain
}
Run-Step "cargo filter nonempty windows ctrl matrix spec" {
    & $assertCargoFilter 1 -- test -p rmux --locked --test windows_ctrl_matrix_spec
}

if (Has-Nextest) {
    Run-Step "cargo nextest workspace" {
        cargo nextest run --workspace --all-targets --locked --target $Target --no-fail-fast --test-threads $TestThreads
    }
} else {
    Write-Warning "cargo-nextest not found; falling back to slower cargo test."
    Write-Warning "Install with: cargo install cargo-nextest --locked"
    Run-Step "cargo test workspace" {
        cargo test --workspace --all-targets --locked --target $Target --no-fail-fast -- --test-threads=$TestThreads
    }
}

if (-not $SkipDoc) {
    Run-Step "cargo doc tests" { cargo test --workspace --doc --locked --target $Target }
    Run-Step "cargo doc" { cargo doc --workspace --locked --no-deps --target $Target }
}

if ($SourceGates) {
    if (-not (Has-Command "bash")) {
        Fail "-SourceGates requires bash in PATH"
    }
    Run-Step "runtime network source scan" { bash scripts/no-network-in-runtime.sh }
    Run-Step "platform neutrality source scan" { bash scripts/check-platform-neutrality.sh }
    Run-Step "debug_assert side-effect scan" { bash scripts/no-debug-assert-side-effects.sh }
}

Write-Host ""
Write-Host "[gate] PASS fast Windows gate"
