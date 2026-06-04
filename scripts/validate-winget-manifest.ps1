param(
    [Parameter(Mandatory = $true)]
    [string]$Manifest,
    [Parameter(Mandatory = $true)]
    [string]$Version,
    [string]$Checksums = "",
    [string]$Repository = "Helvesec/rmux",
    [string]$Identifier = "Helvesec.RMUX",
    [string]$Homepage = "https://rmux.io",
    [string]$Publisher = "Helvesec"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Error "error: $Message"
    exit 1
}

function NormalizeVersion([string]$Raw) {
    $normalized = $Raw.Trim()
    if ($normalized.StartsWith("v")) {
        $normalized = $normalized.Substring(1)
    }
    if ($normalized -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$') {
        Fail "invalid version: $Raw"
    }
    $normalized
}

function UnquoteYamlScalar([string]$Value) {
    $trimmed = $Value.Trim()
    if ($trimmed.Length -ge 2 -and
        (($trimmed.StartsWith('"') -and $trimmed.EndsWith('"')) -or
         ($trimmed.StartsWith("'") -and $trimmed.EndsWith("'")))) {
        return $trimmed.Substring(1, $trimmed.Length - 2)
    }
    $trimmed
}

function ReadManifestValue([string]$Key) {
    $pattern = '^\s*(?:-\s*)?' + [regex]::Escape($Key) + '\s*:\s*(.+?)\s*$'
    foreach ($line in $script:manifestLines) {
        if ($line -match $pattern) {
            return (UnquoteYamlScalar $Matches[1])
        }
    }
    Fail "missing WinGet manifest field: $Key"
}

function AssertManifestValue([string]$Key, [string]$Expected) {
    $actual = ReadManifestValue $Key
    if ($actual -ne $Expected) {
        Fail "unexpected ${Key}: expected '$Expected', got '$actual'"
    }
}

function ReadChecksum([string]$ChecksumsPath, [string]$Asset) {
    if ([string]::IsNullOrWhiteSpace($ChecksumsPath)) {
        return ""
    }
    if (-not (Test-Path -LiteralPath $ChecksumsPath -PathType Leaf)) {
        Fail "checksums file not found: $ChecksumsPath"
    }

    foreach ($line in Get-Content -LiteralPath $ChecksumsPath) {
        $normalized = $line.TrimEnd("`r")
        if ($normalized -match '^([0-9a-fA-F]{64})\s+(.+)$') {
            $hash = $Matches[1].ToLowerInvariant()
            $file = $Matches[2].TrimEnd("`r")
            if ($file -eq $Asset) {
                return $hash
            }
        }
    }
    Fail "checksum entry not found for $Asset"
}

$versionValue = NormalizeVersion $Version

if (-not (Test-Path -LiteralPath $Manifest -PathType Leaf)) {
    Fail "WinGet manifest not found: $Manifest"
}

if ($Repository -notmatch '^[^/\s]+/[^/\s]+$') {
    Fail "repository must look like owner/repo: $Repository"
}

$script:manifestLines = Get-Content -LiteralPath $Manifest
$asset = "rmux-$versionValue-windows-x86_64.zip"
$packageDir = "rmux-$versionValue-windows-x86_64"
$expectedUrl = "https://github.com/$Repository/releases/download/v$versionValue/$asset"
$expectedRelativePath = "$packageDir\rmux.exe"
$expectedSha256 = ReadChecksum $Checksums $asset

AssertManifestValue "PackageIdentifier" $Identifier
AssertManifestValue "PackageVersion" $versionValue
AssertManifestValue "PackageLocale" "en-US"
AssertManifestValue "Publisher" $Publisher
AssertManifestValue "PublisherUrl" "https://github.com/$($Repository.Split('/')[0])"
AssertManifestValue "PackageName" "RMUX"
AssertManifestValue "PackageUrl" $Homepage
AssertManifestValue "License" "MIT OR Apache-2.0"
AssertManifestValue "Moniker" "rmux"
AssertManifestValue "Architecture" "x64"
AssertManifestValue "InstallerType" "zip"
AssertManifestValue "NestedInstallerType" "portable"
AssertManifestValue "RelativeFilePath" $expectedRelativePath
AssertManifestValue "PortableCommandAlias" "rmux"
AssertManifestValue "InstallerUrl" $expectedUrl
AssertManifestValue "ManifestType" "singleton"
AssertManifestValue "ManifestVersion" "1.12.0"

$actualSha256 = ReadManifestValue "InstallerSha256"
if ($actualSha256 -notmatch '^[0-9a-fA-F]{64}$') {
    Fail "invalid InstallerSha256: $actualSha256"
}
if (-not [string]::IsNullOrWhiteSpace($expectedSha256) -and $actualSha256.ToLowerInvariant() -ne $expectedSha256) {
    Fail "InstallerSha256 mismatch: expected $expectedSha256, got $actualSha256"
}

Write-Output "WinGet manifest OK: $Identifier $versionValue"
