param(
    [string]$Rmux = (Join-Path $PSScriptRoot "..\target\release\rmux.exe"),
    [string]$OutDir = (Join-Path $env:TEMP ("rmux-ctrl-matrix-" + (Get-Date -Format "yyyyMMdd-HHmmss"))),
    [string[]]$OnlyTerminal = @(),
    [string[]]$OnlyShell = @(),
    [string[]]$OnlyProgram = @(),
    [string[]]$OnlyKey = @(),
    [switch]$StaticMatrixSpec,
    [switch]$PortableSmokeOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($StaticMatrixSpec) {
    $scriptText = Get-Content -LiteralPath $PSCommandPath -Raw
    $requiredSnippets = @(
        "function Invoke-DirectCase",
        "function Invoke-AttachCase",
        "function Invoke-SendKeysCase",
        "Direct natif",
        "RMUX attach",
        "RMUX send-keys",
        "SendKeys-ControlTokens",
        "`"Ctrl-C`" { return @(`"C-c`", `"Enter`") }",
        "`"Ctrl-Z`" { return @(`"C-z`", `"Enter`") }",
        "Windows Terminal",
        "WezTerm",
        "Alacritty",
        "PortableSmokeOnly",
        "Ctrl-C",
        "Ctrl-D",
        "Ctrl-A",
        "Ctrl-Z",
        "Ctrl-H",
        "Esc",
        "python sleep",
        "python descendant sleep",
        "python stdin",
        "line idle",
        "wsl python sleep",
        "wsl python stdin",
        "powershell.exe",
        "wsl-bash",
        "git-bash",
        "timeout",
        "ping",
        "fzf",
        '$Direct.Returned -eq $Attach.Returned -and $Direct.Returned -eq $Send.Returned',
        '$Results.Count -eq 0',
        'Where-Object { $_.Verdict -eq "NO GO" }',
        "Windows Ctrl matrix found"
    )
    foreach ($snippet in $requiredSnippets) {
        if (-not $scriptText.Contains($snippet)) {
            throw "Windows Ctrl matrix static spec missing required snippet: $snippet"
        }
    }
    Write-Host "windows-ctrl-matrix-static-spec=ok"
    exit 0
}

if ($PortableSmokeOnly -and -not $env:RMUX_FORCE_WINDOWS_CTRL_MATRIX_GUI) {
    $currentSession = (Get-Process -Id $PID).SessionId
    if ($currentSession -eq 0) {
        Write-Host "windows-ctrl-matrix-portable-smoke=skipped reason=non-interactive-session-0"
        Write-Host "Set RMUX_FORCE_WINDOWS_CTRL_MATRIX_GUI=1 to force the GUI focus smoke from this session."
        exit 0
    }
}

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName Microsoft.VisualBasic
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class RmuxWin32Input {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    [StructLayout(LayoutKind.Sequential)]
    public struct INPUT { public int type; public InputUnion U; }
    [StructLayout(LayoutKind.Explicit)]
    public struct InputUnion {
        [FieldOffset(0)] public MOUSEINPUT mi;
        [FieldOffset(0)] public KEYBDINPUT ki;
        [FieldOffset(0)] public HARDWAREINPUT hi;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct MOUSEINPUT {
        public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct KEYBDINPUT {
        public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct HARDWAREINPUT { public uint uMsg; public ushort wParamL; public ushort wParamH; }
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int X, int Y);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern uint SendInput(uint nInputs, INPUT[] pInputs, int cbSize);
    public const int INPUT_MOUSE = 0;
    public const int INPUT_KEYBOARD = 1;
    public const uint MOUSEEVENTF_LEFTDOWN = 2;
    public const uint MOUSEEVENTF_LEFTUP = 4;
    public const uint KEYEVENTF_KEYUP = 2;
    public const uint KEYEVENTF_UNICODE = 4;
    public static void ClickCenter(IntPtr hWnd) {
        RECT r;
        if (!GetWindowRect(hWnd, out r)) return;
        SetCursorPos((r.Left + r.Right) / 2, (r.Top + r.Bottom) / 2);
        INPUT[] input = new INPUT[2];
        input[0].type = INPUT_MOUSE;
        input[0].U.mi.dwFlags = MOUSEEVENTF_LEFTDOWN;
        input[1].type = INPUT_MOUSE;
        input[1].U.mi.dwFlags = MOUSEEVENTF_LEFTUP;
        SendInput(2, input, Marshal.SizeOf(typeof(INPUT)));
    }
    public static void SendText(string text) {
        foreach (char ch in text) {
            INPUT[] input = new INPUT[2];
            input[0].type = INPUT_KEYBOARD;
            input[0].U.ki.wScan = ch;
            input[0].U.ki.dwFlags = KEYEVENTF_UNICODE;
            input[1].type = INPUT_KEYBOARD;
            input[1].U.ki.wScan = ch;
            input[1].U.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
            SendInput(2, input, Marshal.SizeOf(typeof(INPUT)));
        }
    }
    public static void SendVk(ushort vk) {
        INPUT[] input = new INPUT[2];
        input[0].type = INPUT_KEYBOARD;
        input[0].U.ki.wVk = vk;
        input[1].type = INPUT_KEYBOARD;
        input[1].U.ki.wVk = vk;
        input[1].U.ki.dwFlags = KEYEVENTF_KEYUP;
        SendInput(2, input, Marshal.SizeOf(typeof(INPUT)));
    }
    public static void SendChord(ushort modifier, ushort key) {
        INPUT[] input = new INPUT[4];
        input[0].type = INPUT_KEYBOARD;
        input[0].U.ki.wVk = modifier;
        input[1].type = INPUT_KEYBOARD;
        input[1].U.ki.wVk = key;
        input[2].type = INPUT_KEYBOARD;
        input[2].U.ki.wVk = key;
        input[2].U.ki.dwFlags = KEYEVENTF_KEYUP;
        input[3].type = INPUT_KEYBOARD;
        input[3].U.ki.wVk = modifier;
        input[3].U.ki.dwFlags = KEYEVENTF_KEYUP;
        SendInput(4, input, Marshal.SizeOf(typeof(INPUT)));
    }
}
"@

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Results = New-Object System.Collections.Generic.List[object]
$script:LastGuiProcessId = $null
$script:LastGuiKind = $null

function New-Directory([string]$Path) {
    New-Item -ItemType Directory -Force -Path $Path | Out-Null
}

function Escape-SendKeysText([string]$Text) {
    $builder = [System.Text.StringBuilder]::new()
    foreach ($ch in $Text.ToCharArray()) {
        switch ($ch) {
            "+" { [void]$builder.Append("{+}") }
            "^" { [void]$builder.Append("{^}") }
            "%" { [void]$builder.Append("{%}") }
            "~" { [void]$builder.Append("{~}") }
            "(" { [void]$builder.Append("{(}") }
            ")" { [void]$builder.Append("{)}") }
            "{" { [void]$builder.Append("{{}") }
            "}" { [void]$builder.Append("{}}") }
            "[" { [void]$builder.Append("{[}") }
            "]" { [void]$builder.Append("{]}") }
            default { [void]$builder.Append($ch) }
        }
    }
    $builder.ToString()
}

function Send-Line([string]$Text) {
    Send-TextOnly $Text
    Start-Sleep -Milliseconds 80
    [RmuxWin32Input]::SendVk(0x0D)
}

function Send-TextOnly([string]$Text) {
    $chunkSize = 24
    for ($offset = 0; $offset -lt $Text.Length; $offset += $chunkSize) {
        $length = [Math]::Min($chunkSize, $Text.Length - $offset)
        [RmuxWin32Input]::SendText($Text.Substring($offset, $length))
        Start-Sleep -Milliseconds 15
    }
    Start-Sleep -Milliseconds 80
}

function Send-ControlKey([string]$Key) {
    switch ($Key) {
        "Ctrl-C" { [RmuxWin32Input]::SendChord(0x11, 0x43) }
        "Ctrl-D" { [RmuxWin32Input]::SendChord(0x11, 0x44) }
        "Ctrl-A" { [RmuxWin32Input]::SendChord(0x11, 0x41) }
        "Ctrl-H" { [RmuxWin32Input]::SendChord(0x11, 0x48) }
        "Esc" { [RmuxWin32Input]::SendVk(0x1B) }
        "Ctrl-Z" {
            [RmuxWin32Input]::SendChord(0x11, 0x5A)
            Start-Sleep -Milliseconds 80
            [RmuxWin32Input]::SendVk(0x0D)
        }
        default { throw "unsupported control key $Key" }
    }
    Start-Sleep -Milliseconds 150
}

function SendKeys-ControlTokens([string]$Key) {
    switch ($Key) {
        "Ctrl-C" { return @("C-c", "Enter") }
        "Ctrl-D" { return @("C-d") }
        "Ctrl-A" { return @("C-a") }
        "Ctrl-H" { return @("C-h") }
        "Esc" { return @("Escape") }
        "Ctrl-Z" { return @("C-z", "Enter") }
        default { throw "unsupported control key $Key" }
    }
}

function Focus-Window([string]$Title) {
    for ($i = 0; $i -lt 80; $i++) {
        $matched = Get-Process -ErrorAction SilentlyContinue |
            Where-Object { $_.MainWindowTitle -like "*$Title*" } |
            Select-Object -First 1
        if ($matched) {
            [RmuxWin32Input]::SetForegroundWindow($matched.MainWindowHandle) | Out-Null
            [RmuxWin32Input]::ClickCenter($matched.MainWindowHandle)
            Start-Sleep -Milliseconds 250
            return
        }
        try {
            [Microsoft.VisualBasic.Interaction]::AppActivate($Title)
            Start-Sleep -Milliseconds 250
            return
        } catch {
        }
        Start-Sleep -Milliseconds 250
    }
    $candidate = Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowTitle -and $_.ProcessName -in @("WindowsTerminal", "wezterm-gui", "wezterm", "alacritty") } |
        Sort-Object StartTime -Descending |
        Select-Object -First 1
    if ($candidate) {
        try {
            [RmuxWin32Input]::SetForegroundWindow($candidate.MainWindowHandle) | Out-Null
            [RmuxWin32Input]::ClickCenter($candidate.MainWindowHandle)
            Start-Sleep -Milliseconds 250
            return
        } catch {
        }
    }
    throw "could not focus window titled '$Title'"
}

function Focus-ProcessWindow([System.Diagnostics.Process]$Process) {
    [RmuxWin32Input]::SetForegroundWindow($Process.MainWindowHandle) | Out-Null
    [RmuxWin32Input]::ClickCenter($Process.MainWindowHandle)
    Start-Sleep -Milliseconds 250
}

function Refocus-LastGuiWindow([string]$Title) {
    if ($script:LastGuiProcessId) {
        try {
            $process = Get-Process -Id $script:LastGuiProcessId -ErrorAction Stop
            if ($process.MainWindowHandle -ne 0) {
                Focus-ProcessWindow $process
                return
            }
        } catch {
        }
    }
    Focus-Window $Title
}

function Wait-TerminalWindow([string[]]$ProcessNames, [datetime]$StartedAt) {
    $notBefore = $StartedAt.AddSeconds(-2)
    for ($i = 0; $i -lt 80; $i++) {
        $candidate = Get-Process -ErrorAction SilentlyContinue |
            Where-Object {
                $ProcessNames -contains $_.ProcessName -and
                $_.MainWindowHandle -ne 0 -and
                $_.StartTime -ge $notBefore
            } |
            Sort-Object StartTime -Descending |
            Select-Object -First 1
        if ($candidate) {
            return $candidate
        }
        Start-Sleep -Milliseconds 250
    }
    throw "could not find terminal GUI process $($ProcessNames -join ', ')"
}

function Close-Window([string]$Title) {
    if ($script:LastGuiProcessId) {
        try {
            $process = Get-Process -Id $script:LastGuiProcessId -ErrorAction Stop
            if ($process.MainWindowHandle -ne 0) {
                Focus-ProcessWindow $process
                [RmuxWin32Input]::SendChord(0x12, 0x73)
                Start-Sleep -Milliseconds 800
            }
            if ($script:LastGuiKind -in @("wezterm", "alacritty")) {
                $stillRunning = Get-Process -Id $script:LastGuiProcessId -ErrorAction SilentlyContinue
                if ($stillRunning) {
                    Stop-Process -Id $script:LastGuiProcessId -Force -ErrorAction SilentlyContinue
                    Start-Sleep -Milliseconds 300
                }
            }
            return
        } catch {
        } finally {
            $script:LastGuiProcessId = $null
            $script:LastGuiKind = $null
        }
    }
    try {
        Focus-Window $Title
        [RmuxWin32Input]::SendChord(0x12, 0x73)
        Start-Sleep -Milliseconds 500
    } catch {
    }
}

function Command-Exists([string]$Name) {
    [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

function Find-FirstPath([string[]]$Candidates) {
    foreach ($candidate in $Candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate)) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    $null
}

function ConvertTo-ShortPath([string]$Path) {
    if (-not $Path) { return $Path }
    try {
        $escaped = $Path.Replace('"', '""')
        $short = & cmd.exe /D /C "for %I in (`"$escaped`") do @echo %~sI"
        if ($LASTEXITCODE -eq 0 -and $short) {
            return [string](@($short)[0])
        }
    } catch {
    }
    $Path
}

function Get-Terminals {
    $terminals = @()
    $wt = (Get-Command wt.exe -ErrorAction SilentlyContinue)
    if ($wt) {
        $terminals += [pscustomobject]@{ Name = "Windows Terminal"; Kind = "wt"; Exe = $wt.Source }
    }
    $wez = Find-FirstPath @(
        (Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Packages\wez.wezterm_Microsoft.Winget.Source_8wekyb3d8bbwe\wezterm.exe"),
        (Join-Path $env:ProgramFiles "WezTerm\wezterm.exe"),
        (Join-Path $env:TEMP "ctrl-matrix-tools\wezterm\WezTerm-windows-20240203-110809-5046fc22\wezterm.exe")
    )
    if ($wez) {
        $terminals += [pscustomobject]@{ Name = "WezTerm"; Kind = "wezterm"; Exe = $wez }
    }
    $alacritty = Find-FirstPath @(
        (Join-Path $env:ProgramFiles "Alacritty\alacritty.exe"),
        (Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Packages\Alacritty.Alacritty_Microsoft.Winget.Source_8wekyb3d8bbwe\alacritty.exe"),
        (Join-Path $env:TEMP "ctrl-matrix-tools\alacritty-portable.exe")
    )
    if ($alacritty) {
        $terminals += [pscustomobject]@{ Name = "Alacritty"; Kind = "alacritty"; Exe = $alacritty }
    }
    if ($OnlyTerminal.Count -gt 0) {
        $terminals = $terminals | Where-Object { $OnlyTerminal -contains $_.Name }
    }
    $terminals
}

function Shell-Spec([string]$Shell, [string]$Title, [string]$Mode, [string]$Label, [string]$Session) {
    $null = $Title, $Mode, $Label, $Session
    switch ($Shell) {
        "pwsh" {
            return @{ Exe = "pwsh.exe"; Args = @("-NoLogo", "-NoProfile", "-NoExit") }
        }
        "powershell.exe" {
            return @{ Exe = "powershell.exe"; Args = @("-NoLogo", "-NoProfile", "-NoExit") }
        }
        "cmd" {
            return @{ Exe = "cmd.exe"; Args = @("/D", "/K") }
        }
        "git-bash" {
            $bash = Find-FirstPath @(
                (Join-Path $env:ProgramFiles "Git\bin\bash.exe"),
                (Join-Path ${env:ProgramFiles(x86)} "Git\bin\bash.exe")
            )
            if (-not $bash) { return $null }
            $bash = ConvertTo-ShortPath $bash
            return @{ Exe = $bash; Args = @("--login", "-i") }
        }
        "wsl-bash" {
            return @{ Exe = "wsl.exe"; Args = @("--exec", "bash", "-li") }
        }
        default { throw "unknown shell $Shell" }
    }
}

function Start-Gui([object]$Terminal, [string]$Shell, [string]$Title, [string]$Mode, [string]$Label, [string]$Session) {
    $spec = Shell-Spec $Shell $Title $Mode $Label $Session
    if (-not $spec) { throw "shell $Shell is not available" }
    $script:LastGuiProcessId = $null
    $script:LastGuiKind = $null
    $startedAt = Get-Date
    switch ($Terminal.Kind) {
        "wt" {
            Start-Process -FilePath $Terminal.Exe -ArgumentList (@("-w", "new", "new-tab", "--title", $Title, "--suppressApplicationTitle", "--", $spec.Exe) + $spec.Args) | Out-Null
            Focus-Window $Title
        }
        "wezterm" {
            Start-Process -FilePath $Terminal.Exe -ArgumentList (@("start", "--always-new-process", "--workspace", $Title, "--class", $Title, "--cwd", $RepoRoot, "--") + @($spec.Exe) + $spec.Args) | Out-Null
            $process = Wait-TerminalWindow @("wezterm-gui") $startedAt
            $script:LastGuiProcessId = $process.Id
            $script:LastGuiKind = "wezterm"
            Focus-ProcessWindow $process
        }
        "alacritty" {
            Start-Process -FilePath $Terminal.Exe -ArgumentList (@("--title", $Title, "--working-directory", $RepoRoot, "-e", $spec.Exe) + $spec.Args) | Out-Null
            $process = Wait-TerminalWindow @("alacritty") $startedAt
            $script:LastGuiProcessId = $process.Id
            $script:LastGuiKind = "alacritty"
            Focus-ProcessWindow $process
        }
        default { throw "unknown terminal kind $($Terminal.Kind)" }
    }
    Start-Sleep -Milliseconds 1100
    $cwd = Initial-DirectoryCommand $Shell
    if ($cwd) {
        Send-Line $cwd
        Start-Sleep -Milliseconds 550
    } elseif ($Shell -eq "git-bash") {
        Start-Sleep -Milliseconds 5000
    }
    Refocus-LastGuiWindow $Title
    if ($Mode -eq "attach") {
        Send-Line (Attach-Command $Shell $Label $Session)
        Start-Sleep -Milliseconds 1600
    } elseif ($Shell -eq "cmd") {
        Start-Sleep -Milliseconds 1000
    }
}

function Initial-DirectoryCommand([string]$Shell) {
    $escapedRoot = $RepoRoot.Replace("'", "''")
    switch ($Shell) {
        "cmd" { return "cd /d `"$RepoRoot`"" }
        "pwsh" { return "Set-Location -LiteralPath '$escapedRoot'" }
        "powershell.exe" { return "Set-Location -LiteralPath '$escapedRoot'" }
        default { return $null }
    }
}

function Attach-Command([string]$Shell, [string]$Label, [string]$Session) {
    switch ($Shell) {
        "cmd" { return "`"$Rmux`" -L $Label attach-session -t $Session" }
        "pwsh" { return "& '$Rmux' -L '$Label' attach-session -t '$Session'" }
        "powershell.exe" { return "& '$Rmux' -L '$Label' attach-session -t '$Session'" }
        "wsl-bash" { return "'$(ConvertTo-WslPath $Rmux)' -L '$Label' attach-session -t '$Session'" }
        default { return "'$Rmux' -L '$Label' attach-session -t '$Session'" }
    }
}

function Marker-Command([string]$Shell, [string]$Path, [string]$Value) {
    $escaped = $Path.Replace("'", "''")
    switch ($Shell) {
        "cmd" { return "echo $Value> `"$Path`"" }
        "git-bash" { return "printf '$Value' > '$(ConvertTo-MsysPath $Path)'" }
        "wsl-bash" { return "printf '$Value' > '$(ConvertTo-WslPath $Path)'" }
        default { return "Set-Content -LiteralPath '$escaped' -Value '$Value'" }
    }
}

function Shell-Sequence([string]$Shell, [string[]]$Commands) {
    switch ($Shell) {
        "cmd" { return ($Commands -join " & ") }
        default { return ($Commands -join "; ") }
    }
}

function ConvertTo-WslPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $drive = $full.Substring(0, 1).ToLowerInvariant()
    $tail = $full.Substring(2).Replace("\", "/")
    "/mnt/$drive$tail"
}

function ConvertTo-MsysPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $drive = $full.Substring(0, 1).ToLowerInvariant()
    $tail = $full.Substring(2).Replace("\", "/")
    "/$drive$tail"
}

function Python-Literal([string]$Value) {
    "'" + $Value.Replace("\", "\\").Replace("'", "\\'") + "'"
}

function Write-PythonCaseScript([string]$Path, [string]$MarkerPath, [string]$Body) {
    $markerLiteral = Python-Literal $MarkerPath
    $content = @(
        "import pathlib"
        "pathlib.Path($markerLiteral).write_text('START')"
        $Body
    ) -join "`n"
    Set-Content -LiteralPath $Path -Value $content -Encoding UTF8
}

function Test-CaseCommand([string]$Shell, [string]$Program, [string]$StartMarker) {
    $start = Marker-Command $Shell $StartMarker "START"
    $caseDir = Split-Path -Parent $StartMarker
    switch ($Program) {
        "python sleep" {
            if ($Shell -eq "wsl-bash") {
                $script = Join-Path $caseDir "wsl_sleep.py"
                $wslScript = ConvertTo-WslPath $script
                Write-PythonCaseScript $script (ConvertTo-WslPath $StartMarker) "import time`ntime.sleep(10**6)"
                return "python3 $wslScript"
            }
            $script = Join-Path $caseDir "sleep.py"
            Write-PythonCaseScript $script $StartMarker "import time`ntime.sleep(10**6)"
            return "python `"$script`""
        }
        "python descendant sleep" {
            $script = Join-Path $caseDir "descendant_sleep.py"
            Write-PythonCaseScript $script $StartMarker "import subprocess, sys`nsubprocess.call([sys.executable, '-c', 'import time; time.sleep(10**6)'])"
            return "python `"$script`""
        }
        "python stdin" {
            if ($Shell -eq "wsl-bash") {
                $script = Join-Path $caseDir "wsl_stdin.py"
                $wslScript = ConvertTo-WslPath $script
                Write-PythonCaseScript $script (ConvertTo-WslPath $StartMarker) "import sys`nsys.stdin.read()"
                return "python3 $wslScript"
            }
            $script = Join-Path $caseDir "stdin.py"
            Write-PythonCaseScript $script $StartMarker "import sys`nsys.stdin.read()"
            return "python `"$script`""
        }
        "timeout" {
            $command = if ($Shell -eq "cmd") { "timeout /T 10000" } else { "timeout.exe /T 10000" }
            return Shell-Sequence $Shell @($start, $command)
        }
        "ping" {
            return Shell-Sequence $Shell @($start, "ping -t 127.0.0.1")
        }
        "fzf" {
            return Shell-Sequence $Shell @($start, "@('one','two','three') | fzf.exe")
        }
        "line idle" {
            return $start
        }
        "wsl python sleep" {
            $wslMarker = ConvertTo-WslPath $StartMarker
            $script = Join-Path $caseDir "wsl_sleep.py"
            $wslScript = ConvertTo-WslPath $script
            Write-PythonCaseScript $script $wslMarker "import time`ntime.sleep(10**6)"
            return "wsl.exe --exec python3 $wslScript"
        }
        "wsl python stdin" {
            $wslMarker = ConvertTo-WslPath $StartMarker
            $script = Join-Path $caseDir "wsl_stdin.py"
            $wslScript = ConvertTo-WslPath $script
            Write-PythonCaseScript $script $wslMarker "import sys`nsys.stdin.read()"
            return "wsl.exe --exec python3 $wslScript"
        }
        default { throw "unknown program $Program" }
    }
}

function Wait-Marker([string]$Path, [int]$TimeoutMs) {
    $deadline = (Get-Date).AddMilliseconds($TimeoutMs)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path -LiteralPath $Path) { return $true }
        Start-Sleep -Milliseconds 100
    }
    $false
}

function Post-StartDelayMs([string]$Program) {
    if ($Program.StartsWith("wsl python")) {
        return 1200
    }
    150
}

function Line-ProbeText([string]$Key) {
    "rmux_line_probe_$($Key.Replace('-', '').ToLowerInvariant())"
}

function Send-LineProbeIfNeeded([string]$Program, [string]$Key) {
    if ($Program -eq "line idle") {
        switch ($Key) {
            "Ctrl-H" { Send-TextOnly "x" }
            "Esc" { }
            default { Send-TextOnly (Line-ProbeText $Key) }
        }
    }
}

function Kill-Rmux([string]$Label) {
    if (Test-Path -LiteralPath $Rmux) {
        $oldPreference = $ErrorActionPreference
        $ErrorActionPreference = "SilentlyContinue"
        try {
            & $Rmux -L $Label kill-server *> $null
        } catch {
        } finally {
            $ErrorActionPreference = $oldPreference
            $global:LASTEXITCODE = 0
        }
    }
}

function New-Session([string]$Label, [string]$Session, [string]$Shell) {
    Kill-Rmux $Label
    switch ($Shell) {
        "pwsh" { & $Rmux -L $Label new-session -d -s $Session "pwsh.exe -NoLogo -NoProfile -NoExit" }
        "powershell.exe" { & $Rmux -L $Label new-session -d -s $Session "powershell.exe -NoLogo -NoProfile -NoExit" }
        "cmd" { & $Rmux -L $Label new-session -d -s $Session "cmd.exe /D /K" }
        "git-bash" {
            $bash = (Shell-Spec "git-bash" "rmux" "direct" "" "").Exe
            & $Rmux -L $Label new-session -d -s $Session "$bash --login -i"
        }
        "wsl-bash" { & $Rmux -L $Label new-session -d -s $Session "wsl.exe --exec bash -li" }
        default { throw "unknown shell $Shell" }
    }
    & $Rmux -L $Label set-option -g status off *> $null
    Start-Sleep -Milliseconds 2000
}

function Invoke-DirectCase([object]$Terminal, [string]$Shell, [string]$Program, [string]$Key, [string]$CaseDir) {
    $title = "rmux-direct-$([guid]::NewGuid().ToString('N').Substring(0,8))"
    $startMarker = Join-Path $CaseDir "direct.start"
    $marker = Join-Path $CaseDir "direct.marker"
    try {
        Start-Gui $Terminal $Shell $title "direct" "" ""
        Send-Line (Test-CaseCommand $Shell $Program $startMarker)
        if (-not (Wait-Marker $startMarker 10000)) {
            return @{ Returned = $false; Detail = "setup/no start marker"; Setup = $false }
        }
        Start-Sleep -Milliseconds (Post-StartDelayMs $Program)
        Send-LineProbeIfNeeded $Program $Key
        Send-ControlKey $Key
        Start-Sleep -Milliseconds 800
        Send-Line (Marker-Command $Shell $marker "AFTER")
        $returned = Wait-Marker $marker 2500
        return @{ Returned = $returned; Detail = $(if ($returned) { "prompt/marker returned" } else { "blocked/no marker" }); Setup = $true }
    } finally {
        Close-Window $title
    }
}

function Invoke-AttachCase([object]$Terminal, [string]$Shell, [string]$Program, [string]$Key, [string]$CaseDir, [string]$Label) {
    $session = "s"
    $title = "rmux-attach-$([guid]::NewGuid().ToString('N').Substring(0,8))"
    $startMarker = Join-Path $CaseDir "attach.start"
    $marker = Join-Path $CaseDir "attach.marker"
    try {
        New-Session $Label $session $Shell
        Start-Gui $Terminal $Shell $title "attach" $Label $session
        Send-Line (Test-CaseCommand $Shell $Program $startMarker)
        if (-not (Wait-Marker $startMarker 10000)) {
            return @{ Returned = $false; Detail = "setup/no start marker"; Setup = $false }
        }
        Start-Sleep -Milliseconds (Post-StartDelayMs $Program)
        Send-LineProbeIfNeeded $Program $Key
        Send-ControlKey $Key
        Start-Sleep -Milliseconds 800
        Send-Line (Marker-Command $Shell $marker "AFTER")
        $returned = Wait-Marker $marker 2500
        return @{ Returned = $returned; Detail = $(if ($returned) { "prompt/marker returned" } else { "blocked/no marker" }); Setup = $true }
    } finally {
        Close-Window $title
        Kill-Rmux $Label
    }
}

function Invoke-SendKeysCase([string]$Shell, [string]$Program, [string]$Key, [string]$CaseDir, [string]$Label) {
    $session = "s"
    $target = "${session}:0.0"
    $startMarker = Join-Path $CaseDir "send.start"
    $marker = Join-Path $CaseDir "send.marker"
    try {
        New-Session $Label $session $Shell
        $programCommand = Test-CaseCommand $Shell $Program $startMarker
        & $Rmux -L $Label send-keys -t $target -- $programCommand Enter *> $null
        if (-not (Wait-Marker $startMarker 10000)) {
            return @{ Returned = $false; Detail = "setup/no start marker"; Setup = $false }
        }
        Start-Sleep -Milliseconds (Post-StartDelayMs $Program)
        if ($Program -eq "line idle") {
            & $Rmux -L $Label send-keys -t $target -- (Line-ProbeText $Key) *> $null
            Start-Sleep -Milliseconds 150
        }
        $tokens = SendKeys-ControlTokens $Key
        & $Rmux -L $Label send-keys -t $target -- $tokens *> $null
        Start-Sleep -Milliseconds 800
        $markerCommand = Marker-Command $Shell $marker "AFTER"
        & $Rmux -L $Label send-keys -t $target -- $markerCommand Enter *> $null
        $returned = Wait-Marker $marker 2500
        return @{ Returned = $returned; Detail = $(if ($returned) { "prompt/marker returned" } else { "blocked/no marker" }); Setup = $true }
    } finally {
        Kill-Rmux $Label
    }
}

function Add-Result($Terminal, $Shell, $Program, $Key, $Direct, $Attach, $Send) {
    $setupOk = ($Direct.Setup -ne $false) -and ($Attach.Setup -ne $false) -and ($Send.Setup -ne $false)
    $verdict = if ($setupOk -and $Direct.Returned -eq $Attach.Returned -and $Direct.Returned -eq $Send.Returned) { "GREEN" } else { "NO GO" }
    $Results.Add([pscustomobject]@{
        Terminal = $Terminal
        Shell = $Shell
        Programme = $Program
        Touche = $Key
        "Direct natif" = $Direct.Detail
        "RMUX attach" = $Attach.Detail
        "RMUX send-keys" = $Send.Detail
        Verdict = $verdict
    })
}

function New-FailedOutcome([string]$Stage, [object]$ErrorRecord) {
    $message = if ($ErrorRecord -and $ErrorRecord.Exception) { $ErrorRecord.Exception.Message } else { [string]$ErrorRecord }
    if ($message.Length -gt 80) {
        $message = $message.Substring(0, 80)
    }
    @{ Returned = $false; Detail = "setup/$Stage error: $message"; Setup = $false }
}

New-Directory $OutDir
if (-not (Test-Path -LiteralPath $Rmux)) {
    throw "rmux binary not found: $Rmux"
}

$terminalSpecs = @(Get-Terminals)
$portableSmokeCases = @(
    @{ Shells = @("pwsh"); Program = "python sleep"; Key = "Ctrl-C" },
    @{ Shells = @("pwsh"); Program = "python stdin"; Key = "Ctrl-D" },
    @{ Shells = @("pwsh"); Program = "python stdin"; Key = "Ctrl-Z" },
    @{ Shells = @("pwsh"); Program = "line idle"; Key = "Ctrl-A" },
    @{ Shells = @("pwsh"); Program = "line idle"; Key = "Esc" }
)
$caseIndex = 0
$mainCases = @(
    @{ Shells = @("pwsh", "cmd", "powershell.exe"); Program = "python sleep"; Key = "Ctrl-C" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe"); Program = "python descendant sleep"; Key = "Ctrl-C" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe"); Program = "python stdin"; Key = "Ctrl-D" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe"); Program = "python stdin"; Key = "Ctrl-Z" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe"); Program = "timeout"; Key = "Ctrl-C" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe"); Program = "timeout"; Key = "Ctrl-D" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe"); Program = "ping"; Key = "Ctrl-C" },
    @{ Shells = @("pwsh"); Program = "fzf"; Key = "Ctrl-C" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe", "git-bash"); Program = "line idle"; Key = "Ctrl-A" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe", "git-bash"); Program = "line idle"; Key = "Ctrl-H" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe", "git-bash"); Program = "line idle"; Key = "Esc" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe", "git-bash"); Program = "line idle"; Key = "Ctrl-C" },
    @{ Shells = @("pwsh", "cmd", "powershell.exe", "git-bash"); Program = "line idle"; Key = "Ctrl-D" },
    @{ Shells = @("pwsh"); Program = "wsl python sleep"; Key = "Ctrl-C" },
    @{ Shells = @("pwsh"); Program = "wsl python stdin"; Key = "Ctrl-D" },
    @{ Shells = @("wsl-bash"); Program = "python sleep"; Key = "Ctrl-C" },
    @{ Shells = @("wsl-bash"); Program = "python stdin"; Key = "Ctrl-D" }
)
if ($PortableSmokeOnly) {
    $windowsTerminal = @($terminalSpecs | Where-Object { $_.Name -eq "Windows Terminal" })
    if ($windowsTerminal.Count -gt 0) {
        $terminalSpecs = $windowsTerminal
    } elseif ($terminalSpecs.Count -gt 0) {
        $terminalSpecs = @($terminalSpecs[0])
    }
    $mainCases = $portableSmokeCases
}

