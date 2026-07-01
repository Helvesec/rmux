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
    $getFileHash = Get-Command Get-FileHash -ErrorAction SilentlyContinue
    if ($getFileHash) {
        return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
    }

    $stream = [System.IO.File]::OpenRead([System.IO.Path]::GetFullPath($Path))
    try {
        $sha256 = [System.Security.Cryptography.SHA256]::Create()
        try {
            $hashBytes = $sha256.ComputeHash($stream)
            return ([System.BitConverter]::ToString($hashBytes) -replace "-", "").ToLowerInvariant()
        } finally {
            $sha256.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
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

function ValidatePlatformLabel([string]$Label) {
    if ([string]::IsNullOrWhiteSpace($Label) -or $Label -notmatch '^[A-Za-z0-9_.-]+$') {
        Fail "platform label must contain only ASCII letters, digits, '.', '_' or '-'"
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

$repoRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
Set-Location -LiteralPath $repoRoot
[System.IO.Directory]::SetCurrentDirectory($repoRoot)

if ($SkipBuild -and -not $AllowStaleBinary) {
    Fail "-SkipBuild is local-only packaging; pass -AllowStaleBinary to acknowledge that"
}

$version = WorkspaceVersion
if ([string]::IsNullOrWhiteSpace($PlatformLabel)) {
    $PlatformLabel = TargetLabel $Target
}
ValidatePlatformLabel $PlatformLabel

$profileDir = $Configuration
$cargoArgs = @("build", "--package", "rmux", "--locked", "--target", $Target)
if ($Configuration -eq "release") {
    $cargoArgs += "--release"
}

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "target" }
$binary = Join-Path $targetDir (Join-Path $Target (Join-Path $profileDir "rmux.exe"))
$helperBinary = Join-Path $targetDir (Join-Path $Target (Join-Path $profileDir "rmux-full.exe"))
$daemonBinary = Join-Path $targetDir (Join-Path $Target (Join-Path $profileDir "rmux-daemon.exe"))

if (-not $SkipBuild) {
    & cargo @cargoArgs --bin rmux
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo build full rmux helper failed"
    }
    Copy-Item -LiteralPath $binary -Destination $helperBinary -Force

    $tinyCargoArgs = @($cargoArgs)
    $tinyCargoArgs += @("--features", "tiny-cli")
    & cargo @tinyCargoArgs --bin rmux
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo build tiny rmux failed"
    }
    # Keep the packaged hidden daemon web-capable. The public binary can still
    # use the tiny CLI fast path, but sessions it starts must remain shareable
    # later from a fresh `rmux web-share` invocation.
    $daemonCargoArgs = @($cargoArgs)
    & cargo @daemonCargoArgs --bin rmux-daemon
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo build rmux-daemon failed"
    }
}

$completionCache = if ($env:RMUX_COMPLETIONS_DIR) {
    $env:RMUX_COMPLETIONS_DIR
} else {
    Join-Path (Split-Path -Parent $binary) "completions"
}
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    Fail "expected binary was not found: $binary"
}
if (-not (Test-Path -LiteralPath $helperBinary -PathType Leaf)) {
    Fail "expected full helper binary was not found: $helperBinary"
}
if (-not (Test-Path -LiteralPath $daemonBinary -PathType Leaf)) {
    Fail "expected daemon binary was not found: $daemonBinary"
}

$distDir = [System.IO.Path]::GetFullPath($OutputDir)
New-Item -ItemType Directory -Force -Path $distDir | Out-Null

$packageName = "rmux-$version-$PlatformLabel"
$stageDir = Join-Path $distDir $packageName
$archivePath = Join-Path $distDir "$packageName.zip"
$checksumsPath = Join-Path $distDir "SHA256SUMS.txt"

try {
if (Test-Path -LiteralPath $stageDir) {
    Remove-Item -LiteralPath $stageDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $stageDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "share/rmux") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "share/bash-completion/completions") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "share/zsh/site-functions") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "share/fish/vendor_completions.d") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "share/powershell/Completions") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "share/elvish/lib") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "libexec/rmux") | Out-Null

