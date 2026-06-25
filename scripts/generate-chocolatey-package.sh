#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-chocolatey-package.sh --version <semver> --checksums <SHA256SUMS> --output-dir <dir> [options]

Generate the RMUX Chocolatey package source from GitHub Release checksums.

Options:
  --version <semver|vsemver>   Release version, for example 1.2.3 or v1.2.3
  --checksums <path>           SHA256SUMS file from the GitHub Release
  --output-dir <dir>           Write rmux.nuspec and tools/ scripts here
  --repository <owner/repo>    GitHub repository (default: Helvesec/rmux)
  --homepage <url>             Package homepage (default: https://rmux.io)
  -h, --help                   Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

normalize_version() {
  local raw version
  raw="$1"
  version="${raw#v}"
  case "$version" in
    *[!0-9A-Za-z.-]*|""|*..*|.*|*.) die "invalid version: $raw" ;;
  esac
  case "$version" in
    [0-9]*.[0-9]*.[0-9]*) printf '%s\n' "$version" ;;
    *) die "version must look like 1.2.3 or v1.2.3, got: $raw" ;;
  esac
}

asset_sha256() {
  local asset hash
  asset="$1"
  case "$asset" in
    */*|*\\*|../*|*/../*|"") die "invalid asset name: $asset" ;;
  esac

  hash="$(awk -v name="$asset" '{ hash = $1; file = $2; sub(/\r$/, "", hash); sub(/\r$/, "", file); if (file == name) { print hash; found = 1; exit } } END { if (!found) exit 1 }' "$checksums")" ||
    die "checksum entry not found for $asset"
  case "$hash" in
    [0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]*)
      [ "${#hash}" -eq 64 ] || die "invalid checksum length for $asset"
      ;;
    *) die "invalid checksum for $asset" ;;
  esac
  printf '%s\n' "$hash" | tr 'A-F' 'a-f'
}

write_nuspec() {
  local out
  out="$1"
  cat > "$out" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2015/06/nuspec.xsd">
  <metadata>
    <id>rmux</id>
    <version>$version</version>
    <title>RMUX</title>
    <authors>Helvesec</authors>
    <owners>Helvesec</owners>
    <projectUrl>$homepage</projectUrl>
    <packageSourceUrl>https://github.com/$repository</packageSourceUrl>
    <licenseUrl>https://github.com/$repository#license</licenseUrl>
    <requireLicenseAcceptance>false</requireLicenseAcceptance>
    <description>Terminal multiplexer with a tmux-style CLI, daemon runtime, Rust SDK, and native Windows support.</description>
    <summary>Terminal multiplexer with a tmux-style CLI and native Windows support.</summary>
    <releaseNotes>https://github.com/$repository/releases/tag/v$version</releaseNotes>
    <tags>rmux terminal multiplexer tmux cli rust</tags>
    <dependencies>
      <dependency id="vcredist140" />
    </dependencies>
  </metadata>
  <files>
    <file src="tools\\**" target="tools" />
  </files>
</package>
EOF
}

write_install() {
  local out asset sha url package_dir
  out="$1"
  asset="rmux-$version-windows-x86_64.zip"
  sha="$(asset_sha256 "$asset")"
  url="https://github.com/$repository/releases/download/v$version/$asset"
  package_dir="rmux-$version-windows-x86_64"

  cat > "$out" <<EOF
\$ErrorActionPreference = 'Stop'

\$packageName = 'rmux'
\$toolsDir = Split-Path -Parent \$MyInvocation.MyCommand.Definition
\$installDir = Join-Path \$toolsDir '$package_dir'
\$rmuxExe = Join-Path \$installDir 'rmux.exe'
\$rmuxDaemonExe = Join-Path \$installDir 'rmux-daemon.exe'

\$zipArgs = @{
  packageName = \$packageName
  url64bit = '$url'
  unzipLocation = \$toolsDir
  checksum64 = '$sha'
  checksumType64 = 'sha256'
}

Install-ChocolateyZipPackage @zipArgs

if (-not (Test-Path \$rmuxExe)) {
  throw "rmux.exe was not extracted to \$rmuxExe"
}
if (-not (Test-Path \$rmuxDaemonExe)) {
  throw "rmux-daemon.exe was not extracted to \$rmuxDaemonExe"
}

Install-BinFile -Name 'rmux' -Path \$rmuxExe
Install-BinFile -Name 'rmux-daemon' -Path \$rmuxDaemonExe
EOF
}

write_uninstall() {
  local out
  out="$1"
  cat > "$out" <<'EOF'
$ErrorActionPreference = 'Stop'

Uninstall-BinFile -Name 'rmux'
Uninstall-BinFile -Name 'rmux-daemon'
EOF
}

version=""
checksums=""
output_dir=""
repository="${RMUX_GITHUB_REPO:-Helvesec/rmux}"
homepage="${RMUX_HOMEPAGE:-https://rmux.io}"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || die "--version requires a value"
      version="$(normalize_version "$2")"
      shift 2
      ;;
    --checksums)
      [ "$#" -ge 2 ] || die "--checksums requires a value"
      checksums="$2"
      shift 2
      ;;
    --output-dir)
      [ "$#" -ge 2 ] || die "--output-dir requires a value"
      output_dir="$2"
      shift 2
      ;;
    --repository)
      [ "$#" -ge 2 ] || die "--repository requires a value"
      repository="$2"
      shift 2
      ;;
    --homepage)
      [ "$#" -ge 2 ] || die "--homepage requires a value"
      homepage="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[ -n "$version" ] || die "--version is required"
[ -n "$checksums" ] || die "--checksums is required"
[ -f "$checksums" ] || die "checksums file not found: $checksums"
[ -n "$output_dir" ] || die "--output-dir is required"
case "$output_dir" in
  /|.|..) die "--output-dir is too broad: $output_dir" ;;
esac

case "$repository" in
  */*) ;;
  *) die "--repository must look like owner/repo" ;;
esac
case "$homepage" in
  http://*|https://*) ;;
  *) die "--homepage must be an http(s) URL" ;;
esac

mkdir -p "$output_dir/tools"
rm -f \
  "$output_dir/rmux.nuspec" \
  "$output_dir/tools/chocolateyInstall.ps1" \
  "$output_dir/tools/chocolateyUninstall.ps1"
write_nuspec "$output_dir/rmux.nuspec"
write_install "$output_dir/tools/chocolateyInstall.ps1"
write_uninstall "$output_dir/tools/chocolateyUninstall.ps1"
chmod 0644 "$output_dir/rmux.nuspec" "$output_dir/tools/chocolateyInstall.ps1" "$output_dir/tools/chocolateyUninstall.ps1"
