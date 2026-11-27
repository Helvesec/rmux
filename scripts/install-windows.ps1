param(
    [string]$Version = "latest",
    [string]$Repository = "Helvesec/rmux",
    [string]$InstallDir = "$env:LOCALAPPDATA\rmux\bin",
    [switch]$NoVerify,
    # Explicit ownership witness for pruning obsolete files during version rollback.
    [string]$PreviousPackageRoot = "",
    [switch]$Uninstall
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

function Assert-FileReplaceable([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "destination file path exists but is not a file: $Path"
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
        throw "destination file is in use or cannot be replaced safely: $Path"
    } finally {
        if ($null -ne $stream) {
            $stream.Dispose()
        }
    }
}

function Invoke-InstallCheckpoint([string]$Name) {
    if ($env:RMUX_INSTALL_TEST_FAIL_AT -eq $Name) {
        throw "injected installer failure at $Name"
    }
}

function Enter-InstallTransactionLock([string]$InstallRoot) {
    New-Item -ItemType Directory -Force -Path $InstallRoot | Out-Null
    $lockPath = Join-Path $InstallRoot ".rmux-install.lock"
    $deadline = [System.DateTime]::UtcNow.AddSeconds(30)
    $reportedWait = $false

    while ($true) {
        try {
            $stream = [System.IO.File]::Open(
                $lockPath,
                [System.IO.FileMode]::OpenOrCreate,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
            return [pscustomobject]@{
                Path = $lockPath
                Stream = $stream
            }
        } catch [System.IO.IOException] {
            $win32Error = $_.Exception.HResult -band 0xFFFF
            if ($win32Error -ne 32 -and $win32Error -ne 33) {
                throw
            }
            if ([System.DateTime]::UtcNow -ge $deadline) {
                throw "timed out waiting for another rmux installation to finish"
            }
            if (-not $reportedWait) {
                if (-not [string]::IsNullOrWhiteSpace($env:RMUX_INSTALL_TEST_LOCK_WAIT_EVENT)) {
                    try {
                        $waitEvent = [System.Threading.EventWaitHandle]::OpenExisting(
                            $env:RMUX_INSTALL_TEST_LOCK_WAIT_EVENT
                        )
                        try {
                            $null = $waitEvent.Set()
                        } finally {
                            $waitEvent.Dispose()
                        }
                    } catch {
                        # Test-only observer disappeared; lock behavior is unchanged.
                    }
                }
                Write-Host "Another rmux installation is updating this destination; waiting up to 30 seconds."
                $reportedWait = $true
            }
            Start-Sleep -Milliseconds 50
        }
    }
}

function Exit-InstallTransactionLock([object]$Lock) {
    try {
        $Lock.Stream.Dispose()
    } finally {
        # A waiter may acquire the same file between Dispose and Remove-Item.
        # FileShare.None makes that cleanup race safe: deletion then fails and
        # the new owner removes the marker after releasing its own handle.
        Remove-Item -Force -LiteralPath $Lock.Path -ErrorAction SilentlyContinue
    }
}

# Explicit rollback uses a separately qualified previous package as the
# ownership witness. Installed metadata alone never authorizes deletion.
function Assert-NoPackageReparse([string]$Path) {
    $paths = @()
    for ($part = [IO.Path]::GetFullPath($Path); $part; $part = Split-Path -Parent $part) { $paths += $part }
    for ($i = $paths.Count - 1; $i -ge 0; $i--) {
        if (Test-Path -LiteralPath $paths[$i]) {
            if ((Get-Item -LiteralPath $paths[$i] -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "package ownership path is a reparse point: $($paths[$i])"
            }
        }
    }
}

function Resolve-PackageOwnershipPath([string]$Root, [string]$Relative) {
    if ($Relative -cnotmatch '^[A-Za-z0-9_-][A-Za-z0-9_./-]*$') { throw "unsafe ownership path: $Relative" }
    foreach ($part in ($Relative -split '/')) {
        if (-not $part -or $part -in @('.', '..') -or $part.EndsWith('.') -or
                $part -match '^(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])(\.|$)') { throw "ambiguous ownership path: $Relative" }
    }
    $rootFull = [IO.Path]::GetFullPath($Root).TrimEnd([char]'\', [char]'/')
    $full = [IO.Path]::GetFullPath((Join-Path $rootFull $Relative))
    if (-not $full.StartsWith($rootFull + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'ownership path escapes prefix' }
    Assert-NoPackageReparse $full
    return $full
}

function Get-ExplicitRollbackPlan([string]$PreviousRoot, [string]$InstallBin, [object[]]$NewPlan) {
    $previous = [IO.Path]::GetFullPath($PreviousRoot).TrimEnd([char]'\', [char]'/')
    $installRoot = Split-Path -Parent $InstallBin
    Assert-NoPackageReparse $previous
    Assert-NoPackageReparse $installRoot
    if ($previous.StartsWith($installRoot + '\', [StringComparison]::OrdinalIgnoreCase) -or
            $installRoot.StartsWith($previous + '\', [StringComparison]::OrdinalIgnoreCase) -or
            $previous -ieq $installRoot) { throw 'previous package must be separate from installed prefix' }
    $metadataPath = Resolve-PackageOwnershipPath $previous 'share/rmux/artifact-metadata.json'
    $metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
    if ($metadata.package_layout -cnotin @('rmux-windows-package-v3','rmux-windows-package-v4') -or $metadata.git_dirty -or
            $metadata.git_commit -cnotmatch '^[0-9a-f]{40}$') { throw 'previous package identity is not a clean manifest package' }
    $checksums = @{}
    $sumPath = Resolve-PackageOwnershipPath $previous 'SHA256SUMS.txt'
    foreach ($line in (Get-Content -LiteralPath $sumPath)) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        if ($line -cnotmatch '^([0-9a-f]{64})  (.+)$') { throw 'invalid previous package checksum' }
        $digest = $Matches[1]; $relative = $Matches[2]
        $source = Resolve-PackageOwnershipPath $previous $relative
        if ($checksums.ContainsKey($relative) -or (Sha256File $source) -cne $digest) { throw 'previous package checksum mismatch or duplicate' }
        $checksums[$relative] = $digest
    }
    if ($checksums.ContainsKey('SHA256SUMS.txt')) { throw 'ambiguous previous checksum self-entry' }
    $checksums['SHA256SUMS.txt'] = Sha256File $sumPath
    $manifestPath = Resolve-PackageOwnershipPath $previous 'share/rmux/package-layout.tsv'
    foreach ($required in @('share/rmux/package-layout.tsv', 'share/rmux/artifact-metadata.json')) {
        if (-not $checksums.ContainsKey($required)) { throw 'previous identity or layout is not checksummed' }
    }
    $owned = @{}; $sources = @{}; $ids = @{}
    foreach ($line in (Get-Content -LiteralPath $manifestPath)) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $fields = $line -split "`t"
        if ($fields.Count -ne 6 -or $fields[3] -cnotin @('required','optional') -or
                $fields[5] -cnotin @('executable','file') -or $fields[4] -cnotmatch '^[a-z0-9][a-z0-9-]*$') { throw 'invalid previous layout entry' }
        $source = Resolve-PackageOwnershipPath $previous $fields[0]
        if ($sources.ContainsKey($fields[0]) -or $ids.ContainsKey($fields[4]) -or -not $checksums.ContainsKey($fields[0])) { throw 'ambiguous or unchecked previous layout entry' }
        $sources[$fields[0]] = $true; $ids[$fields[4]] = $true
        if ($fields[2] -ceq '-') { continue }
        # Match the installer's existing bin/ override exactly.
        $destination = if ($fields[2] -clike 'bin/*') {
            Resolve-PackageOwnershipPath $InstallBin $fields[2].Substring(4)
        } else { Resolve-PackageOwnershipPath $installRoot $fields[2] }
        if ($owned.ContainsKey($destination)) { throw 'duplicate previous destination' }
        if (-not (Test-Path -LiteralPath $destination -PathType Leaf) -or
                (Sha256File $destination) -cne $checksums[$fields[0]]) { throw "previous installed identity differs: $destination" }
        $owned[$destination] = $checksums[$fields[0]]
    }
    if ($sources.Count -ne $checksums.Count -or -not $owned.ContainsKey((Join-Path $InstallBin 'rmux.exe'))) { throw 'previous layout/checksum population differs' }
    $placed = @{}; $plan = @()
    foreach ($entry in $NewPlan) {
        $destination = [IO.Path]::GetFullPath($entry.Destination)
        if (-not $destination.StartsWith($installRoot + '\', [StringComparison]::OrdinalIgnoreCase) -or $placed.ContainsKey($destination)) { throw 'new destination escapes prefix or is ambiguous' }
        Assert-NoPackageReparse $destination
        Assert-NoPackageReparse $entry.Source
        if ((Test-Path -LiteralPath $destination) -and -not $owned.ContainsKey($destination)) { throw 'new destination collides with an unowned file' }
        $placed[$destination] = $true
        $plan += [pscustomobject]@{Source=$entry.Source; Destination=$destination; PreviousSha256=$owned[$destination]; Remove=$false}
    }
    foreach ($destination in @($owned.Keys | Sort-Object)) {
        if (-not $placed.ContainsKey($destination)) {
            $plan += [pscustomobject]@{Source=$null; Destination=$destination; PreviousSha256=$owned[$destination]; Remove=$true}
        }
    }
    return $plan
}

function Assert-PlannedPackageFile([object]$Entry) {
    Assert-FileReplaceable $Entry.Destination
    if ($Entry.PSObject.Properties.Name -contains 'PreviousSha256') {
        Assert-NoPackageReparse $Entry.Destination
        if ($Entry.PreviousSha256) {
            if ((Sha256File $Entry.Destination) -cne $Entry.PreviousSha256) { throw "previous installed file changed: $($Entry.Destination)" }
        } elseif (Test-Path -LiteralPath $Entry.Destination) { throw 'unowned destination appeared during rollback' }
    }
}

function Install-PackageFileSet(
    [object[]]$Plan,
    [string]$VerifyBinary,
    [bool]$Verify,
    [switch]$RemoveOnly
) {
    if ($RemoveOnly -and (-not $Verify -or -not $Plan.Count -or
            @($Plan | Where-Object { $_.PSObject.Properties.Name -notcontains 'Remove' -or -not $_.Remove -or
                $_.PSObject.Properties.Name -notcontains 'PreviousSha256' -or -not $_.PreviousSha256 }).Count)) {
        throw 'uninstall transaction requires a verified removal-only ownership plan'
    }
    # Refuse the whole upgrade before its first mutation if any installed
    # package file is already locked (most commonly a running rmux daemon).
    foreach ($entry in $Plan) {
        Assert-PlannedPackageFile $entry
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
            $backup = Join-Path $transactionRoot ("file-$index")
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
            Assert-PlannedPackageFile $entry
        }

        $removed = 0
        foreach ($entry in $Plan) {
            if ($entry.PSObject.Properties.Name -contains 'Remove' -and $entry.Remove) {
                Assert-PlannedPackageFile $entry
                Remove-Item -Force -LiteralPath $entry.Destination -ErrorAction Stop
                if (Test-Path -LiteralPath $entry.Destination) { throw "removed package file remains: $($entry.Destination)" }
                $removed++
                if ($RemoveOnly -and $removed -eq 1) { Invoke-InstallCheckpoint 'after-remove-package-first' }
            } else {
                Copy-Item -Force -LiteralPath $entry.Source -Destination $entry.Destination
            }
        }
        if ($RemoveOnly) {
            foreach ($entry in $Plan) {
                Assert-NoPackageReparse $entry.Destination
                if (Test-Path -LiteralPath $entry.Destination) { throw "uninstall postcondition failed: $($entry.Destination)" }
            }
        } else {
            Invoke-InstallCheckpoint "after-copy-package"
            if ($Verify) { Verify-InstalledLayout $VerifyBinary }
        }
    } catch {
        $installError = $_.Exception.Message
        $rollbackErrors = @()
        for ($index = $backups.Count - 1; $index -ge 0; $index--) {
            $record = $backups[$index]
            try {
                if ($record.Existed) {
                    # An unchanged executable may be locked again after preflight.
                    if (-not (Test-Path -LiteralPath $record.Destination -PathType Leaf) -or
                            (Sha256File $record.Backup) -ne (Sha256File $record.Destination)) {
                        Copy-Item -Force -LiteralPath $record.Backup -Destination $record.Destination
                    }
                    if ((Sha256File $record.Backup) -ne (Sha256File $record.Destination)) {
                        throw "restored file does not match its backup: $($record.Destination)"
                    }
                } else {
                    if (Test-Path -LiteralPath $record.Destination) {
                        Remove-Item -Force -LiteralPath $record.Destination -ErrorAction Stop
                    }
                    if (Test-Path -LiteralPath $record.Destination) {
                        throw "new file remained after rollback: $($record.Destination)"
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
            $errorMessage = "package install failed: $installError; "
            $errorMessage += "rollback also failed: $($rollbackErrors -join '; '); "
            $errorMessage += "recovery backup preserved at '$transactionRoot'. "
            $errorMessage += "Stop running rmux processes, inspect the affected files, then manually $recovery"
            throw $errorMessage
        }
        throw "package install failed; previous package restored: $installError"
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

function Uninstall-PackageRoot([string]$PreviousPackageRoot, [string]$DestinationBin) {
    foreach ($name in @('PreviousPackageRoot', 'DestinationBin')) {
        if (-not $PSBoundParameters.ContainsKey($name) -or [string]::IsNullOrWhiteSpace($PSBoundParameters[$name])) {
            throw "uninstall requires an explicit nonempty $name"
        }
        if ($PSBoundParameters[$name] -notmatch '^[A-Za-z]:[\\/]' -or $PSBoundParameters[$name] -match '(^|[\\/])\.{1,2}([\\/]|$)|[\\/]{2}') {
            throw "uninstall requires a canonical absolute local $name"
        }
    }
    # Win32 trims trailing dots/spaces and treats device names specially.
    # Reject aliases in either root before resolving any ownership destination.
    foreach ($path in @($PreviousPackageRoot, $DestinationBin)) {
        foreach ($part in ($path.Substring(3).TrimEnd([char]'\', [char]'/') -split '[\\/]')) {
            if (-not $part -or $part -match '[. ]$|:' -or $part -match '^(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])(\.|$)') {
                throw 'uninstall root contains an ambiguous path segment'
            }
        }
    }
    $installBin = [IO.Path]::GetFullPath($DestinationBin).TrimEnd([char]'\', [char]'/')
    $installRoot = Split-Path -Parent $installBin
    if (-not $installRoot -or $installRoot -eq [IO.Path]::GetPathRoot($installRoot)) { throw 'uninstall destination is too broad' }
    Assert-NoPackageReparse $installBin
    if (-not (Test-Path -LiteralPath $installBin -PathType Container)) { throw 'uninstall destination does not exist' }
    $lockPath = Join-Path $installRoot '.rmux-install.lock'
    Assert-NoPackageReparse $lockPath
    if (Test-Path -LiteralPath $lockPath) { throw 'uninstall requires an unoccupied transaction lock path' }
    $installLock = $null
    try {
        $installLock = Enter-InstallTransactionLock $installRoot
        # An empty new layout makes the existing ownership engine plan only
        # removals. Missing/changed files (including repeat calls) fail closed.
        $plan = @(Get-ExplicitRollbackPlan $PreviousPackageRoot $installBin @())
        if (-not $plan.Count) { throw 'uninstall has no qualified owned files' }
        Install-PackageFileSet $plan '' $true -RemoveOnly
    } finally {
        if ($null -ne $installLock) { Exit-InstallTransactionLock $installLock }
    }
    Write-Host "Uninstalled verified package files from $installRoot; unrelated files and directories retained."
}

function Install-PackageRoot(
    [string]$PackageRoot, [string]$DestinationBin, [bool]$Verify,
    [string]$PreviousPackageRoot = ""
) {
    # Keep omission distinct from an explicitly supplied invalid witness,
    # including direct function calls that do not pass through the CLI guard.
    $explicitRollback = $PSBoundParameters.ContainsKey('PreviousPackageRoot')
    if ($explicitRollback -and [string]::IsNullOrWhiteSpace($PreviousPackageRoot)) {
        throw "PreviousPackageRoot must not be empty or whitespace when explicitly supplied"
    }
    if ($explicitRollback -and -not $Verify) {
        throw "PreviousPackageRoot requires a local package and enabled verification"
    }
    $installBin = [System.IO.Path]::GetFullPath($DestinationBin)
    $installRoot = Split-Path -Parent $installBin

    # The package formats this installer understands, and how each one is laid
    # out. An archive declares its own format in its artifact metadata;
    # nothing is inferred from what a package happens to contain.
    #
    #   rmux-windows-package-v3/v4 carries share/rmux/package-layout.tsv, the
    #                            projection of the layout descriptor the
    #                            package was built from, and is installed from
    #                            it. Only the native transaction, the ConPTY
    #                            verification and the bin/ override stay here.
    #   rmux-windows-package-v2  predates that manifest. It is installed from
    #                            the layout that produced it, listed below, so
    #                            a release built before the manifest stays
    #                            installable by this script.
    #
    # tests/package_layout_spec.py holds these identities against the
    # descriptor, so the two cannot drift apart.
    $layoutCurrent = "rmux-windows-package-v3"
    $layoutLegacy = "rmux-windows-package-v2"
    $legacyLayout = @(
        [pscustomobject]@{ Path = "libexec\rmux\rmux.exe"; InBin = $false },
        [pscustomobject]@{ Path = "rmux-daemon.exe"; InBin = $true },
        [pscustomobject]@{ Path = "rmux.exe"; InBin = $true }
    )

    $metadataPath = Join-Path $PackageRoot "share/rmux/artifact-metadata.json"
    $metadata = $null
    $packageLayout = ""
    if (Test-Path -LiteralPath $metadataPath -PathType Leaf) {
        # Metadata that cannot be read is not metadata that declares nothing:
        # it is answered on its own terms, like the Unix installer does. An
        # empty file reaches the parser as nothing at all, which it reports as
        # a binding failure rather than a refusal, so it is named here.
        $metadataText = Get-Content -LiteralPath $metadataPath -Raw
        if ([string]::IsNullOrWhiteSpace($metadataText)) {
            Fail "archive metadata is not readable JSON: share/rmux/artifact-metadata.json"
        }
        try {
            $metadata = $metadataText | ConvertFrom-Json
        } catch {
            Fail "archive metadata is not readable JSON: share/rmux/artifact-metadata.json"
        }
        if ($metadata -isnot [System.Management.Automation.PSCustomObject]) {
            Fail "archive metadata is not a JSON object: share/rmux/artifact-metadata.json"
        }
        if ($metadata.PSObject.Properties.Name -contains "package_layout") {
            $packageLayout = [string]$metadata.package_layout
            if ([string]::IsNullOrWhiteSpace($packageLayout)) {
                Fail "archive metadata declares a package layout this installer cannot read: share/rmux/artifact-metadata.json"
            }
        }
    }

    $manifestPath = Join-Path $PackageRoot "share/rmux/package-layout.tsv"

    # An archive that declares no format at all predates the declaration. It
    # is installed as $layoutLegacy only when it really is that layout: the
    # complete executable set, and no layout manifest to contradict it. That
    # is a narrow, named shape, not a rule that accepts whatever carries no
    # manifest - a truncated or inconsistent current package matches neither
    # condition and is refused below like any other.
    $undeclaredLegacy = $false
    if ($packageLayout -ceq "") {
        if (Test-Path -LiteralPath $manifestPath -PathType Leaf) {
            Fail "archive carries a layout manifest but declares no package layout"
        }
        foreach ($entry in $legacyLayout) {
            if (-not (Test-Path -LiteralPath (Join-Path $PackageRoot $entry.Path) -PathType Leaf)) {
                Fail ("archive declares no package layout and does not carry the " +
                    "$layoutLegacy layout: $($entry.Path) is missing")
            }
        }
        $packageLayout = $layoutLegacy
        $undeclaredLegacy = $true
    }
    $installPlan = @()
    $dispatcherDestination = $null

    if ($packageLayout -ceq $layoutCurrent -or $packageLayout -ceq 'rmux-windows-package-v4') {
        if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
            Fail "archive declares $layoutCurrent but is missing its layout manifest: share/rmux/package-layout.tsv"
        }
        $declared = @{}
        $declaredIds = @{}
        $placed = @{}
        foreach ($line in (Get-Content -LiteralPath $manifestPath)) {
            if ([string]::IsNullOrWhiteSpace($line)) { continue }
            $fields = $line -split "`t"
            if ($fields.Count -lt 6) { Fail "invalid layout manifest line: $line" }
            $entryPath = $fields[0]
            $entryDestination = $fields[2]
            $entryRequired = $fields[3]
            $entryRole = $fields[5]
            $entryId = $fields[4]
            if ($entryRole -cne "executable" -and $entryRole -cne "file") {
                Fail "unknown layout manifest role: $entryRole"
            }
            if ($entryId -cnotmatch '^[a-z0-9][a-z0-9-]*$') {
                Fail "layout manifest entry id is not usable: $entryId"
            }
            if ($declaredIds.ContainsKey($entryId)) {
                Fail "layout manifest declares the entry id $entryId twice"
            }
            $declaredIds[$entryId] = $true
            # Two spellings of one file are one file: bin\rmux.exe,
            # bin\.\rmux.exe and bin\\rmux.exe name the same destination but
            # compare as three. A path that is not already canonical is
            # refused before the destination comparison that relies on it.
            # Both separators count, because both reach the same file here.
            foreach ($candidate in @($entryPath, $entryDestination)) {
                if ($candidate -ceq "-") { continue }
                if ([string]::IsNullOrWhiteSpace($candidate) -or
                        $candidate -match '^[\\/]' -or $candidate -match '[\\/]$' -or
                        $candidate -match '[\\/]{2}' -or
                        $candidate -match '(^|[\\/])\.{1,2}([\\/]|$)' -or
                        $candidate -match '^[A-Za-z]:') {
                    Fail "non-canonical layout manifest path: $candidate"
                }
            }
            $sourcePath = Join-Path $PackageRoot $entryPath
            if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
                if ($entryRequired -ceq "required") { Fail "archive is missing $entryPath" }
                continue
            }
            $normalised = $entryPath.Replace("\", "/")
            if ($declared.ContainsKey($normalised)) {
                Fail "layout manifest declares $entryPath twice"
            }
            $declared[$normalised] = $true
            if ($entryDestination -ceq "-") { continue }
            if ($entryDestination -clike "bin/*") {
                $destinationPath = Join-Path $installBin $entryDestination.Substring(4)
            } else {
                $destinationPath = Join-Path $installRoot $entryDestination
            }
            # Two entries landing on one file would install onto each other,
            # whatever their roles; the plan is refused before anything is
            # copied. The comparison ignores case, like the filesystem.
            $destinationKey = $destinationPath.ToLowerInvariant()
            if ($placed.ContainsKey($destinationKey)) {
                Fail "layout manifest installs two entries to $entryDestination"
            }
            $placed[$destinationKey] = $true
            $installPlan += [pscustomobject]@{ Source = $sourcePath; Destination = $destinationPath }
            if ($entryDestination -ceq "bin/rmux.exe") { $dispatcherDestination = $destinationPath }
        }
        if ($null -eq $dispatcherDestination) {
            Fail "layout manifest does not place the rmux dispatcher"
        }
        # The archive must be exactly what its manifest declares.
        foreach ($file in (Get-ChildItem -LiteralPath $PackageRoot -Recurse -File -Force)) {
            $relative = $file.FullName.Substring($PackageRoot.Length).TrimStart([char]'\', [char]'/').Replace("\", "/")
            if (-not $declared.ContainsKey($relative)) {
                Fail "archive carries an undeclared file: $relative"
            }
        }
    } elseif ($packageLayout -ceq $layoutLegacy) {
        if (-not $undeclaredLegacy -and (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
            Fail "archive declares $layoutLegacy but carries a layout manifest: share/rmux/package-layout.tsv"
        }
        foreach ($entry in $legacyLayout) {
            $sourcePath = Join-Path $PackageRoot $entry.Path
            if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
                Fail "archive is missing $($entry.Path)"
            }
            $destinationPath = if ($entry.InBin) {
                Join-Path $installBin $entry.Path
            } else {
                Join-Path $installRoot $entry.Path
            }
            $installPlan += [pscustomobject]@{ Source = $sourcePath; Destination = $destinationPath }
            if ($entry.Path -ceq "rmux.exe") { $dispatcherDestination = $destinationPath }
        }
        # This format carries no manifest, so what it installs beyond its
        # executables is what the installer of those releases took: every file
        # under share/, at the same place under the install root, and the five
        # optional files at the archive root when they are there.
        $shareSource = Join-Path $PackageRoot "share"
        if (Test-Path -LiteralPath $shareSource -PathType Container) {
            Get-ChildItem -LiteralPath $shareSource -Recurse -File -Force |
                Sort-Object FullName |
                ForEach-Object {
                    $relative = $_.FullName.Substring($shareSource.Length)
                    $relative = $relative.TrimStart([char]'\', [char]'/')
                    $installPlan += [pscustomobject]@{
                        Source = $_.FullName
                        Destination = Join-Path (Join-Path $installRoot "share") $relative
                    }
                }
        }
        foreach ($optional in @("README.md", "LICENSE-APACHE", "LICENSE-MIT",
                                "rmux.1", "SHA256SUMS.txt")) {
            $source = Join-Path $PackageRoot $optional
            if (Test-Path -LiteralPath $source -PathType Leaf) {
                $installPlan += [pscustomobject]@{
                    Source = $source
                    Destination = Join-Path $installRoot $optional
                }
            }
        }
    } else {
        Fail "unsupported package layout: $packageLayout (supported: $layoutCurrent, $layoutLegacy); also supported: rmux-windows-package-v4"
    }

    # Older archives have no companion declaration. Once declared, the complete
    # pair in both executable directories is mandatory, even with -NoVerify.
    $runtimePath = Join-Path $PackageRoot "share/rmux/conpty-runtime.json"
    $declaresRuntime = Test-Path -LiteralPath $runtimePath -PathType Leaf
    if ($null -ne $metadata -and
            $metadata.PSObject.Properties.Name -contains "conpty_runtime_manifest") {
        if ($metadata.conpty_runtime_manifest -cne "share/rmux/conpty-runtime.json") {
            Fail "invalid ConPTY manifest path"
        }
        $declaresRuntime = $true
    }
    if ($declaresRuntime) {
        $runtime = Get-Content -LiteralPath $runtimePath -Raw | ConvertFrom-Json
        if ($runtime.schema -ne 1 -or $runtime.target -cne "x86_64-pc-windows-msvc" -or
            $runtime.files.Count -ne 2 -or $runtime.files[0].name -cne "conpty.dll" -or
            $runtime.files[1].name -cne "OpenConsole.exe" -or
            $runtime.license.path -cne "share/licenses/Microsoft.Windows.Console.ConPTY/LICENSE") {
            Fail "unsupported ConPTY package manifest"
        }
        $license = Join-Path $PackageRoot $runtime.license.path
        if ((Sha256File $license) -ine $runtime.license.sha256) { Fail "ConPTY license checksum mismatch" }
        # A manifest already places both ConPTY copies, so only their identity
        # is verified here. The format that predates the manifest declares them
        # nowhere else, so that plan gains them here, as its own installer did.
        foreach ($layout in @(
            [pscustomobject]@{ Source = $PackageRoot; Destination = $installBin },
            [pscustomobject]@{ Source = (Join-Path $PackageRoot "libexec/rmux")
                               Destination = (Join-Path $installRoot "libexec/rmux") }
        )) {
            foreach ($file in $runtime.files) {
                $source = Join-Path $layout.Source $file.name
                if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
                    Fail "archive is missing ConPTY runtime file: $($file.name)"
                }
                if ((Sha256File $source) -ine $file.sha256) { Fail "ConPTY checksum mismatch: $source" }
                if ($packageLayout -ceq $layoutLegacy) {
                    $installPlan += [pscustomobject]@{
                        Source = $source
                        Destination = Join-Path $layout.Destination $file.name
                    }
                }
            }
        }
    }

    if ($explicitRollback) {
        $lockPath = Join-Path $installRoot '.rmux-install.lock'
        Assert-NoPackageReparse $lockPath
        if (Test-Path -LiteralPath $lockPath) { throw 'explicit rollback requires an unoccupied transaction lock path' }
    }
    $destination = $dispatcherDestination
    $installLock = $null
    $installError = $null
    try {
        # Serialize the complete backup/copy/verify/rollback interval. Without
        # this lock, a failing installer could restore the snapshot it took
        # before a concurrent installer committed a newer package.
        $installLock = Enter-InstallTransactionLock $installRoot
        if ($explicitRollback) {
            $installPlan = @(Get-ExplicitRollbackPlan $PreviousPackageRoot $installBin $installPlan)
        }
        Install-PackageFileSet $installPlan $destination $Verify
    } catch {
        $installError = $_.Exception.Message
    } finally {
        if ($null -ne $installLock) {
            Exit-InstallTransactionLock $installLock
        }
    }
    if ($null -ne $installError) {
        Fail $installError
    }

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

if ($Uninstall) {
    if (-not $PSBoundParameters.ContainsKey('InstallDir') -or [string]::IsNullOrWhiteSpace($InstallDir) -or
            -not $PSBoundParameters.ContainsKey('PreviousPackageRoot') -or [string]::IsNullOrWhiteSpace($PreviousPackageRoot) -or $NoVerify) {
        Fail "Uninstall requires explicit nonempty InstallDir and PreviousPackageRoot, with verification enabled"
    }
    Uninstall-PackageRoot -PreviousPackageRoot $PreviousPackageRoot -DestinationBin $InstallDir
    exit 0
}

# Forward only a genuinely bound witness. Splatting the default empty value
# would turn ordinary installation into an invalid explicit rollback request.
$previousPackageOptions = @{}
if ($PSBoundParameters.ContainsKey('PreviousPackageRoot')) {
    if ([string]::IsNullOrWhiteSpace($PreviousPackageRoot)) {
        Fail "PreviousPackageRoot must not be empty or whitespace when explicitly supplied"
    }
    $previousPackageOptions.PreviousPackageRoot = $PreviousPackageRoot
}
$localPackageRoot = Get-LocalPackageRoot
if ($previousPackageOptions.Count -and ($NoVerify -or -not $localPackageRoot)) {
    Fail "PreviousPackageRoot requires a local package and enabled verification"
}
if ($localPackageRoot) {
    Install-PackageRoot $localPackageRoot $InstallDir (-not $NoVerify) @previousPackageOptions
    exit 0
}

if ($Version -eq "latest") {
    $Version = Resolve-LatestVersion $Repository
}
if ($Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$') {
    Fail "invalid version: $Version"
}

$semver = $Version -replace '^v', ''
$packageVersion = $semver -replace '-.*$', ''
$platform = "windows-x86_64"
$archive = "rmux-$packageVersion-$platform.zip"
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
    $packageRoot = Join-Path $tmp "rmux-$packageVersion-$platform"
    if (-not (Test-PackageRoot $packageRoot)) {
        Fail "required rmux package layout not found in archive"
    }

    Install-PackageRoot $packageRoot $InstallDir (-not $NoVerify) @previousPackageOptions
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
