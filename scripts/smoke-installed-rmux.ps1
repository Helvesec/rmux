param(
    [string]$Rmux = "rmux",
    [switch]$SkipDaemon,
    [switch]$RequireDaemonCommand
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Error "error: $Message"
    exit 1
}

function Invoke-RmuxSuccess([string[]]$Arguments) {
    $output = & $Rmux @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        Fail "command failed: $Rmux $($Arguments -join ' ')`n$output"
    }
    $output
}

function Assert-RmuxHelperFallback {
    $output = & $Rmux --help 2>&1
    $status = $LASTEXITCODE
    if ($status -ne 0 -and $status -ne 1) {
        Fail "rmux --help failed with unexpected exit code $status`n$output"
    }
    if (($output -join "`n") -notmatch 'usage: rmux') {
        Fail "rmux --help did not reach the private helper`n$output"
    }
}

# `--help` is intentionally outside the tiny direct path. It proves that the
# installed public CLI can reach the complete command surface: directly for full
# CLIs, or through the private helper for tiny dispatchers.
Assert-RmuxHelperFallback
$diagnoseJson = Invoke-RmuxSuccess @("diagnose", "--json")
try {
    $diagnoseJson | ConvertFrom-Json | Out-Null
} catch {
    Fail "rmux diagnose --json returned invalid JSON: $_"
}

if ($RequireDaemonCommand -and -not (Get-Command rmux-daemon -ErrorAction SilentlyContinue)) {
    Fail "rmux-daemon is not discoverable on PATH"
}

if (-not $SkipDaemon) {
    $label = "installed-smoke-$PID-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
    $sessionName = "installed_smoke_$PID"
    try {
        Invoke-RmuxSuccess @("-L", $label, "new-session", "-d", "-s", $sessionName, "cmd.exe", "/d", "/q", "/k") | Out-Null
        Invoke-RmuxSuccess @("-L", $label, "has-session", "-t", $sessionName) | Out-Null
    } finally {
        & $Rmux "-L" $label "kill-server" | Out-Null
    }
}

Write-Output "rmux=$Rmux"
Write-Output "helper_fallback=ok"
Write-Output "daemon_smoke=$(if ($SkipDaemon) { 'skipped' } else { 'ok' })"
