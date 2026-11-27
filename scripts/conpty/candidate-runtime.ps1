param(
    [Parameter(Mandatory = $true)][string]$Stage,
    [string]$Package = "",
    [string]$Target = "x86_64-pc-windows-msvc"
)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
if ((Get-Process -Id $PID).PrivateMemorySize64 -gt 4GB) { throw 'ConPTY verifier memory limit exceeded' }
. (Join-Path $PSScriptRoot 'runtime.ps1')
$manifest = Get-ConptyManifest $Target
$Stage = [IO.Path]::GetFullPath($Stage)
if (-not (Test-Path -LiteralPath $Stage)) {
    New-Item -ItemType Directory -Path $Stage | Out-Null
}
# A failed supplied package is preserved and rejected by Copy-ConptyRuntime.
if (-not (Test-Path -LiteralPath (Join-Path $Stage 'provenance.json'))) {
    New-Item -ItemType Directory -Force -Path (Join-Path $Stage 'libexec/rmux'),
        (Join-Path $Stage 'share/rmux') | Out-Null
    Copy-ConptyRuntime $Target $Package $Stage
}
$files = foreach ($file in $manifest.files) {
    if ((Get-Process -Id $PID).PrivateMemorySize64 -gt 4GB) { throw 'ConPTY verifier memory limit exceeded' }
    $path = Join-Path $Stage $file.name
    Assert-ConptyHash $path $file.sha256
    Assert-ConptyMachine $path $manifest.pe_machine
    Assert-ConptySignature $path $manifest $file
    [ordered]@{ name = $file.name; path = $path; sha256 = $file.sha256;
        signer_thumbprint = $file.signer_thumbprint; authenticode = 'Valid';
        file_version = $manifest.file_version; pe_machine = $manifest.pe_machine }
}
[ordered]@{ package = $manifest.package; version = $manifest.version;
    package_sha256 = $manifest.sha256; url = $manifest.url;
    manifest_sha256 = Get-ConptySha256 $script:ConptyManifestPath;
    publisher = $manifest.publisher; files = @($files) } |
    ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $Stage 'provenance.json') -Encoding UTF8

function Initialize-CandidateConptyRuntime([string]$Target) {
    # This function is also dot-sourced by the gate after its identity context
    # has been validated. Nextest and Cargo inherit the same guarded runner.
    $manifest = Get-ConptyManifest $Target
    $key = 'CARGO_TARGET_' + $Target.ToUpperInvariant().Replace('-', '_') + '_RUNNER'
    if (-not [Environment]::GetEnvironmentVariable($key)) {
        throw 'ConPTY gate requires the candidate identity runner'
    }
    if ($env:RMUX_CONPTY_CONFIGURATION -and $env:RMUX_CONPTY_CONFIGURATION -cne 'pinned') {
        throw 'The VT gate requires the pinned ConPTY configuration'
    }
    if ($env:RMUX_CONPTY_NO_PASSTHROUGH -match '^(1|true|yes)$') {
        throw 'The VT gate cannot disable the pinned ConPTY runtime'
    }
    $stage = Join-Path $env:RMUX_IDENTITY_TARGET 'conpty-runtime'
    foreach ($file in $manifest.files) {
        Assert-ConptyHash (Join-Path $stage $file.name) $file.sha256
    }
}
