param(
    [Parameter(Mandatory = $true)]
    [string] $Out,
    [int] $Iterations = 5,
    [string] $Binary = "",
    [string] $Layout = "",
    [string[]] $OnlyOperations = @(),
    [switch] $IncludePsmux,
    [switch] $NoPsmux,
    [switch] $SkipBuild
)

$ErrorActionPreference = "Stop"

if ($Iterations -lt 1) {
    throw "Iterations must be a positive integer"
}

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Set-Location $RepoRoot

Remove-Item Env:RMUX -ErrorAction SilentlyContinue
Remove-Item Env:TMUX -ErrorAction SilentlyContinue
Remove-Item Env:TERM_PROGRAM -ErrorAction SilentlyContinue
Remove-Item Env:ZELLIJ -ErrorAction SilentlyContinue

$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
$RmuxLayout = "custom-binary"
$Rmux = $null
$RmuxHelper = $null
$RmuxDaemon = $null

if ([string]::IsNullOrWhiteSpace($Layout)) {
    $Layout = Join-Path $TargetDir "benchmarks\layout-windows"
}

$MeasureRmux = $true
$MeasureTmux = $true
$MeasureZellij = $true
$MeasurePsmux = -not $NoPsmux

if ($MeasureRmux -and [string]::IsNullOrWhiteSpace($Binary)) {
    $RmuxLayout = "packaged-tiny"
    $Rmux = Join-Path $Layout "rmux.exe"
    $RmuxHelper = Join-Path $Layout "libexec\rmux\rmux.exe"
    $RmuxDaemon = Join-Path $Layout "rmux-daemon.exe"

    if (-not $SkipBuild) {
        cargo build --locked --release --package rmux --bin rmux
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build full rmux helper failed"
        }
        if (Test-Path -LiteralPath $Layout) {
            Remove-Item -LiteralPath $Layout -Recurse -Force
        }
        New-Item -ItemType Directory -Force -Path $Layout | Out-Null
        New-Item -ItemType Directory -Force -Path (Join-Path $Layout "libexec\rmux") | Out-Null
        Copy-Item -LiteralPath (Join-Path $TargetDir "release\rmux.exe") -Destination $RmuxHelper -Force

        cargo build --locked --release --package rmux --features tiny-cli --bin rmux
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build tiny rmux failed"
        }
        Copy-Item -LiteralPath (Join-Path $TargetDir "release\rmux.exe") -Destination $Rmux -Force

        cargo build --locked --release --package rmux --bin rmux-daemon
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build rmux-daemon failed"
        }
        Copy-Item -LiteralPath (Join-Path $TargetDir "release\rmux-daemon.exe") -Destination $RmuxDaemon -Force
    }
} elseif ($MeasureRmux) {
    $Rmux = $Binary
    if (-not $SkipBuild) {
        cargo build --locked --release --package rmux --bin rmux
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build rmux failed"
        }
    }
}

if ($MeasureRmux) {
    if (-not (Test-Path -LiteralPath $Rmux -PathType Leaf)) {
        throw "rmux binary not found: $Rmux"
    }
    if ($RmuxLayout -eq "packaged-tiny") {
        if (-not (Test-Path -LiteralPath $RmuxHelper -PathType Leaf)) {
            throw "rmux helper not found: $RmuxHelper"
        }
        if (-not (Test-Path -LiteralPath $RmuxDaemon -PathType Leaf)) {
            throw "rmux daemon not found: $RmuxDaemon"
        }
    }
    $Rmux = (Resolve-Path $Rmux).Path
    $RmuxPublic = $Rmux
    if ($null -ne $RmuxHelper) {
        $RmuxHelper = (Resolve-Path $RmuxHelper).Path
    }
    if ($null -ne $RmuxDaemon) {
        $RmuxDaemon = (Resolve-Path $RmuxDaemon).Path
    }
} else {
    $RmuxLayout = "not-measured"
    $RmuxPublic = $null
}
$PsmuxCommand = if ($MeasurePsmux) { Get-Command "psmux" -ErrorAction SilentlyContinue } else { $null }
$Psmux = if ($PsmuxCommand) { $PsmuxCommand.Source } else { $null }
$script:ClearPsmuxAfterEachOperation = $true
$ZellijCommand = if ($MeasureZellij) { Get-Command "zellij" -ErrorAction SilentlyContinue } else { $null }
$ZellijFallback = Join-Path $env:LOCALAPPDATA "zellij\zellij.exe"
$Zellij = if (-not $MeasureZellij) {
    $null
} elseif ($ZellijCommand) {
    $ZellijCommand.Source
} elseif (Test-Path -LiteralPath $ZellijFallback -PathType Leaf) {
    $ZellijFallback
} else {
    $null
}

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null

function Get-GitValue {
    param([Parameter(ValueFromRemainingArguments = $true)] [string[]] $GitArgs)
    try {
        return ((& git @GitArgs 2>$null | Select-Object -First 1).ToString().Trim())
    } catch {
        return "unknown"
    }
}

