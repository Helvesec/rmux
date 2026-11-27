param([Parameter(Mandatory=$true)][string]$Verifier)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($Verifier, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'verifier parse failed' }
$helper = @($ast.FindAll({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -eq 'Stop-ProcessIfRunning'
}, $true))
$cleanup = @($ast.FindAll({ param($node)
    $node -is [Management.Automation.Language.TryStatementAst] -and
    $node.Extent.Text.Contains("throw 'owned cleanup timeout'") -and
    $node.Extent.Text.Contains('$killer.Dispose()') -and
    -not $node.Extent.Text.Contains('$process.Start()')
}, $true))
if ($helper.Count -ne 1 -or $cleanup.Count -ne 1) { throw 'ambiguous cleanup source' }
. ([scriptblock]::Create($helper[0].Extent.Text))
$body = [scriptblock]::Create($cleanup[0].Extent.Text)
$powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$records = @()
foreach ($case in @('already-exited','exits-after-wait','live','kill-error')) {
    $process = $null
    $birth = $null
    try {
        $process = Start-Process -FilePath $powershell -ArgumentList @('-NoProfile','-Command','Start-Sleep -Seconds 60') -PassThru
        [void]$process.Handle
        $birth = $process.StartTime
        $processId = $process.Id
        if ($case -in @('already-exited','exits-after-wait')) {
            $process.Kill()
            $process.WaitForExit()
        }
        if ($case -eq 'already-exited') {
            Stop-ProcessIfRunning $process
            if (-not $process.HasExited) { throw 'exited process changed state' }
        } else {
            # Model a timeout followed by process exit before the caller's Kill.
            # The exit and the subsequent Kill use a real, retained Windows handle.
            $process | Add-Member -MemberType ScriptMethod -Name WaitForExit -Force -Value {
                param($milliseconds)
                if ($null -ne $milliseconds) { return $false }
                $this.PSBase.WaitForExit()
            }
            if ($case -eq 'kill-error') {
                $process | Add-Member -MemberType ScriptMethod -Name Kill -Force -Value {
                    throw [ComponentModel.Win32Exception]::new(5, 'cleanup-access-denied-sentinel')
                }
            }
            $killer = $process
            $failure = $null
            try { & $body | Out-Null } catch { $failure = $_ }
            if ($null -eq $failure) { throw "missing cleanup error: $case" }
            if ($case -eq 'kill-error') {
                if ($failure.ToString() -notlike '*cleanup-access-denied-sentinel*') {
                    throw "real cleanup error lost: $failure"
                }
            } elseif ($failure.ToString() -cne 'owned cleanup timeout') {
                throw "cleanup timeout replaced by exit race: $failure"
            }
            # The production finalizer disposes the object; inspect only the
            # original birth, never target a newly recycled process identifier.
            $current = Get-Process -Id $processId -ErrorAction SilentlyContinue
            if ($current -and $current.StartTime -eq $birth -and $case -ne 'kill-error') {
                throw "owned cleanup process survived: $case"
            }
        }
        $records += [pscustomobject]@{case=$case; pid=$processId; birth_utc=$birth.ToUniversalTime().ToString('o'); status='PASS'}
    } catch {
        $records += [pscustomobject]@{case=$case; pid=$processId; status='FAIL'; error=$_.ToString()}
    } finally {
        if ($birth) {
            $current = Get-Process -Id $processId -ErrorAction SilentlyContinue
            if ($current) {
                try {
                    [void]$current.Handle
                    if ($current.StartTime -eq $birth -and -not $current.HasExited) {
                        try { $current.Kill() } catch [InvalidOperationException] {}
                        $current.WaitForExit()
                    }
                } finally { $current.Dispose() }
            }
        }
        if ($process) { $process.Dispose() }
    }
}
$records | ConvertTo-Json -Compress
if (@($records | Where-Object status -eq 'FAIL').Count) { throw 'native package cleanup regression' }
