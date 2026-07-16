#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-scoop-manifest.sh --version <semver> --checksums <SHA256SUMS> --output <path> [options]

Generate the RMUX Scoop manifest from GitHub Release checksums.

Options:
  --version <semver|vsemver>   Release version, for example 1.2.3 or v1.2.3
  --release-tag <tag>          GitHub tag containing the assets (default: v<version>)
  --checksums <path>           SHA256SUMS file from the GitHub Release
  --output <path>              Write rmux.json to this path
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

manifest() {
  local asset sha base_url
  asset="rmux-$version-windows-x86_64.zip"
  sha="$(asset_sha256 "$asset")"
  base_url="https://github.com/$repository/releases/download/$release_tag"

  cat <<EOF
{
  "version": "$version",
  "description": "Terminal multiplexer with a tmux-style CLI, daemon runtime, and native Windows support.",
  "homepage": "$homepage",
  "license": "MIT OR Apache-2.0",
  "architecture": {
    "64bit": {
      "url": "$base_url/$asset",
      "hash": "$sha",
      "extract_dir": "rmux-$version-windows-x86_64"
    }
  },
  "depends": "vcredist2022",
  "bin": [
    "rmux.exe",
    "rmux-daemon.exe"
  ],
  "checkver": {
    "github": "https://github.com/$repository"
  },
  "autoupdate": {
    "architecture": {
      "64bit": {
        "url": "https://github.com/$repository/releases/download/v\$version/rmux-\$version-windows-x86_64.zip",
        "extract_dir": "rmux-\$version-windows-x86_64"
      }
    }
  }
}
EOF
}

version=""
release_tag=""
checksums=""
output=""
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

out_dir="$(dirname "$output")"
mkdir -p "$out_dir"
tmp="$(mktemp "$out_dir/.rmux-scoop.XXXXXX")"
manifest > "$tmp"
mv "$tmp" "$output"
chmod 0644 "$output"
