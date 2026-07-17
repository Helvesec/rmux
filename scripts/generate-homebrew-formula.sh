#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-homebrew-formula.sh --version <semver> --checksums <SHA256SUMS> [options]

Generate the RMUX Homebrew tap formula from GitHub Release checksums.

Options:
  --version <semver|vsemver>   Release version, for example 1.2.3 or v1.2.3
  --release-tag <tag>          GitHub tag containing the assets (default: v<version>)
  --checksums <path>           SHA256SUMS file from the GitHub Release
  --output <path>              Write formula to path instead of stdout
  --repository <owner/repo>    GitHub repository (default: Helvesec/rmux)
  --homepage <url>             Formula homepage (default: https://rmux.io)
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
  if [ "$tag_version" = "$version" ]; then
    printf '%s\n' "$raw"
    return
  fi
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

formula() {
  local base_url macos_arm macos_intel
  base_url="https://github.com/$repository/releases/download/$release_tag"

  macos_arm="rmux-$version-macos-aarch64.tar.gz"
  macos_intel="rmux-$version-macos-x86_64.tar.gz"
  macos_arm_sha="$(asset_sha256 "$macos_arm")"
  macos_intel_sha="$(asset_sha256 "$macos_intel")"

  cat <<EOF
# typed: strict
# frozen_string_literal: true

# Do not edit by hand; regenerate from the RMUX GitHub Release SHA256SUMS.
class Rmux < Formula
  desc "Local terminal multiplexer with a tmux-style CLI and daemon runtime"
  homepage "$homepage"
  version "$version"
  url "$base_url/$macos_arm"
  sha256 "$macos_arm_sha"
  license any_of: ["MIT", "Apache-2.0"]

  depends_on :macos

  on_macos do
    on_arm do
      url "$base_url/$macos_arm"
      sha256 "$macos_arm_sha"
    end

    on_intel do
      url "$base_url/$macos_intel"
      sha256 "$macos_intel_sha"
    end
  end

  def install
    bin.install "bin/rmux"
    bin.install "bin/rmux-daemon" if File.exist?("bin/rmux-daemon")
    libexec.install "libexec/rmux" if File.exist?("libexec/rmux")
    man1.install "share/man/man1/rmux.1"
    bash_completion.install "share/bash-completion/completions/rmux" if File.exist?("share/bash-completion/completions/rmux")
    zsh_completion.install "share/zsh/site-functions/_rmux" if File.exist?("share/zsh/site-functions/_rmux")
    fish_completion.install "share/fish/vendor_completions.d/rmux.fish" if File.exist?("share/fish/vendor_completions.d/rmux.fish")
    pkgshare.install "share/rmux/artifact-metadata.json" if File.exist?("share/rmux/artifact-metadata.json")

    license_files = Dir["LICENSE*"]
    prefix.install license_files unless license_files.empty?
  end

  test do
    assert_match "rmux #{version}", shell_output("#{bin}/rmux -V")
  end
end
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

case "$repository" in
  */*) ;;
  *) die "--repository must look like owner/repo" ;;
esac
case "$homepage" in
  http://*|https://*) ;;
  *) die "--homepage must be an http(s) URL" ;;
esac

if [ -n "$output" ]; then
  out_dir="$(dirname "$output")"
  mkdir -p "$out_dir"
  tmp="$(mktemp "$out_dir/.rmux-formula.XXXXXX")"
  formula > "$tmp"
  mv "$tmp" "$output"
  chmod 0644 "$output"
else
  formula
fi
