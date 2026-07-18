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
    [string]$CtrlMatrixEvidence = "",
    [string]$NextestArchive = ""
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

function NewPackagePipeName([string]$Binary, [string]$Label) {
    $result = Invoke-NativeCapture $Binary @("-L", $Label, "diagnose", "--json")
    if ($result.Status -ne 0) {
        Fail "failed to resolve package pipe for label '$Label': $Binary diagnose --json; $($result.Output)"
    }

    try {
        $diagnostics = ($result.Output -join [Environment]::NewLine) | ConvertFrom-Json
    } catch {
        Fail "package binary returned invalid diagnose JSON for label '$Label': $($result.Output)"
    }

    $pipePath = [string]$diagnostics.socket_path
    if ([string]::IsNullOrWhiteSpace($pipePath) -or
        $pipePath -notmatch '^\\\\\.\\pipe\\rmux-.+-il-(untrusted|low|medium|high|system)-.+$') {
        Fail "package binary returned a non-canonical Windows pipe for label '$Label': $pipePath"
    }
    return $pipePath
}

function Remove-PackageDaemon([object]$Daemon) {
    if ($null -eq $Daemon) {
        return
    }
    try {
        $Daemon.Process.Refresh()
        if (-not $Daemon.Process.HasExited) {
            $Daemon.Process.Kill()
            $Daemon.Process.WaitForExit()
        }
    } finally {
        $Daemon.Process.Dispose()
        Remove-Item -Force -LiteralPath $Daemon.Stdout -ErrorAction SilentlyContinue
        Remove-Item -Force -LiteralPath $Daemon.Stderr -ErrorAction SilentlyContinue
    }
}

function Start-PackageDaemon([string]$DaemonBinary, [string]$PipePath, [string[]]$ExtraArguments = @()) {
    if ($PipePath -notmatch '^\\\\\.\\pipe\\rmux-') {
        Fail "package daemon requires an explicit RMUX named pipe: $PipePath"
    }

    $id = "$PID-$([guid]::NewGuid().ToString('N'))"
    $readyEventName = "Local\rmux-package-ready-$id"
    $stdout = Join-Path ([System.IO.Path]::GetTempPath()) "rmux-package-daemon-$id.stdout"
    $stderr = Join-Path ([System.IO.Path]::GetTempPath()) "rmux-package-daemon-$id.stderr"
    $readyEvent = [System.Threading.EventWaitHandle]::new(
        $false,
        [System.Threading.EventResetMode]::ManualReset,
        $readyEventName
    )
    $process = $null
    try {
        $arguments = @(
            "--__internal-daemon",
            $PipePath,
            "--startup-ready-event",
            $readyEventName
        ) + $ExtraArguments
        $process = Start-Process `
            -FilePath $DaemonBinary `
            -ArgumentList $arguments `
            -RedirectStandardOutput $stdout `
            -RedirectStandardError $stderr `
            -PassThru

        if (-not $readyEvent.WaitOne(15000)) {
            $process.Refresh()
            if (-not $process.HasExited) {
                $process.Kill()
                $process.WaitForExit()
            }
            $details = if (Test-Path -LiteralPath $stderr) {
                Get-Content -LiteralPath $stderr -Raw
            } else {
                ""
            }
            Fail "package daemon did not become ready on $PipePath`n$details"
        }
        $process.Refresh()
        if ($process.HasExited) {
            $details = if (Test-Path -LiteralPath $stderr) {
                Get-Content -LiteralPath $stderr -Raw
            } else {
                ""
            }
            Fail "package daemon exited during startup with $($process.ExitCode)`n$details"
        }

        return [pscustomobject]@{
            Process = $process
            Pipe = $PipePath
            Stdout = $stdout
            Stderr = $stderr
        }
    } catch {
        if ($null -ne $process) {
            Remove-PackageDaemon ([pscustomobject]@{
                Process = $process
                Pipe = $PipePath
                Stdout = $stdout
                Stderr = $stderr
            })
        } else {
            Remove-Item -Force -LiteralPath $stdout -ErrorAction SilentlyContinue
            Remove-Item -Force -LiteralPath $stderr -ErrorAction SilentlyContinue
        }
        throw
    } finally {
        $readyEvent.Dispose()
    }
}

