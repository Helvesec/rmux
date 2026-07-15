param(
    [string]$Version = "latest",
    [string]$Repository = "Helvesec/rmux",
    [string]$InstallDir = "$env:LOCALAPPDATA\rmux\bin",
    [switch]$NoVerify
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# GitHub requires TLS 1.2+; Windows PowerShell 5.1 does not negotiate it by
# default, which fails the release download with an opaque error.
if ([System.Net.ServicePointManager]::SecurityProtocol -notmatch 'Tls12') {
    [System.Net.ServicePointManager]::SecurityProtocol = `
        [System.Net.ServicePointManager]::SecurityProtocol -bor [System.Net.SecurityProtocolType]::Tls12
}

function Fail([string]$Message) {
    # Under `irm ... | iex` there is no script file and `exit` would close the
    # user's interactive shell; throw instead so only the installer stops.
    if ([string]::IsNullOrWhiteSpace($PSCommandPath)) {
        throw "rmux install: $Message"
    }
    Write-Error "rmux install: $Message"
    exit 1
}

function Invoke-NativeCapture([string]$Program, [string[]]$Arguments) {
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $output = & $Program @Arguments 2>&1
        $status = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }

    [pscustomobject]@{
        Output = $output
        Status = $status
    }
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

function Copy-Tree([string]$Source, [string]$Destination) {
    if (-not (Test-Path -LiteralPath $Source -PathType Container)) {
        return
    }

    New-Item -ItemType Directory -Force -Path $Destination | Out-Null
    Get-ChildItem -LiteralPath $Source -Force | ForEach-Object {
        Copy-Item -Recurse -Force -LiteralPath $_.FullName -Destination $Destination
    }
}

function Assert-BinaryReplaceable([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "destination binary path exists but is not a file: $Path"
    }

    $stream = $null
    try {
        $stream = [System.IO.File]::Open(
            $Path,
            [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::ReadWrite,
            [System.IO.FileShare]::None
        )
    } catch {
        throw "destination binary is in use or cannot be replaced safely: $Path"
    } finally {
        if ($null -ne $stream) {
            $stream.Dispose()
        }
    }
}

function Install-BinarySet([object[]]$Plan, [bool]$Verify) {
    # Refuse the whole upgrade before its first mutation if any installed
    # executable is already locked (most commonly a running rmux daemon).
    foreach ($entry in $Plan) {
        Assert-BinaryReplaceable $entry.Destination
    }

    $transactionRoot = Join-Path ([System.IO.Path]::GetTempPath()) `
        ("rmux-install-backup-" + [System.Guid]::NewGuid().ToString("N"))
    $backups = @()
    $preserveTransactionBackup = $false
    New-Item -ItemType Directory -Path $transactionRoot | Out-Null

    try {
        for ($index = 0; $index -lt $Plan.Count; $index++) {
            $entry = $Plan[$index]
            $destinationDirectory = Split-Path -Parent $entry.Destination
            New-Item -ItemType Directory -Force -Path $destinationDirectory | Out-Null

            $existed = Test-Path -LiteralPath $entry.Destination -PathType Leaf
            $backup = Join-Path $transactionRoot ("binary-$index.exe")
            if ($existed) {
                Copy-Item -LiteralPath $entry.Destination -Destination $backup
            }
            $backups += [pscustomobject]@{
                Destination = $entry.Destination
                Backup = $backup
                Existed = $existed
            }
        }

        # Close the small check-to-copy window created while taking backups.
        foreach ($entry in $Plan) {
            Assert-BinaryReplaceable $entry.Destination
        }

        foreach ($entry in $Plan) {
            Copy-Item -Force -LiteralPath $entry.Source -Destination $entry.Destination
        }
        if ($Verify) {
            Verify-InstalledLayout $Plan[$Plan.Count - 1].Destination
        }
    } catch {
        $installError = $_.Exception.Message
        $rollbackErrors = @()
        for ($index = $backups.Count - 1; $index -ge 0; $index--) {
            $record = $backups[$index]
            try {
                if ($record.Existed) {
                    Copy-Item -Force -LiteralPath $record.Backup -Destination $record.Destination
                    if ((Sha256File $record.Backup) -ne (Sha256File $record.Destination)) {
                        throw "restored binary does not match its backup: $($record.Destination)"
                    }
                } else {
                    if (Test-Path -LiteralPath $record.Destination) {
                        Remove-Item -Force -LiteralPath $record.Destination -ErrorAction Stop
                    }
                    if (Test-Path -LiteralPath $record.Destination) {
                        throw "new binary remained after rollback: $($record.Destination)"
                    }
                }
            } catch {
                $rollbackErrors += $_.Exception.Message
            }
        }

        if ($rollbackErrors.Count -gt 0) {
            $preserveTransactionBackup = $true
            $recoveryActions = @()
            foreach ($record in $backups) {
                if ($record.Existed) {
                    $recoveryActions += "restore '$($record.Backup)' to '$($record.Destination)'"
                } else {
                    $recoveryActions += "remove newly created '$($record.Destination)' if it exists"
                }
            }
            $recovery = $recoveryActions -join "; "
            $errorMessage = "binary install failed: $installError; "
            $errorMessage += "rollback also failed: $($rollbackErrors -join '; '); "
            $errorMessage += "recovery backup preserved at '$transactionRoot'. "
            $errorMessage += "Stop running rmux processes, inspect the affected files, then manually $recovery"
            throw $errorMessage
        }
        throw "binary install failed; previous binaries restored: $installError"
    } finally {
        if (-not $preserveTransactionBackup) {
            Remove-Item -Recurse -Force -LiteralPath $transactionRoot -ErrorAction SilentlyContinue
        }
    }
}

function Test-PackageRoot([string]$Root) {
    foreach ($required in @("rmux.exe", "libexec\rmux\rmux.exe", "rmux-daemon.exe")) {
        if (-not (Test-Path -LiteralPath (Join-Path $Root $required) -PathType Leaf)) {
            return $false
        }
    }
    return $true
}

function Resolve-LatestVersion([string]$RepositoryName) {
    $response = Invoke-WebRequest `
        -UseBasicParsing `
        -Headers @{ "User-Agent" = "rmux-installer" } `
        -Uri "https://api.github.com/repos/$RepositoryName/releases/latest"
    $release = $response.Content | ConvertFrom-Json
    if ([string]::IsNullOrWhiteSpace($release.tag_name)) {
        Fail "latest release response did not include tag_name"
    }
    $release.tag_name
}

function Get-LocalPackageRoot {
    $scriptPath = $MyInvocation.ScriptName
    if ([string]::IsNullOrWhiteSpace($scriptPath)) {
        $scriptPath = $PSCommandPath
    }
    if ([string]::IsNullOrWhiteSpace($scriptPath)) {
        return $null
    }

    $root = Split-Path -Parent ([System.IO.Path]::GetFullPath($scriptPath))
    if (Test-PackageRoot $root) {
        return $root
    }
    $null
}

function Verify-InstalledLayout([string]$RmuxBinary) {
    $result = Invoke-NativeCapture $RmuxBinary @("--help")
    $output = $result.Output -join "`n"
    if (($result.Status -ne 0 -and $result.Status -ne 1) -or
        $output -notmatch 'usage: rmux') {
        Fail "installed rmux could not reach its full CLI helper`n$output"
    }
}

function Install-PackageRoot([string]$PackageRoot, [string]$DestinationBin, [bool]$Verify) {
    $installBin = [System.IO.Path]::GetFullPath($DestinationBin)
    $installRoot = Split-Path -Parent $installBin

    foreach ($required in @("rmux.exe", "libexec\rmux\rmux.exe", "rmux-daemon.exe")) {
        $requiredPath = Join-Path $PackageRoot $required
        if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
            Fail "archive is missing $required"
        }
    }

    $binaryPlan = @(
        [pscustomobject]@{
            Source = Join-Path $PackageRoot "libexec\rmux\rmux.exe"
            Destination = Join-Path $installRoot "libexec\rmux\rmux.exe"
        },
        [pscustomobject]@{
            Source = Join-Path $PackageRoot "rmux-daemon.exe"
            Destination = Join-Path $installBin "rmux-daemon.exe"
        },
        [pscustomobject]@{
            Source = Join-Path $PackageRoot "rmux.exe"
            Destination = Join-Path $installBin "rmux.exe"
        }
    )

    try {
        Install-BinarySet $binaryPlan $Verify
    } catch {
        Fail $_.Exception.Message
    }

    # Non-executable assets are installed only after the versioned binary set
    # has committed successfully, so a locked daemon cannot leave mixed assets.
    Copy-Tree (Join-Path $PackageRoot "share") (Join-Path $installRoot "share")

    foreach ($optional in @("README.md", "LICENSE-APACHE", "LICENSE-MIT", "rmux.1", "SHA256SUMS.txt")) {
        $source = Join-Path $PackageRoot $optional
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Copy-Item -Force -LiteralPath $source -Destination (Join-Path $installRoot $optional)
        }
    }

    $destination = Join-Path $installBin "rmux.exe"

    Write-Host "Installed rmux to $destination"

    $pathParts = $env:Path -split [System.IO.Path]::PathSeparator
    if ($pathParts -notcontains $installBin) {
        Write-Host "Add $installBin to PATH if rmux is not found by PowerShell."
    }
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    Fail "InstallDir must not be empty"
}

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($arch -ne [System.Runtime.InteropServices.Architecture]::X64) {
    Fail "Windows prebuilt binary is only available for x64. Use: cargo install rmux --locked"
}

