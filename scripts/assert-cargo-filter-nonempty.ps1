$Arguments = @($args)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    [Console]::Error.WriteLine("error: $Message")
    exit 1
}

function Write-Stderr([object]$Value) {
    [Console]::Error.WriteLine([string]$Value)
}

if ($Arguments.Count -eq 0) {
    Fail "usage: scripts/assert-cargo-filter-nonempty.ps1 <min-tests> -- <cargo-test-args...>"
}

$minTestsText = $Arguments[0]
$MinTests = 0
if (-not [int]::TryParse($minTestsText, [ref]$MinTests)) {
    Fail "<min-tests> must be a positive integer"
}
if ($MinTests -lt 1) {
    Fail "<min-tests> must be greater than zero"
}
$CargoArgs = @($Arguments | Select-Object -Skip 1)
if ($CargoArgs.Count -gt 0 -and $CargoArgs[0] -eq "--") {
    $CargoArgs = @($CargoArgs | Select-Object -Skip 1)
}
if ($CargoArgs.Count -eq 0) {
    Fail "cargo arguments are required"
}
if ($CargoArgs[0] -ne "test") {
    Fail "cargo arguments must start with test"
}

$previousErrorActionPreference = $ErrorActionPreference
try {
    $ErrorActionPreference = "Continue"
    $output = & cargo @CargoArgs -- --list 2>&1
    $status = $LASTEXITCODE
} finally {
    $ErrorActionPreference = $previousErrorActionPreference
}

if ($status -ne 0) {
    $output | ForEach-Object { Write-Stderr $_ }
    Fail "cargo $($CargoArgs -join ' ') -- --list failed with exit code $status"
}

$count = @($output | Where-Object { $_ -match ':\s+test$' }).Count
if ($count -lt $MinTests) {
    $output | ForEach-Object { Write-Stderr $_ }
    Fail "cargo $($CargoArgs -join ' ') selected $count tests; expected at least $MinTests"
}

Write-Host "cargo-filter-nonempty=ok selected=$count min=$MinTests command=cargo $($CargoArgs -join ' ')"
