param(
    [string]$OutputDir = "target\release-review-gate-windows",
    [string]$TargetDir = "target\release-review-gate-windows-cargo",
    [switch]$SkipPackage,
    [switch]$SkipClippy,
    [switch]$RunCtrlMatrixSmoke
)

$ErrorActionPreference = "Stop"

$env:CARGO_TARGET_DIR = $TargetDir
if (-not $env:CARGO_INCREMENTAL) {
    $env:CARGO_INCREMENTAL = "0"
}
if (-not $env:CARGO_BUILD_JOBS) {
    $env:CARGO_BUILD_JOBS = "1"
}
if (-not $env:CARGO_PROFILE_DEV_DEBUG) {
    $env:CARGO_PROFILE_DEV_DEBUG = "0"
}
if (-not $env:CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG) {
    $env:CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG = "0"
}
if (-not $env:CARGO_PROFILE_TEST_DEBUG) {
    $env:CARGO_PROFILE_TEST_DEBUG = "0"
}
$pdbSuppressFlag = "-Clink-arg=/DEBUG:NONE"
if (-not $env:RUSTFLAGS) {
    $env:RUSTFLAGS = $pdbSuppressFlag
} elseif ($env:RUSTFLAGS -notlike "*$pdbSuppressFlag*") {
    $env:RUSTFLAGS = "$env:RUSTFLAGS $pdbSuppressFlag"
}

$assertCargoFilter = Join-Path $PSScriptRoot "assert-cargo-filter-nonempty.ps1"

function Step([string]$Name, [scriptblock]$Body) {
    Write-Host ""
    Write-Host "[release-review-windows] $Name"
    & $Body
}

function Run([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Program $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

function Assert-CargoFilter([int]$MinTests, [string[]]$CargoArguments) {
    $arguments = @([string]$MinTests, "--") + $CargoArguments
    & $assertCargoFilter @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo filter check failed for cargo $($CargoArguments -join ' ')"
    }
}

function Run-PythonScript([string]$Script, [string[]]$Arguments = @()) {
    $python = Get-Command python -ErrorAction SilentlyContinue
    if (-not $python) {
        $python = Get-Command python3 -ErrorAction SilentlyContinue
    }
    if (-not $python) {
        throw "python is required for $Script"
    }
    Run $python.Source (@($Script) + $Arguments)
}

function Read-CargoPackageVersion([string]$Manifest) {
    $inPackage = $false
    $workspaceVersion = $null
    $inWorkspacePackage = $false
    foreach ($line in Get-Content -LiteralPath $Manifest) {
        if ($line -match '^\s*\[workspace\.package\]\s*$') {
            $inWorkspacePackage = $true
            $inPackage = $false
            continue
        }
        if ($line -match '^\s*\[package\]\s*$') {
            $inPackage = $true
            $inWorkspacePackage = $false
            continue
        }
        if ($line -match '^\s*\[') {
            $inPackage = $false
            $inWorkspacePackage = $false
        }
        if ($inWorkspacePackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            $workspaceVersion = $Matches[1]
        }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            return $Matches[1]
        }
        if ($inPackage -and $line -match '^\s*version\.workspace\s*=\s*true') {
            if ($null -ne $workspaceVersion) {
                return $workspaceVersion
            }
            $rootCargo = Join-Path (Get-Location) "Cargo.toml"
            $rootText = Get-Content -LiteralPath $rootCargo -Raw
            if ($rootText -match '(?ms)^\s*\[workspace\.package\].*?^\s*version\s*=\s*"([^"]+)"') {
                return $Matches[1]
            }
            throw "$Manifest uses version.workspace but Cargo.toml has no [workspace.package].version"
        }
    }
    throw "no [package].version found in $Manifest"
}

function Check-ReleaseVersions {
    $rootVersion = Read-CargoPackageVersion "Cargo.toml"
    $rootText = Get-Content -LiteralPath "Cargo.toml" -Raw
    if ($rootText -notmatch '(?ms)^\s*\[package\].*?^\s*publish\s*=\s*false') {
        throw "root rmux package must keep publish=false"
    }
    Write-Host "root-publish=false"
    $manpage = Get-Content -LiteralPath "docs\man\rmux.1" -Raw
    if ($manpage -notmatch [regex]::Escape("RMUX $rootVersion")) {
        throw "docs\man\rmux.1 does not contain RMUX $rootVersion"
    }

    $manifests = @(
        "crates\ratatui-rmux\Cargo.toml",
        "crates\rmux-client\Cargo.toml",
        "crates\rmux-core\Cargo.toml",
        "crates\rmux-ipc\Cargo.toml",
        "crates\rmux-os\Cargo.toml",
        "crates\rmux-proto\Cargo.toml",
        "crates\rmux-pty\Cargo.toml",
        "crates\rmux-render-core\Cargo.toml",
        "crates\rmux-sdk\Cargo.toml",
        "crates\rmux-server\Cargo.toml",
        "crates\rmux-types\Cargo.toml",
        "crates\rmux-web-crypto\Cargo.toml",
        "xtask\Cargo.toml"
    )
    foreach ($manifest in $manifests) {
        if (-not (Test-Path -LiteralPath $manifest)) {
            throw "missing manifest $manifest"
        }
        $version = Read-CargoPackageVersion $manifest
        Write-Host "$manifest $version"
        if ($version -ne $rootVersion) {
            throw "$manifest version $version != root version $rootVersion"
        }
    }
    Write-Host "release-version-check=ok"
}

