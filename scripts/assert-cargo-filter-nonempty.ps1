param(
    [Parameter(Mandatory = $true, Position = 0)]
    [int]$MinTests,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Error "error: $Message"
    exit 1
}

if ($MinTests -lt 1) {
    Fail "<min-tests> must be greater than zero"
}
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
    $output | ForEach-Object { Write-Error $_ }
    Fail "cargo $($CargoArgs -join ' ') -- --list failed with exit code $status"
}

$count = @($output | Where-Object { $_ -match ':\s+test$' }).Count
if ($count -lt $MinTests) {
    $output | ForEach-Object { Write-Error $_ }
    Fail "cargo $($CargoArgs -join ' ') selected $count tests; expected at least $MinTests"
}

Write-Host "cargo-filter-nonempty=ok selected=$count min=$MinTests command=cargo $($CargoArgs -join ' ')"
