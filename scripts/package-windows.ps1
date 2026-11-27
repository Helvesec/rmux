param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "release",
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$OutputDir = "target/dist",
    [string]$PlatformLabel = "",
    [switch]$SkipBuild,
    [switch]$AllowStaleBinary,
    [switch]$ReuseReleaseBinaries,
    [string]$ReleaseBinaryManifest = "",
    [string]$ConptyPackage = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "conpty/runtime.ps1")
. (Join-Path $PSScriptRoot "packaging/layout-tool.ps1")
. (Join-Path $PSScriptRoot "packaging/stage-layout.ps1")
. (Join-Path $PSScriptRoot "package-windows-archive.ps1")

function Invoke-Notices([string[]]$Arguments) {
    $python = Resolve-LayoutPython
    & $python (Join-Path $PSScriptRoot "packaging/notices.py") @Arguments
    if ($LASTEXITCODE -ne 0) { Fail "third-party notice generation or payload binding failed" }
}

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
    [string[]]$entries = @(Get-ChildItem -LiteralPath $rootFull -Recurse -File |
        Where-Object { $_.Name -ne "SHA256SUMS.txt" } |
        ForEach-Object {
            $relative = (RelativePath $rootFull $_.FullName).Replace("\", "/")
            if ($relative.StartsWith("../") -or $relative.Contains("/../") -or $relative.Contains("\")) {
                Fail "non-portable package checksum path: $relative"
            }
            $relative
        })
    [Array]::Sort($entries, [StringComparer]::Ordinal)

    $lines = foreach ($entry in $entries) {
        "$(Sha256File (Join-Path $rootFull $entry))  $entry"
    }
    WriteAsciiLfFile $Output $lines
}

function RequireManifestProperty([object]$Manifest, [string]$Name) {
    if ($null -eq $Manifest -or -not ($Manifest.PSObject.Properties.Name -contains $Name)) {
        Fail "release binary manifest is missing $Name"
    }
    return $Manifest.$Name
}

function ValidateReleaseBinaryManifest(
    [string]$ManifestPath,
    [string]$ExpectedGitCommit,
    [string]$ExpectedTarget,
    [string]$ExpectedConfiguration,
    [string]$Binary,
    [string]$HelperBinary,
    [string]$DaemonBinary
) {
    if (-not (Test-Path -LiteralPath $ManifestPath -PathType Leaf)) {
        Fail "release binary manifest was not found: $ManifestPath"
    }
    try {
        $manifest = Get-Content -LiteralPath $ManifestPath -Raw -Encoding utf8 | ConvertFrom-Json
    } catch {
        Fail "release binary manifest is not valid JSON: $_"
    }
    if ((RequireManifestProperty $manifest "schema") -ne 1) {
        Fail "release binary manifest schema is not 1"
    }
    if ((RequireManifestProperty $manifest "kind") -ne "rmux-windows-release-binaries") {
        Fail "release binary manifest kind is invalid"
    }
    if ((RequireManifestProperty $manifest "git_commit") -ne $ExpectedGitCommit) {
        Fail "release binary manifest Git commit does not match HEAD"
    }
    if ((RequireManifestProperty $manifest "target") -ne $ExpectedTarget) {
        Fail "release binary manifest target does not match $ExpectedTarget"
    }
    if ((RequireManifestProperty $manifest "configuration") -ne $ExpectedConfiguration) {
        Fail "release binary manifest configuration does not match $ExpectedConfiguration"
    }

    foreach ($entry in @(
        [pscustomobject]@{ Name = "binary_sha256"; Path = $Binary }
        [pscustomobject]@{ Name = "helper_binary_sha256"; Path = $HelperBinary }
        [pscustomobject]@{ Name = "daemon_binary_sha256"; Path = $DaemonBinary }
    )) {
        $name = $entry.Name
        $path = $entry.Path
        $expectedHash = [string](RequireManifestProperty $manifest $name)
        if ($expectedHash -notmatch '^[0-9a-fA-F]{64}$') {
            Fail "release binary manifest $name is not a SHA-256 digest"
        }
        $actualHash = Sha256File $path
        if ($actualHash -ne $expectedHash.ToLowerInvariant()) {
            Fail "release binary manifest $name does not match $path"
        }
    }
}

$repoRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
Set-Location -LiteralPath $repoRoot
[System.IO.Directory]::SetCurrentDirectory($repoRoot)

if ($SkipBuild -and $ReuseReleaseBinaries) {
    Fail "-SkipBuild and -ReuseReleaseBinaries are mutually exclusive"
}
if ($AllowStaleBinary -and $ReuseReleaseBinaries) {
    Fail "-AllowStaleBinary cannot be combined with -ReuseReleaseBinaries"
}
if ($SkipBuild -and -not $AllowStaleBinary) {
    Fail "-SkipBuild is local-only packaging; pass -AllowStaleBinary to acknowledge that"
}
if ($ReuseReleaseBinaries -and $Configuration -ne "release") {
    Fail "-ReuseReleaseBinaries requires -Configuration release"
}
if ($ReuseReleaseBinaries -ne (-not [string]::IsNullOrWhiteSpace($ReleaseBinaryManifest))) {
    Fail "-ReuseReleaseBinaries requires exactly one -ReleaseBinaryManifest"
}

$version = WorkspaceVersion
$null = Get-ConptyManifest $Target
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

$buildsReleaseMsvc = $Configuration -eq "release" -and
    $Target -match '-pc-windows-msvc$' -and
    -not $SkipBuild -and
    -not $ReuseReleaseBinaries
$originalRustFlags = $env:RUSTFLAGS
if ($buildsReleaseMsvc -and $originalRustFlags -notlike "*+crt-static*") {
    $env:RUSTFLAGS = "$originalRustFlags -C target-feature=+crt-static".Trim()
}
try {
    if (-not $SkipBuild -and -not $ReuseReleaseBinaries) {
        Invoke-Notices (@("build", "--binary-dir", (Split-Path -Parent $binary), "--", "cargo") + $cargoArgs)
    }
} finally {
    if ($null -eq $originalRustFlags) {
        Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue
    } else {
        $env:RUSTFLAGS = $originalRustFlags
    }
}

$noticeRequirement = if ($SkipBuild) { "--optional" } else { "--required" }
Invoke-Notices @("verify", "--binary-dir", (Split-Path -Parent $binary), $noticeRequirement)

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
if ($ReuseReleaseBinaries) {
    $head = GitOutput @("rev-parse", "HEAD")
    $trackedStatus = GitOutput @("status", "--porcelain", "--untracked-files=no")
    if (-not [string]::IsNullOrWhiteSpace($trackedStatus)) {
        Fail "-ReuseReleaseBinaries requires a clean tracked worktree"
    }
    ValidateReleaseBinaryManifest `
        ([System.IO.Path]::GetFullPath($ReleaseBinaryManifest)) `
        $head `
        $Target `
        $Configuration `
        $binary `
        $helperBinary `
        $daemonBinary
}
if ($Configuration -eq "release" -and $Target -match '-pc-windows-msvc$') {
    & (Join-Path $PSScriptRoot "assert-windows-static-crt.ps1") `
        -Binary $binary `
        -HelperBinary $helperBinary `
        -DaemonBinary $daemonBinary
    if ($LASTEXITCODE -ne 0) {
        Fail "Windows static CRT verification failed"
    }
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
# The two directories the ConPTY runtime and the generated files are written
# into directly. Every placement that comes from the layout plan creates its
# own parent.
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "share/rmux") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "libexec/rmux") | Out-Null

