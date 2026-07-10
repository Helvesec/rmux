param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Archive,
    [string]$Checksums = "",
    [switch]$RunBinary,
    [switch]$RunDaemonSmoke,
    [switch]$RunSdkSmoke,
    [switch]$RunMouseBorderSmoke,
    [switch]$RunCtrlMatrixSmoke,
    [switch]$RequireReleaseArtifact,
    [string]$ExpectedGitSha = $env:RMUX_EXPECTED_GIT_SHA,
    [string]$CtrlMatrixEvidence = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$assertCargoFilter = Join-Path $PSScriptRoot "assert-cargo-filter-nonempty.ps1"

function Fail([string]$Message) {
    Write-Error "error: $Message"
    exit 1
}

function Invoke-NativeCapture([string]$Program, [string[]]$Arguments) {
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        # Native stderr redirection is surfaced as NativeCommandError under
        # pwsh when ErrorActionPreference is Stop. Capture it as data instead.
        $ErrorActionPreference = "Continue"
        $output = & $Program @Arguments 2>&1
        $status = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }

    [pscustomobject]@{
        Output = $output
        Status = $status
    }
}

function Assert-CargoFilter([int]$MinTests, [string[]]$CargoArguments) {
    $arguments = @([string]$MinTests, "--") + $CargoArguments
    & $assertCargoFilter @arguments
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo filter check failed for cargo $($CargoArguments -join ' ')"
    }
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
    $result = Invoke-NativeCapture $Binary $Arguments
    if ($result.Status -ne 0) {
        Fail "command failed: $Binary $($Arguments -join ' ')`n$($result.Output)"
    }
    $result.Output
}

function AssertSuccessNoCapture([string]$Binary, [string[]]$Arguments) {
    & $Binary @Arguments
    if ($LASTEXITCODE -ne 0) {
        Fail "command failed: $Binary $($Arguments -join ' ')"
    }
}

function AssertHelperFallback([string]$Binary) {
    $result = Invoke-NativeCapture $Binary @("--help")
    $output = $result.Output
    $status = $result.Status
    if ($status -ne 0 -and $status -ne 1) {
        Fail "command failed with unexpected exit code $($status): $Binary --help`n$output"
    }
    if (($output -join "`n") -notmatch 'usage: rmux') {
        Fail "command did not reach private helper: $Binary --help`n$output"
    }
}

function AssertDaemonBinary([string]$Binary) {
    $result = Invoke-NativeCapture $Binary @("--help")
    $output = $result.Output
    if ($result.Status -ne 1) {
        Fail "daemon helper returned unexpected exit code $($result.Status): $Binary --help`n$output"
    }
    if (($output -join "`n") -notmatch 'rmux-daemon is internal') {
        Fail "daemon helper did not report the internal-launch contract: $Binary --help`n$output"
    }
}

function NewPortableAliasSmoke([string]$Binary, [string]$Root) {
    $links = Join-Path $Root "winget-links"
    New-Item -ItemType Directory -Force -Path $links | Out-Null
    $alias = Join-Path $links ([System.IO.Path]::GetFileName($Binary))
    try {
        New-Item -ItemType SymbolicLink -Path $alias -Target $Binary -ErrorAction Stop | Out-Null
    } catch {
        Fail "portable alias smoke requires a symlink alias and could not create one. Enable Windows Developer Mode or rerun from an elevated PowerShell before using -RunBinary/-RunDaemonSmoke. Original error: $($_.Exception.Message)"
    }

    [pscustomobject]@{
        Available = $true
        Binary = $alias
        Directory = Split-Path -Parent $alias
        Reason = ""
    }
}

function InvokeWithPathPrefix([string]$Directory, [scriptblock]$Body) {
    $previousPath = $env:Path
    try {
        $env:Path = "$Directory$([System.IO.Path]::PathSeparator)$previousPath"
        & $Body
    } finally {
        $env:Path = $previousPath
    }
}

function InvokeWithPackageOnlyPath([string]$PackageRoot, [scriptblock]$Body) {
    $previousPath = $env:Path
    $systemRoot = if ($env:SystemRoot) { $env:SystemRoot } else { "C:\Windows" }
    $system32 = Join-Path $systemRoot "System32"
    $pathEntries = @(
        $PackageRoot,
        (Join-Path $PackageRoot "libexec\rmux"),
        $system32,
        $systemRoot,
        (Join-Path $system32 "WindowsPowerShell\v1.0")
    )
    try {
        $env:Path = ($pathEntries -join [System.IO.Path]::PathSeparator)
        & $Body
    } finally {
        $env:Path = $previousPath
    }
}

function Get-FreeTcpPort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    try {
        $listener.Start()
        return $listener.LocalEndpoint.Port
    } finally {
        $listener.Stop()
    }
}

function AssertOutputContains([object[]]$Output, [string]$Needle, [string]$Context) {
    $text = $Output -join "`n"
    if ($text -notmatch [regex]::Escape($Needle)) {
        Fail "$Context did not contain '$Needle'`n$text"
    }
}

