param(
    [string]$Installer = (Join-Path $PSScriptRoot '../scripts/install-windows.ps1'),
    [Parameter(Mandatory = $true)][string]$OutputDir,
    [Parameter(Mandatory = $true)][ValidatePattern('^[a-f0-9]{40}$')][string]$InstallerCommit,
    [Parameter(Mandatory = $true)][ValidatePattern('^[a-f0-9]{64}$')][string]$InstallerSha256
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'Windows file-sharing semantics required; this test cannot provide Unix evidence'
}
$Installer = (Resolve-Path -LiteralPath $Installer).Path
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $Installer).Hash.ToLowerInvariant() -cne $InstallerSha256) {
    throw 'installer identity mismatch'
}
$root = [IO.Path]::GetFullPath($OutputDir)
if (Test-Path -LiteralPath $root) { throw "test output must be fresh: $root" }
New-Item -ItemType Directory -Path $root | Out-Null

# Load the actual installer functions without its download/install entry point.
# Only the verification callback is replaced; preflights, copies and rollback run unchanged.
$tokens = $null; $parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($Installer, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw "installer parse failed: $parseErrors" }
$functions = @($ast.EndBlock.Statements | Where-Object {
    $_ -is [Management.Automation.Language.FunctionDefinitionAst]
})
foreach ($name in @('Sha256File', 'Assert-FileReplaceable', 'Assert-PlannedPackageFile',
        'Invoke-InstallCheckpoint', 'Install-PackageFileSet', 'Verify-InstalledLayout')) {
    if (@($functions | Where-Object { $_.Name -ceq $name }).Count -ne 1) {
        throw "expected one production function: $name"
    }
}
foreach ($function in $functions) { . ([scriptblock]::Create($function.Extent.Text)) }

$script:checks = @()
$script:cases = @()
function Check([bool]$Condition, [string]$Name) {
    $script:checks += [pscustomobject]@{ name = "$script:case/$Name"; passed = $Condition }
}
function Check-PreviousFiles {
    Check ((Sha256File $script:unchanged) -ceq $script:beforeUnchanged) 'unchanged bytes preserved'
    Check ((Sha256File $script:changed) -ceq $script:beforeChanged) 'modified file restored'
    Check (-not (Test-Path -LiteralPath $script:created)) 'new file absent'
}
function Verify-InstalledLayout([string]$Binary) {
    $script:verificationCalls++
    Check ($Binary -ceq $script:unchanged) 'verification target'
    foreach ($entry in $script:plan) {
        Check ((Sha256File $entry.Source) -ceq (Sha256File $entry.Destination)) 'copy completed before verification'
    }
    Check ((Sha256File $script:changed) -cne $script:beforeChanged) 'changed file actually replaced'
    Check (Test-Path -LiteralPath $script:created -PathType Leaf) 'new file actually installed'
    # This callback is reached only after both production preflights and all copies.
    $script:lateHandle = [IO.File]::Open($script:unchanged, [IO.FileMode]::Open,
        [IO.FileAccess]::Read, [IO.FileShare]::Read)
    Check ((Sha256File $script:unchanged) -ceq $script:beforeUnchanged) 'late lock allows SHA reading'
    $writeHandle = $null
    $writeDenied = $false
    try {
        $writeHandle = [IO.File]::Open($script:unchanged, [IO.FileMode]::Open,
            [IO.FileAccess]::Write, [IO.FileShare]::ReadWrite)
    } catch [IO.IOException] { $writeDenied = $true }
    finally { if ($null -ne $writeHandle) { $writeHandle.Dispose() } }
    Check $writeDenied 'late lock denies writing'
    throw 'unchanged-lock-verification-sentinel'
}