function Count-CfgTargetOs([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return 0
    }
    $count = 0
    Get-ChildItem -LiteralPath $Path -Recurse -Filter *.rs | ForEach-Object {
        $matches = Select-String -LiteralPath $_.FullName -Pattern '#\s*\[\s*cfg\s*\(\s*target_os\s*=' -AllMatches
        foreach ($match in $matches) {
            $count += $match.Matches.Count
        }
    }
    return $count
}

function Check-CfgBudgets {
    $budgets = @(
        @("rmux-types", "crates\rmux-types\src", 0),
        @("rmux-core", "crates\rmux-core\src", 0),
        @("rmux-proto", "crates\rmux-proto\src", 0),
        @("rmux-server", "crates\rmux-server\src", 5),
        @("rmux-client", "crates\rmux-client\src", 10),
        @("rmux-ipc", "crates\rmux-ipc\src", 15),
        @("rmux-pty", "crates\rmux-pty\src", 20),
        @("rmux-os", "crates\rmux-os\src", 30),
        @("rmux-bin", "src", 10)
    )
    foreach ($budget in $budgets) {
        $count = Count-CfgTargetOs $budget[1]
        "{0,-14} {1,4} / {2}" -f $budget[0], $count, $budget[2]
        if ($count -gt $budget[2]) {
            throw "cfg(target_os) budget exceeded for $($budget[0])"
        }
    }
    Write-Host "cfg(target_os) check passed."
}

