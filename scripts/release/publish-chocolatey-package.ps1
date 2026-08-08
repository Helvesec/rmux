param(
    [Parameter(Mandatory = $true)][string]$PayloadDir,
    [Parameter(Mandatory = $true)][string]$ReleaseRef,
    [Parameter(Mandatory = $true)][string]$TargetEvidence,
    [Parameter(Mandatory = $true)][string]$GitHubOutput
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

class ChocolateyPublicBytesMismatchException : System.Exception {
    ChocolateyPublicBytesMismatchException([string]$message) : base($message) {}
}

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
$metadataUrl = "https://community.chocolatey.org/api/v2/Packages(Id='rmux',Version='$version')"
$packageUrl = "https://community.chocolatey.org/api/v2/package/rmux/$version"
$pageUrl = "https://community.chocolatey.org/packages/rmux/$version"
$metadata = Join-Path $env:RUNNER_TEMP "rmux-$version-chocolatey-metadata.xml"
$download = Join-Path $env:RUNNER_TEMP "rmux-$version-public.nupkg"

function Get-ExactPackageState {
    try {
        Invoke-WebRequest -Uri $metadataUrl -OutFile $metadata -MaximumRedirection 5
    }
    catch {
        if ($_.Exception.Response -and [int]$_.Exception.Response.StatusCode -eq 404) {
            return "missing"
        }
        throw
    }

    $statusLines = @(
        python scripts/release/chocolatey-package-status.py `
            --document $metadata `
            --expected-version $version
    )
    if ($LASTEXITCODE -ne 0 -or $statusLines.Count -ne 1) {
        throw "Chocolatey package metadata classification failed"
    }
    $packageState = $statusLines[0].Trim()
    if ($packageState -notin @("pending", "public")) {
        throw "Chocolatey package metadata returned an unknown state"
    }

    Invoke-WebRequest -Uri $packageUrl -OutFile $download -MaximumRedirection 5
    $actualHash = (Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -cne $expectedHash) {
        throw [ChocolateyPublicBytesMismatchException]::new(
            "Existing Chocolatey package bytes differ from the canonical payload"
        )
    }
    return $packageState
}

$mutationStarted = $false
$remoteId = $null
$state = $null
$packageState = $null
try {
    $packageState = Get-ExactPackageState
}
catch [ChocolateyPublicBytesMismatchException] {
    throw
}
catch {
    Write-Warning "Chocolatey package lookup failed before mutation"
    $state = "failed-transient"
}

if ($null -eq $state -and $packageState -eq "public") {
    $state = "no-op-exact"
    $remoteId = "rmux.$version"
}
elseif ($null -eq $state -and $packageState -eq "pending") {
    # The result contract uses this bit to prevent a duplicate retry once a remote submission exists.
    $mutationStarted = $true
    $state = "pending-moderation"
    $remoteId = "rmux.$version"
}
elseif ($null -eq $state -and $packageState -eq "missing") {
    $mutationStarted = $true
    $remoteId = "rmux.$version"
    choco push $package `
        --source "https://push.chocolatey.org/" `
        --api-key $env:CHOCOLATEY_API_KEY `
        --yes `
        --no-progress
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "Chocolatey submission did not return success after mutation began"
        $state = "failed-terminal"
    }
    else {
        $state = "submitted"
    }
}
elseif ($null -eq $state) {
    throw "Chocolatey package lookup returned an invalid state"
}

$observedAt = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
$evidenceArgs = @(
    "scripts/release/channel-target-evidence.py", "create",
    "--channel", "chocolatey", "--state", $state, "--version", $version,
    "--url", $pageUrl, "--observed-at", $observedAt,
    "--output", $TargetEvidence
)
if ($null -ne $remoteId) {
    $evidenceArgs += @("--external-id", $remoteId)
}
python @evidenceArgs
if ($LASTEXITCODE -ne 0) {
    throw "Chocolatey target evidence validation failed"
}

"state=$state" | Out-File -FilePath $GitHubOutput -Append -Encoding utf8
"mutation_started=$($mutationStarted.ToString().ToLowerInvariant())" |
    Out-File -FilePath $GitHubOutput -Append -Encoding utf8
"remote_request_id=$remoteId" | Out-File -FilePath $GitHubOutput -Append -Encoding utf8
"observed_at=$observedAt" | Out-File -FilePath $GitHubOutput -Append -Encoding utf8