function Convert-ToRepoRelativePath {
    param([string] $Path)
    if ([string]::IsNullOrWhiteSpace($Path)) {
        return $null
    }
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $rootPath = [System.IO.Path]::GetFullPath($RepoRoot)
    if (-not $rootPath.EndsWith([System.IO.Path]::DirectorySeparatorChar)) {
        $rootPath = "$rootPath$([System.IO.Path]::DirectorySeparatorChar)"
    }
    if ($fullPath.StartsWith($rootPath, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $fullPath.Substring($rootPath.Length).Replace("\", "/")
    }
    return $fullPath
}

function Convert-ToPublicText {
    param([object] $Value)
    if ($null -eq $Value) {
        return $null
    }
    $text = [string] $Value
    if ([string]::IsNullOrWhiteSpace($text)) {
        return $null
    }
    $text = $text -replace "`e\[[0-9;?]*[ -/]*[@-~]", ""
    $text = $text -replace "[\u0000-\u001F\u007F]", " "
    $text = $text -replace "\s+", " "
    $text = $text.Trim()
    if ($text.Length -eq 0) {
        return $null
    }
    if ($text.Length -gt 120 -or $text -match "[A-Za-z]:\\|\\Users\\") {
        return "available"
    }
    return $text
}

function Invoke-Captured {
    param([string[]] $Command, [int] $TimeoutSeconds = 10)
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $Command[0]
    if ($Command.Length -gt 1) {
        $psi.Arguments = (($Command[1..($Command.Length - 1)]) |
            ForEach-Object { Convert-ToProcessArgument $_ }) -join " "
    }
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $psi
    $stdout = [System.IO.MemoryStream]::new()
    $stderr = [System.IO.MemoryStream]::new()
    [void] $process.Start()
    $stdoutTask = $process.StandardOutput.BaseStream.CopyToAsync($stdout)
    $stderrTask = $process.StandardError.BaseStream.CopyToAsync($stderr)
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        try {
            & taskkill.exe /PID $process.Id /T /F | Out-Null
        } catch {
        }
        try {
            if (-not $process.HasExited) {
                $process.Kill($true)
            }
        } catch {
            try {
                if (-not $process.HasExited) {
                    $process.Kill()
                }
            } catch {
            }
        }
        [void] $process.WaitForExit(1000)
        [void] $stdoutTask.Wait(1000)
        [void] $stderrTask.Wait(1000)
        return @{
            ExitCode = -1
            Stdout = [System.Text.Encoding]::UTF8.GetString($stdout.ToArray())
            Stderr = [System.Text.Encoding]::UTF8.GetString($stderr.ToArray())
        }
    }
    $process.WaitForExit()
    [void] $stdoutTask.Wait(1000)
    [void] $stderrTask.Wait(1000)
    return @{
        ExitCode = $process.ExitCode
        Stdout = [System.Text.Encoding]::UTF8.GetString($stdout.ToArray())
        Stderr = [System.Text.Encoding]::UTF8.GetString($stderr.ToArray())
    }
}

function Convert-ToProcessArgument {
    param([string] $Argument)
    if ($Argument -notmatch '[\s"]') {
        return $Argument
    }
    return '"' + ($Argument -replace '\\+$', '$0$0' -replace '"', '\"') + '"'
}

function Get-Version {
    param([string] $Command, [Parameter(ValueFromRemainingArguments = $true)] [string[]] $VersionArgs)
    $cmd = Get-Command $Command -ErrorAction SilentlyContinue
    if (-not $cmd) {
        return $null
    }
    $fullCommand = @($Command) + $VersionArgs
    $result = Invoke-Captured -Command $fullCommand
    if ($result.ExitCode -ne 0) {
        return $null
    }
    $text = ([string] $result.Stdout) + " " + ([string] $result.Stderr)
    foreach ($line in ($text -split "`r?`n")) {
        $publicText = Convert-ToPublicText $line
        if ($null -ne $publicText) {
            return $publicText
        }
    }
    return $null
}

function Test-WslCommand {
    param([string] $Command)
    $wsl = Get-Command "wsl.exe" -ErrorAction SilentlyContinue
    if (-not $wsl) {
        return $false
    }
    if ($Command -eq "tmux") {
        $version = Get-Version "wsl.exe" "tmux" "-V"
        if ($null -eq $version) {
            return $false
        }
        return $version.ToString().StartsWith("tmux")
    }
    $result = Invoke-Captured -Command @("wsl.exe", "sh", "-lc", "command -v $Command") -TimeoutSeconds 10
    $path = Convert-ToPublicText $result.Stdout
    return $result.ExitCode -eq 0 -and $null -ne $path -and $path.StartsWith("/")
}

function Invoke-Quiet {
    param([string[]] $Command, [int] $TimeoutSeconds = 15)
    $result = Invoke-Captured -Command $Command -TimeoutSeconds $TimeoutSeconds
    if ($result.ExitCode -eq -1) {
        throw "command timed out: $($Command -join ' ')"
    }
    if ($result.ExitCode -ne 0) {
        throw "command failed: $($Command -join ' ')"
    }
}

function Measure-CommandMs {
    param([string[]] $Command, [int] $TimeoutSeconds = 30)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    Invoke-Quiet -Command $Command -TimeoutSeconds $TimeoutSeconds
    $sw.Stop()
    return $sw.Elapsed.TotalMilliseconds
}

function Get-Stats {
    param([double[]] $Samples)
    $ordered = $Samples | Sort-Object
    $mid = [int][Math]::Floor(($ordered.Count - 1) / 2)
    $p95Index = [Math]::Min($ordered.Count - 1, [Math]::Max(0, [int][Math]::Ceiling($ordered.Count * 0.95) - 1))
    return @{
        p50_ms = [Math]::Round($ordered[$mid], 3)
        p95_ms = [Math]::Round($ordered[$p95Index], 3)
        samples_ms = @($Samples | ForEach-Object { [Math]::Round($_, 3) })
    }
}

function New-SocketName {
    param([string] $Operation)
    return "rmux-bench-$Operation-$PID-$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"
}

function New-WslSocketName {
    param([string] $Operation)
    return "rmux-bench-wsl-$Operation-$PID-$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"
}

function New-WslTmuxCommand {
    param([Parameter(ValueFromRemainingArguments = $true)] [string[]] $TmuxArgs)
    return @("wsl.exe", "--exec", "tmux") + $TmuxArgs
}

function Clear-WslTmuxServer {
    param([string] $Socket)
    $script = 'for p in $(pgrep -f "tmux -L ' + $Socket + '" || true); do comm=$(ps -p "$p" -o comm= 2>/dev/null | tr -d " "); if [ "$comm" = "tmux" ]; then kill -TERM "$p" 2>/dev/null || true; fi; done; sleep 0.1; for p in $(pgrep -f "tmux -L ' + $Socket + '" || true); do comm=$(ps -p "$p" -o comm= 2>/dev/null | tr -d " "); if [ "$comm" = "tmux" ]; then kill -KILL "$p" 2>/dev/null || true; fi; done; true'
    [void] (Invoke-Captured -Command @("wsl.exe", "--exec", "sh", "-lc", $script) -TimeoutSeconds 3)
}

function New-PsmuxSessionName {
    param([string] $Operation)
    return "rmuxbench$($Operation -replace '[^A-Za-z0-9]', '')$PID$([Guid]::NewGuid().ToString('N').Substring(0, 8))"
}

function Test-PsmuxSession {
    if ($null -eq $Psmux) {
        return $false
    }
    $session = New-PsmuxSessionName "psmux_probe"
    try {
        $result = Invoke-Captured -Command @($Psmux, "new-session", "-d", "-s", $session) -TimeoutSeconds 5
        return $result.ExitCode -eq 0
    } finally {
        try {
            Invoke-Quiet -Command @($Psmux, "kill-session", "-t", $session) -TimeoutSeconds 5
        } catch {
        }
        Clear-PsmuxIdleServer
    }
}

function Get-WslOutputCommand {
    param([int] $Lines)
    return "/bin/sh -c '$(Get-WslOutputScript $Lines)'"
}

function Get-WslOutputScript {
    param([int] $Lines)
    return "i=0; while [ `$i -lt $Lines ]; do printf `"rmux-bench-%05d\n`" `"`$i`"; i=`$((i+1)); done; sleep 60"
}

function Get-WindowsOutputCommand {
    param([int] $Lines)
    return "powershell.exe -NoProfile -Command `"for (`$i = 0; `$i -lt $Lines; `$i++) { Write-Output ('rmux-bench-{0:D5}' -f `$i) }; Start-Sleep 60`""
}

function Get-WindowsOutputCommandArgs {
    param([int] $Lines)
    return @(
        "powershell.exe",
        "-NoProfile",
        "-Command",
        "for (`$i = 0; `$i -lt $Lines; `$i++) { Write-Output ('rmux-bench-{0:D5}' -f `$i) }; Start-Sleep 60"
    )
}

function Test-OperationNeedsReadyPane {
    param([string] $Operation)
    return $Operation -notin @(
        "list_commands",
        "new_session_cold_sh",
        "new_session_warm_sh",
        "list_sessions_default"
    )
}

function Wait-NativeTmuxLikeSessionReady {
    param([string] $Executable, [string] $Socket)
    Invoke-Quiet -Command @($Executable, "-L", $Socket, "list-panes", "-t", "bench", "-F", "#{pane_pid}") -TimeoutSeconds 15
}

function Wait-NativeTmuxLikeTarget {
    param([string] $Executable, [string] $Socket, [string] $Target)
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ($true) {
        try {
            Invoke-Quiet -Command @($Executable, "-L", $Socket, "list-panes", "-t", $Target) -TimeoutSeconds 5
            return
        } catch {
            if ([DateTime]::UtcNow -ge $deadline) {
                throw
            }
            Start-Sleep -Milliseconds 10
        }
    }
}

function Wait-WslTmuxSessionReady {
    param([string] $Socket)
    Invoke-Quiet -Command (New-WslTmuxCommand "-L" $Socket "list-panes" "-t" "bench" "-F" "#{pane_pid}") -TimeoutSeconds 15
}

function Wait-WslTmuxTarget {
    param([string] $Socket, [string] $Target)
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ($true) {
        try {
            Invoke-Quiet -Command (New-WslTmuxCommand "-L" $Socket "list-panes" "-t" $Target) -TimeoutSeconds 5
            return
        } catch {
            if ([DateTime]::UtcNow -ge $deadline) {
                throw
            }
            Start-Sleep -Milliseconds 10
        }
    }
}

function Wait-PsmuxSessionReady {
    param([string] $Session)
    Invoke-Quiet -Command @($Psmux, "list-panes", "-t", $Session, "-F", "#{pane_pid}") -TimeoutSeconds 15
}

function Wait-PsmuxTarget {
    param([string] $Target)
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while ($true) {
        try {
            Invoke-Quiet -Command @($Psmux, "list-panes", "-t", $Target) -TimeoutSeconds 5
            return
        } catch {
            if ([DateTime]::UtcNow -ge $deadline) {
                throw
            }
            Start-Sleep -Milliseconds 10
        }
    }
}

function New-WslSourceFile {
    $script = 'tmp=$(mktemp); printf "%s\n" "set-option -g status on" > "$tmp"; printf "%s\n" "$tmp"'
    $result = Invoke-Captured -Command @("wsl.exe", "--exec", "sh", "-lc", $script) -TimeoutSeconds 5
    if ($result.ExitCode -ne 0) {
        throw "failed to create WSL source file"
    }
    $path = Convert-ToPublicText $result.Stdout
    if ($null -eq $path -or -not $path.StartsWith("/")) {
        throw "invalid WSL source file path"
    }
    return $path
}

function Remove-WslPath {
    param([string] $Path)
    if (-not [string]::IsNullOrWhiteSpace($Path)) {
        [void] (Invoke-Captured -Command @("wsl.exe", "--exec", "rm", "-f", $Path) -TimeoutSeconds 5)
    }
}

function Clear-PsmuxIdleServer {
    if ($null -eq $Psmux) {
        return
    }
    $sessions = Invoke-Captured -Command @($Psmux, "list-sessions") -TimeoutSeconds 3
    if ($sessions.ExitCode -ne 0 -or -not [string]::IsNullOrWhiteSpace([string] $sessions.Stdout)) {
        return
    }
    $servers = Get-CimInstance Win32_Process -Filter "name = 'psmux.exe'" |
        Where-Object { $_.CommandLine -match '\bpsmux\.exe"?\s+server\s+-s\s+__warm__\b' }
    foreach ($server in $servers) {
        try {
            Stop-Process -Id $server.ProcessId -Force -ErrorAction Stop
        } catch {
        }
    }
}

function Invoke-Zellij {
    param([string[]] $ZellijArgs, [int] $TimeoutSeconds = 15)
    if ($null -eq $Zellij) {
        throw "zellij not available"
    }
    Invoke-Quiet -Command (@($Zellij) + $ZellijArgs) -TimeoutSeconds $TimeoutSeconds
}

function Measure-ZellijCommandMs {
    param([string[]] $ZellijArgs, [int] $TimeoutSeconds = 15)
    if ($null -eq $Zellij) {
        throw "zellij not available"
    }
    return Measure-CommandMs -Command (@($Zellij) + $ZellijArgs) -TimeoutSeconds $TimeoutSeconds
}

function Get-ZellijVersion {
    if ($null -eq $Zellij) {
        return $null
    }
    $result = Invoke-Captured -Command @($Zellij, "--version") -TimeoutSeconds 10
    if ($result.ExitCode -ne 0) {
        return $null
    }
    foreach ($line in ([string] $result.Stdout -split "`r?`n")) {
        $publicText = Convert-ToPublicText $line
        if ($null -ne $publicText) {
            return $publicText
        }
    }
    return $null
}

function Test-Zellij {
    $version = Get-ZellijVersion
    return $null -ne $version
}

function New-ZellijSessionName {
    param([string] $Operation)
    return "rmux-zj-$($Operation.Replace('_', '-'))-$PID-$([Guid]::NewGuid().ToString('N').Substring(0, 8))"
}

function Get-ZellijKeepAliveLayout {
    return @'
layout {
    pane command="powershell.exe" {
        args "-NoProfile" "-Command" "Start-Sleep 60"
    }
}
'@
}

function Clear-ZellijSession {
    param([string] $Name)
    Get-Process -Name zellij -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue
}

function New-ZellijSession {
    param([string] $Name)
    Invoke-Zellij -ZellijArgs @("--layout-string", (Get-ZellijKeepAliveLayout), "attach", "--create-background", $Name) -TimeoutSeconds 30
}

function Measure-ZellijWithSession {
    param(
        [string] $Operation,
        [string[]] $ZellijArgs,
        [scriptblock] $Setup = $null,
        [int] $TimeoutSeconds = 15
    )
    $name = New-ZellijSessionName $Operation
    try {
        New-ZellijSession $name
        if ($null -ne $Setup) {
            & $Setup $name
        }
        return Measure-ZellijCommandMs -ZellijArgs (@("--session", $name) + $ZellijArgs) -TimeoutSeconds $TimeoutSeconds
    } finally {
        Clear-ZellijSession $name
    }
}

function Measure-ZellijOperation {
    param([string] $Operation)
    if ($Operation -eq "new_session_cold_sh") {
        $name = New-ZellijSessionName $Operation
        try {
            Clear-ZellijSession $name
            return Measure-ZellijCommandMs -ZellijArgs @("--layout-string", (Get-ZellijKeepAliveLayout), "attach", "--create-background", $name) -TimeoutSeconds 30
        } finally {
            Clear-ZellijSession $name
        }
    }
    if ($Operation -eq "list_sessions_default") {
        $name = New-ZellijSessionName $Operation
        try {
            New-ZellijSession $name
            return Measure-ZellijCommandMs -ZellijArgs @("list-sessions") -TimeoutSeconds 15
        } finally {
            Clear-ZellijSession $name
        }
    }
    switch ($Operation) {
        "new_window_detached_sh" {
            return Measure-ZellijWithSession $Operation @("action", "new-tab", "--name", "bench-tab") $null 20
        }
        "send_keys_detached_round_trip" {
            return Measure-ZellijWithSession $Operation @("action", "write-chars", "printf rmux-bench`n") $null 15
        }
        "capture_pane_80x24" {
            return Measure-ZellijWithSession $Operation @("action", "dump-screen") $null 15
        }
        "capture_pane_200x50_scrollback_10k" {
            $setup = {
                param([string] $Name)
                Invoke-Zellij -ZellijArgs (@("--session", $Name, "action", "new-pane", "--") + (Get-WindowsOutputCommandArgs 10000)) -TimeoutSeconds 30
                Start-Sleep -Milliseconds 50
            }
            return Measure-ZellijWithSession $Operation @("action", "dump-screen", "--full") $setup 30
        }
        "list_windows_20" {
            $setup = {
                param([string] $Name)
                for ($i = 1; $i -lt 20; $i++) {
                    Invoke-Zellij -ZellijArgs @("--session", $Name, "action", "new-tab", "--name", "w$i") -TimeoutSeconds 10
                }
            }
            return Measure-ZellijWithSession $Operation @("action", "query-tab-names") $setup 30
        }
        "kill_session" {
            $name = New-ZellijSessionName $Operation
            try {
                New-ZellijSession $name
                return Measure-ZellijCommandMs -ZellijArgs @("kill-session", $name) -TimeoutSeconds 15
            } finally {
                Clear-ZellijSession $name
            }
        }
        "split_window_h_detached_sh" {
            return Measure-ZellijWithSession $Operation @("action", "new-pane", "--direction", "right") $null 15
        }
        "split_window_v_detached_sh" {
            return Measure-ZellijWithSession $Operation @("action", "new-pane", "--direction", "down") $null 15
        }
        "resize_pane_right_1" {
            $setup = {
                param([string] $Name)
                Invoke-Zellij -ZellijArgs @("--session", $Name, "action", "new-pane", "--direction", "right") -TimeoutSeconds 10
            }
            return Measure-ZellijWithSession $Operation @("action", "resize", "increase", "right") $setup 15
        }
        "list_panes_80" {
            $setup = {
                param([string] $Name)
                for ($i = 0; $i -lt 79; $i++) {
                    Invoke-Zellij -ZellijArgs @("--session", $Name, "action", "new-pane") -TimeoutSeconds 10
                }
            }
            return Measure-ZellijWithSession $Operation @("action", "list-panes") $setup 60
        }
        default {
            throw "unsupported zellij operation $Operation"
        }
    }
}

function Measure-NativeTmuxLikeOperation {
    param([string] $Executable, [string] $Operation)
    $socket = New-SocketName $Operation
    try {
        if ($Operation -eq "list_commands") {
            return Measure-CommandMs -Command @($Executable, "list-commands")
        }
        if ($Operation -eq "new_session_cold_sh") {
            try {
                Invoke-Quiet -Command @($Executable, "-L", $socket, "kill-server") -TimeoutSeconds 5
            } catch {
            }
            return Measure-CommandMs -Command @($Executable, "-L", $socket, "new-session", "-d", "-s", "bench")
        }
        if ($Operation -eq "new_session_warm_sh") {
            Invoke-Quiet -Command @($Executable, "-L", $socket, "start-server")
            return Measure-CommandMs -Command @($Executable, "-L", $socket, "new-session", "-d", "-s", "bench")
        }
        $command = $null
        if ($Operation -eq "capture_pane_5000_lines") {
            $command = Get-WindowsOutputCommand 5000
        }
        if ($Operation -eq "capture_pane_200x50_scrollback_10k") {
            $command = Get-WindowsOutputCommand 10000
        }
        $newSession = @($Executable, "-L", $socket, "new-session", "-d", "-s", "bench")
        if ($null -ne $command) {
            $newSession += $command
        }
        Invoke-Quiet -Command $newSession
        if (Test-OperationNeedsReadyPane $Operation) {
            Wait-NativeTmuxLikeSessionReady $Executable $socket
        }
        switch ($Operation) {
            "split_window_h_detached_sh" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "split-window", "-h", "-d", "-t", "bench") }
            "split_window_v_detached_sh" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "split-window", "-v", "-d", "-t", "bench") }
            "split_window_h_attached_sh" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "split-window", "-h", "-t", "bench") }
            "split_window_v_attached_sh" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "split-window", "-v", "-t", "bench") }
            "resize_pane_right_1" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "resize-pane", "-t", "bench", "-R", "1") }
            "resize_pane_right_10" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "resize-pane", "-t", "bench", "-R", "10") }
            "resize_pane_left_1" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "resize-pane", "-t", "bench", "-L", "1") }
            "resize_pane_absolute_100x30" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "resize-pane", "-x", "100", "-y", "30", "-t", "bench") }
            "resize_pane_absolute_200x50" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "resize-pane", "-x", "200", "-y", "50", "-t", "bench") }
            "list_sessions_default" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "list-sessions") }
            "capture_pane_5000_lines" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "capture-pane", "-p", "-t", "bench") }
            "capture_pane_80x24" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "capture-pane", "-p", "-t", "bench") }
            "capture_pane_200x50_scrollback_10k" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "capture-pane", "-p", "-S", "-10000", "-t", "bench") }
            "new_window_detached_sh" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "new-window", "-d", "-t", "bench") }
            "send_keys_detached_round_trip" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "send-keys", "-t", "bench:0.0", "printf rmux-bench", "Enter") }
            "display_message_default" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "display-message", "-p", "-t", "bench", "#{session_name}") }
            "show_options_global" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "show-options", "-g") }
            "show_window_options" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "show-window-options", "-g") }
            "list_windows_20" {
                for ($i = 1; $i -lt 20; $i++) {
                    Invoke-Quiet -Command @($Executable, "-L", $socket, "new-window", "-d", "-n", "w$i", "-t", "bench")
                }
                return Measure-CommandMs -Command @($Executable, "-L", $socket, "list-windows", "-t", "bench")
            }
            "list_panes_80" {
                for ($i = 1; $i -lt 20; $i++) {
                    Invoke-Quiet -Command @($Executable, "-L", $socket, "new-window", "-d", "-n", "w$i", "-t", "bench")
                    Wait-NativeTmuxLikeTarget $Executable $socket "bench:$i"
                }
                for ($window = 0; $window -lt 20; $window++) {
                    Wait-NativeTmuxLikeTarget $Executable $socket "bench:$window"
                    for ($pane = 0; $pane -lt 3; $pane++) {
                        Invoke-Quiet -Command @($Executable, "-L", $socket, "split-window", "-d", "-t", "bench:$window")
                    }
                }
                return Measure-CommandMs -Command @($Executable, "-L", $socket, "list-panes", "-a")
            }
            "rename_window" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "rename-window", "-t", "bench:0", "renamed") }
            "select_window_next" {
                Invoke-Quiet -Command @($Executable, "-L", $socket, "new-window", "-d", "-n", "next", "-t", "bench")
                return Measure-CommandMs -Command @($Executable, "-L", $socket, "select-window", "-t", "bench:1")
            }
            "join_pane_detached" {
                Invoke-Quiet -Command @($Executable, "-L", $socket, "new-window", "-d", "-n", "join", "-t", "bench")
                return Measure-CommandMs -Command @($Executable, "-L", $socket, "join-pane", "-d", "-s", "bench:1.0", "-t", "bench:0.0")
            }
            "source_file_minimal" {
                $sourceFile = New-TemporaryFile
                try {
                    Set-Content -LiteralPath $sourceFile.FullName -Value "set-option -g status on" -Encoding ASCII
                    return Measure-CommandMs -Command @($Executable, "-L", $socket, "source-file", $sourceFile.FullName)
                } finally {
                    Remove-Item -LiteralPath $sourceFile.FullName -Force -ErrorAction SilentlyContinue
                }
            }
            "set_option_quiet" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "set-option", "-g", "status", "on") }
            "set_window_option_quiet" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "set-window-option", "-g", "automatic-rename", "off") }
            "kill_pane" {
                Invoke-Quiet -Command @($Executable, "-L", $socket, "split-window", "-d", "-t", "bench")
                return Measure-CommandMs -Command @($Executable, "-L", $socket, "kill-pane", "-t", "bench:0.1")
            }
            "kill_session" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "kill-session", "-t", "bench") }
            "kill_server" { return Measure-CommandMs -Command @($Executable, "-L", $socket, "kill-server") }
            default { throw "unsupported operation $Operation" }
        }
    } finally {
        try {
            Invoke-Quiet -Command @($Executable, "-L", $socket, "kill-server") -TimeoutSeconds 5
        } catch {
        }
    }
}

