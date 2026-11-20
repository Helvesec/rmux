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
    [string]$NextestArchive = "",
    # JSON pins both delivered archives, installed inventories and finite bounds.
    [string]$ArchivePairPlan = "",
    [switch]$ExplicitRollback,
    [switch]$Uninstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "conpty/runtime.ps1")
. (Join-Path $PSScriptRoot "packaging/layout-tool.ps1")
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
    } 
finally {
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

function Stop-ProcessIfRunning([System.Diagnostics.Process]$Process) {
    $Process.Refresh()
    if ($Process.HasExited) {
        return
    }

    try {
        $Process.Kill()
    } catch [System.InvalidOperationException] {
        return
    }
    $Process.WaitForExit()
}

function Remove-PackageDaemon([object]$Daemon) {
    if ($null -eq $Daemon) {
        return
    }
    try {
        Stop-ProcessIfRunning $Daemon.Process
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
        # Windows PowerShell 5.1 only preserves ExitCode for redirected
        # Start-Process objects when their handle is opened while still live.
        [void]$process.Handle

        if (-not $readyEvent.WaitOne(15000)) {
            Stop-ProcessIfRunning $process
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
            $nextestSdkTest = "windows::$sdkTest"
            & (Join-Path $PSScriptRoot "run-nextest-package-smoke.ps1") `
                -Archive $NextestArchive `
                -Package "rmux-sdk" `
                -TestTarget "smoke_v1_windows" `
                -TestName $nextestSdkTest
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
        $arguments = @{
            Rmux = [System.IO.Path]::GetFullPath($Binary)
            OutDir = $outDir
            PortableSmokeOnly = $true
            ExpectedGitSha = $GitSha
        }
        if (-not [string]::IsNullOrWhiteSpace($Evidence)) {
            $arguments.EvidencePath = [System.IO.Path]::GetFullPath($Evidence)
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
    # Every projected destination of the same payload, after installation.
    Invoke-LayoutTool @("--for-package", $PackageRoot, "check-installed", "--variant", "windows-archive",
        "--package-root", $PackageRoot, "--prefix", (Split-Path -Parent $installBin)) | Out-Null

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
            if ($null -ne $concurrentInstaller) {
                Stop-ProcessIfRunning $concurrentInstaller
            }
            Fail $concurrentFailure
        }
        if (-not $concurrentInstaller.WaitForExit(15000)) {
            Stop-ProcessIfRunning $concurrentInstaller
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

# Runs only the explicitly pinned, daemon-absent archive transition. It does
# not enter the general runtime campaign or invoke the installed daemon.
function Test-ExactArchivePair([string]$PlanPath, [string]$CurrentArchive, [switch]$ExplicitRollback, [switch]$Uninstall) {
    $plan = Get-Content -LiteralPath $PlanPath -Raw | ConvertFrom-Json
    if ($plan.schema -ne 1) { throw 'unsupported archive pair plan' }
    $root = [IO.Path]::GetFullPath($plan.fresh_root)
    for ($ancestor=[IO.DirectoryInfo]::new($root); $null -ne $ancestor; $ancestor=$ancestor.Parent) {
        if ($ancestor.Exists -and ($ancestor.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'pair fixture ancestor is a reparse point' }
    }
    if (Test-Path -LiteralPath $root) { throw "archive pair fixture must be new: $root" }
    if ([IO.Path]::GetFullPath($CurrentArchive) -cne $plan.packages.B.archive) { throw 'B archive differs from plan' }
    if (-not $Uninstall -and ($plan.packages.A.version -eq $plan.packages.B.version -or $plan.packages.A.source -eq $plan.packages.B.source)) { throw 'pair must cross versions and sources' }
    if ($plan.lot_seconds -gt 900 -or $plan.process_seconds -gt 60 -or $plan.logs_bytes -gt 67108864) { throw 'pair bounds exceed supported limits' }
    $drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($root))
    if ($drive.AvailableFreeSpace -lt ($plan.free_floor_bytes + $plan.new_growth_bytes)) { throw 'insufficient pair headroom' }
    New-Item -ItemType Directory -Path $root | Out-Null
    $prefix = Join-Path $root 'prefix'
    $temp = Join-Path $root 'temp'
    $logs = Join-Path $root 'logs'
    foreach ($path in @($prefix, $temp, $logs)) { New-Item -ItemType Directory -Path $path | Out-Null }
    $started = [DateTime]::UtcNow
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $commands = [Collections.Generic.List[object]]::new()
    $result = [ordered]@{ status='FAIL'; started_at=$started.ToString('o'); plan_sha256=(Sha256File $PlanPath); controller_pid=$PID; controller_birth=(Get-Process -Id $PID).StartTime.ToUniversalTime().ToString('o'); commands=$commands; snapshots=[ordered]@{}; packages=[ordered]@{}; daemon_executed=$false; rollback='UNKNOWN'; upgrade='UNKNOWN' }
    function Save-PairJson($Value, [string]$Name) {
        $Value | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath (Join-Path $root $Name) -Encoding UTF8
    }
    function Assert-PairRelative([string]$Path) {
        if ($Path -notmatch '^[A-Za-z0-9_][A-Za-z0-9_./-]*$' -or $Path -match '(^|/)\.{1,2}(/|$)|//|/$') { throw "unsafe pair path: $Path" }
    }
    function Get-PairSnapshot([string]$Path) {
        $files = @(); $dirs = @()
        foreach ($item in @(Get-ChildItem -LiteralPath $Path -Recurse -Force | Sort-Object FullName)) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "pair reparse point: $($item.FullName)" }
            $rel = $item.FullName.Substring($Path.Length + 1).Replace('\','/')
            if ($item.PSIsContainer) { $dirs += $rel } else {
                $files += [pscustomobject]@{path=$rel; bytes=$item.Length; sha256=(Sha256File $item.FullName); last_write_utc=$item.LastWriteTimeUtc.ToString('o')}
            }
        }
        return [pscustomobject]@{files=@($files); directories=@($dirs)}
    }
    function Compare-PairFiles($Actual, $Expected) {
        $a = @($Actual | ForEach-Object { "$($_.path)`t$($_.bytes)`t$($_.sha256)" } | Sort-Object)
        $b = @($Expected | ForEach-Object { "$($_.path)`t$($_.bytes)`t$($_.sha256)" } | Sort-Object)
        if (@(Compare-Object $a $b -CaseSensitive).Count) { throw 'complete installed file population or bytes differ' }
    }
    function Invoke-PairProcess([string]$Label, [string]$Program, [string[]]$Arguments, [string]$Checkpoint='') {
        if ($watch.Elapsed.TotalSeconds -ge ($plan.lot_seconds - $plan.process_seconds - 60)) { throw 'insufficient remaining bounded process time' }
        foreach ($arg in $Arguments) { if ($arg.Contains('"') -or $arg.EndsWith('\')) { throw 'unsupported process argument quoting' } }
        $info = [Diagnostics.ProcessStartInfo]::new()
        $info.FileName=$Program; $info.Arguments=(@($Arguments | ForEach-Object { '"' + $_ + '"' }) -join ' ')
        $info.WorkingDirectory=$root; $info.UseShellExecute=$false; $info.CreateNoWindow=$true
        $info.RedirectStandardOutput=$true; $info.RedirectStandardError=$true
        foreach ($key in @($info.EnvironmentVariables.Keys)) {
            if ($key -match '^(RMUX|TMUX)') { $info.EnvironmentVariables.Remove($key) }
        }
        $info.EnvironmentVariables['TEMP']=$temp; $info.EnvironmentVariables['TMP']=$temp
        $info.EnvironmentVariables['PATH']=(Join-Path $prefix 'bin') + ';' + (Join-Path $env:SystemRoot 'System32') + ';' + $PSHOME
        if ($Checkpoint) { $info.EnvironmentVariables['RMUX_INSTALL_TEST_FAIL_AT']=$Checkpoint }
        $process = [Diagnostics.Process]::new(); $process.StartInfo=$info
        $stdoutPath=Join-Path $logs ($Label+'.stdout'); $stderrPath=Join-Path $logs ($Label+'.stderr')
        $stdout=[IO.File]::Open($stdoutPath,'CreateNew','Write','Read'); $stderr=[IO.File]::Open($stderrPath,'CreateNew','Write','Read')
        $record=[ordered]@{label=$Label; program=$Program; arguments=$Arguments; checkpoint=$Checkpoint; started_at=[DateTime]::UtcNow.ToString('o'); status='RUNNING'}
        $commands.Add($record)
        $timer=[Diagnostics.Stopwatch]::StartNew(); $born=$null; $copies=@()
        try {
            if (-not $process.Start()) { throw 'process did not start' }
            $born=$process.StartTime; $record.pid=$process.Id; $record.birth_utc=$born.ToUniversalTime().ToString('o')
            $copies=@($process.StandardOutput.BaseStream.CopyToAsync($stdout), $process.StandardError.BaseStream.CopyToAsync($stderr))
            while (-not $process.WaitForExit(100)) {
                if ($timer.Elapsed.TotalSeconds -gt $plan.process_seconds) { throw "pair process timeout: $Label" }
                if (($stdout.Length + $stderr.Length) -gt 1048576) { throw "pair output limit: $Label" }
            }
            if (-not [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]$copies, 5000)) { throw 'pair output pipe remained open' }
            $record.exit_code=$process.ExitCode; $record.status='EXITED'
        } finally {
            if ($born -and -not $process.HasExited -and $process.StartTime -eq $born) {
                # Only the process handle created above and its descendants are
                # eligible for timeout cleanup; no name-based/global process kill.
                $killInfo=[Diagnostics.ProcessStartInfo]::new()
                $killInfo.FileName=Join-Path $env:SystemRoot 'System32/taskkill.exe'
                $killInfo.Arguments="/PID $($process.Id) /T /F"; $killInfo.UseShellExecute=$false; $killInfo.CreateNoWindow=$true
                $killer=[Diagnostics.Process]::Start($killInfo)
                try { if (-not $killer.WaitForExit(10000)) { Stop-ProcessIfRunning $killer; throw 'owned cleanup timeout' } } finally { $killer.Dispose() }
                if (-not $process.WaitForExit(5000)) { throw 'owned process survived cleanup' }
                $record.status='TERMINATED_OWNED_TREE'
            }
            $stdout.Dispose(); $stderr.Dispose(); $process.Dispose()
            $record.seconds=$timer.Elapsed.TotalSeconds; $record.finished_at=[DateTime]::UtcNow.ToString('o')
            Save-PairJson $commands 'commands.json'
        }
        $text=(Get-Content -LiteralPath $stdoutPath -Raw) + (Get-Content -LiteralPath $stderrPath -Raw)
        return [pscustomobject]@{exit_code=$record.exit_code; text=$text}
    }
    function Assert-PairVersionHelp([string]$Label, [string]$Version) {
        foreach ($relative in @('bin/rmux.exe','libexec/rmux/rmux.exe')) {
            $kind=if ($relative.StartsWith('bin/')) {'dispatcher'} else {'helper'}
            $exe=Join-Path $prefix $relative
            $versionResult=Invoke-PairProcess "$Label-$kind-version" $exe @('-V')
            if ($versionResult.exit_code -ne 0 -or $versionResult.text.Trim() -cne "rmux $Version") { throw "pair version failed: $Label/$kind" }
            $helpResult=Invoke-PairProcess "$Label-$kind-help" $exe @('--help')
            if ($helpResult.exit_code -notin @(0,1) -or $helpResult.text -notmatch 'usage: rmux') { throw "pair help failed: $Label/$kind" }
        }
    }
    try {
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $packageNames=if ($Uninstall) {@('B')} else {@('A','B')}
        foreach ($name in $packageNames) {
            $package=$plan.packages.$name
            if ((Get-Item -LiteralPath $package.archive).Length -ne $package.bytes -or (Sha256File $package.archive) -cne $package.sha256) { throw "archive $name identity mismatch" }
            $archive=[IO.Compression.ZipFile]::OpenRead($package.archive)
            try {
                $seen=@{}; $total=0L; $fileCount=0
                if ($archive.Entries.Count -ne $package.entries) { throw 'archive entry count mismatch' }
                foreach ($entry in $archive.Entries) {
                    $relative=$entry.FullName.TrimEnd('/')
                    Assert-PairRelative $relative
                    if (-not $relative.StartsWith($package.root_name+'/',[StringComparison]::Ordinal) -and $relative -cne $package.root_name) { throw 'archive root mismatch' }
                    if ($seen.ContainsKey($relative)) { throw 'duplicate archive path' }; $seen[$relative]=$true
                    if (($entry.ExternalAttributes -shr 16 -band 61440) -eq 40960) { throw 'archive symlink is not supported' }
                    $total += $entry.Length
                    if ($entry.Name) { $fileCount++ }
                    if ($total -gt $plan.expanded_bytes_max) { throw 'archive expansion limit' }
                }
                if ($total -ne $package.expanded_bytes -or $fileCount -ne $package.files.Count) { throw 'archive population mismatch' }
            } finally { $archive.Dispose() }
            $extract=Join-Path $root $name
            [IO.Compression.ZipFile]::ExtractToDirectory($package.archive,$extract)
            $packageRoot=Join-Path $extract $package.root_name
            $snapshot=Get-PairSnapshot $packageRoot
            Compare-PairFiles $snapshot.files $package.files
            VerifyChecksumManifest $packageRoot (Join-Path $packageRoot 'SHA256SUMS.txt')
            $metadata=Get-Content -LiteralPath (Join-Path $packageRoot 'share/rmux/artifact-metadata.json') -Raw | ConvertFrom-Json
            if ($metadata.rmux_version -cne $package.version -or $metadata.git_commit -cne $package.source -or $metadata.git_dirty) { throw "archive $name metadata mismatch" }
            if ((Sha256File (Join-Path $packageRoot 'install.ps1')) -cne $package.installer_sha256) { throw 'shipped installer mismatch' }
            $result.packages[$name]=[ordered]@{root=$packageRoot; archive_sha256=$package.sha256; source=$metadata.git_commit; version=$metadata.rmux_version; snapshot=$snapshot}
        }
        if ($Uninstall) {
            if ($plan.PSObject.Properties.Name -notcontains 'candidate_installer' -or
                    (Sha256File $plan.candidate_installer.path) -cne $plan.candidate_installer.sha256) { throw 'uninstall candidate identity mismatch' }
            $result.installer_kind='CANDIDATE_NOT_SHIPPED'; $result.transition='verified-B-uninstall'
            $result.uninstall_cases=[ordered]@{}
            foreach ($case in @('happy','edited','failure')) {
                if ($case -ne 'happy') { $prefix=Join-Path $root ('prefix-'+$case); New-Item -ItemType Directory -Path $prefix | Out-Null }
                $outside=Join-Path $root ('outside-'+$case)
                foreach ($dir in @((Join-Path $prefix 'unrelated'),(Join-Path $prefix 'user-data'),$outside)) { New-Item -ItemType Directory -Path $dir | Out-Null }
                [IO.File]::WriteAllText((Join-Path $prefix 'unrelated/keep.txt'),'unrelated owned sentinel')
                [IO.File]::WriteAllText((Join-Path $prefix 'user-data/config.json'),'{"owned_setting":true}')
                [IO.File]::WriteAllBytes((Join-Path $prefix 'user-data/data.bin'),[byte[]]@(0,1,2,253,254,255))
                [IO.File]::WriteAllText((Join-Path $outside 'keep.txt'),'outside owned sentinel')
                $initial=Get-PairSnapshot $prefix; $outsideBefore=Get-PairSnapshot $outside
                $row=[ordered]@{status='FAIL';initial=$initial;outside_before=$outsideBefore}
                $result.uninstall_cases[$case]=$row
                $installArgs=@('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',(Join-Path $result.packages.B.root 'install.ps1'),'-InstallDir',(Join-Path $prefix 'bin'),'-Version',$plan.packages.B.version)
                $install=Invoke-PairProcess "$case-B-install-verified" $plan.powershell $installArgs
                $before=Get-PairSnapshot $prefix; $row.installed=$before
                if ($install.exit_code -ne 0) { throw 'uninstall prerequisite B installation failed' }
                Compare-PairFiles $before.files (@($plan.packages.B.installed)+@($initial.files))
                if ($case -eq 'happy') { Assert-PairVersionHelp 'B-before-uninstall' $plan.packages.B.version }
                $uninstallArgs=@('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',$plan.candidate_installer.path,'-Uninstall','-InstallDir',(Join-Path $prefix 'bin'),'-PreviousPackageRoot',$result.packages.B.root)
                if ($case -eq 'edited') {
                    [IO.File]::AppendAllText((Join-Path $prefix 'README.md'),"`nowned user modification")
                    $before=Get-PairSnapshot $prefix; $row.before_edited_refusal=$before
                }
                $checkpoint=if ($case -eq 'failure') {'after-remove-package-first'} else {''}
                $remove=Invoke-PairProcess "$case-uninstall" $plan.powershell $uninstallArgs $checkpoint
                $after=Get-PairSnapshot $prefix; $row.after_first_call=$after
                if ($case -eq 'happy') {
                    if ($remove.exit_code -ne 0) { throw 'verified uninstall failed' }
                    Compare-PairFiles $after.files $initial.files
                    $repeat=Invoke-PairProcess 'happy-repeat-uninstall' $plan.powershell $uninstallArgs
                    $row.after_repeat=Get-PairSnapshot $prefix
                    if ($repeat.exit_code -eq 0 -or $repeat.text -notmatch 'previous\s+installed\s+identity\s+differs') { throw 'repeated uninstall must refuse missing owned files' }
                    Compare-PairFiles $row.after_repeat.files $initial.files
                } elseif ($case -eq 'edited') {
                    if ($remove.exit_code -eq 0 -or $remove.text -notmatch 'previous\s+installed\s+identity\s+differs') { throw 'modified owned file must refuse uninstall' }
                    Compare-PairFiles $after.files $before.files
                } else {
                    if ($remove.exit_code -eq 0 -or $remove.text -notmatch 'injected\s+installer\s+failure\s+at\s+after-remove-package-first(?=\s|$)' -or $remove.text -notmatch 'previous\s+package\s+restored') { throw 'first-removal recovery diagnostic absent' }
                    Compare-PairFiles $after.files $before.files
                    $success=Invoke-PairProcess 'failure-recovered-uninstall' $plan.powershell $uninstallArgs
                    $row.after_success=Get-PairSnapshot $prefix
                    if ($success.exit_code -ne 0) { throw 'uninstall after proven recovery failed' }
                    Compare-PairFiles $row.after_success.files $initial.files
                }
                $row.final=Get-PairSnapshot $prefix; $row.outside_after=Get-PairSnapshot $outside; $row.temporary=Get-PairSnapshot $temp
                Compare-PairFiles $row.outside_after.files $outsideBefore.files
                if ($row.temporary.files.Count -or $row.temporary.directories.Count) { throw 'uninstall temporary/recovery residue' }
                if (Test-Path -LiteralPath (Join-Path $prefix '.rmux-install.lock')) { throw 'uninstall lock remains' }
                $row.status='PASS'; Save-PairJson $result 'result.json'
            }
            $result.status='PASS'; return
        }
        $sentinelDir=Join-Path $prefix 'unrelated'
        New-Item -ItemType Directory -Path $sentinelDir | Out-Null
        [IO.File]::WriteAllText((Join-Path $sentinelDir 'keep.txt'),'archive pair unrelated owned sentinel')
        if ($ExplicitRollback) {
            # Operator-requested version rollback, without a failure checkpoint.
            # User fixtures are distinct from every delivered package path.
            $userDir=Join-Path $prefix 'user-data'
            $outside=Join-Path $root 'outside-prefix'
            foreach ($dir in @($userDir,$outside)) { New-Item -ItemType Directory -Path $dir | Out-Null }
            [IO.File]::WriteAllText((Join-Path $userDir 'config.json'),'{"owned_user_setting":true}')
            [IO.File]::WriteAllBytes((Join-Path $userDir 'data.bin'),[byte[]]@(0,1,2,253,254,255))
            [IO.File]::WriteAllText((Join-Path $outside 'keep.txt'),'outside-prefix owned sentinel')
            $initial=Get-PairSnapshot $prefix; $outsideBefore=Get-PairSnapshot $outside
            $result.snapshots['fresh']=$initial; $result.outside_before=$outsideBefore
            $result.transition='explicit-B-to-A'; $result.installer_kind='SHIPPED_BASELINE'; $result.explicit_rollback='UNKNOWN'
            $installArgs=@('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',(Join-Path $result.packages.B.root 'install.ps1'),'-InstallDir',(Join-Path $prefix 'bin'),'-Version',$plan.packages.B.version)
            $install=Invoke-PairProcess 'B-install-verified' $plan.powershell $installArgs
            $before=Get-PairSnapshot $prefix; $result.snapshots['B']=$before; Save-PairJson $before 'B-installed.json'
            if ($install.exit_code -ne 0) { throw 'verified prerequisite B install failed' }
            Compare-PairFiles $before.files (@($plan.packages.B.installed)+@($initial.files))
            Assert-PairVersionHelp 'B' $plan.packages.B.version
            Save-PairJson (Get-PairSnapshot $prefix) 'before-explicit-rollback.json'
            $installArgs[5]=Join-Path $result.packages.A.root 'install.ps1'; $installArgs[9]=$plan.packages.A.version
            if ($plan.PSObject.Properties.Name -contains 'candidate_installer') {
                $candidate=$plan.candidate_installer
                if ((Sha256File $candidate.path) -cne $candidate.sha256) { throw 'candidate installer identity mismatch' }
                $overlay=Join-Path $root 'A-candidate-overlay'
                Copy-Item -LiteralPath $result.packages.A.root -Destination $overlay -Recurse
                Copy-Item -LiteralPath $candidate.path -Destination (Join-Path $overlay 'install.ps1') -Force
                $overlaySnapshot=Get-PairSnapshot $overlay
                Compare-PairFiles @($overlaySnapshot.files | Where-Object path -cne 'install.ps1') @($plan.packages.A.files | Where-Object path -cne 'install.ps1')
                $result.installer_kind='CANDIDATE_NOT_SHIPPED'
                $result.candidate_overlay=[ordered]@{installer_sha256=$candidate.sha256; snapshot=$overlaySnapshot; original_A_installer_sha256=$plan.packages.A.installer_sha256; original_checksum_manifest_unchanged=$true; installer_is_explicit_checksum_exception=$true}
                $installArgs[5]=Join-Path $overlay 'install.ps1'
                $installArgs+=@('-PreviousPackageRoot',$result.packages.B.root)
            }
            $rollback=Invoke-PairProcess 'A-explicit-rollback-verified' $plan.powershell $installArgs
            $after=Get-PairSnapshot $prefix; $result.snapshots['A-after-explicit']=$after
            $result.rollback_temporary=Get-PairSnapshot $temp; $result.outside_after=Get-PairSnapshot $outside
            $expectedFiles=@($plan.packages.A.installed)+@($initial.files)
            $expectedPaths=@($expectedFiles | ForEach-Object path)
            $result.residual_files=@($after.files | Where-Object { $_.path -cnotin $expectedPaths })
            $result.directory_inventory=$after.directories
            Save-PairJson $after 'after-explicit-rollback.json'; Save-PairJson $result 'result.json'
            if ($rollback.exit_code -ne 0) { throw 'explicit B-to-A installer refused or failed' }
            # Identify target bytes before display commands, including daemon
            # bytes only. Report obsolete files even if displays are correct.
            Compare-PairFiles @($after.files | Where-Object { $_.path -cin $expectedPaths }) $expectedFiles
            Assert-PairVersionHelp 'A-after-explicit' $plan.packages.A.version
            Compare-PairFiles $result.outside_after.files $outsideBefore.files
            Compare-PairFiles $after.files $expectedFiles
            if ($result.rollback_temporary.files.Count -or $result.rollback_temporary.directories.Count) { throw 'explicit rollback backup/temporary residue' }
            $result.explicit_rollback='PASS'; $result.status='PASS'
            return
        }
        $initial=Get-PairSnapshot $prefix; $result.snapshots['fresh']=$initial
        $installArgs=@('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',(Join-Path $result.packages.A.root 'install.ps1'),'-InstallDir',(Join-Path $prefix 'bin'))
        $install=Invoke-PairProcess 'A-install-verified' $plan.powershell $installArgs
        $a=Get-PairSnapshot $prefix; $result.snapshots['A']=$a; Save-PairJson $a 'A-installed.json'
        if ($install.exit_code -ne 0) { throw 'verified A install failed' }
        Compare-PairFiles $a.files (@($plan.packages.A.installed) + @($initial.files))
        Assert-PairVersionHelp 'A' $plan.packages.A.version
        $installArgs[5]=Join-Path $result.packages.B.root 'install.ps1'
        # Freeze the actual preimage before invoking the cross-version failure.
        $before=Get-PairSnapshot $prefix; Save-PairJson $before 'before-failure.json'
        $failure=Invoke-PairProcess 'B-after-copy-failure' $plan.powershell $installArgs 'after-copy-package'
        $restored=Get-PairSnapshot $prefix; $result.snapshots['rollback']=$restored
        $result.rollback_temporary=Get-PairSnapshot $temp
        $result.rollback_extra_directories=@($restored.directories | Where-Object { $_ -cnotin $before.directories })
        Save-PairJson $restored 'after-failure.json'
        Save-PairJson $result 'result.json'
        if ($failure.exit_code -eq 0 -or $failure.text -notmatch 'injected\s+installer\s+failure\s+at\s+after-copy-package(?=\s|$)' -or $failure.text -notmatch 'previous\s+package\s+restored') { throw 'after-copy failure/restoration diagnostic absent' }
        Compare-PairFiles $restored.files $before.files
        if (Test-Path -LiteralPath (Join-Path $prefix '.rmux-install.lock')) { throw 'installer lock remains after rollback' }
        if ($result.rollback_temporary.files.Count -or $result.rollback_temporary.directories.Count) { throw 'rollback temporary/backup residue' }
        $result.rollback='PASS'
        Assert-PairVersionHelp 'restored-A' $plan.packages.A.version
        Save-PairJson (Get-PairSnapshot $prefix) 'before-upgrade.json'
        $upgrade=Invoke-PairProcess 'B-upgrade-verified' $plan.powershell $installArgs
        $b=Get-PairSnapshot $prefix; $result.snapshots['B']=$b; Save-PairJson $b 'B-installed.json'
        if ($upgrade.exit_code -ne 0) { throw 'verified B upgrade failed' }
        Compare-PairFiles $b.files (@($plan.packages.B.installed) + @($initial.files))
        Assert-PairVersionHelp 'B' $plan.packages.B.version
        $result.final_temporary=Get-PairSnapshot $temp
        if ($result.final_temporary.files.Count -or $result.final_temporary.directories.Count) { throw 'final temporary/backup residue' }
        $result.upgrade='PASS'; $result.status='PASS'
    } catch {
        $result.error=$_.Exception.Message
        throw
    } finally {
        $result.finished_at=[DateTime]::UtcNow.ToString('o'); $result.seconds=$watch.Elapsed.TotalSeconds
        $result.free_bytes_after=$drive.AvailableFreeSpace
        Save-PairJson $result 'result.json'
    }
}

if ($Uninstall -and (-not $ArchivePairPlan -or $ExplicitRollback)) { throw "uninstall requires ArchivePairPlan without ExplicitRollback" }

if ($ExplicitRollback -and -not $ArchivePairPlan) { throw "explicit rollback requires ArchivePairPlan" }

if ($ArchivePairPlan) {
    # Test binding, not value: explicit false switches and empty SHA strings
    # are still unsupported release-verification requests in pair mode.
    if ($PSBoundParameters.ContainsKey("RequireReleaseArtifact") -or $PSBoundParameters.ContainsKey("ExpectedGitSha")) {
        throw "archive pair mode cannot accept explicit RequireReleaseArtifact or ExpectedGitSha; use ordinary mode for release verification"
    }
    if ($RunBinary -or $RunDaemonSmoke -or $RunSdkSmoke -or $RunMouseBorderSmoke -or $RunCtrlMatrixSmoke -or $NextestArchive) { throw "archive pair mode cannot enter runtime campaigns" }
    Test-ExactArchivePair $ArchivePairPlan $Archive -ExplicitRollback:$ExplicitRollback -Uninstall:$Uninstall
    exit 0
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

    # The layout, and the absence of anything else, come from the one
    # descriptor every builder and installer projects.
    Invoke-LayoutTool @("--for-package", $packageRoot, "check-tree", "--variant", "windows-archive", "--root", $packageRoot) | Out-Null
    $expectedManifest = @(Invoke-LayoutTool @("--for-package", $packageRoot, "manifest", "--variant", "windows-archive"))
    $shippedManifest = Get-Content -LiteralPath (Join-Path $packageRoot "share/rmux/package-layout.tsv")
    if ((($expectedManifest | Out-String).Trim()) -cne (($shippedManifest | Out-String).Trim())) {
        Fail "packaged layout manifest does not match the layout descriptor"
    }

    VerifyChecksumManifest $packageRoot (Join-Path $packageRoot "SHA256SUMS.txt")

    $binary = Join-Path $packageRoot "rmux.exe"
    $metadataPath = Join-Path $packageRoot "share/rmux/artifact-metadata.json"
    $metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
    if ($metadata.artifact_kind -ne "windows-package-binary") {
        Fail "metadata artifact_kind is not windows-package-binary"
    }
    $packageLayoutId = Get-LayoutLine @("--for-package", $packageRoot, "layout-id", "--variant", "windows-archive")
    if ($metadata.package_layout -ne $packageLayoutId) {
        Fail "metadata package_layout is not $packageLayoutId"
    }
    if ($metadata.conpty_runtime_manifest -cne "share/rmux/conpty-runtime.json") {
        Fail "metadata ConPTY manifest path is invalid"
    }
    Assert-ConptyPackage $packageRoot $metadata.target
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
