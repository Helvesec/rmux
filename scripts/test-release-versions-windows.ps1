param(
    [Parameter(Mandatory = $true)][string]$EvidenceDir,
    [string]$Cargo,
    [string]$Gate,
    [string]$ParentGate
)

# No build or release stages: execute the actual gate's functions and version Step
# in child shells, so a thrown error must also become a nonzero process exit.
$ErrorActionPreference = 'Stop'
if (-not $Cargo) { $Cargo = (Get-Command cargo -ErrorAction Stop).Source }
if (-not $Gate) { $Gate = Join-Path $PSScriptRoot 'release-review-gate-windows.ps1' }
$Cargo = (Resolve-Path -LiteralPath $Cargo).Path
$Gate = (Resolve-Path -LiteralPath $Gate).Path
$repo = Split-Path $PSScriptRoot -Parent
$shell = (Get-Process -Id $PID).Path
if (Test-Path -LiteralPath $EvidenceDir) { throw 'EvidenceDir must be new; retain prior evidence' }
$null = New-Item -ItemType Directory -Path $EvidenceDir
$EvidenceDir = (Resolve-Path -LiteralPath $EvidenceDir).Path
$results = @()

function Write-Text([string]$Path, [string]$Text) {
    $null = New-Item -ItemType Directory -Force -Path (Split-Path $Path -Parent)
    [System.IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding($false)))
}

function Export-Checker([string]$Source, [string]$Destination) {
    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($Source, [ref]$tokens, [ref]$errors)
    if ($errors.Count) { throw "gate parse failed: $errors" }
    $parts = @('param([string]$Workspace, [string]$CargoDirectory)',
        '$ErrorActionPreference = "Stop"',
        'Set-Location -LiteralPath $Workspace',
        '$env:PATH = "$CargoDirectory;$env:PATH"',
        'Write-Host "checker-pid=$PID cargo=$((Get-Command cargo).Source)"')
    foreach ($name in @('Step', 'Read-CargoPackageVersion', 'Check-ReleaseVersions')) {
        $nodes = @($ast.FindAll({ param($node)
            $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
        }, $false))
        if ($nodes.Count -ne 1) { throw "expected one $name function" }
        $parts += $nodes[0].Extent.Text
    }
    $entries = @($ast.EndBlock.Statements | Where-Object {
        $_.Extent.Text -match '^Step\s+"release versions"\s+\{\s*Check-ReleaseVersions\s*\}$'
    })
    if ($entries.Count -ne 1) { throw 'release version entry wiring changed' }
    $parts += $entries[0].Extent.Text
    Write-Text $Destination ($parts -join "`r`n")
    [pscustomobject]@{
        source = $Source; sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $Source).Hash
        extracted = $Destination; extractedSha256 = (Get-FileHash -Algorithm SHA256 $Destination).Hash
        entry = $entries[0].Extent.Text
    } | ConvertTo-Json | Set-Content -LiteralPath "$Destination.binding.json"
}

function Read-Metadata([string]$Workspace, [string]$Name) {
    $child = Start-Process -FilePath $Cargo -WorkingDirectory $Workspace -Wait -PassThru -NoNewWindow `
        -ArgumentList @('metadata', '--locked', '--no-deps', '--format-version', '1') `
        -RedirectStandardOutput "$EvidenceDir\$Name.metadata.json" -RedirectStandardError "$EvidenceDir\$Name.cargo.stderr"
    Write-Text "$EvidenceDir\$Name.cargo.exit" "pid=$($child.Id) exit=$($child.ExitCode)"
    if ($child.ExitCode -ne 0) { throw "real cargo metadata failed: $Name exit=$($child.ExitCode)" }
    $metadata = Get-Content -LiteralPath "$EvidenceDir\$Name.metadata.json" -Raw | ConvertFrom-Json
    $metadata.workspace_members | Set-Content -LiteralPath "$EvidenceDir\$Name.member-ids.txt"
    return $metadata
}

