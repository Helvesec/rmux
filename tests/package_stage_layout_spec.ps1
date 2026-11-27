param([Parameter(Mandatory = $true)][string]$FixtureRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $repo 'scripts/packaging/layout-tool.ps1')
. (Join-Path $repo 'scripts/packaging/stage-layout.ps1')
if (Test-Path -LiteralPath $FixtureRoot) { throw 'use a fresh fixture directory' }
New-Item -ItemType Directory -Path $FixtureRoot | Out-Null
$FixtureRoot = (Resolve-Path -LiteralPath $FixtureRoot).Path
$records = @()

function Assert-Names([string[]]$Actual, [string[]]$Expected) {
    if ($Actual.Count -ne $Expected.Count) { throw 'source array count changed' }
    for ($i = 0; $i -lt $Expected.Count; $i++) {
        if ($Actual[$i] -cne $Expected[$i]) { throw 'source array element changed' }
    }
}

function Assert-Copied([string]$From, [string]$To, [string[]]$Names) {
    Assert-Names -Actual @(Get-ChildItem -LiteralPath $To -File |
        Select-Object -ExpandProperty Name | Sort-Object) -Expected @($Names | Sort-Object)
    foreach ($name in $Names) {
        $source = [IO.File]::ReadAllBytes((Join-Path $From $name))
        $copy = [IO.File]::ReadAllBytes((Join-Path $To $name))
        if ([Convert]::ToBase64String($source) -cne [Convert]::ToBase64String($copy)) {
            throw "copied content changed: $name"
        }
    }
}

function Assert-Refused([scriptblock]$Action, [string]$Message = '') {
    $failure = $null
    try { & $Action | Out-Null } catch { $failure = $_ }
    if ($null -eq $failure) { throw 'invalid basename was accepted' }
    if ($Message -and $failure.ToString() -notlike "*$Message*") {
        throw "wrong refusal: $failure"
    }
}

# Exercise the real descriptor projection, cache, generated preparation and
# staging. No RMUX executable, Cargo, Pester or mocked copy command is used.
foreach ($rename in @('_rmux renamed', 'completion config', '_rmux été 日本', '_rmux')) {
    foreach ($mode in @('single', 'many')) {
        foreach ($flow in @('cache', 'generated')) {
            $label = "$flow-$mode-$rename"
            try {
                $root = Join-Path $FixtureRoot $label
                $from = Join-Path $root 'from'
                $prepared = Join-Path $root 'prepared'
                $cached = Join-Path $root 'cached'
                New-Item -ItemType Directory -Path $from -Force | Out-Null
                $descriptor = Get-Content -LiteralPath (Join-Path $repo 'scripts/packaging/rmux-package-layout.json') -Raw |
                    ConvertFrom-Json
                ($descriptor.entries | Where-Object id -eq 'completion-zsh').source.name = $rename
                $descriptorPath = Join-Path $root 'descriptor.json'
                [IO.File]::WriteAllText($descriptorPath, ($descriptor | ConvertTo-Json -Depth 100))
                $arguments = @('--descriptor', $descriptorPath, 'source-names', '--variant',
                    'windows-archive', '--kind', 'completion')
                $names = @(Invoke-LayoutTool $arguments)
                $expected = @('rmux.bash', $rename, 'rmux.fish', '_rmux.ps1', 'rmux.elv')
                Assert-Names -Actual $names -Expected $expected
                $origins = @(Invoke-LayoutTool ($arguments + '--with-origin'))
                Assert-Names -Actual @($origins | ForEach-Object { ($_ -split "`t")[1] }) -Expected $expected
                if ($mode -eq 'single') {
                    $names = @($names | Where-Object { $_ -ceq $rename })
                    $origins = @($origins | Where-Object { ($_ -split "`t")[0] -eq 'zsh' })
                    Assert-Names -Actual $names -Expected @($rename)
                }
                $reported = @()
                foreach ($origin in $origins) {
                    $shell, $name = $origin -split "`t"
                    # The generator keeps its own filenames when a descriptor
                    # renames a packaged source; preparation must join by shell.
                    $source = Join-Path $from "$shell.generated"
                    [IO.File]::WriteAllText($source, "content for $shell / $name")
                    $reported += "$shell`t$source"
                    if ($flow -eq 'cache') {
                        [IO.File]::WriteAllText((Join-Path $from $name), "content for $shell / $name")
                    }
                }
                if ($flow -eq 'cache') {
                    $answer = @(Copy-LayoutSourceFiles -Names $names -From $from -To $prepared -Missing 'cache missing')
                } else {
                    $answer = @(Copy-LayoutGeneratedSources -Reported $reported -Expected $origins -To $prepared)
                }
                Assert-Names -Actual $answer -Expected $names
                foreach ($origin in $origins) {
                    $shell, $name = $origin -split "`t"
                    if ([IO.File]::ReadAllText((Join-Path $prepared $name)) -cne
                        "content for $shell / $name") { throw 'prepared content changed' }
                }
                $answer = @(Copy-LayoutSourceFiles -Names $names -From $prepared -To $cached -Missing 'refresh missing')
                Assert-Names -Actual $answer -Expected $names
                Assert-Copied -From $prepared -To $cached -Names $names
                $plan = @("$(Join-Path $cached $rename)`tshare/zsh/site-functions/_rmux`t0644`tcopy")
                $stage = Join-Path $root 'stage'
                Assert-Names -Actual @(Copy-LayoutStagePlan -Plan $plan -Stage $stage) -Expected @('share/zsh/site-functions/_rmux')
                if ([IO.File]::ReadAllText((Join-Path $stage 'share/zsh/site-functions/_rmux')) -cne
                    [IO.File]::ReadAllText((Join-Path $prepared $rename))) { throw 'staged content changed' }
                $records += [pscustomobject]@{case=$label; status='PASS'}
            } catch { $records += [pscustomobject]@{case=$label; status='FAIL'; error=$_.ToString()} }
        }
    }
}

