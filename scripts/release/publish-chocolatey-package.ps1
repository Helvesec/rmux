param(
    [Parameter(Mandatory = $true)][string]$PayloadDir,
    [Parameter(Mandatory = $true)][string]$ReleaseRef,
    [Parameter(Mandatory = $true)][string]$TargetEvidence,
    [Parameter(Mandatory = $true)][string]$GitHubOutput
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($ReleaseRef -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') {
    throw "Chocolatey release ref must be one stable version"
}
if ([string]::IsNullOrWhiteSpace($env:CHOCOLATEY_API_KEY)) {
    throw "Chocolatey API key is missing"
}

$version = $ReleaseRef.Substring(1)
$payloadItem = Get-Item -LiteralPath $PayloadDir -Force
if (-not $payloadItem.PSIsContainer -or ($payloadItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw "Chocolatey payload root must be one real directory"
}
$payloadRoot = (Resolve-Path -LiteralPath $PayloadDir).Path
$entries = @(Get-ChildItem -LiteralPath $payloadRoot -Force -Recurse)
if ($entries | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }) {
    throw "Chocolatey payload cannot contain reparse points"
}
$files = @($entries | Where-Object { -not $_.PSIsContainer })
$expectedName = "rmux.$version.nupkg"
if (
    $files.Count -ne 1 -or
    $files[0].Name -cne $expectedName -or
    $files[0].DirectoryName -cne $payloadRoot
) {
    throw "Chocolatey payload file set differs"
}
$package = $files[0].FullName
$expectedHash = (Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash.ToLowerInvariant()
$packageUrl = "https://community.chocolatey.org/api/v2/package/rmux/$version"
$pageUrl = "https://community.chocolatey.org/packages/rmux/$version"
$download = Join-Path $env:RUNNER_TEMP "rmux-$version-public.nupkg"

function Get-ExactPublicPackage {
    try {
        Invoke-WebRequest -Uri $packageUrl -OutFile $download -MaximumRedirection 5
    }
    catch {
        if ($_.Exception.Response -and [int]$_.Exception.Response.StatusCode -eq 404) {
            return $false
        }
        throw
    }
    $actualHash = (Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -cne $expectedHash) {
        throw "Public Chocolatey package bytes differ from the canonical payload"
    }
    return $true
}

$mutationStarted = $false
if (Get-ExactPublicPackage) {
    $state = "no-op-exact"
}
else {
    choco push $package `
        --source "https://push.chocolatey.org/" `
        --api-key $env:CHOCOLATEY_API_KEY `
        --yes `
        --no-progress
    if ($LASTEXITCODE -ne 0) {
        throw "Chocolatey rejected the exact package"
    }
    $mutationStarted = $true
    $state = "submitted"
}

$observedAt = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
$evidenceArgs = @(
    "scripts/release/channel-target-evidence.py", "create",
    "--channel", "chocolatey", "--state", $state, "--version", $version,
    "--external-id", "rmux.$version", "--url", $pageUrl,
    "--observed-at", $observedAt, "--output", $TargetEvidence
)
python @evidenceArgs
if ($LASTEXITCODE -ne 0) {
    throw "Chocolatey target evidence validation failed"
}

"state=$state" | Out-File -FilePath $GitHubOutput -Append -Encoding utf8
"mutation_started=$($mutationStarted.ToString().ToLowerInvariant())" |
    Out-File -FilePath $GitHubOutput -Append -Encoding utf8
"remote_request_id=rmux.$version" | Out-File -FilePath $GitHubOutput -Append -Encoding utf8
"observed_at=$observedAt" | Out-File -FilePath $GitHubOutput -Append -Encoding utf8