function Git-LsFiles([string[]]$Arguments) {
    $output = @(& git ls-files @Arguments)
    if ($LASTEXITCODE -ne 0) {
        throw "git ls-files $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
    $output
}

function Check-WorktreeHygiene {
    $trackedLocal = @(Git-LsFiles @(".claude", ".claude/**", ".codex", ".codex/**"))
    if ($trackedLocal.Count -gt 0) {
        $trackedLocal | ForEach-Object { Write-Error $_ }
        throw "tracked local assistant metadata is forbidden"
    }
    $trackedArtifacts = @(Git-LsFiles @(".release-deployment", ".release-deployment/**", ".rmux-audit", ".rmux-audit/**", "dist", "dist/**"))
    if ($trackedArtifacts.Count -gt 0) {
        $trackedArtifacts | ForEach-Object { Write-Error $_ }
        throw "tracked local deployment artifacts are forbidden"
    }
    $untrackedSockets = @(Git-LsFiles @("--others", "--exclude-standard") | Where-Object { $_ -match '\.(sock|socket)$' })
    if ($untrackedSockets.Count -gt 0) {
        $untrackedSockets | ForEach-Object { Write-Error $_ }
        throw "untracked socket-like files are forbidden in the worktree"
    }
    Write-Host "worktree-hygiene=ok"
}

Step "release versions" { Check-ReleaseVersions }
Step "changelog release audit" { Run-PythonScript "scripts\check-changelog-release.py" @("CHANGELOG.md") }
Step "tmux divergence ledger" { Run-PythonScript "scripts\check-tmux-release-ledger.py" }
Step "feature inventory" { Run "cargo" @("run", "--locked", "--package", "xtask", "--", "feature-inventory", "--check") }
Write-Host "cargo-target-dir=$env:CARGO_TARGET_DIR"
Write-Host "cargo-incremental=$env:CARGO_INCREMENTAL"
Write-Host "cargo-build-jobs=$env:CARGO_BUILD_JOBS"
Write-Host "cargo-profile-dev-debug=$env:CARGO_PROFILE_DEV_DEBUG"
Write-Host "cargo-profile-dev-build-override-debug=$env:CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG"
Write-Host "cargo-profile-test-debug=$env:CARGO_PROFILE_TEST_DEBUG"
Write-Host "rustflags=$env:RUSTFLAGS"
Step "formatting" { Run "cargo" @("fmt", "--all", "--check") }
Step "platform cfg budget" { Check-CfgBudgets }
Step "worktree hygiene" { Check-WorktreeHygiene }

if (-not $SkipClippy) {
    Step "workspace clippy" {
        Run "cargo" @("clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings")
    }
}

Step "tiny parser and boundary tests" {
    Assert-CargoFilter 1 @("test", "-p", "rmux", "--features", "tiny-cli", "tiny_main", "--locked")
    Run "cargo" @("test", "-p", "rmux", "--features", "tiny-cli", "tiny_main", "--locked")
}
Step "mutating target-action retry tests" {
    Assert-CargoFilter 1 @("test", "-p", "rmux", "--bin", "rmux", "--locked", "target_action_retry_is_limited")
    Run "cargo" @("test", "-p", "rmux", "--bin", "rmux", "--locked", "target_action_retry_is_limited")
}
Step "server lib tests" {
    Run "cargo" @("test", "-p", "rmux-server", "--lib", "--locked", "--", "--test-threads=1")
}
Step "SDK lib tests" {
    Run "cargo" @("test", "-p", "rmux-sdk", "--lib", "--locked", "--", "--test-threads=1")
}
Step "CLI acceptance matrix" {
    Run "cargo" @("test", "--locked", "--test", "acceptance_cli_matrix", "--", "--test-threads=1")
}
Step "source/config acceptance matrix" {
    Run "cargo" @("test", "--locked", "--test", "acceptance_source_config_matrix", "--", "--test-threads=1")
}
Step "target/format acceptance matrix" {
    Run "cargo" @("test", "--locked", "--test", "acceptance_target_format_matrix", "--", "--test-threads=1")
}
Step "Windows attach stream queue regressions" {
    Assert-CargoFilter 1 @("test", "-p", "rmux-client", "--locked", "output_writer_failure_wakes")
    Run "cargo" @("test", "-p", "rmux-client", "--locked", "output_writer_failure_wakes", "--", "--test-threads=1")
    Assert-CargoFilter 1 @("test", "-p", "rmux-client", "--locked", "blocked_console_output_does_not_block_input_forwarding")
    Run "cargo" @("test", "-p", "rmux-client", "--locked", "blocked_console_output_does_not_block_input_forwarding", "--", "--test-threads=1")
    Assert-CargoFilter 1 @("test", "-p", "rmux-client", "--locked", "output_backpressure_keeps_local_input_and_resize_live")
    Run "cargo" @("test", "-p", "rmux-client", "--locked", "output_backpressure_keeps_local_input_and_resize_live", "--", "--test-threads=1")
}
Step "Windows Ctrl matrix spec" {
    Run "powershell" @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "scripts\windows_ctrl_matrix.ps1", "-StaticMatrixSpec")
    Run "cargo" @("test", "--locked", "-p", "rmux", "--test", "windows_ctrl_matrix_spec", "--", "--test-threads=1")
}
Step "Windows attach exit probes" {
    Run "cargo" @("test", "--locked", "-p", "rmux", "--test", "windows_attach_exit", "--", "--test-threads=1")
}
Step "Windows mouse border resize probes" {
    Run "cargo" @("test", "--locked", "-p", "rmux", "--test", "windows_mouse_border_resize", "--", "--test-threads=1")
}
Step "Windows daemon integration" {
    Run "cargo" @("test", "--locked", "-p", "rmux", "--test", "internal_daemon_windows")
}
Step "Windows ConPTY integration" {
    Run "cargo" @("test", "--locked", "-p", "rmux-pty", "--test", "windows_conpty")
}

if (-not $SkipPackage) {
    Step "Windows package" {
        Run "powershell" @(
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            "scripts\package-windows.ps1",
            "-Configuration",
            "release",
            "-Target",
            "x86_64-pc-windows-msvc",
            "-OutputDir",
            $OutputDir,
            "-AllowStaleBinary"
        )
    }
    Step "Windows package verify" {
        $archive = Join-Path $OutputDir "rmux-$(Read-CargoPackageVersion 'Cargo.toml')-windows-x86_64.zip"
        Run "powershell" @(
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            "scripts\verify-package-windows.ps1",
            "-Archive",
            $archive,
            "-RunBinary",
            "-RunDaemonSmoke",
            "-RunSdkSmoke",
            "-RunMouseBorderSmoke",
            "-RequireReleaseArtifact"
        )
    }
}

if ($RunCtrlMatrixSmoke) {
    Step "Windows Ctrl matrix portable smoke" {
        $rmux = Join-Path $TargetDir "x86_64-pc-windows-msvc\release\rmux.exe"
        Run "powershell" @(
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            "scripts\windows_ctrl_matrix.ps1",
            "-Rmux",
            $rmux,
            "-PortableSmokeOnly"
        )
    }
}

Write-Host ""
Write-Host "release-review-gate-windows=ok"
