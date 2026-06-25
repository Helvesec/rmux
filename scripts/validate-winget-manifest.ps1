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

function FormatScalarForError([string]$Value) {
    $Value.Replace("\", "\\").Replace("`r", "\r").Replace("`n", "\n").Replace("`t", "\t")
}

function DecodeYamlEscape([string]$Escape) {
    switch ($Escape) {
        "0" { return "`0" }
        "a" { return [string][char]0x07 }
        "b" { return "`b" }
        "t" { return "`t" }
        "n" { return "`n" }
        "v" { return [string][char]0x0B }
        "f" { return "`f" }
        "r" { return "`r" }
        "e" { return [string][char]0x1B }
        '"' { return '"' }
        "/" { return "/" }
        "\" { return "\" }
        "_" { return [string][char]0xA0 }
        "N" { return [string][char]0x85 }
        "L" { return [string][char]0x2028 }
        "P" { return [string][char]0x2029 }
    }
    if ($Escape -match '^x([0-9A-Fa-f]{2})$') {
        return [string][char]([Convert]::ToInt32($Matches[1], 16))
    }
    if ($Escape -match '^u([0-9A-Fa-f]{4})$') {
        return [string][char]([Convert]::ToInt32($Matches[1], 16))
    }
    if ($Escape -match '^U([0-9A-Fa-f]{8})$') {
        return [char]::ConvertFromUtf32([Convert]::ToInt32($Matches[1], 16))
    }
    Fail "unsupported YAML escape sequence: \${Escape}"
}

function DecodeYamlScalar([string]$Value) {
    $trimmed = $Value.Trim()
    if ($trimmed.Length -ge 2 -and $trimmed.StartsWith("'") -and $trimmed.EndsWith("'")) {
        return $trimmed.Substring(1, $trimmed.Length - 2).Replace("''", "'")
    }
    if ($trimmed.Length -ge 2 -and $trimmed.StartsWith('"') -and $trimmed.EndsWith('"')) {
        $body = $trimmed.Substring(1, $trimmed.Length - 2)
        $builder = [System.Text.StringBuilder]::new()
        for ($index = 0; $index -lt $body.Length; $index++) {
            $character = $body[$index]
            if ($character -ne "\") {
                [void]$builder.Append($character)
                continue
            }
            $index++
            if ($index -ge $body.Length) {
                Fail "unterminated YAML escape sequence"
            }
            $escape = [string]$body[$index]
            if ($escape -in @("x", "u", "U")) {
                $digits = if ($escape -eq "x") { 2 } elseif ($escape -eq "u") { 4 } else { 8 }
                if (($index + $digits) -ge $body.Length) {
                    Fail "truncated YAML escape sequence: \${escape}"
                }
                $hex = $body.Substring($index + 1, $digits)
                $escape = "$escape$hex"
                $index += $digits
            }
            [void]$builder.Append((DecodeYamlEscape $escape))
        }
        return $builder.ToString()
    }
    $trimmed
}

function UseManifest([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Fail "WinGet manifest not found: $Path"
    }
    $script:currentManifest = $Path
    $script:manifestLines = Get-Content -LiteralPath $Path
    foreach ($line in $script:manifestLines) {
        if ($line -match 'singleton') {
            Fail "singleton WinGet manifests are deprecated: $Path"
        }
    }
}

function ReadManifestValue([string]$Key) {
    $pattern = '^\s*(?:-\s*)?' + [regex]::Escape($Key) + '\s*:\s*(.+?)\s*$'
    foreach ($line in $script:manifestLines) {
        if ($line -match $pattern) {
            return (DecodeYamlScalar $Matches[1])
        }
    }
    Fail "missing WinGet manifest field in ${script:currentManifest}: $Key"
}

function AssertManifestValue([string]$Key, [string]$Expected) {
    $actual = ReadManifestValue $Key
    if ($actual -ne $Expected) {
        Fail "unexpected ${Key} in ${script:currentManifest}: expected '$(FormatScalarForError $Expected)', got '$(FormatScalarForError $actual)'"
    }
}