$completionDir = Join-Path ([System.IO.Path]::GetTempPath()) "rmux-completions-$([System.Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $completionDir | Out-Null
try {
    # Which completion files this package needs is the descriptor's answer,
    # not a list kept here: a source renamed there is generated, cached and
    # staged under its new name.
    $completionFiles = @(Invoke-LayoutTool @("--with-notices", "source-names", "--variant", "windows-archive",
        "--kind", "completion"))
    # The same projection, with the shell each completion is generated from:
    # the key the generator reports its own files under.
    $completionOrigins = @(Invoke-LayoutTool @("--with-notices", "source-names", "--variant", "windows-archive",
        "--kind", "completion", "--with-origin"))
    if (-not $SkipBuild) {
        # The generator writes under its own names and says what it wrote.
        # Those files are then prepared under the names the descriptor gives
        # them, so a renamed source is served by the generator's real output
        # instead of being looked for under a name nothing produces.
        $generatedDir = Join-Path $completionDir "generated"
        $reportedCompletions = @(cargo run --quiet --package xtask -- generate-completions --output-dir $generatedDir)
        if ($LASTEXITCODE -ne 0) {
            Fail "failed to generate shell completions"
        }
        Copy-LayoutGeneratedSources -Reported $reportedCompletions `
            -Expected $completionOrigins -To $completionDir | Out-Null
        try {
            if (Test-Path -LiteralPath $completionCache) {
                Remove-Item -LiteralPath $completionCache -Recurse -Force -ErrorAction SilentlyContinue
            }
            New-Item -ItemType Directory -Force -Path $completionCache -ErrorAction Stop | Out-Null
            Copy-LayoutSourceFiles -Names $completionFiles -From $completionDir `
                -To $completionCache -Missing "generated completion is missing" | Out-Null
        } catch {
            Write-Warning "unable to refresh completion cache ${completionCache}: $_"
        }
    } else {
        Copy-LayoutSourceFiles -Names $completionFiles -From $completionCache `
            -To $completionDir `
            -Missing ("-SkipBuild requires prebuilt completions in $completionCache; " +
                "rerun without -SkipBuild or set RMUX_COMPLETIONS_DIR") | Out-Null
    }

    # What this package carries from the build, the repository and the
    # completions is the projection of the shared layout descriptor: which
    # file is read, and where it is placed, are both the plan's answer. A
    # source renamed in the descriptor is read under its new name here.
    $stagePlan = @(Invoke-LayoutTool @("--with-notices", "stage-plan", "--variant", "windows-archive",
        "--repo", (Get-Location).Path,
        "--binaries", (Split-Path -Parent $binary),
        "--completions", $completionDir,
        "--only-sources", "binary,repo,completion"))
    Copy-LayoutStagePlan -Plan $stagePlan -Stage $stageDir | Out-Null
} finally {
    if (Test-Path -LiteralPath $completionDir) {
        Remove-Item -LiteralPath $completionDir -Recurse -Force
    }
}