function Run-Checker([string]$Name, [string]$Workspace, [int]$ExpectedExit,
    [string]$Diagnostic, [int]$ExpectedCount = -1, [string]$Checker = $candidate,
    [string]$CargoDirectory = (Split-Path $Cargo -Parent)) {
    $arguments = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'RemoteSigned',
        '-File', "`"$Checker`"", '-Workspace', "`"$Workspace`"", '-CargoDirectory', "`"$CargoDirectory`"")
    $child = Start-Process -FilePath $shell -ArgumentList $arguments -Wait -PassThru -NoNewWindow `
        -RedirectStandardOutput "$EvidenceDir\$Name.stdout" -RedirectStandardError "$EvidenceDir\$Name.stderr"
    $code = $child.ExitCode
    $stdout = Get-Content -LiteralPath "$EvidenceDir\$Name.stdout" -Raw
    $stderr = Get-Content -LiteralPath "$EvidenceDir\$Name.stderr" -Raw
    $count = @($stdout -split '\r?\n' | Where-Object { $_ -match 'Cargo\.toml \S+$' }).Count
    $ok = $code -eq $ExpectedExit -and "$stdout`n$stderr" -match $Diagnostic
    if ($ExpectedCount -ge 0) { $ok = $ok -and $count -eq $ExpectedCount }
    if ($ExpectedExit -ne 0) { $ok = $ok -and $stdout -notmatch 'release-version-check=ok' }
    $script:results += [pscustomobject]@{
        name = $Name; pid = $child.Id; exit = $code; expectedExit = $ExpectedExit
        checkedManifests = $count; expectedCount = $ExpectedCount; passed = $ok
        shell = $shell; arguments = $arguments; workspace = $Workspace
    }
    $script:results | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath "$EvidenceDir\results.json"
    Write-Host "$Name exit=$code manifests=$count passed=$ok"
    if (-not $ok) { throw "checker regression failed: $Name; see retained stdout/stderr" }
}

function New-Fixture([string]$Name) {
    $path = Join-Path $EvidenceDir $Name
    Copy-Item -LiteralPath $fixture -Destination $path -Recurse
    return $path
}

function Replace-Text([string]$Path, [string]$Before, [string]$After) {
    $text = Get-Content -LiteralPath $Path -Raw
    if (-not $text.Contains($Before)) { throw "fixture replacement missing: $Before" }
    Write-Text $Path ($text.Replace($Before, $After))
}

function Lock-Fixture([string]$Workspace, [string]$Name) {
    $child = Start-Process -FilePath $Cargo -WorkingDirectory $Workspace -Wait -PassThru -NoNewWindow `
        -ArgumentList @('generate-lockfile', '--offline') `
        -RedirectStandardOutput "$EvidenceDir\$Name.lock.stdout" -RedirectStandardError "$EvidenceDir\$Name.lock.stderr"
    Write-Text "$EvidenceDir\$Name.lock.exit" "pid=$($child.Id) exit=$($child.ExitCode)"
    if ($child.ExitCode -ne 0) { throw "fixture lock failed: $Name exit=$($child.ExitCode)" }
}

$candidate = Join-Path $EvidenceDir 'candidate-checker.ps1'
Export-Checker $Gate $candidate
$parent = Join-Path $EvidenceDir 'parent-checker.ps1'
if ($ParentGate) { Export-Checker (Resolve-Path -LiteralPath $ParentGate).Path $parent }
Write-Host "powershell=$($PSVersionTable.PSVersion) shell=$shell cargo=$Cargo"
$canonical = Read-Metadata $repo 'canonical'
Run-Checker 'canonical' $repo 0 'release-version-check=ok' $canonical.workspace_members.Count

# The original twelve paths are fixture inputs, not derived from the checker.
# Add the omitted bootstrap and a non-rmux package outside crates, with a space
# in its location, to exercise Cargo membership independently of names/layout.
$legacy = @('ratatui-rmux', 'rmux-client', 'rmux-core', 'rmux-ipc', 'rmux-os', 'rmux-proto',
    'rmux-pty', 'rmux-sdk', 'rmux-server', 'rmux-types', 'rmux-web-crypto')