function Stop-PackageDaemon([string]$ClientBinary, [object]$Daemon) {
    if ($null -eq $Daemon) {
        return
    }

    $failure = $null
    try {
        $Daemon.Process.Refresh()
        if (-not $Daemon.Process.HasExited) {
            $shutdown = Invoke-NativeCapture $ClientBinary @("-S", $Daemon.Pipe, "kill-server")
            if ($shutdown.Status -ne 0) {
                if (-not $Daemon.Process.WaitForExit(2000)) {
                    $failure = "package daemon shutdown failed: $($shutdown.Output -join "`n")"
                }
            } elseif (-not $Daemon.Process.WaitForExit(10000)) {
                $failure = "package daemon did not exit after kill-server"
            }
        }
        $Daemon.Process.Refresh()
        if ($null -eq $failure -and $Daemon.Process.HasExited -and $Daemon.Process.ExitCode -ne 0) {
            $details = if (Test-Path -LiteralPath $Daemon.Stderr) {
                Get-Content -LiteralPath $Daemon.Stderr -Raw
            } else {
                ""
            }
            $failure = "package daemon exited with $($Daemon.Process.ExitCode)`n$details"
        }
    } finally {
        Remove-PackageDaemon $Daemon
    }

    if ($null -ne $failure) {
        Fail $failure
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

function InvokeSdkWindowsSmoke(
    [string]$Binary,
    [string]$DaemonBinary,
    [string]$NextestArchive
) {
    $previousBinary = $env:RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN
    $previousPipe = $env:RMUX_SDK_WINDOWS_SMOKE_PIPE
    $pipePath = NewPackagePipeName $Binary "sdk-smoke-$PID-$([guid]::NewGuid().ToString('N'))"
    $daemon = Start-PackageDaemon $DaemonBinary $pipePath
    try {
        $env:RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN = [System.IO.Path]::GetFullPath($Binary)
        $env:RMUX_SDK_WINDOWS_SMOKE_PIPE = $pipePath
        $sdkTest = "daemon_backed_sdk_windows_happy_path_uses_named_pipe_and_cleans_daemon"
        if ([string]::IsNullOrWhiteSpace($NextestArchive)) {
            Assert-CargoFilter 1 @(
                "test",
                "--locked",
                "-p",
                "rmux-sdk",
                "--test",
                "smoke_v1_windows",
                $sdkTest
            )
            & cargo @(
                "test",
                "--locked",
                "-p",
                "rmux-sdk",
                "--test",
                "smoke_v1_windows",
                $sdkTest
            )
            if ($LASTEXITCODE -ne 0) {
                Fail "Windows SDK package smoke failed with exit code $LASTEXITCODE"
            }
        } else {
            & (Join-Path $PSScriptRoot "run-nextest-package-smoke.ps1") `
                -Archive $NextestArchive `
                -Package "rmux-sdk" `
                -TestTarget "smoke_v1_windows" `
                -TestName $sdkTest
        }
    } finally {
        if ($null -eq $previousBinary) {
            Remove-Item Env:\RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN -ErrorAction SilentlyContinue
        } else {
            $env:RMUX_SDK_WINDOWS_SMOKE_RMUX_BIN = $previousBinary
        }
        if ($null -eq $previousPipe) {
            Remove-Item Env:\RMUX_SDK_WINDOWS_SMOKE_PIPE -ErrorAction SilentlyContinue
        } else {
            $env:RMUX_SDK_WINDOWS_SMOKE_PIPE = $previousPipe
        }
        Stop-PackageDaemon $Binary $daemon
    }
}

function InvokeMouseBorderSmoke(
    [string]$Binary,
    [string]$DaemonBinary,
    [string]$NextestArchive
) {
    $previousBinary = $env:RMUX_MOUSE_BORDER_RMUX_BIN
    $previousDaemon = $env:RMUX_MOUSE_BORDER_RMUX_DAEMON_BIN
    $mouseTest = "mouse_drag_on_vertical_border_resizes_horizontal_split_through_attach_binding"
    try {
        $env:RMUX_MOUSE_BORDER_RMUX_BIN = [System.IO.Path]::GetFullPath($Binary)
        $env:RMUX_MOUSE_BORDER_RMUX_DAEMON_BIN = [System.IO.Path]::GetFullPath($DaemonBinary)
        if ([string]::IsNullOrWhiteSpace($NextestArchive)) {
            $filterArgs = @(
                "1",
                "--",
                "test",
                "--locked",
                "-p",
                "rmux",
                "--test",
                "windows_mouse_border_resize",
                $mouseTest
            )
            & "$PSScriptRoot/assert-cargo-filter-nonempty.ps1" @filterArgs
            if ($LASTEXITCODE -ne 0) {
                Fail "Windows mouse border package smoke filter failed with exit code $LASTEXITCODE"
            }
            & cargo @(
                "test",
                "--locked",
                "-p",
                "rmux",
                "--test",
                "windows_mouse_border_resize",
                $mouseTest,
                "--",
                "--exact",
                "--test-threads=1"
            )
            if ($LASTEXITCODE -ne 0) {
                Fail "Windows mouse border package smoke failed with exit code $LASTEXITCODE"
            }
        } else {
            & (Join-Path $PSScriptRoot "run-nextest-package-smoke.ps1") `
                -Archive $NextestArchive `
                -Package "rmux" `
                -TestTarget "windows_mouse_border_resize" `
                -TestName $mouseTest
        }
    } finally {
        if ($null -eq $previousBinary) {
            Remove-Item Env:\RMUX_MOUSE_BORDER_RMUX_BIN -ErrorAction SilentlyContinue
        } else {
            $env:RMUX_MOUSE_BORDER_RMUX_BIN = $previousBinary
        }
        if ($null -eq $previousDaemon) {
            Remove-Item Env:\RMUX_MOUSE_BORDER_RMUX_DAEMON_BIN -ErrorAction SilentlyContinue
        } else {
            $env:RMUX_MOUSE_BORDER_RMUX_DAEMON_BIN = $previousDaemon
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

function AssertArchiveInstaller([string]$InstallScript, [string]$Root) {
    $installRoot = Join-Path $Root "installed-rmux"
    $installBin = Join-Path $installRoot "bin"

    $global:LASTEXITCODE = 0
    & $InstallScript -InstallDir $installBin
    if ($LASTEXITCODE -ne 0) {
        Fail "archive install.ps1 failed with exit code $LASTEXITCODE"
    }

    # rmux-daemon.exe is checked next to the installed rmux.exe (bin\), where the
    # hidden-daemon resolver looks for it; checking it at the install root would
    # pass even when the daemon is unreachable at runtime.
    foreach ($required in @("bin\rmux.exe", "libexec\rmux\rmux.exe", "bin\rmux-daemon.exe", "share\rmux\artifact-metadata.json")) {
        if (-not (Test-Path -LiteralPath (Join-Path $installRoot $required) -PathType Leaf)) {
            Fail "install.ps1 did not install required file: $required"
        }
    }

    $installedBinary = Join-Path $installBin "rmux.exe"
    AssertSuccess $installedBinary @("-V") | Out-Null
    AssertHelperFallback $installedBinary
}

function Wait-BinaryReplaceable([string]$Path, [int]$TimeoutMilliseconds) {
    $deadline = [System.DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    do {
        $stream = $null
        try {
            $stream = [System.IO.File]::Open(
                $Path,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
            return
        } catch {
            Start-Sleep -Milliseconds 50
        } finally {
            if ($null -ne $stream) {
                $stream.Dispose()
            }
        }
    } while ([System.DateTime]::UtcNow -lt $deadline)

    Fail "timed out waiting for binary to become replaceable: $Path"
}

function AssertArchiveInstallerTransaction([string]$InstallScript, [string]$PackageRoot, [string]$Root) {
    $installRoot = Join-Path $Root "installed-rmux"
    $installBin = Join-Path $installRoot "bin"
    $installedBinary = Join-Path $installBin "rmux.exe"
    $installedHelper = Join-Path $installRoot "libexec\rmux\rmux.exe"
    $installedDaemon = Join-Path $installBin "rmux-daemon.exe"
    $installedReadme = Join-Path $installRoot "README.md"
    $packageHelper = Join-Path $PackageRoot "libexec\rmux\rmux.exe"
    $packageReadme = Join-Path $PackageRoot "README.md"
    $helperBackup = Join-Path $Root "package-helper-backup.exe"
    $readmeBackup = Join-Path $Root "package-readme-backup.md"
    $label = "package-installer-transaction-$PID-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
    $pipePath = NewPackagePipeName $installedBinary $label
    $daemon = $null

    Copy-Item -LiteralPath $packageHelper -Destination $helperBackup
    Copy-Item -LiteralPath $packageReadme -Destination $readmeBackup
    try {
        $daemon = Start-PackageDaemon $installedDaemon $pipePath
        AssertSuccessNoCapture $installedBinary @(
            "-S", $pipePath, "new-session", "-d", "-s", "installer_transaction", "cmd.exe", "/d", "/q", "/k"
        )

        $before = @{
            Rmux = Sha256File $installedBinary
            Helper = Sha256File $installedHelper
            Daemon = Sha256File $installedDaemon
            Readme = Sha256File $installedReadme
        }
        [System.IO.File]::WriteAllText(
            $packageHelper,
            "rmux installer transaction marker $label",
            [System.Text.Encoding]::ASCII
        )
        [System.IO.File]::WriteAllText(
            $packageReadme,
            "rmux installer asset transaction marker $label",
            [System.Text.Encoding]::ASCII
        )

        $powerShell = (Get-Process -Id $PID).Path
        $result = Invoke-NativeCapture $powerShell @(
            "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $InstallScript,
            "-InstallDir", $installBin, "-NoVerify"
        )
        $failureOutput = $result.Output -join "`n"
        if ($result.Status -eq 0) {
            Fail "install.ps1 unexpectedly upgraded a layout whose daemon was running"
        }
        if ($failureOutput -notmatch 'destination file is in use or cannot be replaced safely') {
            Fail "install.ps1 failed for an unexpected reason while the daemon was running`n$failureOutput"
        }
        if ((Sha256File $installedBinary) -ne $before.Rmux -or
            (Sha256File $installedHelper) -ne $before.Helper -or
            (Sha256File $installedDaemon) -ne $before.Daemon -or
            (Sha256File $installedReadme) -ne $before.Readme) {
            Fail "install.ps1 left a mixed package after the locked-daemon failure"
        }

        Stop-PackageDaemon $helperBackup $daemon
        $daemon = $null
        Wait-BinaryReplaceable $installedDaemon 5000

        $rollback = Invoke-NativeCapture $powerShell @(
            "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $InstallScript,
            "-InstallDir", $installBin
        )
        $rollbackOutput = $rollback.Output -join "`n"
        if ($rollback.Status -eq 0 -or $rollbackOutput -notmatch 'previous package restored') {
            Fail "install.ps1 did not report a verified rollback for an invalid helper`n$rollbackOutput"
        }
        if ((Sha256File $installedBinary) -ne $before.Rmux -or
            (Sha256File $installedHelper) -ne $before.Helper -or
            (Sha256File $installedDaemon) -ne $before.Daemon -or
            (Sha256File $installedReadme) -ne $before.Readme) {
            Fail "install.ps1 did not restore the existing package after verification failed"
        }

        $freshRoot = Join-Path $Root "fresh-rollback-rmux"
        $freshBin = Join-Path $freshRoot "bin"
        $freshRollback = Invoke-NativeCapture $powerShell @(
            "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $InstallScript,
            "-InstallDir", $freshBin
        )
        if ($freshRollback.Status -eq 0) {
            Fail "install.ps1 unexpectedly committed an invalid helper into a fresh layout"
        }
        foreach ($unexpected in @(
            (Join-Path $freshBin "rmux.exe"),
            (Join-Path $freshBin "rmux-daemon.exe"),
            (Join-Path $freshRoot "libexec\rmux\rmux.exe"),
            (Join-Path $freshRoot "README.md")
        )) {
            if (Test-Path -LiteralPath $unexpected) {
                Fail "install.ps1 left a new package file behind after fresh-layout rollback: $unexpected"
            }
        }

        Copy-Item -Force -LiteralPath $helperBackup -Destination $packageHelper
        Copy-Item -Force -LiteralPath $readmeBackup -Destination $packageReadme

        $previousFailAt = $env:RMUX_INSTALL_TEST_FAIL_AT
        try {
            $env:RMUX_INSTALL_TEST_FAIL_AT = "after-copy-package"
            [System.IO.File]::WriteAllText(
                $packageReadme,
                "rmux late package failure marker $label",
                [System.Text.Encoding]::ASCII
            )
            $lateFailure = Invoke-NativeCapture $powerShell @(
                "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $InstallScript,
                "-InstallDir", $installBin, "-NoVerify"
            )
        } finally {
            if ($null -eq $previousFailAt) {
                Remove-Item Env:RMUX_INSTALL_TEST_FAIL_AT -ErrorAction SilentlyContinue
            } else {
                $env:RMUX_INSTALL_TEST_FAIL_AT = $previousFailAt
            }
        }
        $lateFailureOutput = $lateFailure.Output -join "`n"
        if ($lateFailure.Status -eq 0 -or $lateFailureOutput -notmatch 'previous package restored') {
            Fail "install.ps1 did not roll back a failure after package files were copied`n$lateFailureOutput"
        }
        if ((Sha256File $installedBinary) -ne $before.Rmux -or
            (Sha256File $installedHelper) -ne $before.Helper -or
            (Sha256File $installedDaemon) -ne $before.Daemon -or
            (Sha256File $installedReadme) -ne $before.Readme) {
            Fail "install.ps1 left a mixed package after a late copy failure"
        }
        Copy-Item -Force -LiteralPath $readmeBackup -Destination $packageReadme

        $success = Invoke-NativeCapture $powerShell @(
            "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $InstallScript,
            "-InstallDir", $installBin
        )
        if ($success.Status -ne 0) {
            Fail "install.ps1 could not complete a valid upgrade after rollback`n$($success.Output)"
        }
        if ((Sha256File $installedHelper) -ne (Sha256File $packageHelper) -or
            (Sha256File $installedBinary) -ne (Sha256File (Join-Path $PackageRoot "rmux.exe")) -or
            (Sha256File $installedDaemon) -ne (Sha256File (Join-Path $PackageRoot "rmux-daemon.exe")) -or
            (Sha256File $installedReadme) -ne (Sha256File $packageReadme)) {
            Fail "install.ps1 did not commit the complete valid package"
        }

        # Hold the destination lock from this process, then prove that a real
        # installer process reaches lock contention before mutating any package
        # file and completes only after the lock is released.
        $concurrentRoot = Join-Path $Root "concurrent-rmux"
        $concurrentBin = Join-Path $concurrentRoot "bin"
        $concurrentLockPath = Join-Path $concurrentRoot ".rmux-install.lock"
        $concurrentWaitEventName = "Local\rmux-installer-test-$PID-$([guid]::NewGuid().ToString('N'))"
        $concurrentWaitEvent = [System.Threading.EventWaitHandle]::new(
            $false,
            [System.Threading.EventResetMode]::ManualReset,
            $concurrentWaitEventName
        )
        New-Item -ItemType Directory -Force -Path $concurrentRoot | Out-Null
        $heldInstallLock = [System.IO.File]::Open(
            $concurrentLockPath,
            [System.IO.FileMode]::OpenOrCreate,
            [System.IO.FileAccess]::ReadWrite,
            [System.IO.FileShare]::None
        )
        $concurrentInstaller = $null
        $concurrentFailure = $null
        try {
            $previousWaitEvent = $env:RMUX_INSTALL_TEST_LOCK_WAIT_EVENT
            try {
                $env:RMUX_INSTALL_TEST_LOCK_WAIT_EVENT = $concurrentWaitEventName
                $concurrentInstaller = Start-Process `
                    -FilePath $powerShell `
                    -ArgumentList @(
                        "-NoProfile", "-ExecutionPolicy", "Bypass",
                        "-File", "`"$InstallScript`"",
                        "-InstallDir", "`"$concurrentBin`"",
                        "-NoVerify"
                    ) `
                    -PassThru
            } finally {
                if ($null -eq $previousWaitEvent) {
                    Remove-Item Env:RMUX_INSTALL_TEST_LOCK_WAIT_EVENT -ErrorAction SilentlyContinue
                } else {
                    $env:RMUX_INSTALL_TEST_LOCK_WAIT_EVENT = $previousWaitEvent
                }
            }

            if (-not $concurrentWaitEvent.WaitOne(10000)) {
                throw "contending installer did not report destination-lock contention"
            }
            if ($concurrentInstaller.HasExited) {
                throw "contending installer exited before the held transaction lock was released"
            }

            foreach ($unexpected in @(
                (Join-Path $concurrentBin "rmux.exe"),
                (Join-Path $concurrentBin "rmux-daemon.exe"),
                (Join-Path $concurrentRoot "libexec\rmux\rmux.exe"),
                (Join-Path $concurrentRoot "README.md")
            )) {
                if (Test-Path -LiteralPath $unexpected) {
                    throw "contending installer mutated the package before acquiring its transaction lock: $unexpected"
                }
            }
        } catch {
            $concurrentFailure = $_.Exception.Message
        } finally {
            $heldInstallLock.Dispose()
            $concurrentWaitEvent.Dispose()
        }

        if ($null -ne $concurrentFailure) {
            if ($null -ne $concurrentInstaller -and -not $concurrentInstaller.HasExited) {
                $concurrentInstaller.Kill()
                $concurrentInstaller.WaitForExit()
            }
            Fail $concurrentFailure
        }
        if (-not $concurrentInstaller.WaitForExit(15000)) {
            $concurrentInstaller.Kill()
            $concurrentInstaller.WaitForExit()
            Fail "contending installer did not finish after the transaction lock was released"
        }
        $concurrentInstaller.WaitForExit()
        $concurrentInstaller.Refresh()
        if ($concurrentInstaller.ExitCode -ne 0) {
            Fail "contending installer exited with $($concurrentInstaller.ExitCode) after the transaction lock was released"
        }
        $concurrentInstaller.Dispose()
        if ((Sha256File (Join-Path $concurrentBin "rmux.exe")) -ne
                (Sha256File (Join-Path $PackageRoot "rmux.exe")) -or
            (Sha256File (Join-Path $concurrentBin "rmux-daemon.exe")) -ne
                (Sha256File (Join-Path $PackageRoot "rmux-daemon.exe")) -or
            (Sha256File (Join-Path $concurrentRoot "libexec\rmux\rmux.exe")) -ne
                (Sha256File $packageHelper) -or
            (Sha256File (Join-Path $concurrentRoot "README.md")) -ne
                (Sha256File $packageReadme)) {
            Fail "serialized installer did not commit the complete valid package"
        }
        if (Test-Path -LiteralPath $concurrentLockPath) {
            Fail "serialized installer left its transaction lock marker behind"
        }

        $nonLeafRoot = Join-Path $Root "non-leaf-rmux"
        $nonLeafBin = Join-Path $nonLeafRoot "bin"
        $nonLeafHelper = Join-Path $nonLeafRoot "libexec\rmux\rmux.exe"
        New-Item -ItemType Directory -Force -Path $nonLeafHelper | Out-Null
        $nonLeaf = Invoke-NativeCapture $powerShell @(
            "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $InstallScript,
            "-InstallDir", $nonLeafBin, "-NoVerify"
        )
        if ($nonLeaf.Status -eq 0 -or ($nonLeaf.Output -join "`n") -notmatch 'exists but is not a file') {
            Fail "install.ps1 did not reject a non-file binary destination"
        }
        if ((Test-Path -LiteralPath (Join-Path $nonLeafBin "rmux.exe")) -or
            (Test-Path -LiteralPath (Join-Path $nonLeafBin "rmux-daemon.exe"))) {
            Fail "install.ps1 mutated another slot before rejecting a non-file destination"
        }
    } finally {
        if ($null -ne $daemon) {
            Remove-PackageDaemon $daemon
        }
        Copy-Item -Force -LiteralPath $helperBackup -Destination $packageHelper
        Copy-Item -Force -LiteralPath $readmeBackup -Destination $packageReadme
        Remove-Item -Force -LiteralPath $helperBackup -ErrorAction SilentlyContinue
        Remove-Item -Force -LiteralPath $readmeBackup -ErrorAction SilentlyContinue
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
if (-not [string]::IsNullOrWhiteSpace($NextestArchive)) {
    $NextestArchive = [System.IO.Path]::GetFullPath($NextestArchive)
    if (-not (Test-Path -LiteralPath $NextestArchive -PathType Leaf)) {
        Fail "nextest archive not found: $NextestArchive"
    }
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

    foreach ($required in @("rmux.exe", "libexec/rmux/rmux.exe", "rmux-daemon.exe", "install.ps1", "SHA256SUMS.txt", "share/rmux/artifact-metadata.json", "README.md", "LICENSE-APACHE", "LICENSE-MIT", "rmux.1")) {
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
        if (-not [string]::IsNullOrWhiteSpace($ExpectedGitSha)) {
            if ($ExpectedGitSha -notmatch '^[0-9a-f]{40}$') {
                Fail "expected Git SHA must be a canonical full lowercase SHA"
            }
            if (-not ($metadata.PSObject.Properties.Name -contains "git_commit") -or
                -not [string]::Equals(
                    [string]$metadata.git_commit,
                    $ExpectedGitSha,
                    [System.StringComparison]::Ordinal
                )) {
                Fail "release artifact metadata git_commit does not match expected Git SHA"
            }
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
    if ($RequireReleaseArtifact) {
        & (Join-Path $PSScriptRoot "assert-windows-static-crt.ps1") `
            -Binary $binary `
            -HelperBinary $helperBinary `
            -DaemonBinary $daemonBinary
        if ($LASTEXITCODE -ne 0) {
            Fail "packaged Windows static CRT verification failed"
        }
    }

    $portableAlias = $null
    if ($RunBinary -or $RunDaemonSmoke) {
        $portableAlias = NewPortableAliasSmoke $binary $tmpRoot
        if (-not $portableAlias.Available) {
            Fail "portable alias smoke unexpectedly returned unavailable: $($portableAlias.Reason)"
        }
    }

    if ($RunBinary) {
        AssertArchiveInstaller (Join-Path $packageRoot "install.ps1") $tmpRoot
        AssertArchiveInstallerTransaction (Join-Path $packageRoot "install.ps1") $packageRoot $tmpRoot
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
        $pipePath = NewPackagePipeName $binary $label
        $daemon = $null
        try {
            $webPort = Get-FreeTcpPort
            $daemon = Start-PackageDaemon $daemonBinary $pipePath @("--web-port", "$webPort")
            AssertSuccessNoCapture $binary @("-S", $pipePath, "new-session", "-d", "-s", "package_smoke", "cmd.exe", "/d", "/q", "/k")
            $sessions = AssertSuccess $binary @("-S", $pipePath, "list-sessions", "-F", "#{session_name}")
            if (($sessions -join "`n") -notmatch 'package_smoke') {
                Fail "daemon smoke did not list package_smoke session"
            }
            $sourceFile = Join-Path $tmpRoot "package-source.conf"
            Set-Content -LiteralPath $sourceFile -Encoding ASCII -Value "set -g status off"
            AssertSuccessNoCapture $binary @("-S", $pipePath, "source-file", $sourceFile)
            $status = AssertSuccess $binary @("-S", $pipePath, "show-options", "-gv", "status")
            AssertOutputContains $status "off" "package source-file smoke"
            $webShare = AssertSuccess $binary @("-S", $pipePath, "web-share", "-t", "package_smoke", "--no-pin", "--ttl", "30")
            AssertOutputContains $webShare "http" "package web-share smoke"
            $webList = AssertSuccess $binary @("-S", $pipePath, "web-share", "list")
            AssertOutputContains $webList "package_smoke" "package web-share list smoke"
            AssertSuccessNoCapture $binary @("-S", $pipePath, "web-share", "off")
        } finally {
            if ($null -ne $daemon) {
                Stop-PackageDaemon $binary $daemon
            }
        }

        $fallbackLabel = "package-fallback-smoke-$PID-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
        $fallbackPipePath = NewPackagePipeName $binary $fallbackLabel
        $previousDisableTiny = $env:RMUX_DISABLE_TINY_CLI
        $fallbackDaemon = $null
        try {
            $env:RMUX_DISABLE_TINY_CLI = "1"
            $fallbackDaemon = Start-PackageDaemon $daemonBinary $fallbackPipePath
            AssertSuccessNoCapture $binary @("-S", $fallbackPipePath, "new-session", "-d", "-s", "package_fallback_smoke", "cmd.exe", "/d", "/q", "/k")
            $sessions = AssertSuccess $binary @("-S", $fallbackPipePath, "list-sessions", "-F", "#{session_name}")
            if (($sessions -join "`n") -notmatch 'package_fallback_smoke') {
                Fail "fallback daemon smoke did not list package_fallback_smoke session"
            }
        } finally {
            if ($null -eq $previousDisableTiny) {
                Remove-Item Env:\RMUX_DISABLE_TINY_CLI -ErrorAction SilentlyContinue
            } else {
                $env:RMUX_DISABLE_TINY_CLI = $previousDisableTiny
            }
            if ($null -ne $fallbackDaemon) {
                Stop-PackageDaemon $binary $fallbackDaemon
            }
        }

        if ($portableAlias.Available) {
            $portableAliasLabel = "package-alias-smoke-$PID-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
            $portableAliasPipePath = NewPackagePipeName $binary $portableAliasLabel
            $portableAliasDaemon = $null
            try {
                $portableAliasDaemon = Start-PackageDaemon $daemonBinary $portableAliasPipePath
                InvokeWithPathPrefix $portableAlias.Directory {
                    AssertSuccessNoCapture "rmux" @("-S", $portableAliasPipePath, "new-session", "-d", "-s", "package_alias_smoke", "cmd.exe", "/d", "/q", "/k")
                    $sessions = AssertSuccess "rmux" @("-S", $portableAliasPipePath, "list-sessions", "-F", "#{session_name}")
                    if (($sessions -join "`n") -notmatch 'package_alias_smoke') {
                        Fail "portable alias daemon smoke did not list package_alias_smoke session"
                    }
                }
            } finally {
                if ($null -ne $portableAliasDaemon) {
                    Stop-PackageDaemon $binary $portableAliasDaemon
                }
            }
        }
    }

    if ($RunSdkSmoke) {
        InvokeSdkWindowsSmoke $binary $daemonBinary $NextestArchive
    }

    if ($RunMouseBorderSmoke) {
        InvokeMouseBorderSmoke $binary $daemonBinary $NextestArchive
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
