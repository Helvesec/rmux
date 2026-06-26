param(
    [string]$OutputDir = "target\release-review-gate-windows",
    [string]$TargetDir = "target\release-review-gate-windows-cargo",
    [switch]$SkipPackage,
    [switch]$SkipClippy
)

$ErrorActionPreference = "Stop"

$env:CARGO_TARGET_DIR = $TargetDir

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

Step "release versions" { Check-ReleaseVersions }
Write-Host "cargo-target-dir=$env:CARGO_TARGET_DIR"
Step "formatting" { Run "cargo" @("fmt", "--all", "--check") }
Step "platform cfg budget" { Check-CfgBudgets }

if (-not $SkipClippy) {
    Step "workspace clippy" {
        Run "cargo" @("clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings")
    }
}

Step "tiny parser and boundary tests" {
    Run "cargo" @("test", "-p", "rmux", "--features", "tiny-cli", "tiny_main", "--locked")
}
Step "mutating target-action retry tests" {
    Run "cargo" @("test", "-p", "rmux", "--bin", "rmux", "--locked", "target_action_retry_is_limited")
}
Step "Windows attach exit probes" {
    Run "cargo" @("test", "--locked", "-p", "rmux", "--test", "windows_attach_exit")
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
            $archive
        )
    }
}

Write-Host ""
Write-Host "release-review-gate-windows=ok"