function AssertManifestLine([string]$Expected) {
    foreach ($line in $script:manifestLines) {
        if ($line.Trim() -eq $Expected) {
            return
        }
    }
    Fail "missing line in ${script:currentManifest}: $Expected"
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

if ($Repository -notmatch '^[^/\s]+/[^/\s]+$') {
    Fail "repository must look like owner/repo: $Repository"
}

if (Test-Path -LiteralPath $Manifest -PathType Container) {
    $versionManifest = Join-Path $Manifest "$Identifier.yaml"
} else {
    $versionManifest = $Manifest
}

if (-not $versionManifest.EndsWith(".yaml", [StringComparison]::OrdinalIgnoreCase)) {
    Fail "version manifest path must end with .yaml: $versionManifest"
}

$stem = $versionManifest.Substring(0, $versionManifest.Length - 5)
$installerManifest = "$stem.installer.yaml"
$localeManifest = "$stem.locale.en-US.yaml"

$asset = "rmux-$versionValue-windows-x86_64.zip"
$packageDir = "rmux-$versionValue-windows-x86_64"
$expectedUrl = "https://github.com/$Repository/releases/download/v$versionValue/$asset"
$expectedRelativePath = "$packageDir\rmux.exe"
$expectedSha256 = ReadChecksum $Checksums $asset
$owner = $Repository.Split('/')[0]

UseManifest $versionManifest
AssertManifestValue "PackageIdentifier" $Identifier
AssertManifestValue "PackageVersion" $versionValue
AssertManifestValue "DefaultLocale" "en-US"
AssertManifestValue "ManifestType" "version"
AssertManifestValue "ManifestVersion" "1.12.0"

UseManifest $installerManifest
AssertManifestValue "PackageIdentifier" $Identifier
AssertManifestValue "PackageVersion" $versionValue
AssertManifestValue "InstallerType" "zip"
AssertManifestValue "NestedInstallerType" "portable"
AssertManifestValue "RelativeFilePath" $expectedRelativePath
AssertManifestValue "PortableCommandAlias" "rmux"
AssertManifestLine "Dependencies:"
AssertManifestLine "PackageDependencies:"
AssertManifestLine "- PackageIdentifier: Microsoft.VCRedist.2015+.x64"
AssertManifestValue "Architecture" "x64"
AssertManifestValue "InstallerUrl" $expectedUrl
AssertManifestValue "ManifestType" "installer"
AssertManifestValue "ManifestVersion" "1.12.0"

$releaseDate = ReadManifestValue "ReleaseDate"
if ($releaseDate -notmatch '^[0-9]{4}-[0-9]{2}-[0-9]{2}$') {
    Fail "invalid ReleaseDate in ${script:currentManifest}: $releaseDate"
}

$actualSha256 = ReadManifestValue "InstallerSha256"
if ($actualSha256 -notmatch '^[0-9a-fA-F]{64}$') {
    Fail "invalid InstallerSha256 in ${script:currentManifest}: $actualSha256"
}
if (-not [string]::IsNullOrWhiteSpace($expectedSha256) -and $actualSha256.ToLowerInvariant() -ne $expectedSha256) {
    Fail "InstallerSha256 mismatch: expected $expectedSha256, got $actualSha256"
}

UseManifest $localeManifest
AssertManifestValue "PackageIdentifier" $Identifier
AssertManifestValue "PackageVersion" $versionValue
AssertManifestValue "PackageLocale" "en-US"
AssertManifestValue "Publisher" $Publisher
AssertManifestValue "PublisherUrl" "https://github.com/$owner"
AssertManifestValue "PublisherSupportUrl" "https://github.com/$Repository/issues"
AssertManifestValue "Author" $Publisher
AssertManifestValue "PackageName" "RMUX"
AssertManifestValue "PackageUrl" $Homepage
AssertManifestValue "License" "MIT OR Apache-2.0"
AssertManifestValue "Moniker" "rmux"
AssertManifestValue "ReleaseNotesUrl" "https://github.com/$Repository/releases/tag/v$versionValue"
AssertManifestValue "ManifestType" "defaultLocale"
AssertManifestValue "ManifestVersion" "1.12.0"

Write-Output "WinGet manifest OK: $Identifier $versionValue multi-file"