function Measure-RmuxOperation {
    param([string] $Operation)
    return Measure-NativeTmuxLikeOperation $Rmux $Operation
}

function Measure-PsmuxOperation {
    param([string] $Operation)
    if ($null -eq $Psmux) {
        throw "psmux not available"
    }
    $session = New-PsmuxSessionName $Operation
    try {
        if ($Operation -eq "list_commands") {
            return Measure-CommandMs -Command @($Psmux, "list-commands")
        }
        if ($Operation -eq "new_session_cold_sh") {
            Clear-PsmuxIdleServer
            return Measure-CommandMs -Command @($Psmux, "new-session", "-d", "-s", $session)
        }
        if ($Operation -eq "new_session_warm_sh") {
            Invoke-Quiet -Command @($Psmux, "start-server") -TimeoutSeconds 5
            return Measure-CommandMs -Command @($Psmux, "new-session", "-d", "-s", $session)
        }
        $command = $null
        if ($Operation -eq "capture_pane_5000_lines") {
            $command = Get-WindowsOutputCommand 5000
        }
        if ($Operation -eq "capture_pane_200x50_scrollback_10k") {
            $command = Get-WindowsOutputCommand 10000
        }
        $newSession = @($Psmux, "new-session", "-d", "-s", $session)
        if ($null -ne $command) {
            $newSession += $command
        }
        Invoke-Quiet -Command $newSession
        if (Test-OperationNeedsReadyPane $Operation) {
            Wait-PsmuxSessionReady $session
        }
        switch ($Operation) {
            "split_window_h_detached_sh" { return Measure-CommandMs -Command @($Psmux, "split-window", "-h", "-d", "-t", $session) }
            "split_window_v_detached_sh" { return Measure-CommandMs -Command @($Psmux, "split-window", "-v", "-d", "-t", $session) }
            "split_window_h_attached_sh" { return Measure-CommandMs -Command @($Psmux, "split-window", "-h", "-t", $session) }
            "split_window_v_attached_sh" { return Measure-CommandMs -Command @($Psmux, "split-window", "-v", "-t", $session) }
            "resize_pane_right_1" { return Measure-CommandMs -Command @($Psmux, "resize-pane", "-t", $session, "-R", "1") }
            "resize_pane_right_10" { return Measure-CommandMs -Command @($Psmux, "resize-pane", "-t", $session, "-R", "10") }
            "resize_pane_left_1" { return Measure-CommandMs -Command @($Psmux, "resize-pane", "-t", $session, "-L", "1") }
            "resize_pane_absolute_100x30" { return Measure-CommandMs -Command @($Psmux, "resize-pane", "-x", "100", "-y", "30", "-t", $session) }
            "resize_pane_absolute_200x50" { return Measure-CommandMs -Command @($Psmux, "resize-pane", "-x", "200", "-y", "50", "-t", $session) }
            "list_sessions_default" { return Measure-CommandMs -Command @($Psmux, "list-sessions") }
            "capture_pane_5000_lines" { return Measure-CommandMs -Command @($Psmux, "capture-pane", "-p", "-t", $session) }
            "capture_pane_80x24" { return Measure-CommandMs -Command @($Psmux, "capture-pane", "-p", "-t", $session) }
            "capture_pane_200x50_scrollback_10k" { return Measure-CommandMs -Command @($Psmux, "capture-pane", "-p", "-S", "-10000", "-t", $session) }
            "new_window_detached_sh" { return Measure-CommandMs -Command @($Psmux, "new-window", "-d", "-t", $session) }
            "send_keys_detached_round_trip" { return Measure-CommandMs -Command @($Psmux, "send-keys", "-t", "$($session):0.0", "printf rmux-bench", "Enter") }
            "display_message_default" { return Measure-CommandMs -Command @($Psmux, "display-message", "-p", "-t", $session, "#{session_name}") }
            "show_options_global" { return Measure-CommandMs -Command @($Psmux, "show-options", "-g") }
            "show_window_options" { return Measure-CommandMs -Command @($Psmux, "show-window-options", "-g") }
            "list_windows_20" {
                for ($i = 1; $i -lt 20; $i++) {
                    Invoke-Quiet -Command @($Psmux, "new-window", "-d", "-n", "w$i", "-t", $session)
                }
                return Measure-CommandMs -Command @($Psmux, "list-windows", "-t", $session)
            }
            "list_panes_80" {
                for ($i = 1; $i -lt 20; $i++) {
                    Invoke-Quiet -Command @($Psmux, "new-window", "-d", "-n", "w$i", "-t", $session)
                    Wait-PsmuxTarget "$($session):$i"
                }
                for ($window = 0; $window -lt 20; $window++) {
                    Wait-PsmuxTarget "$($session):$window"
                    for ($pane = 0; $pane -lt 3; $pane++) {
                        Invoke-Quiet -Command @($Psmux, "split-window", "-d", "-t", "$($session):$window")
                    }
                }
                return Measure-CommandMs -Command @($Psmux, "list-panes", "-a")
            }
            "rename_window" { return Measure-CommandMs -Command @($Psmux, "rename-window", "-t", "$($session):0", "renamed") }
            "select_window_next" {
                Invoke-Quiet -Command @($Psmux, "new-window", "-d", "-n", "next", "-t", $session)
                return Measure-CommandMs -Command @($Psmux, "select-window", "-t", "$($session):1")
            }
            "join_pane_detached" {
                Invoke-Quiet -Command @($Psmux, "new-window", "-d", "-n", "join", "-t", $session)
                return Measure-CommandMs -Command @($Psmux, "join-pane", "-d", "-s", "$($session):1.0", "-t", "$($session):0.0")
            }
            "source_file_minimal" {
                $sourceFile = New-TemporaryFile
                try {
                    Set-Content -LiteralPath $sourceFile.FullName -Value "set-option -g status on" -Encoding ASCII
                    return Measure-CommandMs -Command @($Psmux, "source-file", $sourceFile.FullName)
                } finally {
                    Remove-Item -LiteralPath $sourceFile.FullName -Force -ErrorAction SilentlyContinue
                }
            }
            "set_option_quiet" { return Measure-CommandMs -Command @($Psmux, "set-option", "-g", "status", "on") }
            "set_window_option_quiet" { return Measure-CommandMs -Command @($Psmux, "set-window-option", "-g", "automatic-rename", "off") }
            "kill_pane" {
                Invoke-Quiet -Command @($Psmux, "split-window", "-d", "-t", $session)
                return Measure-CommandMs -Command @($Psmux, "kill-pane", "-t", "$($session):0.1")
            }
            "kill_session" { return Measure-CommandMs -Command @($Psmux, "kill-session", "-t", $session) }
            "kill_server" {
                Clear-PsmuxIdleServer
                return Measure-CommandMs -Command @($Psmux, "kill-server")
            }
            default { throw "unsupported psmux operation $Operation" }
        }
    } finally {
        try {
            Invoke-Quiet -Command @($Psmux, "kill-session", "-t", $session) -TimeoutSeconds 5
        } catch {
        }
        if ($script:ClearPsmuxAfterEachOperation) {
            Clear-PsmuxIdleServer
        }
    }
}

