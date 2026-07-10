#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-winget-manifest.sh --version <semver> --checksums <SHA256SUMS> --output <path> [options]

Generate the RMUX WinGet multi-file manifest from GitHub Release checksums.

Options:
  --version <semver|vsemver>   Release version, for example 1.2.3 or v1.2.3
  --release-tag <tag>          GitHub tag containing the assets (default: v<version>)
  --checksums <path>           SHA256SUMS file from the GitHub Release
  --output <path>              Write Helvesec.RMUX.yaml to this path and sibling manifest files beside it
  --repository <owner/repo>    GitHub repository (default: Helvesec/rmux)
  --homepage <url>             Package homepage (default: https://rmux.io)
  --publisher <name>           Package publisher (default: Helvesec)
  --identifier <id>            WinGet package id (default: Helvesec.RMUX)
  --release-date <YYYY-MM-DD>  Release date for the installer manifest (default: today in UTC)
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
  printf '%s\n' "$hash" | tr 'a-f' 'A-F'
}

version_manifest() {
  cat <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.1.10.0.schema.json

PackageIdentifier: $identifier
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.10.0
EOF
}

installer_manifest() {
  local asset sha base_url nested_path
  asset="rmux-$version-windows-x86_64.zip"
  sha="$(asset_sha256 "$asset")"
  base_url="https://github.com/$repository/releases/download/$release_tag"
  nested_path="rmux-$version-windows-x86_64\\rmux.exe"

  cat <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.1.10.0.schema.json

PackageIdentifier: $identifier
PackageVersion: $version
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: $nested_path
    PortableCommandAlias: rmux
ReleaseDate: "$release_date"
Dependencies:
  PackageDependencies:
    - PackageIdentifier: Microsoft.VCRedist.2015+.x64
Installers:
  - Architecture: x64
    InstallerUrl: $base_url/$asset
    InstallerSha256: $sha
ManifestType: installer
ManifestVersion: 1.10.0
EOF
}

default_locale_manifest() {
  local owner
  owner="${repository%%/*}"

  cat <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.1.10.0.schema.json

PackageIdentifier: $identifier
PackageVersion: $version
PackageLocale: en-US
Publisher: $publisher
PublisherUrl: https://github.com/$owner
PublisherSupportUrl: https://github.com/$repository/issues
Author: $publisher
PackageName: RMUX
PackageUrl: $homepage
License: MIT OR Apache-2.0
ShortDescription: Terminal multiplexer with a tmux-style CLI, daemon runtime, and native Windows support.
Description: |-
  RMUX is a terminal multiplexer with a tmux-style command line, daemon-backed persistent sessions, native Windows support, and a Rust SDK for automation.
Moniker: rmux
Tags:
  - cli
  - multiplexer
  - rust
  - terminal
  - tmux
ReleaseNotesUrl: https://github.com/$repository/releases/tag/$release_tag
ManifestType: defaultLocale
ManifestVersion: 1.10.0
EOF
}

write_manifest() {
  local destination generator tmp
  destination="$1"
  generator="$2"
  tmp="$(mktemp "$(dirname "$destination")/.rmux-winget.XXXXXX")"
  "$generator" > "$tmp"
  mv "$tmp" "$destination"
  chmod 0644 "$destination"
}

version=""
release_tag=""
checksums=""
output=""
repository="${RMUX_GITHUB_REPO:-Helvesec/rmux}"
homepage="${RMUX_HOMEPAGE:-https://rmux.io}"
publisher="${RMUX_PACKAGE_PUBLISHER:-Helvesec}"
identifier="${RMUX_WINGET_IDENTIFIER:-Helvesec.RMUX}"
release_date="$(date -u +%F)"

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
    --output)
      [ "$#" -ge 2 ] || die "--output requires a value"
      output="$2"
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
    --publisher)
      [ "$#" -ge 2 ] || die "--publisher requires a value"
      publisher="$2"
      shift 2
      ;;
    --identifier)
      [ "$#" -ge 2 ] || die "--identifier requires a value"
      identifier="$2"
      shift 2
      ;;
    --release-date)
      [ "$#" -ge 2 ] || die "--release-date requires a value"
      release_date="$2"
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
[ -n "$output" ] || die "--output is required"

case "$repository" in
  */*) ;;
  *) die "--repository must look like owner/repo" ;;
esac
case "$homepage" in
  http://*|https://*) ;;
  *) die "--homepage must be an http(s) URL" ;;
esac
case "$identifier" in
  *.*) ;;
  *) die "--identifier must look like Publisher.Package" ;;
esac
case "$release_date" in
  [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
  *) die "--release-date must look like YYYY-MM-DD" ;;
esac
case "$output" in
  *.yaml) ;;
  *) die "--output must end with .yaml" ;;
esac

out_dir="$(dirname "$output")"
stem="${output%.yaml}"
installer_output="$stem.installer.yaml"
locale_output="$stem.locale.en-US.yaml"

mkdir -p "$out_dir"
write_manifest "$output" version_manifest
write_manifest "$installer_output" installer_manifest
write_manifest "$locale_output" default_locale_manifest