$localPackageRoot = Get-LocalPackageRoot
if ($localPackageRoot) {
    Install-PackageRoot $localPackageRoot $InstallDir (-not $NoVerify)
    exit 0
}

if ($Version -eq "latest") {
    $Version = Resolve-LatestVersion $Repository
}
if ($Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$') {
    Fail "invalid version: $Version"
}

$semver = $Version -replace '^v', ''
$platform = "windows-x86_64"
$archive = "rmux-$semver-$platform.zip"
$baseUrl = "https://github.com/$Repository/releases/download/$Version"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("rmux-install-" + [System.Guid]::NewGuid().ToString("N"))

New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    $zipPath = Join-Path $tmp $archive
    $sumsPath = Join-Path $tmp "SHA256SUMS"

    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/SHA256SUMS" -OutFile $sumsPath
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$archive" -OutFile $zipPath

    $line = Get-Content -LiteralPath $sumsPath |
        Where-Object { $_ -match "\s+$([regex]::Escape($archive))$" } |
        Select-Object -First 1
    if (-not $line) {
        Fail "checksum entry not found for $archive"
    }

    $expected = (($line -split "\s+")[0]).ToLowerInvariant()
    $actual = Sha256File $zipPath
    if ($actual -ne $expected) {
        Fail "checksum mismatch for $archive"
    }

    Expand-Archive -Force -LiteralPath $zipPath -DestinationPath $tmp
    $packageRoot = Join-Path $tmp "rmux-$semver-$platform"
    if (-not (Test-PackageRoot $packageRoot)) {
        Fail "required rmux package layout not found in archive"
    }

    Install-PackageRoot $packageRoot $InstallDir (-not $NoVerify)
    # Normalize the exit code the same way the local branch does with `exit 0`.
    # The install succeeded, but Verify-InstalledLayout's `rmux --help` probe
    # leaves $LASTEXITCODE at the usage exit code (1); a caller that trusts the
    # exit code (as verify-package-windows.ps1 does) would misread success as
    # failure. Reset without `exit`, which under `irm | iex` would close the
    # user's shell on a successful install.
    $global:LASTEXITCODE = 0
} finally {
    Remove-Item -Recurse -Force -LiteralPath $tmp -ErrorAction SilentlyContinue
}