function InvokeSdkWindowsSmoke([string]$Binary) {
    $previousBinary = $env:RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN
    try {
        $env:RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN = [System.IO.Path]::GetFullPath($Binary)
        Assert-CargoFilter 1 @(
            "test",
            "--locked",
            "-p",
            "rmux-sdk",
            "--test",
            "smoke_v1_windows",
            "daemon_backed_sdk_windows_happy_path_uses_named_pipe_and_cleans_daemon"
        )
        & cargo @(
            "test",
            "--locked",
            "-p",
            "rmux-sdk",
            "--test",
            "smoke_v1_windows",
            "daemon_backed_sdk_windows_happy_path_uses_named_pipe_and_cleans_daemon"
        )
        if ($LASTEXITCODE -ne 0) {
            Fail "Windows SDK package smoke failed with exit code $LASTEXITCODE"
        }
    } finally {
        if ($null -eq $previousBinary) {
            Remove-Item Env:\RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN -ErrorAction SilentlyContinue
        } else {
            $env:RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN = $previousBinary
        }
    }
}

function InvokeMouseBorderSmoke([string]$Binary) {
    $previousBinary = $env:RMUX_MOUSE_BORDER_RMUX_BIN
    try {
        $env:RMUX_MOUSE_BORDER_RMUX_BIN = [System.IO.Path]::GetFullPath($Binary)
        & cargo @(
            "test",
            "--locked",
            "-p",
            "rmux",
            "--test",
            "windows_mouse_border_resize"
        )
        if ($LASTEXITCODE -ne 0) {
            Fail "Windows mouse border package smoke failed with exit code $LASTEXITCODE"
        }
    } finally {
        if ($null -eq $previousBinary) {
            Remove-Item Env:\RMUX_MOUSE_BORDER_RMUX_BIN -ErrorAction SilentlyContinue
        } else {
            $env:RMUX_MOUSE_BORDER_RMUX_BIN = $previousBinary
        }
    }
}