function Measure-WslTmuxOperation {
    param([string] $Operation)
    $socket = New-WslSocketName $Operation
    try {
        if ($Operation -eq "list_commands") {
            return Measure-CommandMs -Command (New-WslTmuxCommand "list-commands")
        }
        if ($Operation -eq "new_session_cold_sh") {
            Clear-WslTmuxServer $socket
            return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "new-session" "-d" "-s" "bench")
        }
        if ($Operation -eq "new_session_warm_sh") {
            Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "start-server")
            return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "new-session" "-d" "-s" "bench")
        }
        $command = $null
        if ($Operation -eq "capture_pane_5000_lines") {
            $command = Get-WslOutputCommand 5000
        }
        if ($Operation -eq "capture_pane_200x50_scrollback_10k") {
            $command = Get-WslOutputCommand 10000
        }
        $newSession = New-WslTmuxCommand "-L" $socket "new-session" "-d" "-s" "bench"
        if ($null -ne $command) {
            $newSession += $command
        }
        Invoke-Quiet -Command $newSession
        if (Test-OperationNeedsReadyPane $Operation) {
            Wait-WslTmuxSessionReady $socket
        }
        switch ($Operation) {
            "split_window_h_detached_sh" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "split-window" "-h" "-d" "-t" "bench") }
            "split_window_v_detached_sh" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "split-window" "-v" "-d" "-t" "bench") }
            "split_window_h_attached_sh" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "split-window" "-h" "-t" "bench") }
            "split_window_v_attached_sh" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "split-window" "-v" "-t" "bench") }
            "resize_pane_right_1" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "resize-pane" "-t" "bench" "-R" "1") }
            "resize_pane_right_10" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "resize-pane" "-t" "bench" "-R" "10") }
            "resize_pane_left_1" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "resize-pane" "-t" "bench" "-L" "1") }
            "resize_pane_absolute_100x30" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "resize-pane" "-x" "100" "-y" "30" "-t" "bench") }
            "resize_pane_absolute_200x50" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "resize-pane" "-x" "200" "-y" "50" "-t" "bench") }
            "list_sessions_default" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "list-sessions") }
            "capture_pane_5000_lines" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "capture-pane" "-p" "-t" "bench") }
            "capture_pane_80x24" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "capture-pane" "-p" "-t" "bench") }
            "capture_pane_200x50_scrollback_10k" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "capture-pane" "-p" "-S" "-10000" "-t" "bench") }
            "new_window_detached_sh" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "new-window" "-d" "-t" "bench") }
            "send_keys_detached_round_trip" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "send-keys" "-t" "bench:0.0" "printf rmux-bench" "Enter") }
            "display_message_default" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "display-message" "-p" "-t" "bench" "#{session_name}") }
            "show_options_global" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "show-options" "-g") }
            "show_window_options" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "show-window-options" "-g") }
            "list_windows_20" {
                for ($i = 1; $i -lt 20; $i++) {
                    Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "new-window" "-d" "-n" "w$i" "-t" "bench")
                }
                return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "list-windows" "-t" "bench")
            }
            "list_panes_80" {
                for ($i = 1; $i -lt 20; $i++) {
                    Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "new-window" "-d" "-n" "w$i" "-t" "bench")
                    Wait-WslTmuxTarget $socket "bench:$i"
                }
                for ($window = 0; $window -lt 20; $window++) {
                    Wait-WslTmuxTarget $socket "bench:$window"
                    for ($pane = 0; $pane -lt 3; $pane++) {
                        Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "split-window" "-d" "-t" "bench:$window")
                    }
                }
                return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "list-panes" "-a")
            }
            "rename_window" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "rename-window" "-t" "bench:0" "renamed") }
            "select_window_next" {
                Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "new-window" "-d" "-n" "next" "-t" "bench")
                return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "select-window" "-t" "bench:1")
            }
            "join_pane_detached" {
                Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "new-window" "-d" "-n" "join" "-t" "bench")
                return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "join-pane" "-d" "-s" "bench:1.0" "-t" "bench:0.0")
            }
            "source_file_minimal" {
                $sourcePath = New-WslSourceFile
                try {
                    return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "source-file" $sourcePath)
                } finally {
                    Remove-WslPath $sourcePath
                }
            }
            "set_option_quiet" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "set-option" "-g" "status" "on") }
            "set_window_option_quiet" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "set-window-option" "-g" "automatic-rename" "off") }
            "kill_pane" {
                Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "split-window" "-d" "-t" "bench")
                return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "kill-pane" "-t" "bench:0.1")
            }
            "kill_session" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "kill-session" "-t" "bench") }
            "kill_server" { return Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "kill-server") }
            default { throw "unsupported operation $Operation" }
        }
    } finally {
        Clear-WslTmuxServer $socket
    }
}

