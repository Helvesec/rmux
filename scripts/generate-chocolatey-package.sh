#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-chocolatey-package.sh --version <semver> --checksums <SHA256SUMS> --output-dir <dir> [options]

Generate the RMUX Chocolatey package source from GitHub Release checksums.

Options:
  --version <semver|vsemver>   Release version, for example 1.2.3 or v1.2.3
  --release-tag <tag>          GitHub tag containing the assets (default: v<version>)
  --checksums <path>           SHA256SUMS file from the GitHub Release
  --output-dir <dir>           Write rmux.nuspec and tools/ scripts here
  --repository <owner/repo>    GitHub repository (default: Helvesec/rmux)
  --homepage <url>             Package homepage (default: https://rmux.io)
  -h, --help                   Show this help
USAGE
}

layout_tool() {
  printf '%s\n' "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/packaging/layout"
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

normalize_release_tag() {
  local raw tag_version rc_number
  raw="$1"
  case "$raw" in v*) ;; *) die "release tag must start with v: $raw" ;; esac
  tag_version="${raw#v}"
  case "$tag_version" in *[!0-9A-Za-z.-]*|""|*..*|.*|*.) die "invalid release tag: $raw" ;; esac
  if [ "$tag_version" = "$version" ]; then printf '%s\n' "$raw"; return; fi
  case "$tag_version" in
    "$version"-rc.*)
      rc_number="${tag_version#"$version-rc."}"
      case "$rc_number" in ''|*[!0-9]*|0|0*) die "release tag RC suffix must be -rc.N with N >= 1 and no leading zero: $raw" ;; esac
      printf '%s\n' "$raw"
      ;;
    *) die "release tag $raw does not contain package version $version" ;;
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
    <releaseNotes>https://github.com/$repository/releases/tag/$release_tag</releaseNotes>
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

# The commands this package installs, and the archive-relative executable each
# one points at, exactly as the descriptor declares them. The install and the
# uninstall script are written from this one list, so they cannot disagree.
chocolatey_shims() {
  "$(layout_tool)" chocolatey-shims
}

# Every executable the package carries. Chocolatey shims each of them after the
# install script runs, under their own basenames, unless a <file>.ignore sits
# beside them; the script writes those markers from this list so the command
# surface is the descriptor's decision instead of whatever the archive happens
# to contain, even when a command is named differently from its file.
chocolatey_ignores() {
  "$(layout_tool)" chocolatey-ignores
}

write_install() {
  local out asset sha url package_dir shim_lines command path
  out="$1"
  asset="rmux-$version-windows-x86_64.zip"
  sha="$(asset_sha256 "$asset")"
  url="https://github.com/$repository/releases/download/$release_tag/$asset"
  package_dir="rmux-$version-windows-x86_64"

  # Both projections are taken as values and their status is read. Feeding a
  # command substitution straight into the loop would run the refusal inside a
  # subshell, so a refused descriptor would turn into an empty list here and
  # the package would be written as if nothing had been declared.
  local shim_projection ignore_projection ignore_lines relative
  shim_projection="$(chocolatey_shims)" || die "could not project the Chocolatey shims"
  ignore_projection="$(chocolatey_ignores)" || die "could not project the Chocolatey ignores"

  shim_lines=""
  while IFS="$(printf '\t')" read -r command path; do
    [ -n "$command" ] || continue
    shim_lines="$shim_lines  [pscustomobject]@{ Name = '$command'; Path = (Join-Path \$installDir '$path') }
"
  done <<EOF
$shim_projection
EOF
  [ -n "$shim_lines" ] || die "the descriptor declares no Chocolatey shim"

  ignore_lines=""
  while IFS= read -r relative; do
    [ -n "$relative" ] || continue
    ignore_lines="$ignore_lines  '$relative'
"
  done <<EOF
$ignore_projection
EOF

  cat > "$out" <<EOF
\$ErrorActionPreference = 'Stop'

\$packageName = 'rmux'
\$toolsDir = Split-Path -Parent \$MyInvocation.MyCommand.Definition
\$installDir = Join-Path \$toolsDir '$package_dir'

\$shims = @(
$shim_lines)

\$zipArgs = @{
  packageName = \$packageName
  url64bit = '$url'
  unzipLocation = \$toolsDir
  checksum64 = '$sha'
  checksumType64 = 'sha256'
}

Install-ChocolateyZipPackage @zipArgs

foreach (\$shim in \$shims) {
  if (-not (Test-Path \$shim.Path)) {
    throw "\$(\$shim.Name) was not extracted to \$(\$shim.Path)"
  }
}

# Chocolatey's automatic pass names a command after a file's basename, so every
# executable this package carries is marked before any command is installed.
# The commands below are then the only ones this package adds, whatever the
# files happen to be called. A missing marker target is a packaging error, not
# a silent extra command.
\$ignored = @(
$ignore_lines)

foreach (\$relative in \$ignored) {
  \$target = Join-Path \$installDir \$relative
  if (-not (Test-Path \$target)) {
    throw "\$relative was not extracted to \$target"
  }
  New-Item -ItemType File -Path "\$target.ignore" -Force | Out-Null
}

foreach (\$shim in \$shims) {
  Install-BinFile -Name \$shim.Name -Path \$shim.Path
}
EOF
}

write_uninstall() {
  local out command path names
  out="$1"
  local projection
  projection="$(chocolatey_shims)" || die "could not project the Chocolatey shims"
  names=""
  while IFS="$(printf '\t')" read -r command path; do
    [ -n "$command" ] || continue
    names="$names '$command'"
  done <<EOF
$projection
EOF
  [ -n "$names" ] || die "the descriptor declares no Chocolatey shim"

  cat > "$out" <<EOF
\$ErrorActionPreference = 'Stop'

foreach (\$name in @($(printf '%s' "$names" | sed 's/^ //; s/ /, /g'))) {
  Uninstall-BinFile -Name \$name
}
EOF
}

version=""
release_tag=""
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
    --release-tag)
      [ "$#" -ge 2 ] || die "--release-tag requires a value"
      release_tag="$2"
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
[ -n "$release_tag" ] || release_tag="v$version"
release_tag="$(normalize_release_tag "$release_tag")"
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