function InvokeCtrlMatrixSmoke([string]$Binary, [string]$GitSha, [string]$Evidence) {
    if ($GitSha -notmatch '^[0-9a-fA-F]{40}$') {
        Fail "Windows Ctrl matrix package smoke requires a full expected Git SHA"
    }
    $outDir = Join-Path ([System.IO.Path]::GetTempPath()) "rmux-package-ctrl-matrix-$PID-$([guid]::NewGuid().ToString('N'))"
    try {
        $global:LASTEXITCODE = 0
        $arguments = @(
            "-Rmux", [System.IO.Path]::GetFullPath($Binary),
            "-OutDir", $outDir,
            "-PortableSmokeOnly",
            "-ExpectedGitSha", $GitSha
        )
        if (-not [string]::IsNullOrWhiteSpace($Evidence)) {
            $arguments += @("-EvidencePath", [System.IO.Path]::GetFullPath($Evidence))
        }
        & (Join-Path $PSScriptRoot "windows_ctrl_matrix.ps1") @arguments
        if ($LASTEXITCODE -ne 0) {
            Fail "Windows Ctrl matrix package smoke failed with exit code $LASTEXITCODE"
        }
        $resultEvidence = Join-Path $outDir "portable-smoke.evidence.json"
        if (-not (Test-Path -LiteralPath $resultEvidence -PathType Leaf)) {
            Fail "Windows Ctrl matrix package smoke produced no passing evidence"
        }
        $payload = Get-Content -LiteralPath $resultEvidence -Raw | ConvertFrom-Json
        if ($payload.status -ne "passed" -or $payload.git_commit -ne $GitSha.ToLowerInvariant()) {
            Fail "Windows Ctrl matrix package evidence is not a passing result for $GitSha"
        }
        return "passed"
    } finally {
        Remove-Item -LiteralPath $outDir -Recurse -Force -ErrorAction SilentlyContinue
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

function AssertPackageHygiene([string]$Root) {
    [char[]]$separators = @(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    )
    $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd($separators)
    foreach ($entry in Get-ChildItem -LiteralPath $rootFull -Force -Recurse) {
        $relative = $entry.FullName.Substring($rootFull.Length).TrimStart($separators)
        $portable = $relative -replace '\\', '/'
        $segments = $portable -split '/'
        if ($segments -contains ".claude" -or $segments -contains ".codex") {
            Fail "forbidden package entry: $portable"
        }
        if ($entry.Name -like "*.sock" -or
            $entry.Name -like "*.tmp" -or
            $entry.Name -like "*.bak" -or
            $entry.Name -like "*.orig" -or
            $entry.Name -like "*~") {
            Fail "forbidden package entry: $portable"
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
    AssertPackageHygiene $packageRoot

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
        if ($metadata.configuration -ne "release") {
            Fail "release artifact metadata configuration is not release"
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

    $portableAlias = $null
    if ($RunBinary -or $RunDaemonSmoke) {
        $portableAlias = NewPortableAliasSmoke $binary $tmpRoot
        if (-not $portableAlias.Available) {
            Fail "portable alias smoke unexpectedly returned unavailable: $($portableAlias.Reason)"
        }
    }

    if ($RunBinary) {
        AssertSuccess $binary @("-V") | Out-Null
        AssertHelperFallback $binary
        AssertSuccess $helperBinary @("-V") | Out-Null
        AssertDaemonBinary $daemonBinary
        AssertSuccess $binary @("diagnose", "--json") | Out-Null
        if ($portableAlias.Available) {
            AssertSuccess $portableAlias.Binary @("-V") | Out-Null
            AssertHelperFallback $portableAlias.Binary
            AssertSuccess $portableAlias.Binary @("diagnose", "--json") | Out-Null
            InvokeWithPathPrefix $portableAlias.Directory {
                AssertHelperFallback "rmux"
                AssertSuccess "rmux" @("diagnose", "--json") | Out-Null
            }
        }
        InvokeWithPackageOnlyPath $packageRoot {
            AssertSuccess "rmux" @("-V") | Out-Null
            AssertHelperFallback "rmux"
            AssertSuccess "rmux" @("diagnose", "--json") | Out-Null
        }
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
            $webPort = Get-FreeTcpPort
            AssertSuccessNoCapture $binary @("-L", $label, "start-server", "--web-port", "$webPort")
            AssertSuccessNoCapture $binary @("-L", $label, "new-session", "-d", "-s", "package_smoke", "cmd.exe", "/d", "/q", "/k")
            $sessions = AssertSuccess $binary @("-L", $label, "list-sessions", "-F", "#{session_name}")
            if (($sessions -join "`n") -notmatch 'package_smoke') {
                Fail "daemon smoke did not list package_smoke session"
            }
            $sourceFile = Join-Path $tmpRoot "package-source.conf"
            Set-Content -LiteralPath $sourceFile -Encoding ASCII -Value "set -g status off"
            AssertSuccessNoCapture $binary @("-L", $label, "source-file", $sourceFile)
            $status = AssertSuccess $binary @("-L", $label, "show-options", "-gv", "status")
            AssertOutputContains $status "off" "package source-file smoke"
            $webShare = AssertSuccess $binary @("-L", $label, "web-share", "-t", "package_smoke", "--no-pin", "--ttl", "30")
            AssertOutputContains $webShare "http" "package web-share smoke"
            $webList = AssertSuccess $binary @("-L", $label, "web-share", "list")
            AssertOutputContains $webList "package_smoke" "package web-share list smoke"
            AssertSuccessNoCapture $binary @("-L", $label, "web-share", "off")
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

        if ($portableAlias.Available) {
            $portableAliasLabel = "package-alias-smoke-$PID-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
            try {
                InvokeWithPathPrefix $portableAlias.Directory {
                    AssertSuccessNoCapture "rmux" @("-L", $portableAliasLabel, "new-session", "-d", "-s", "package_alias_smoke", "cmd.exe", "/d", "/q", "/k")
                    $sessions = AssertSuccess "rmux" @("-L", $portableAliasLabel, "list-sessions", "-F", "#{session_name}")
                    if (($sessions -join "`n") -notmatch 'package_alias_smoke') {
                        Fail "portable alias daemon smoke did not list package_alias_smoke session"
                    }
                }
            } finally {
                InvokeWithPathPrefix $portableAlias.Directory {
                    & "rmux" "-L" $portableAliasLabel "kill-server" | Out-Null
                }
            }
        }
    }

    if ($RunSdkSmoke) {
        InvokeSdkWindowsSmoke $binary
    }

    if ($RunMouseBorderSmoke) {
        InvokeMouseBorderSmoke $binary
    }

    $ctrlMatrixStatus = "not-requested"
    if ($RunCtrlMatrixSmoke) {
        $ctrlMatrixStatus = InvokeCtrlMatrixSmoke $binary $ExpectedGitSha $CtrlMatrixEvidence
    }

    Write-Output "archive=$archiveFull"
    Write-Output "sha256=$actualHash"
    Write-Output "binary_sha256=$packagedBinaryHash"
    Write-Output "helper_binary_sha256=$packagedHelperHash"
    Write-Output "daemon_binary_sha256=$packagedDaemonHash"
    Write-Output "run_binary=$($RunBinary.ToString().ToLowerInvariant())"
    Write-Output "run_daemon_smoke=$($RunDaemonSmoke.ToString().ToLowerInvariant())"
    Write-Output "run_sdk_smoke=$($RunSdkSmoke.ToString().ToLowerInvariant())"
    Write-Output "run_mouse_border_smoke=$($RunMouseBorderSmoke.ToString().ToLowerInvariant())"
    Write-Output "run_ctrl_matrix_smoke=$($RunCtrlMatrixSmoke.ToString().ToLowerInvariant())"
    Write-Output "ctrl_matrix_status=$ctrlMatrixStatus"
    Write-Output "require_release_artifact=$($RequireReleaseArtifact.ToString().ToLowerInvariant())"
} finally {
    Remove-Item -LiteralPath $tmpRoot -Recurse -Force -ErrorAction SilentlyContinue
}
