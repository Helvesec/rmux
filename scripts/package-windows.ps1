param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "release",
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$OutputDir = "target/dist",
    [string]$PlatformLabel = "",
    [switch]$SkipBuild,
    [switch]$AllowStaleBinary
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Error "error: $Message"
    exit 1
}

function Sha256File([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function WorkspaceVersion {
    $inWorkspacePackage = $false
    foreach ($line in Get-Content -LiteralPath "Cargo.toml") {
        if ($line -match '^\[workspace\.package\]$') {
            $inWorkspacePackage = $true
            continue
        }
        if ($line -match '^\[') {
            $inWorkspacePackage = $false
        }
        if ($inWorkspacePackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            return $Matches[1]
        }
    }
    Fail "unable to read workspace package version"
}

function TargetLabel([string]$TargetTriple) {
    switch ($TargetTriple) {
        "x86_64-pc-windows-msvc" { return "windows-x86_64" }
        "aarch64-pc-windows-msvc" { return "windows-aarch64" }
        default {
            return ($TargetTriple -replace '[^A-Za-z0-9_.-]', '-')
        }
    }
}

function GitOutput([string[]]$Arguments) {
    $output = & git @Arguments
    if ($LASTEXITCODE -ne 0) {
        Fail "git $($Arguments -join ' ') failed"
    }
    ($output | Out-String).Trim()
}

function RelativePath([string]$Root, [string]$Path) {
    $rootFull = [System.IO.Path]::GetFullPath($Root)
    $pathFull = [System.IO.Path]::GetFullPath($Path)

    if (-not $rootFull.EndsWith([System.IO.Path]::DirectorySeparatorChar) -and
        -not $rootFull.EndsWith([System.IO.Path]::AltDirectorySeparatorChar)) {
        $rootFull = "$rootFull$([System.IO.Path]::DirectorySeparatorChar)"
    }

    $rootUri = [System.Uri]::new($rootFull)
    $pathUri = [System.Uri]::new($pathFull)
    if ($rootUri.Scheme -ne $pathUri.Scheme) {
        Fail "cannot make relative path across URI schemes: $rootFull -> $pathFull"
    }

    [System.Uri]::UnescapeDataString(
        $rootUri.MakeRelativeUri($pathUri).ToString()
    ).Replace("/", [System.IO.Path]::DirectorySeparatorChar)
}

function WriteAsciiLfFile([string]$Output, [string[]]$Lines) {
    $content = ""
    if ($Lines.Count -gt 0) {
        $content = ($Lines -join "`n") + "`n"
    }
    [System.IO.File]::WriteAllText($Output, $content, [System.Text.Encoding]::ASCII)
}

function WritePackageChecksums([string]$Root, [string]$Output) {
    $rootFull = [System.IO.Path]::GetFullPath($Root)
    $entries = Get-ChildItem -LiteralPath $rootFull -Recurse -File |
        Where-Object { $_.Name -ne "SHA256SUMS.txt" } |
        ForEach-Object {
            $relative = (RelativePath $rootFull $_.FullName).Replace("\", "/")
            if ($relative.StartsWith("../") -or $relative.Contains("/../") -or $relative.Contains("\")) {
                Fail "non-portable package checksum path: $relative"
            }
            [pscustomobject]@{
                Path = $_.FullName
                Relative = $relative
            }
        } |
        Sort-Object -Property Relative

    $lines = foreach ($entry in $entries) {
        "$(Sha256File $entry.Path)  $($entry.Relative)"
    }
    WriteAsciiLfFile $Output $lines
}

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

if ($SkipBuild -and -not $AllowStaleBinary) {
    Fail "-SkipBuild is local-only packaging; pass -AllowStaleBinary to acknowledge that"
}

$version = WorkspaceVersion
if ([string]::IsNullOrWhiteSpace($PlatformLabel)) {
    $PlatformLabel = TargetLabel $Target
}

$profileDir = $Configuration
$cargoArgs = @("build", "--package", "rmux", "--locked", "--target", $Target)
if ($Configuration -eq "release") {
    $cargoArgs += "--release"
}

if (-not $SkipBuild) {
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo build failed"
    }
}

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
$binary = Join-Path $targetDir (Join-Path $Target (Join-Path $profileDir "rmux.exe"))
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    Fail "expected binary was not found: $binary"
}

$distDir = [System.IO.Path]::GetFullPath($OutputDir)
New-Item -ItemType Directory -Force -Path $distDir | Out-Null

$packageName = "rmux-$version-$PlatformLabel"
$stageDir = Join-Path $distDir $packageName
$archivePath = Join-Path $distDir "$packageName.zip"
$checksumsPath = Join-Path $distDir "SHA256SUMS.txt"

if (Test-Path -LiteralPath $stageDir) {
    Remove-Item -LiteralPath $stageDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $stageDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "share/rmux") | Out-Null

Copy-Item -LiteralPath $binary -Destination (Join-Path $stageDir "rmux.exe")
Copy-Item -LiteralPath "README.md", "LICENSE-APACHE", "LICENSE-MIT", "rmux.1" -Destination $stageDir

$binaryAbs = [System.IO.Path]::GetFullPath($binary)
$binarySha256 = Sha256File $binaryAbs
$binaryBytes = (Get-Item -LiteralPath $binaryAbs).Length
$gitCommit = GitOutput @("rev-parse", "HEAD")
$gitStatus = GitOutput @("status", "--porcelain", "--untracked-files=no")
$gitDirty = -not [string]::IsNullOrWhiteSpace($gitStatus)
$releaseArtifact = (-not $SkipBuild) -and (-not $gitDirty)
$generatedAtUtc = GitOutput @("show", "-s", "--format=%cI", "HEAD")

$metadata = [ordered]@{
    schema = 1
    artifact_kind = "windows-package-binary"
    binary_path = $binaryAbs
    binary_sha256 = $binarySha256
    binary_bytes = $binaryBytes
    rmux_version = $version
    git_commit = $gitCommit
    git_dirty = $gitDirty
    target = $Target
    platform_label = $PlatformLabel
    configuration = $Configuration
    package_schema = 1
    package_name = $packageName
    package_target = $Target
    package_target_label = $PlatformLabel
    package_layout = "rmux-windows-package-v1"
    archive_format = "zip"
    skip_build = [bool]$SkipBuild
    release_artifact = $releaseArtifact
    generated_at_utc = $generatedAtUtc
}
$metadataPath = Join-Path $stageDir "share/rmux/artifact-metadata.json"
$metadata | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $metadataPath -Encoding utf8

WritePackageChecksums $stageDir (Join-Path $stageDir "SHA256SUMS.txt")

if (Test-Path -LiteralPath $archivePath) {
    Remove-Item -LiteralPath $archivePath -Force
}
Compress-Archive -Path $stageDir -DestinationPath $archivePath -Force

$archiveSha256 = Sha256File $archivePath
WriteAsciiLfFile $checksumsPath @("$archiveSha256  $([System.IO.Path]::GetFileName($archivePath))")

Write-Output "package=$archivePath"
Write-Output "sha256=$archiveSha256"
Write-Output "binary_sha256=$binarySha256"
Write-Output "release_artifact=$($releaseArtifact.ToString().ToLowerInvariant())"