function Measure-NativeTmuxLikeListWindows20Stats {
    param([string] $Executable)
    $socket = New-SocketName "list_windows_20"
    $samples = @()
    try {
        Invoke-Quiet -Command @($Executable, "-L", $socket, "new-session", "-d", "-s", "bench")
        Wait-NativeTmuxLikeTarget $Executable $socket "bench:0"
        for ($i = 1; $i -lt 20; $i++) {
            Invoke-Quiet -Command @($Executable, "-L", $socket, "new-window", "-d", "-n", "w$i", "-t", "bench")
            Wait-NativeTmuxLikeTarget $Executable $socket "bench:$i"
        }
        for ($i = 0; $i -lt $Iterations; $i++) {
            $samples += Measure-CommandMs -Command @($Executable, "-L", $socket, "list-windows", "-t", "bench") -TimeoutSeconds 30
        }
        return Get-Stats @($samples)
    } finally {
        try {
            Invoke-Quiet -Command @($Executable, "-L", $socket, "kill-server") -TimeoutSeconds 5
        } catch {
        }
    }
}

function Measure-WslTmuxListWindows20Stats {
    $socket = New-WslSocketName "list_windows_20"
    $samples = @()
    try {
        Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "new-session" "-d" "-s" "bench")
        Wait-WslTmuxTarget $socket "bench:0"
        for ($i = 1; $i -lt 20; $i++) {
            Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "new-window" "-d" "-n" "w$i" "-t" "bench")
            Wait-WslTmuxTarget $socket "bench:$i"
        }
        for ($i = 0; $i -lt $Iterations; $i++) {
            $samples += Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "list-windows" "-t" "bench") -TimeoutSeconds 30
        }
        return Get-Stats @($samples)
    } finally {
        Clear-WslTmuxServer $socket
    }
}

