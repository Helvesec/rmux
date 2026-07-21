param(
    [Parameter(Mandatory = $true)]
    [string]$Archive,
    [Parameter(Mandatory = $true)]
    [string]$Package,
    [Parameter(Mandatory = $true)]
    [string]$TestTarget,
    [Parameter(Mandatory = $true)]
    [string]$TestName,
    [string]$WorkspaceRoot = (Join-Path $PSScriptRoot ".."),
    [int]$TestThreads = 1
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    throw "error: $Message"
}

function Invoke-CargoCapture([string[]]$Arguments, [string]$StderrPath) {
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        # Keep native stderr separate so cargo progress cannot corrupt the
        # machine-readable nextest listing on stdout.
        $ErrorActionPreference = "Continue"
        $stdout = & cargo @Arguments 2> $StderrPath
        $status = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }

    $stderr = if (Test-Path -LiteralPath $StderrPath -PathType Leaf) {
        Get-Content -LiteralPath $StderrPath -Raw
    } else {
        ""
    }
    [pscustomobject]@{
        Stdout = $stdout
        Stderr = $stderr
        Status = $status
    }
}

function Assert-ExactSelection([string[]]$Lines) {
    $selected = @($Lines | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    if ($selected.Count -ne 1) {
        Fail "cargo nextest list selected $($selected.Count) tests; expected exactly one"
    }
    $expected = "$Package::$TestTarget $TestName"
    if (-not [string]::Equals(
            $selected[0].Trim(),
            $expected,
            [System.StringComparison]::Ordinal
        )) {
        Fail "cargo nextest list selected an unexpected test: $($selected[0]); expected $expected"
    }
}

foreach ($value in @($Archive, $Package, $TestTarget, $TestName, $WorkspaceRoot)) {
    if ([string]::IsNullOrWhiteSpace($value)) {
        Fail "archive, package, test target, test name, and workspace root must be non-empty"
    }
}
foreach ($identity in @($Package, $TestTarget, $TestName)) {
    if ($identity -notmatch '^[A-Za-z0-9_:-]+$') {
        Fail "package and test identities contain unsupported filter characters: $identity"
    }
}
if ($TestThreads -lt 1) {
    Fail "test threads must be greater than zero"
}

$archiveFull = [System.IO.Path]::GetFullPath($Archive)
if (-not (Test-Path -LiteralPath $archiveFull -PathType Leaf)) {
    Fail "nextest archive not found: $archiveFull"
}
$workspaceFull = [System.IO.Path]::GetFullPath($WorkspaceRoot)
if (-not (Test-Path -LiteralPath $workspaceFull -PathType Container)) {
    Fail "workspace root not found: $workspaceFull"
}

$temporaryRoot = Join-Path (
    [System.IO.Path]::GetTempPath()
) "rmux-nextest-package-smoke-$PID-$([guid]::NewGuid().ToString('N'))"
$extractRoot = Join-Path $temporaryRoot "extract"
$listStderr = Join-Path $temporaryRoot "nextest-list.stderr"
New-Item -ItemType Directory -Force -Path $extractRoot | Out-Null
try {
    # Archive mode rejects Cargo's -p/--test selectors. Express the same
    # package, integration-test target, and exact test identity as a filterset.
    $filterExpression = "kind(test) & package(=$Package) & binary(=$TestTarget) & test(=$TestName)"
    $selection = @(
        "nextest", "list",
        "--archive-file", $archiveFull,
        "--filterset", $filterExpression,
        "--message-format", "oneline",
        "--color", "never",
        "--", $TestName, "--exact"
    )
    $listResult = Invoke-CargoCapture $selection $listStderr
    if ($listResult.Status -ne 0) {
        Fail "cargo nextest list failed with exit code $($listResult.Status)`n$($listResult.Stderr)"
    }
    Assert-ExactSelection $listResult.Stdout

    & cargo @(
        "nextest", "run",
        "--archive-file", $archiveFull,
        "--extract-to", $extractRoot,
        "--extract-overwrite",
        "--workspace-remap", $workspaceFull,
        "--filterset", $filterExpression,
        "--test-threads", "$TestThreads",
        "--retries", "0",
        "--flaky-result", "fail",
        "--", $TestName, "--exact"
    )
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo nextest run failed for $Package/$TestTarget::$TestName with exit code $LASTEXITCODE"
    }

    Write-Output "nextest-package-smoke=ok package=$Package target=$TestTarget test=$TestName"
} finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