# A literal wildcard basename must not copy the neighboring matching leaf.
$literalRoot = Join-Path $FixtureRoot 'literal'
New-Item -ItemType Directory -Path $literalRoot | Out-Null
[IO.File]::WriteAllText((Join-Path $literalRoot '_rmux[1]'), 'literal')
[IO.File]::WriteAllText((Join-Path $literalRoot '_rmux1'), 'neighbor')
foreach ($flow in @('cache', 'generated')) {
    try {
        $to = Join-Path $FixtureRoot "literal-$flow"
        if ($flow -eq 'cache') {
            $answer = @(Copy-LayoutSourceFiles -Names @('_rmux[1]') -From $literalRoot -To $to -Missing 'missing')
        } else {
            $answer = @(Copy-LayoutGeneratedSources -Reported @("zsh`t$(Join-Path $literalRoot '_rmux[1]')") -Expected @("zsh`t_rmux[1]") -To $to)
        }
        Assert-Names -Actual $answer -Expected @('_rmux[1]')
        Assert-Copied -From $literalRoot -To $to -Names @('_rmux[1]')
        $records += [pscustomobject]@{case="literal-$flow"; status='PASS'}
    } catch { $records += [pscustomobject]@{case="literal-$flow"; status='FAIL'; error=$_.ToString()} }
}

foreach ($name in @('', ' ', '.', '..', '../escape', '..\escape', 'nested/file', 'nested\file')) {
    foreach ($flow in @('cache', 'generated')) {
        try {
            $to = Join-Path $FixtureRoot "refusal-$($records.Count)"
            $message = 'not a source file name'
            # PowerShell may reject an empty mandatory string[] element while
            # binding parameters, before the helper body is entered.
            if ($name -eq '' -and $flow -eq 'cache') { $message = '' }
            if ($flow -eq 'cache') {
                Assert-Refused { Copy-LayoutSourceFiles -Names @($name) -From $literalRoot -To $to -Missing 'missing' } -Message $message
            } else {
                Assert-Refused { Copy-LayoutGeneratedSources -Reported @("zsh`t$(Join-Path $literalRoot '_rmux1')") -Expected @("zsh`t$name") -To $to } -Message $message
            }
            $records += [pscustomobject]@{case="$flow-refuse-[$name]"; status='PASS'}
        } catch { $records += [pscustomobject]@{case="$flow-refuse-[$name]"; status='FAIL'; error=$_.ToString()} }
    }
}
Assert-Refused { Copy-LayoutSourceFiles -Names @() -From $literalRoot -To (Join-Path $FixtureRoot 'empty-cache') -Missing 'missing' }
Assert-Refused { Copy-LayoutGeneratedSources -Reported @() -Expected @() -To (Join-Path $FixtureRoot 'empty-generated') }
$records | ConvertTo-Json -Depth 5
if (@($records | Where-Object status -eq 'FAIL').Count) { throw 'package stage layout regression' }