function Measure-PsmuxListWindows20Stats {
    if ($null -eq $Psmux) {
        throw "psmux not available"
    }
    $session = New-PsmuxSessionName "list_windows_20"
    $samples = @()
    try {
        Invoke-Quiet -Command @($Psmux, "new-session", "-d", "-s", $session)
        Wait-PsmuxTarget "$($session):0"
        for ($i = 1; $i -lt 20; $i++) {
            Invoke-Quiet -Command @($Psmux, "new-window", "-d", "-n", "w$i", "-t", $session)
            Wait-PsmuxTarget "$($session):$i"
        }
        for ($i = 0; $i -lt $Iterations; $i++) {
            $samples += Measure-CommandMs -Command @($Psmux, "list-windows", "-t", $session) -TimeoutSeconds 30
        }
        return Get-Stats @($samples)
    } finally {
        try {
            Invoke-Quiet -Command @($Psmux, "kill-session", "-t", $session) -TimeoutSeconds 5
        } catch {
        }
        Clear-PsmuxIdleServer
    }
}

function Measure-ZellijListWindows20Stats {
    $name = New-ZellijSessionName "list_windows_20"
    $samples = @()
    try {
        New-ZellijSession $name
        for ($i = 1; $i -lt 20; $i++) {
            Invoke-Zellij -ZellijArgs @("--session", $name, "action", "new-tab", "--name", "w$i") -TimeoutSeconds 10
        }
        for ($i = 0; $i -lt $Iterations; $i++) {
            $samples += Measure-ZellijCommandMs -ZellijArgs @("--session", $name, "action", "query-tab-names") -TimeoutSeconds 30
        }
        return Get-Stats @($samples)
    } finally {
        Clear-ZellijSession $name
    }
}