$savedEnvironment = @{}
foreach ($name in @('TEMP', 'TMP', 'RMUX_INSTALL_TEST_FAIL_AT')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
try {
    foreach ($script:case in @('preflight-directory', 'initial-lock', 'late-checkpoint', 'late-unchanged-lock')) {
        $caseRoot = Join-Path $root $script:case
        $source = Join-Path $caseRoot 'source'
        $destination = Join-Path $caseRoot 'installed'
        $temporary = Join-Path $caseRoot 'temporary'
        New-Item -ItemType Directory -Path $source, $destination, $temporary | Out-Null
        $env:TEMP = $temporary; $env:TMP = $temporary
        $env:RMUX_INSTALL_TEST_FAIL_AT = ''
        Check ([IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') -ceq
            [IO.Path]::GetFullPath($temporary).TrimEnd('\')) 'private transaction temporary directory'
        $script:unchanged = Join-Path $destination 'unchanged.exe'
        $script:changed = Join-Path $destination 'changed.dll'
        $script:created = Join-Path $destination 'new.txt'
        [IO.File]::WriteAllText($script:unchanged, 'identical installed bytes')
        [IO.File]::WriteAllText($script:changed, 'previous installed bytes')
        [IO.File]::WriteAllText((Join-Path $source 'unchanged.exe'), 'identical installed bytes')
        [IO.File]::WriteAllText((Join-Path $source 'changed.dll'), 'replacement installed bytes')
        [IO.File]::WriteAllText((Join-Path $source 'new.txt'), 'new installed bytes')
        $script:beforeUnchanged = Sha256File $script:unchanged
        $script:beforeChanged = Sha256File $script:changed
        $script:plan = @('unchanged.exe', 'changed.dll', 'new.txt') | ForEach-Object {
            [pscustomobject]@{ Source = (Join-Path $source $_); Destination = (Join-Path $destination $_) }
        }
        $script:verificationCalls = 0
        $script:lateHandle = $null
        $initialHandle = $null
        $failure = ''
        try {
            if ($script:case -ceq 'preflight-directory') {
                # Put the invalid destination last to detect mutation before full preflight.
                New-Item -ItemType Directory -Path $script:created | Out-Null
            } elseif ($script:case -ceq 'initial-lock') {
                $initialHandle = [IO.File]::Open($script:unchanged, [IO.FileMode]::Open,
                    [IO.FileAccess]::Read, [IO.FileShare]::Read)
            } elseif ($script:case -ceq 'late-checkpoint') {
                $env:RMUX_INSTALL_TEST_FAIL_AT = 'after-copy-package'
            }
            try {
                Install-PackageFileSet -Plan $script:plan -VerifyBinary $script:unchanged -Verify $true
            } catch { $failure = $_.Exception.Message }
            Check (-not [string]::IsNullOrEmpty($failure)) 'failure reported'
            if ($script:case -ceq 'late-unchanged-lock') {
                Check ($script:verificationCalls -eq 1) 'verification callback reached once'
                Check ($null -ne $script:lateHandle -and -not $script:lateHandle.SafeFileHandle.IsClosed) 'late handle still live after rollback'
                Check ($failure -like '*unchanged-lock-verification-sentinel*') 'verification failure retained'
                Check ($failure -like '*previous package restored*') 'rollback reports restoration'
                Check ($failure -notlike '*rollback also failed*') 'no rollback failure'
                Check ($failure -notlike '*recovery backup preserved*') 'no recovery backup advertised'
            } elseif ($script:case -ceq 'late-checkpoint') {
                Check ($script:verificationCalls -eq 0) 'checkpoint precedes verification'
                Check ($failure -like '*injected installer failure at after-copy-package*') 'late checkpoint failure retained'
                Check ($failure -like '*previous package restored*') 'late checkpoint restored'
                Check ($failure -notlike '*rollback also failed*') 'late checkpoint rollback succeeds'
            } else {
                Check ($script:verificationCalls -eq 0) 'preflight precedes verification'
                $pattern = if ($script:case -ceq 'initial-lock') {
                    '*destination file is in use or cannot be replaced safely*'
                } else { '*destination file path exists but is not a file*' }
                Check ($failure -like $pattern) 'preflight failure retained'
                Check ($failure -notlike '*previous package restored*') 'preflight avoids rollback'
            }
            if ($script:case -ceq 'preflight-directory') {
                Check (Test-Path -LiteralPath $script:created -PathType Container) 'invalid destination untouched'
                Check ((Sha256File $script:unchanged) -ceq $script:beforeUnchanged) 'preflight preserves unchanged file'
                Check ((Sha256File $script:changed) -ceq $script:beforeChanged) 'preflight preserves changed file'
            } else { Check-PreviousFiles }
            Check (@(Get-ChildItem -LiteralPath $temporary -Force).Count -eq 0) 'no residual transaction backup'
            $script:cases += [pscustomobject]@{ name = $script:case; failure = $failure }
        } finally {
            if ($null -ne $script:lateHandle) { $script:lateHandle.Dispose() }
            if ($null -ne $initialHandle) { $initialHandle.Dispose() }
        }
    }
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
    }
    [ordered]@{
        os = [Environment]::OSVersion.VersionString
        powershell = $PSVersionTable.PSVersion.ToString()
        installer = $Installer; installer_commit = $InstallerCommit; installer_sha256 = $InstallerSha256
        test_path = $PSCommandPath; test_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $PSCommandPath).Hash.ToLowerInvariant()
        cases = $script:cases; assertions = $script:checks
    } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $root 'result.json') -Encoding UTF8
}
$failed = @($script:checks | Where-Object { -not $_.passed })
if ($failed.Count) { throw "assertions failed: $($failed.name -join '; ')" }
Write-Output "PASS Windows installer rollback: $($script:cases.Count) cases, $($script:checks.Count) assertions"