foreach ($terminal in $terminalSpecs) {
    $shells = if ($terminal.Name -eq "Windows Terminal") { @("pwsh", "cmd", "powershell.exe", "git-bash", "wsl-bash") } else { @("pwsh", "cmd", "git-bash") }
    if ($PortableSmokeOnly) { $shells = @("pwsh") }
    foreach ($case in $mainCases) {
        foreach ($shell in $case.Shells) {
            if ($shells -notcontains $shell) { continue }
            if ($case.Program.StartsWith("wsl ") -and $terminal.Name -ne "Windows Terminal") { continue }
            if ($OnlyShell.Count -gt 0 -and $OnlyShell -notcontains $shell) { continue }
            if ($OnlyProgram.Count -gt 0 -and $OnlyProgram -notcontains $case.Program) { continue }
            if ($OnlyKey.Count -gt 0 -and $OnlyKey -notcontains $case.Key) { continue }
            if ($shell -eq "pwsh" -and -not (Command-Exists "pwsh.exe")) { continue }
            if ($shell -eq "powershell.exe" -and -not (Command-Exists "powershell.exe")) { continue }
            if ($shell -eq "wsl-bash" -and -not (Command-Exists "wsl.exe")) { continue }
            if ($shell -eq "git-bash") {
                try {
                    [void](Shell-Spec "git-bash" "probe" "direct" "" "")
                } catch {
                    continue
                }
            }
            if (-not (Command-Exists "python.exe") -and $case.Program.StartsWith("python")) { continue }
            $caseIndex++
            $caseName = "c{0:D3}" -f $caseIndex
            $caseDir = Join-Path $OutDir $caseName
            New-Directory $caseDir
            $label = "cm-$([guid]::NewGuid().ToString('N').Substring(0,12))"
            try {
                $direct = Invoke-DirectCase $terminal $shell $case.Program $case.Key $caseDir
            } catch {
                $direct = New-FailedOutcome "direct" $_
            }
            try {
                $attach = Invoke-AttachCase $terminal $shell $case.Program $case.Key $caseDir $label
            } catch {
                $attach = New-FailedOutcome "attach" $_
            }
            try {
                $send = Invoke-SendKeysCase $shell $case.Program $case.Key $caseDir $label
            } catch {
                $send = New-FailedOutcome "send-keys" $_
            }
            Add-Result $terminal.Name $shell $case.Program $case.Key $direct $attach $send
        }
    }
}

$csv = Join-Path $OutDir "results.csv"
$md = Join-Path $OutDir "results.md"
$Results | Export-Csv -NoTypeInformation -Path $csv

$lines = @()
$lines += "| Terminal | Shell | Programme | Touche | Direct natif | RMUX attach | RMUX send-keys | Verdict |"
$lines += "|---|---|---|---|---|---|---|---|"
foreach ($row in $Results) {
    $lines += "| $($row.Terminal) | $($row.Shell) | $($row.Programme) | $($row.Touche) | $($row.'Direct natif') | $($row.'RMUX attach') | $($row.'RMUX send-keys') | $($row.Verdict) |"
}
$lines | Set-Content -LiteralPath $md -Encoding UTF8

Write-Host "Results: $md"
$lines | ForEach-Object { Write-Host $_ }

if ($Results.Count -eq 0) {
    throw "Windows Ctrl matrix produced no cases"
}

$failed = @($Results | Where-Object { $_.Verdict -eq "NO GO" })
if ($failed.Count -gt 0) {
    throw "Windows Ctrl matrix found $($failed.Count) NO GO case(s); see $md"
}