function Measure-NativeTmuxLikeListPanes80Stats {
    param([string] $Executable)
    $socket = New-SocketName "list_panes_80"
    $samples = @()
    try {
        Invoke-Quiet -Command @($Executable, "-L", $socket, "new-session", "-d", "-s", "bench")
        Wait-NativeTmuxLikeTarget $Executable $socket "bench:0"
        for ($i = 1; $i -lt 20; $i++) {
            Invoke-Quiet -Command @($Executable, "-L", $socket, "new-window", "-d", "-n", "w$i", "-t", "bench")
            Wait-NativeTmuxLikeTarget $Executable $socket "bench:$i"
        }
        for ($window = 0; $window -lt 20; $window++) {
            Wait-NativeTmuxLikeTarget $Executable $socket "bench:$window"
            for ($pane = 0; $pane -lt 3; $pane++) {
                Invoke-Quiet -Command @($Executable, "-L", $socket, "split-window", "-d", "-t", "bench:$window")
            }
        }
        for ($i = 0; $i -lt $Iterations; $i++) {
            $samples += Measure-CommandMs -Command @($Executable, "-L", $socket, "list-panes", "-a") -TimeoutSeconds 30
        }
        return Get-Stats @($samples)
    } finally {
        try {
            Invoke-Quiet -Command @($Executable, "-L", $socket, "kill-server") -TimeoutSeconds 5
        } catch {
        }
    }
}

function Measure-WslTmuxListPanes80Stats {
    $socket = New-WslSocketName "list_panes_80"
    $samples = @()
    try {
        Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "new-session" "-d" "-s" "bench")
        Wait-WslTmuxTarget $socket "bench:0"
        for ($i = 1; $i -lt 20; $i++) {
            Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "new-window" "-d" "-n" "w$i" "-t" "bench")
            Wait-WslTmuxTarget $socket "bench:$i"
        }
        for ($window = 0; $window -lt 20; $window++) {
            Wait-WslTmuxTarget $socket "bench:$window"
            for ($pane = 0; $pane -lt 3; $pane++) {
                Invoke-Quiet -Command (New-WslTmuxCommand "-L" $socket "split-window" "-d" "-t" "bench:$window")
            }
        }
        for ($i = 0; $i -lt $Iterations; $i++) {
            $samples += Measure-CommandMs -Command (New-WslTmuxCommand "-L" $socket "list-panes" "-a") -TimeoutSeconds 30
        }
        return Get-Stats @($samples)
    } finally {
        Clear-WslTmuxServer $socket
    }
}

function Measure-PsmuxListPanes80Stats {
    if ($null -eq $Psmux) {
        throw "psmux not available"
    }
    $session = New-PsmuxSessionName "list_panes_80"
    $samples = @()
    try {
        Invoke-Quiet -Command @($Psmux, "new-session", "-d", "-s", $session)
        Wait-PsmuxTarget "$($session):0"
        for ($i = 1; $i -lt 20; $i++) {
            Invoke-Quiet -Command @($Psmux, "new-window", "-d", "-n", "w$i", "-t", $session)
            Wait-PsmuxTarget "$($session):$i"
        }
        for ($window = 0; $window -lt 20; $window++) {
            Wait-PsmuxTarget "$($session):$window"
            for ($pane = 0; $pane -lt 3; $pane++) {
                Invoke-Quiet -Command @($Psmux, "split-window", "-d", "-t", "$($session):$window")
            }
        }
        for ($i = 0; $i -lt $Iterations; $i++) {
            $samples += Measure-CommandMs -Command @($Psmux, "list-panes", "-a") -TimeoutSeconds 30
        }
        return Get-Stats @($samples)
    } finally {
        try {
            Invoke-Quiet -Command @($Psmux, "kill-session", "-t", $session) -TimeoutSeconds 5
        } catch {
        }
        Clear-PsmuxIdleServer
    }
}

function Measure-ZellijListPanes80Stats {
    $name = New-ZellijSessionName "list_panes_80"
    $samples = @()
    try {
        New-ZellijSession $name
        for ($i = 0; $i -lt 79; $i++) {
            Invoke-Zellij -ZellijArgs @("--session", $name, "action", "new-pane") -TimeoutSeconds 10
        }
        for ($i = 0; $i -lt $Iterations; $i++) {
            $samples += Measure-ZellijCommandMs -ZellijArgs @("--session", $name, "action", "list-panes") -TimeoutSeconds 60
        }
        return Get-Stats @($samples)
    } finally {
        Clear-ZellijSession $name
    }
}

function Measure-ToolStats {
    param([scriptblock] $MeasureOperation, [string] $Operation)
    $samples = @()
    try {
        for ($i = 0; $i -lt $Iterations; $i++) {
            $samples += & $MeasureOperation $Operation
        }
        return Get-Stats @($samples)
    } catch {
        Write-Warning "Skipping $Operation for one tool: $($_.Exception.Message)"
        return $null
    }
}