$paths = @($legacy | ForEach-Object { "crates/$_" }) + @('xtask', 'crates/rmux-bootstrap', 'tools/renamed component')
$fixture = Join-Path $EvidenceDir 'matching'
$members = (@('.') + $paths | ForEach-Object { '"' + $_ + '"' }) -join ', '
Write-Text "$fixture\Cargo.toml" @"
[workspace]
members = [$members]
resolver = "2"
[workspace.package]
version = "1.2.3"
[package]
name = "rmux"
version = "1.2.3"
publish = true
"@
Write-Text "$fixture\src\lib.rs" ''
Write-Text "$fixture\docs\man\rmux.1" '.TH RMUX 1 "test" "RMUX 1.2.3"'
for ($index = 0; $index -lt $paths.Count; $index++) {
    $path = $paths[$index]
    $name = if ($path -eq 'tools/renamed component') { 'widget' } else { Split-Path $path -Leaf }
    $version = if ($index % 2 -eq 0) { 'version.workspace = true' } else { 'version = "1.2.3"' }
    Write-Text "$fixture\$path\Cargo.toml" "[package]`nname = `"$name`"`n$version`n"
    Write-Text "$fixture\$path\src\lib.rs" ''
}
Lock-Fixture $fixture 'matching'
$fixtureMetadata = Read-Metadata $fixture 'matching'
if ($fixtureMetadata.workspace_members.Count -ne 15) { throw 'fixture must have exactly 15 members' }
Run-Checker 'matching' $fixture 0 'workspace-members=15' 15
$singleton = New-Fixture 'singleton'
Replace-Text "$singleton\Cargo.toml" "members = [$members]" 'members = ["."]'
Lock-Fixture $singleton 'singleton'
$singletonMetadata = Read-Metadata $singleton 'singleton'
if ($singletonMetadata.workspace_members.Count -ne 1) { throw 'singleton must have one member' }
Run-Checker 'singleton' $singleton 0 'workspace-members=1' 1

$mismatch = New-Fixture 'bootstrap-mismatch'
Replace-Text "$mismatch\crates\rmux-bootstrap\Cargo.toml" 'version.workspace = true' 'version = "9.8.7"'
Lock-Fixture $mismatch 'bootstrap-mismatch'
$mismatchMetadata = Read-Metadata $mismatch 'bootstrap-mismatch'
if ($mismatchMetadata.workspace_members.Count -ne 15 -or
    @($mismatchMetadata.packages | Where-Object { $_.name -eq 'rmux-bootstrap' -and $_.version -eq '9.8.7' }).Count -ne 1) {
    throw 'real Cargo did not observe the omitted member mismatch'
}
if ($ParentGate) { Run-Checker 'parent-bootstrap-mismatch' $mismatch 0 'release-version-check=ok' 12 $parent }
Run-Checker 'bootstrap-mismatch' $mismatch 1 'version 9.8.7 != root version 1.2.3'

$relocated = New-Fixture 'relocated-mismatch'
Replace-Text "$relocated\tools\renamed component\Cargo.toml" 'version = "1.2.3"' 'version = "9.8.7"'
Lock-Fixture $relocated 'relocated-mismatch'
Run-Checker 'relocated-mismatch' $relocated 1 'version 9.8.7 != root version 1.2.3'
$inherited = New-Fixture 'inherited-mismatch'
Replace-Text "$inherited\Cargo.toml" "[workspace.package]`nversion = `"1.2.3`"" "[workspace.package]`nversion = `"9.8.7`""
Lock-Fixture $inherited 'inherited-mismatch'
Run-Checker 'inherited-mismatch' $inherited 1 'version 9.8.7 != root version 1.2.3'
$rootInherited = New-Fixture 'root-inherited'
Replace-Text "$rootInherited\Cargo.toml" "name = `"rmux`"`nversion = `"1.2.3`"" "name = `"rmux`"`nversion.workspace = true"
Run-Checker 'root-inherited' $rootInherited 0 'workspace-members=15' 15
$publish = New-Fixture 'publish-false'
Replace-Text "$publish\Cargo.toml" 'publish = true' 'publish = false'
Run-Checker 'publish-false' $publish 1 'root rmux package must keep publish=true' 0
$manpage = New-Fixture 'manpage-mismatch'
Write-Text "$manpage\docs\man\rmux.1" 'RMUX 9.8.7'
Run-Checker 'manpage-mismatch' $manpage 1 'does not contain RMUX 1.2.3' 0
$broken = New-Fixture 'cargo-invalid-manifest'
Write-Text "$broken\crates\rmux-bootstrap\Cargo.toml" '[invalid'
Run-Checker 'cargo-invalid-manifest' $broken 1 'error|cargo metadata failed' 0

# Malformed transport/control cases use a native .cmd, including a valid JSON
# body followed by exit 37. The same child-shell checker path must fail closed.
$stub = Join-Path $EvidenceDir 'cargo-stub'
Write-Text "$stub\cargo.cmd" "@echo off`r`ntype `"%RMUX_TEST_METADATA%`"`r`nexit /b %RMUX_TEST_CARGO_EXIT%`r`n"
$env:RMUX_TEST_CARGO_EXIT = '0'
function Run-MetadataCase([string]$Name, [string]$Json, [int]$Exit = 1, [int]$Count = 0) {
    $env:RMUX_TEST_METADATA = Join-Path $EvidenceDir "$Name.input.json"
    Write-Text $env:RMUX_TEST_METADATA $Json
    Run-Checker $Name $fixture $Exit 'metadata|JSON|Json|release-version-check' $Count $candidate $stub
}
$validJson = $fixtureMetadata | ConvertTo-Json -Depth 30
# Keep the real Cargo IDs and change only the root record's path. Both relative
# forms resolve to the existing matching manifest on Windows, so file absence
# cannot accidentally make these rejection regressions pass.
$rootPackage = @($fixtureMetadata.packages | Where-Object { $_.name -ceq 'rmux' })
if ($rootPackage.Count -ne 1 -or $rootPackage[0].manifest_path -notmatch '^[A-Za-z]:[\\/]') {
    throw 'path regressions require a drive-absolute matching fixture'
}
$rootManifest = $rootPackage[0].manifest_path
foreach ($case in @(
    @{ name = 'drive-absolute'; path = $rootManifest; exit = 0; count = 15 },
    @{ name = 'drive-absolute-forward-slash'; path = $rootManifest.Replace('\', '/'); exit = 0; count = 15 },
    @{ name = 'drive-relative'; path = $rootManifest.Substring(0, 2) + 'Cargo.toml'; exit = 1; count = 0 },
    @{ name = 'root-relative'; path = $rootManifest.Substring(2); exit = 1; count = 0 },
    @{ name = 'root-relative-forward-slash'; path = $rootManifest.Substring(2).Replace('\', '/'); exit = 1; count = 0 }
)) {
    $data = $validJson | ConvertFrom-Json
    $record = @($data.packages | Where-Object { $_.id -ceq $rootPackage[0].id })
    $record[0].manifest_path = $case.path
    Run-MetadataCase $case.name ($data | ConvertTo-Json -Depth 30) $case.exit $case.count
    if ($case.exit -ne 0 -and (Get-Content "$EvidenceDir\$($case.name).stderr" -Raw) -notmatch 'no unique manifest path') {
        throw "path was not rejected by metadata validation: $($case.name)"
    }
}

# Exercise the actual guard's predicate for UNC/device forms without accessing
# or configuring a share. This is predicate coverage, not filesystem I/O proof.
$tokens = $null
$errors = $null
$gateAst = [System.Management.Automation.Language.Parser]::ParseFile($Gate, [ref]$tokens, [ref]$errors)
$guards = @($gateAst.FindAll({ param($node)
    $node -is [System.Management.Automation.Language.IfStatementAst] -and
    $node.Extent.Text -match 'throw "cargo metadata has no unique manifest path'
}, $true))
if ($errors.Count -or $guards.Count -ne 1) { throw 'expected one manifest path guard' }
$predicate = [scriptblock]::Create($guards[0].Clauses[0].Item1.Extent.Text)
$predicateResults = foreach ($path in @(
    '\\server\share\Cargo.toml', '//server/share/Cargo.toml',
    '\\?\C:\workspace\Cargo.toml', '\\.\C:\workspace\Cargo.toml',
    '\\?\UNC\server\share\Cargo.toml', '\\.\UNC\server\share\Cargo.toml',
    '\??\C:\workspace\Cargo.toml'
)) {
    $packages = @([pscustomobject]@{ manifest_path = $path })
    $rejected = & $predicate
    if ($rejected -isnot [bool] -or $rejected) { throw "qualified path rejected: $path" }
    [pscustomobject]@{ path = $path; rejected = $rejected; pid = $PID; scope = 'predicate-only' }
}
$predicateResults | ConvertTo-Json | Set-Content -LiteralPath "$EvidenceDir\path-predicate.json"
Write-Text "$EvidenceDir\path-predicate.txt" $predicate.ToString()
Run-MetadataCase 'empty-output' ''
Run-MetadataCase 'malformed-json' '{'
Run-MetadataCase 'null-metadata' 'null'
Run-MetadataCase 'missing-membership' '{}'
Run-MetadataCase 'empty-membership' '{"packages":[],"workspace_members":[]}'
foreach ($case in @('missing-packages', 'empty-packages', 'scalar-member', 'unknown-member',
    'case-changed-id', 'duplicate-member', 'duplicate-package', 'missing-path', 'relative-path', 'missing-manifest')) {
    $data = $validJson | ConvertFrom-Json
    switch ($case) {
        'missing-packages' { $data.PSObject.Properties.Remove('packages') }
        'empty-packages' { $data.packages = @() }
        'scalar-member' { $data.workspace_members = $data.workspace_members[0] }
        'unknown-member' { $data.workspace_members += 'absent-id' }
        'case-changed-id' { $data.workspace_members[0] = $data.workspace_members[0].ToUpperInvariant() }
        'duplicate-member' { $data.workspace_members += $data.workspace_members[0] }
        'duplicate-package' { $data.packages += $data.packages[0] }
        'missing-path' { $data.packages[0].PSObject.Properties.Remove('manifest_path') }
        'relative-path' { $data.packages[0].manifest_path = 'Cargo.toml' }
        'missing-manifest' { $data.packages[0].manifest_path = Join-Path $fixture 'absent\Cargo.toml' }
    }
    # Missing manifest is detected while walking the otherwise valid list.
    if ($case -eq 'missing-manifest') {
        $env:RMUX_TEST_METADATA = Join-Path $EvidenceDir "$case.input.json"
        Write-Text $env:RMUX_TEST_METADATA ($data | ConvertTo-Json -Depth 30)
        Run-Checker $case $fixture 1 'missing manifest' -1 $candidate $stub
    } else { Run-MetadataCase $case ($data | ConvertTo-Json -Depth 30) }
}
$withNonmember = $validJson | ConvertFrom-Json
$withNonmember.packages += [pscustomobject]@{ id = 'not-a-member'; name = 'rmux-decoy'; manifest_path = 'missing'; version = '9.8.7' }
Run-MetadataCase 'ignore-nonmember' ($withNonmember | ConvertTo-Json -Depth 30) 0 15
$env:RMUX_TEST_CARGO_EXIT = '37'
Run-MetadataCase 'cargo-exit-37' $validJson
if ((Get-Content "$EvidenceDir\cargo-exit-37.stderr" -Raw) -notmatch 'exit code 37') { throw 'native cargo exit was lost' }
Write-Host "release-version-regressions=ok cases=$($results.Count)"