Copy-Item -LiteralPath $binary -Destination (Join-Path $stageDir "rmux.exe")
Copy-Item -LiteralPath $helperBinary -Destination (Join-Path $stageDir "libexec/rmux/rmux.exe")
Copy-Item -LiteralPath $daemonBinary -Destination (Join-Path $stageDir "rmux-daemon.exe")
Copy-Item -LiteralPath "README.md", "LICENSE-APACHE", "LICENSE-MIT", "docs/man/rmux.1" -Destination $stageDir
$completionDir = Join-Path ([System.IO.Path]::GetTempPath()) "rmux-completions-$([System.Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $completionDir | Out-Null
try {
    $completionFiles = @("rmux.bash", "_rmux", "rmux.fish", "_rmux.ps1", "rmux.elv")
    if (-not $SkipBuild) {
        cargo run --quiet --package xtask -- generate-completions --output-dir $completionDir | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Fail "failed to generate shell completions"
        }
        try {
            if (Test-Path -LiteralPath $completionCache) {
                Remove-Item -LiteralPath $completionCache -Recurse -Force -ErrorAction SilentlyContinue
            }
            New-Item -ItemType Directory -Force -Path $completionCache -ErrorAction Stop | Out-Null
            foreach ($completionFile in $completionFiles) {
                Copy-Item -LiteralPath (Join-Path $completionDir $completionFile) -Destination (Join-Path $completionCache $completionFile) -Force -ErrorAction Stop
            }
        } catch {
            Write-Warning "unable to refresh completion cache ${completionCache}: $_"
        }
    } else {
        foreach ($completionFile in $completionFiles) {
            $source = Join-Path $completionCache $completionFile
            if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
                Fail "-SkipBuild requires prebuilt completions in $completionCache; rerun without -SkipBuild or set RMUX_COMPLETIONS_DIR"
            }
            Copy-Item -LiteralPath $source -Destination (Join-Path $completionDir $completionFile)
        }
    }
    Copy-Item -LiteralPath (Join-Path $completionDir "rmux.bash") -Destination (Join-Path $stageDir "share/bash-completion/completions/rmux")
    Copy-Item -LiteralPath (Join-Path $completionDir "_rmux") -Destination (Join-Path $stageDir "share/zsh/site-functions/_rmux")
    Copy-Item -LiteralPath (Join-Path $completionDir "rmux.fish") -Destination (Join-Path $stageDir "share/fish/vendor_completions.d/rmux.fish")
    Copy-Item -LiteralPath (Join-Path $completionDir "_rmux.ps1") -Destination (Join-Path $stageDir "share/powershell/Completions/_rmux.ps1")
    Copy-Item -LiteralPath (Join-Path $completionDir "rmux.elv") -Destination (Join-Path $stageDir "share/elvish/lib/rmux.elv")
} finally {
    if (Test-Path -LiteralPath $completionDir) {
        Remove-Item -LiteralPath $completionDir -Recurse -Force
    }
}

$binaryAbs = [System.IO.Path]::GetFullPath($binary)
$helperBinaryAbs = [System.IO.Path]::GetFullPath($helperBinary)
$daemonBinaryAbs = [System.IO.Path]::GetFullPath($daemonBinary)
$binarySha256 = Sha256File $binaryAbs
$helperBinarySha256 = Sha256File $helperBinaryAbs
$daemonBinarySha256 = Sha256File $daemonBinaryAbs
$binaryBytes = (Get-Item -LiteralPath $binaryAbs).Length
$helperBinaryBytes = (Get-Item -LiteralPath $helperBinaryAbs).Length
$daemonBinaryBytes = (Get-Item -LiteralPath $daemonBinaryAbs).Length
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
    helper_binary_path = $helperBinaryAbs
    helper_binary_sha256 = $helperBinarySha256
    helper_binary_bytes = $helperBinaryBytes
    daemon_binary_path = $daemonBinaryAbs
    daemon_binary_sha256 = $daemonBinarySha256
    daemon_binary_bytes = $daemonBinaryBytes
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
    package_layout = "rmux-windows-package-v2"
    archive_format = "zip"
    skip_build = [bool]$SkipBuild
    release_artifact = $releaseArtifact
    generated_at_utc = $generatedAtUtc
}
$metadataPath = Join-Path $stageDir "share/rmux/artifact-metadata.json"
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $metadataPath) | Out-Null
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
Write-Output "helper_binary_sha256=$helperBinarySha256"
Write-Output "daemon_binary_sha256=$daemonBinarySha256"
Write-Output "release_artifact=$($releaseArtifact.ToString().ToLowerInvariant())"
} finally {
    if (Test-Path -LiteralPath $stageDir) {
        Remove-Item -LiteralPath $stageDir -Recurse -Force
    }
}