$allOperations = @(
    "list_commands",
    "split_window_h_detached_sh",
    "split_window_v_detached_sh",
    "new_session_warm_sh",
    "list_sessions_default",
    "capture_pane_5000_lines",
    "new_window_detached_sh",
    "resize_pane_absolute_100x30",
    "resize_pane_right_1",
    "new_session_cold_sh",
    "split_window_h_attached_sh",
    "split_window_v_attached_sh",
    "resize_pane_left_1",
    "resize_pane_right_10",
    "resize_pane_absolute_200x50",
    "capture_pane_80x24",
    "capture_pane_200x50_scrollback_10k",
    "send_keys_detached_round_trip",
    "display_message_default",
    "show_options_global",
    "show_window_options",
    "list_windows_20",
    "list_panes_80",
    "rename_window",
    "select_window_next",
    "join_pane_detached",
    "source_file_minimal",
    "set_option_quiet",
    "set_window_option_quiet",
    "kill_pane",
    "kill_session",
    "kill_server"
)

$psmuxOperations = $allOperations

$zellijOperations = @(
    "split_window_h_detached_sh",
    "split_window_v_detached_sh",
    "list_sessions_default",
    "new_window_detached_sh",
    "new_session_cold_sh",
    "capture_pane_80x24",
    "send_keys_detached_round_trip",
    "list_windows_20",
    "list_panes_80",
    "kill_session"
)

if ($OnlyOperations.Count -gt 0) {
    $wanted = @{}
    foreach ($value in $OnlyOperations) {
        foreach ($operation in ($value -split ",")) {
            $operation = $operation.Trim()
            if (-not [string]::IsNullOrWhiteSpace($operation)) {
                $wanted[$operation] = $true
            }
        }
    }
    $operations = @($allOperations | Where-Object { $wanted.ContainsKey($_) })
    if ($operations.Count -eq 0) {
        throw "No benchmark operations matched -OnlyOperations"
    }
} else {
    $operations = $allOperations
}

$hasWslTmux = $MeasureTmux -and (Test-WslCommand "tmux")
$hasZellij = $MeasureZellij -and (Test-Zellij)
$hasPsmuxSessions = $MeasurePsmux -and (Test-PsmuxSession)

$metricsByOperation = @{}
foreach ($operation in $operations) {
    $metricsByOperation[$operation] = @{}
}

function Measure-ToolBlock {
    param(
        [string] $Tool,
        [scriptblock] $MeasureOperation,
        [string[]] $SupportedOperations = $operations,
        [scriptblock] $MeasureBatchedOperation = $null
    )
    foreach ($operation in $operations) {
        if ($SupportedOperations -notcontains $operation) {
            continue
        }
        Write-Host "bench: $Tool $operation"
        if ($operation -in @("list_windows_20", "list_panes_80") -and $null -ne $MeasureBatchedOperation) {
            try {
                $stats = & $MeasureBatchedOperation $operation
            } catch {
                Write-Warning "Skipping $operation for one tool: $($_.Exception.Message)"
                $stats = $null
            }
        } else {
            $stats = Measure-ToolStats $MeasureOperation $operation
        }
        if ($null -ne $stats) {
            $metricsByOperation[$operation][$Tool] = $stats
        }
    }
}

if ($MeasureRmux) {
    Measure-ToolBlock "rmux" ${function:Measure-RmuxOperation} $operations {
        param([string] $Operation)
        if ($Operation -eq "list_windows_20") { return Measure-NativeTmuxLikeListWindows20Stats $Rmux }
        if ($Operation -eq "list_panes_80") { return Measure-NativeTmuxLikeListPanes80Stats $Rmux }
        throw "unsupported batched operation $Operation"
    }
}
if ($hasWslTmux) {
    Measure-ToolBlock "tmux" ${function:Measure-WslTmuxOperation} $operations {
        param([string] $Operation)
        if ($Operation -eq "list_windows_20") { return Measure-WslTmuxListWindows20Stats }
        if ($Operation -eq "list_panes_80") { return Measure-WslTmuxListPanes80Stats }
        throw "unsupported batched operation $Operation"
    }
}
if ($hasZellij) {
    Measure-ToolBlock "zellij" ${function:Measure-ZellijOperation} $zellijOperations {
        param([string] $Operation)
        if ($Operation -eq "list_windows_20") { return Measure-ZellijListWindows20Stats }
        if ($Operation -eq "list_panes_80") { return Measure-ZellijListPanes80Stats }
        throw "unsupported batched operation $Operation"
    }
}
if ($hasPsmuxSessions) {
    Clear-PsmuxIdleServer
    $script:ClearPsmuxAfterEachOperation = $false
    try {
        Measure-ToolBlock "psmux" ${function:Measure-PsmuxOperation} $psmuxOperations {
            param([string] $Operation)
            if ($Operation -eq "list_windows_20") { return Measure-PsmuxListWindows20Stats }
            if ($Operation -eq "list_panes_80") { return Measure-PsmuxListPanes80Stats }
            throw "unsupported batched operation $Operation"
        }
    } finally {
        $script:ClearPsmuxAfterEachOperation = $true
        Clear-PsmuxIdleServer
    }
}

$rows = foreach ($operation in $operations) {
    $metrics = $metricsByOperation[$operation]
    if ($metrics.Count -gt 0) {
        @{
            id = $operation
            label = $operation
            metrics = $metrics
        }
    }
}

$tools = @()
if ($MeasureRmux) {
    $tools += "rmux"
}
if ($hasWslTmux) {
    $tools += "tmux"
}
if ($hasPsmuxSessions) {
    $tools += "psmux"
}
if ($hasZellij) {
    $tools += "zellij"
}
$baseline = if ($tools -contains "tmux") {
    "tmux"
} elseif ($tools.Count -gt 0) {
    $tools[0]
} else {
    $null
}

$payload = @{
    schema = 1
    kind = "rmux-public-benchmark"
    generated_at = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
    platform = @{ id = "windows"; name = "Windows" }
    git = @{
        branch = Get-GitValue "rev-parse" "--abbrev-ref" "HEAD"
        commit = Get-GitValue "rev-parse" "HEAD"
    }
    tools = $tools
    tool_versions = @{
        rmux = if ($MeasureRmux) { Get-Version $Rmux "-V" } else { $null }
        psmux = if ($hasPsmuxSessions) { Get-Version "cmd.exe" "/c" "psmux -V" } else { $null }
        zellij = if ($hasZellij) { Get-ZellijVersion } else { $null }
        tmux = if ($hasWslTmux) { Get-Version "wsl.exe" "tmux" "-V" } else { $null }
    }
    baseline = $baseline
    units = "ms"
    lower_is_better = $true
    rmux_layout = $RmuxLayout
    rmux_binaries = @{
        public = Convert-ToRepoRelativePath $RmuxPublic
        helper = Convert-ToRepoRelativePath $RmuxHelper
        daemon = Convert-ToRepoRelativePath $RmuxDaemon
    }
    notes = @(
        if ($MeasurePsmux -and $null -eq $Psmux) {
            "psmux was selected but was not found on PATH."
        }
        if ($MeasurePsmux -and $null -ne $Psmux -and -not $hasPsmuxSessions) {
            "psmux is installed but did not create benchmark sessions reproducibly on this host."
        }
    )
    operations = @($rows)
}

$payload | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $Out -Encoding UTF8
Write-Host $Out
