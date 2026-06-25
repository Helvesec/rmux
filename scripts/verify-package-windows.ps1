param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Archive,
    [string]$Checksums = "",
    [switch]$RunBinary,
    [switch]$RunDaemonSmoke,
    [switch]$RequireReleaseArtifact
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Error "error: $Message"
    exit 1
}

function Sha256File([string]$Path) {
    $getFileHash = Get-Command Get-FileHash -ErrorAction SilentlyContinue
    if ($getFileHash) {
        return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
    }

    $stream = [System.IO.File]::OpenRead([System.IO.Path]::GetFullPath($Path))
    try {
        $sha256 = [System.Security.Cryptography.SHA256]::Create()
        try {
            $hashBytes = $sha256.ComputeHash($stream)
            return ([System.BitConverter]::ToString($hashBytes) -replace "-", "").ToLowerInvariant()
        } finally {
            $sha256.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

function AssertSuccess([string]$Binary, [string[]]$Arguments) {
    $output = & $Binary @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        Fail "command failed: $Binary $($Arguments -join ' ')`n$output"
    }
    $output
}

function AssertSuccessNoCapture([string]$Binary, [string[]]$Arguments) {
    & $Binary @Arguments
    if ($LASTEXITCODE -ne 0) {
        Fail "command failed: $Binary $($Arguments -join ' ')"
    }
}

function VerifyChecksumManifest([string]$Root, [string]$Manifest) {
    $rootFull = [System.IO.Path]::GetFullPath($Root)
    foreach ($line in Get-Content -LiteralPath $Manifest) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        if ($line -notmatch '^([0-9a-fA-F]{64})  (.+)$') {
            Fail "invalid checksum line: $line"
        }
        $expected = $Matches[1].ToLowerInvariant()
        $relative = $Matches[2]
        if ($relative.StartsWith("/") -or $relative.StartsWith("../") -or $relative.Contains("/../") -or $relative.Contains("\") -or $relative -match '^[A-Za-z]:') {
            Fail "non-portable checksum path: $relative"
        }
        $parts = $relative -split '/'
        $path = Join-Path $rootFull ($parts -join [System.IO.Path]::DirectorySeparatorChar)
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            Fail "checksum target is missing: $relative"
        }
        $actual = Sha256File $path
        if ($actual -ne $expected) {
            Fail "checksum mismatch for $relative"
        }
    }
}

$archiveFull = [System.IO.Path]::GetFullPath($Archive)
if (-not (Test-Path -LiteralPath $archiveFull -PathType Leaf)) {
    Fail "archive not found: $Archive"
}
if (-not $archiveFull.EndsWith(".zip", [System.StringComparison]::OrdinalIgnoreCase)) {
    Fail "unsupported archive extension, expected .zip: $Archive"
}

$archiveDir = Split-Path -Parent $archiveFull
$archiveName = [System.IO.Path]::GetFileName($archiveFull)
if ([string]::IsNullOrWhiteSpace($Checksums)) {
    $Checksums = Join-Path $archiveDir "SHA256SUMS.txt"
}
if (-not (Test-Path -LiteralPath $Checksums -PathType Leaf)) {
    Fail "checksum manifest not found: $Checksums"
}

$expectedHash = ""
foreach ($line in Get-Content -LiteralPath $Checksums) {
    if ($line -match "^([0-9a-fA-F]{64})  $([regex]::Escape($archiveName))$") {
        $expectedHash = $Matches[1].ToLowerInvariant()
        break
    }
}
if ([string]::IsNullOrWhiteSpace($expectedHash)) {
    Fail "archive is missing from checksum manifest: $archiveName"
}

$actualHash = Sha256File $archiveFull
if ($actualHash -ne $expectedHash) {
    Fail "checksum mismatch for $archiveName"
}

$tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) "rmux-package-verify-$PID-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $tmpRoot | Out-Null
try {
    Expand-Archive -LiteralPath $archiveFull -DestinationPath $tmpRoot -Force
    $packageRoot = Join-Path $tmpRoot ([System.IO.Path]::GetFileNameWithoutExtension($archiveName))
    if (-not (Test-Path -LiteralPath $packageRoot -PathType Container)) {
        Fail "archive root directory is missing: $([System.IO.Path]::GetFileNameWithoutExtension($archiveName))"
    }

    foreach ($required in @("rmux.exe", "libexec/rmux/rmux.exe", "rmux-daemon.exe", "SHA256SUMS.txt", "share/rmux/artifact-metadata.json", "README.md", "LICENSE-APACHE", "LICENSE-MIT", "rmux.1")) {
        if (-not (Test-Path -LiteralPath (Join-Path $packageRoot $required))) {
            Fail "missing package file: $required"
        }
    }

    VerifyChecksumManifest $packageRoot (Join-Path $packageRoot "SHA256SUMS.txt")

    $binary = Join-Path $packageRoot "rmux.exe"
    $metadataPath = Join-Path $packageRoot "share/rmux/artifact-metadata.json"
    $metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
    if ($metadata.artifact_kind -ne "windows-package-binary") {
        Fail "metadata artifact_kind is not windows-package-binary"
    }
    if ($metadata.package_layout -ne "rmux-windows-package-v2") {
        Fail "metadata package_layout is not rmux-windows-package-v2"
    }
    if ($RequireReleaseArtifact) {
        if (-not ($metadata.PSObject.Properties.Name -contains "release_artifact") -or
            $metadata.release_artifact -ne $true) {
            Fail "metadata release_artifact is not true"
        }
    }
    $packagedBinaryHash = Sha256File $binary
    if ($metadata.binary_sha256.ToLowerInvariant() -ne $packagedBinaryHash) {
        Fail "metadata binary_sha256 does not match packaged binary"
    }
    $helperBinary = Join-Path $packageRoot "libexec/rmux/rmux.exe"
    $packagedHelperHash = Sha256File $helperBinary
    if ($metadata.helper_binary_sha256.ToLowerInvariant() -ne $packagedHelperHash) {
        Fail "metadata helper_binary_sha256 does not match packaged helper binary"
    }
    $daemonBinary = Join-Path $packageRoot "rmux-daemon.exe"
    $packagedDaemonHash = Sha256File $daemonBinary
    if ($metadata.daemon_binary_sha256.ToLowerInvariant() -ne $packagedDaemonHash) {
        Fail "metadata daemon_binary_sha256 does not match packaged daemon binary"
    }

    if ($RunBinary) {
        AssertSuccess $binary @("-V") | Out-Null
        AssertSuccess $binary @("diagnose", "--json") | Out-Null
        $previousDisableTiny = $env:RMUX_DISABLE_TINY_CLI
        try {
            $env:RMUX_DISABLE_TINY_CLI = "1"
            AssertSuccess $binary @("-V") | Out-Null
            AssertSuccess $binary @("diagnose", "--json") | Out-Null
        } finally {
            if ($null -eq $previousDisableTiny) {
                Remove-Item Env:\RMUX_DISABLE_TINY_CLI -ErrorAction SilentlyContinue
            } else {
                $env:RMUX_DISABLE_TINY_CLI = $previousDisableTiny
            }
        }
    }

    if ($RunDaemonSmoke) {
        $label = "package-smoke-$PID-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
        try {
            AssertSuccessNoCapture $binary @("-L", $label, "new-session", "-d", "-s", "package_smoke", "cmd.exe", "/d", "/q", "/k")
            $sessions = AssertSuccess $binary @("-L", $label, "list-sessions", "-F", "#{session_name}")
            if (($sessions -join "`n") -notmatch 'package_smoke') {
                Fail "daemon smoke did not list package_smoke session"
            }
        } finally {
            & $binary "-L" $label "kill-server" | Out-Null
        }

        $fallbackLabel = "package-fallback-smoke-$PID-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
        $previousDisableTiny = $env:RMUX_DISABLE_TINY_CLI
        try {
            $env:RMUX_DISABLE_TINY_CLI = "1"
            AssertSuccessNoCapture $binary @("-L", $fallbackLabel, "new-session", "-d", "-s", "package_fallback_smoke", "cmd.exe", "/d", "/q", "/k")
            $sessions = AssertSuccess $binary @("-L", $fallbackLabel, "list-sessions", "-F", "#{session_name}")
            if (($sessions -join "`n") -notmatch 'package_fallback_smoke') {
                Fail "fallback daemon smoke did not list package_fallback_smoke session"
            }
        } finally {
            if ($null -eq $previousDisableTiny) {
                Remove-Item Env:\RMUX_DISABLE_TINY_CLI -ErrorAction SilentlyContinue
            } else {
                $env:RMUX_DISABLE_TINY_CLI = $previousDisableTiny
            }
            & $binary "-L" $fallbackLabel "kill-server" | Out-Null
        }
    }

    Write-Output "archive=$archiveFull"
    Write-Output "sha256=$actualHash"
    Write-Output "binary_sha256=$packagedBinaryHash"
    Write-Output "helper_binary_sha256=$packagedHelperHash"
    Write-Output "daemon_binary_sha256=$packagedDaemonHash"
    Write-Output "run_binary=$($RunBinary.ToString().ToLowerInvariant())"
    Write-Output "run_daemon_smoke=$($RunDaemonSmoke.ToString().ToLowerInvariant())"
    Write-Output "require_release_artifact=$($RequireReleaseArtifact.ToString().ToLowerInvariant())"
} finally {
    Remove-Item -LiteralPath $tmpRoot -Recurse -Force -ErrorAction SilentlyContinue
}
