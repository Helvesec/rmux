<#
.SYNOPSIS
Run review checks, or the complete native qualification recipe with -QualificationDir.
.PARAMETER QualificationDir
New persistent evidence directory. Requires measured RMUX_FROZEN_TMUX (tmux 3.7b).
No skip flags are accepted. A successful native cell leaves other OS cells pending.
#>
param(
    [string]$QualificationDir,
    [string]$OutputDir = "target\release-review-gate-windows",
    [string]$TargetDir = "target\release-review-gate-windows-cargo",
    [switch]$SkipPackage,
    [switch]$SkipClippy,
    [switch]$RunCtrlMatrixSmoke,
    [switch]$EndpointSmokeOnly
)

$ErrorActionPreference = "Stop"

if ($QualificationDir) {
    if ($SkipPackage -or $SkipClippy -or $RunCtrlMatrixSmoke -or $EndpointSmokeOnly -or
        $PSBoundParameters.ContainsKey("TargetDir") -or $PSBoundParameters.ContainsKey("OutputDir")) {
        throw "qualification requires the complete recipe; no skip or smoke-only flags"
    }
    $python = Get-Command python -ErrorAction SilentlyContinue
    if (-not $python) { $python = Get-Command python3 -ErrorAction Stop }
    & $python.Source (Join-Path $PSScriptRoot "validation\contracts.py") `
        --root (Split-Path $PSScriptRoot -Parent) --run-qualification $QualificationDir
    exit $LASTEXITCODE
}

$env:RUST_MIN_STACK = "8388608"
$env:RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB = "1"
$env:CARGO_TARGET_DIR = $TargetDir
if (-not $EndpointSmokeOnly) {
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

function Run-InCandidateIdentity([string[]]$Arguments) {
    # candidate_identity builds into its own private target and refuses the gate's shared one.
    $sharedTargetDir = $env:CARGO_TARGET_DIR
    Remove-Item Env:CARGO_TARGET_DIR
    try {
        Run-PythonScript "scripts\candidate_identity.py" (@("with", "--") + $Arguments)
    } finally {
        $env:CARGO_TARGET_DIR = $sharedTargetDir
    }
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
    if ($rootText -notmatch '(?ms)^\s*\[package\].*?^\s*publish\s*=\s*true') {
        throw "root rmux package must keep publish=true"
    }
    Write-Host "root-publish=true"
    $manpage = Get-Content -LiteralPath "docs\man\rmux.1" -Raw
    if ($manpage -notmatch [regex]::Escape("RMUX $rootVersion")) {
        throw "docs\man\rmux.1 does not contain RMUX $rootVersion"
    }

    $metadataJson = @(& cargo metadata --locked --no-deps --format-version 1)
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed with exit code $LASTEXITCODE"
    }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json -ErrorAction Stop
    if ($null -eq $metadata -or $metadata -is [array] -or
        $metadata.workspace_members -isnot [array] -or $metadata.workspace_members.Count -eq 0 -or
        $metadata.packages -isnot [array] -or $metadata.packages.Count -eq 0) {
        throw "cargo metadata has no valid workspace membership"
    }
    # IDs are opaque and case-sensitive; names and directory layout are not membership.
    $memberIds = @()
    $manifests = foreach ($memberId in $metadata.workspace_members) {
        if ($memberId -isnot [string] -or [string]::IsNullOrWhiteSpace($memberId) -or
            $memberIds -ccontains $memberId) {
            throw "cargo metadata has an invalid or duplicate workspace member ID"
        }
        $memberIds += $memberId
        $packages = @($metadata.packages | Where-Object { $_.id -is [string] -and $_.id -ceq $memberId })
        # .NET Framework's IsPathRooted also accepts C:foo and \foo. Require a
        # drive plus separator, UNC/device prefix, or the NT device prefix \??\.
        if ($packages.Count -ne 1 -or $packages[0].manifest_path -isnot [string] -or
            [string]::IsNullOrWhiteSpace($packages[0].manifest_path) -or
            -not [System.IO.Path]::IsPathRooted($packages[0].manifest_path) -or
            $packages[0].manifest_path -notmatch '^(?:[A-Za-z]:[\\/]|[\\/]{2}|\\\?\?\\)') {
            throw "cargo metadata has no unique manifest path for workspace member $memberId"
        }
        $packages[0].manifest_path
    }
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
    Write-Host "workspace-members=$($memberIds.Count)"
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
        @("rmux-server", "crates\rmux-server\src", 9),
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
    $macUser = "pingu" + "delfuego"
    $linuxUser = "pi" + "ngu"
    $personalPathPattern = "/Users/$macUser/|/home/$linuxUser/|[A-Za-z]:[\\/]Users[\\/]($linuxUser|$macUser)[\\/]"
    $trackedPersonalPaths = @(& git grep -I -n -E $personalPathPattern -- .)
    $grepExit = $LASTEXITCODE
    if ($grepExit -gt 1) {
        throw "git grep for tracked personal paths failed with exit code $grepExit"
    }
    if ($trackedPersonalPaths.Count -gt 0) {
        $trackedPersonalPaths | ForEach-Object { Write-Error $_ }
        throw "tracked personal filesystem paths are forbidden"
    }
    $untrackedSockets = @(Git-LsFiles @("--others", "--exclude-standard") | Where-Object { $_ -match '\.(sock|socket)$' })
    if ($untrackedSockets.Count -gt 0) {
        $untrackedSockets | ForEach-Object { Write-Error $_ }
        throw "untracked socket-like files are forbidden in the worktree"
    }
    Write-Host "worktree-hygiene=ok"
}

function Invoke-WindowsEndpointSmoke {
    Assert-CargoFilter 1 @("test", "-p", "rmux-ipc", "--locked", "--test", "named_pipe_integration")
    Run "cargo" @("test", "-p", "rmux-ipc", "--locked", "--test", "named_pipe_integration", "--", "--test-threads=1")
    Assert-CargoFilter 1 @("test", "-p", "rmux-client", "--locked", "--test", "windows_legacy_shutdown")
    Run "cargo" @("test", "-p", "rmux-client", "--locked", "--test", "windows_legacy_shutdown", "--", "--test-threads=1")
}

if ($EndpointSmokeOnly) {
    Step "Windows endpoint discovery and legacy shutdown" {
        Invoke-WindowsEndpointSmoke
    }
    Write-Host ""
    Write-Host "release-review-gate-windows=ok"
    return
}

Step "release TOML reader self-test" { Run-PythonScript "scripts\toml_reader.py" @("--self-test") }
Step "publication identity regression" { Run-PythonScript "-B" @("-m", "unittest", "discover", "-s", "scripts/release", "-p", "test_publication*.py") }
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
Write-Host "rust-min-stack=$env:RUST_MIN_STACK"
Write-Host "rustflags=$env:RUSTFLAGS"
Step "formatting" { Run "cargo" @("fmt", "--all", "--check") }
Step "feature compilation gate regression" { Run-PythonScript "scripts/test_feature_powerset.py" }
Step "workspace and isolated feature powerset" { Run-PythonScript "scripts/check_feature_powerset.py" }
Step "platform cfg budget" { Check-CfgBudgets }
Step "worktree hygiene" { Check-WorktreeHygiene }

if (-not $SkipClippy) {
    Step "workspace clippy" {
        Run "cargo" @("clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings")
    }
}

Step "tiny parser and boundary tests" {
    # tiny_main tests live in the rmux binaries; integration targets select none of them.
    Assert-CargoFilter 1 @("test", "-p", "rmux", "--features", "tiny-cli", "--bins", "tiny_main", "--locked")
    Run "cargo" @("test", "-p", "rmux", "--features", "tiny-cli", "--bins", "tiny_main", "--locked")
}
Step "mutating target-action retry tests" {
    Assert-CargoFilter 1 @("test", "-p", "rmux", "--bin", "rmux", "--locked", "target_action_retry_is_limited")
    Run "cargo" @("test", "-p", "rmux", "--bin", "rmux", "--locked", "target_action_retry_is_limited")
}
Step "server lib tests" {
    # The Windows final-sink child is certified by candidate_identity (RMUX_IDENTITY_PYTHON).
    Run-InCandidateIdentity @("cargo", "test", "-p", "rmux-server", "--lib", "--locked", "--", "--test-threads=1")
}
Step "independent xterm.js recovery oracle" {
    $npm = Get-Command npm.cmd -ErrorAction SilentlyContinue
    if (-not $npm) {
        throw "npm.cmd is required for the pinned independent xterm.js recovery oracle"
    }
    Assert-CargoFilter 1 @(
        "test", "-p", "rmux-server", "--lib", "--locked",
        "pane_recovery::tests::keyframes_converge_in_independent_xterm_oracle"
    )
    Run $npm.Source @("--prefix", "tests\xterm-oracle", "ci", "--ignore-scripts", "--no-audit", "--no-fund")
    Run "cargo" @(
        "test", "-p", "rmux-server", "--lib", "--locked",
        "pane_recovery::tests::keyframes_converge_in_independent_xterm_oracle",
        "--", "--ignored", "--exact", "--test-threads=1"
    )
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
Step "Windows endpoint discovery and legacy shutdown" {
    Invoke-WindowsEndpointSmoke
}
Step "Windows CLI queue formats" {
    Assert-CargoFilter 1 @("test", "-p", "rmux", "--locked", "--test", "windows_cli_queue_formats")
    Run "cargo" @("test", "--locked", "-p", "rmux", "--test", "windows_cli_queue_formats", "--", "--test-threads=1")
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
        $expectedGitSha = (& git rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0 -or $expectedGitSha -notmatch '^[0-9a-fA-F]{40}$') {
            throw "unable to resolve the expected Git SHA for Windows Ctrl matrix evidence"
        }
        Run "powershell" @(
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            "scripts\windows_ctrl_matrix.ps1",
            "-Rmux",
            $rmux,
            "-PortableSmokeOnly",
            "-ExpectedGitSha",
            $expectedGitSha
        )
    }
}

Write-Host ""
Write-Host "release-review-gate-windows=ok"