Invoke-Notices @("stage", "--binary-dir", (Split-Path -Parent $binary),
    "--root", $stageDir, "--variant", "windows-archive", $noticeRequirement)

# The ConPTY runtime keeps its own native acquisition: a digest-pinned
# archive, the two pinned entries, their PE machine and their signatures. Its
# placements are declared by the runtime manifest and held against the
# descriptor by the tree check below.
Copy-ConptyRuntime $Target $ConptyPackage $stageDir
Assert-ConptyPackage $stageDir $Target

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
$releaseArtifact = ($Configuration -eq "release") -and
    ((-not $SkipBuild -and -not $ReuseReleaseBinaries) -or $ReuseReleaseBinaries) -and
    (-not $gitDirty)
$generatedAtUtc = GitOutput @("show", "-s", "--format=%cI", "HEAD")

# The format identity the package declares is the descriptor's, so a change of
# package contract moves it in one place for builders, validators and
# installers at once.
$packageLayoutId = Get-LayoutLine @("--with-notices", "layout-id", "--variant", "windows-archive")

$metadata = [ordered]@{
    schema = 1
    artifact_kind = "windows-package-binary"
    binary_path = "rmux.exe"
    binary_sha256 = $binarySha256
    binary_bytes = $binaryBytes
    helper_binary_path = "libexec/rmux/rmux.exe"
    helper_binary_sha256 = $helperBinarySha256
    helper_binary_bytes = $helperBinaryBytes
    daemon_binary_path = "rmux-daemon.exe"
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
    package_layout = $packageLayoutId
    conpty_runtime_manifest = "share/rmux/conpty-runtime.json"
    archive_format = "zip"
    skip_build = [bool]$SkipBuild
    reuse_release_binaries = [bool]$ReuseReleaseBinaries
    release_artifact = $releaseArtifact
    generated_at_utc = $generatedAtUtc
}
$metadataPath = Join-Path $stageDir "share/rmux/artifact-metadata.json"
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $metadataPath) | Out-Null
$metadata | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $metadataPath -Encoding utf8

# The installer inside the package installs what the shared layout descriptor
# projects, so that projection travels with the package.
$layoutManifestPath = Join-Path $stageDir "share/rmux/package-layout.tsv"
$layoutManifestLines = @(Invoke-LayoutTool @("--with-notices", "manifest", "--variant", "windows-archive"))
WriteAsciiLfFile $layoutManifestPath $layoutManifestLines

WritePackageChecksums $stageDir (Join-Path $stageDir "SHA256SUMS.txt")

# Refuse a staged package that omits a declared file or carries an extra one.
Invoke-LayoutTool @("--with-notices", "check-tree", "--variant", "windows-archive", "--root", $stageDir) | Out-Null

if (Test-Path -LiteralPath $archivePath) {
    Remove-Item -LiteralPath $archivePath -Force
}
Write-ReproducibleWindowsArchive $stageDir $archivePath $generatedAtUtc

$archiveSha256 = Sha256File $archivePath
WriteAsciiLfFile $checksumsPath @("$archiveSha256  $([System.IO.Path]::GetFileName($archivePath))")
Invoke-Notices @("publish", "--binary-dir", (Split-Path -Parent $binary), "--archive", $archivePath, $noticeRequirement)

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
