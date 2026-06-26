#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/verify-debian-package.sh <package.deb> [options]

Verify an RMUX Debian/Ubuntu package.

Options:
  --checksums <path>     SHA256SUMS file (default: package directory)
  --run-binary           Execute rmux -V plus helper fallback and daemon smokes
  --require-release-artifact
                         Fail unless package metadata marks this as a release artifact
  -h, --help             Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

sha256_file() {
  sha256sum "$1" | awk '{print $1}'
}

archive=""
checksums=""
run_binary=0
require_release_artifact=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --checksums)
      [ "$#" -ge 2 ] || die "--checksums requires a value"
      checksums="$2"
      shift 2
      ;;
    --run-binary)
      run_binary=1
      shift
      ;;
    --require-release-artifact)
      require_release_artifact=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [ -n "$archive" ]; then
        die "unexpected extra argument: $1"
      fi
      archive="$1"
      shift
      ;;
  esac
done

[ -n "$archive" ] || die "package path is required"
[ -f "$archive" ] || die "package not found: $archive"
case "$archive" in *.deb) ;; *) die "unsupported package extension, expected .deb: $archive" ;; esac

need dpkg-deb
need sha256sum

archive_dir="$(cd "$(dirname "$archive")" && pwd)"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
archive_name="$(basename "$archive")"
archive_abs="$archive_dir/$archive_name"
if [ -z "$checksums" ]; then
  checksums="$archive_dir/SHA256SUMS.txt"
fi
[ -f "$checksums" ] || die "checksum manifest not found: $checksums"

expected_hash="$(awk -v name="$archive_name" '{ hash = $1; file = $2; sub(/\r$/, "", hash); sub(/\r$/, "", file); if (file == name) { print hash; exit } }' "$checksums")"
[ -n "$expected_hash" ] || die "package is missing from checksum manifest: $archive_name"
actual_hash="$(sha256_file "$archive_abs")"
[ "$expected_hash" = "$actual_hash" ] || die "checksum mismatch for $archive_name"

package_field="$(dpkg-deb -f "$archive_abs" Package)"
version_field="$(dpkg-deb -f "$archive_abs" Version)"
arch_field="$(dpkg-deb -f "$archive_abs" Architecture)"
[ "$package_field" = "rmux" ] || die "unexpected package field: $package_field"
[ -n "$version_field" ] || die "missing Version field"
case "$arch_field" in amd64|arm64) ;; *) die "unexpected Architecture field: $arch_field" ;; esac

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/rmux-deb-verify.XXXXXX")"
trap 'rm -rf "$tmpdir"' EXIT
dpkg-deb -x "$archive_abs" "$tmpdir/root"

for required in \
  usr/bin/rmux \
  usr/libexec/rmux/rmux \
  usr/bin/rmux-daemon \
  usr/share/man/man1/rmux.1.gz \
  usr/share/doc/rmux/README.md \
  usr/share/doc/rmux/LICENSE-APACHE \
  usr/share/doc/rmux/LICENSE-MIT \
  usr/share/rmux/artifact-metadata.json
do
  [ -e "$tmpdir/root/$required" ] || die "missing package file: $required"
done
[ -x "$tmpdir/root/usr/bin/rmux" ] || die "packaged rmux is not executable"
[ -x "$tmpdir/root/usr/libexec/rmux/rmux" ] || die "packaged private helper is not executable"
[ -x "$tmpdir/root/usr/bin/rmux-daemon" ] || die "packaged rmux-daemon is not executable"

metadata="$tmpdir/root/usr/share/rmux/artifact-metadata.json"
grep -q '"artifact_kind"[[:space:]]*:[[:space:]]*"debian-package-binary"' "$metadata" || die "metadata artifact_kind is not debian-package-binary"
grep -q '"package_layout"[[:space:]]*:[[:space:]]*"rmux-debian-package-v2"' "$metadata" || die "metadata package_layout is not rmux-debian-package-v2"
if [ "$require_release_artifact" -eq 1 ]; then
  grep -q '"release_artifact"[[:space:]]*:[[:space:]]*true' "$metadata" || die "metadata release_artifact is not true"
fi
metadata_binary_hash="$(sed -n 's/.*"binary_sha256"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F]\{64\}\)".*/\1/p' "$metadata" | head -n 1 | tr 'A-F' 'a-f')"
[ -n "$metadata_binary_hash" ] || die "metadata binary_sha256 is missing or invalid"
packaged_binary_hash="$(sha256_file "$tmpdir/root/usr/bin/rmux")"
[ "$metadata_binary_hash" = "$packaged_binary_hash" ] || die "metadata binary_sha256 does not match packaged binary"
metadata_helper_hash="$(sed -n 's/.*"helper_binary_sha256"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F]\{64\}\)".*/\1/p' "$metadata" | head -n 1 | tr 'A-F' 'a-f')"
[ -n "$metadata_helper_hash" ] || die "metadata helper_binary_sha256 is missing or invalid"
packaged_helper_hash="$(sha256_file "$tmpdir/root/usr/libexec/rmux/rmux")"
[ "$metadata_helper_hash" = "$packaged_helper_hash" ] || die "metadata helper_binary_sha256 does not match packaged private helper"
metadata_daemon_hash="$(sed -n 's/.*"daemon_binary_sha256"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F]\{64\}\)".*/\1/p' "$metadata" | head -n 1 | tr 'A-F' 'a-f')"
[ -n "$metadata_daemon_hash" ] || die "metadata daemon_binary_sha256 is missing or invalid"
packaged_daemon_hash="$(sha256_file "$tmpdir/root/usr/bin/rmux-daemon")"
[ "$metadata_daemon_hash" = "$packaged_daemon_hash" ] || die "metadata daemon_binary_sha256 does not match packaged daemon binary"

if [ "$run_binary" -eq 1 ]; then
  "$tmpdir/root/usr/bin/rmux" -V >/dev/null
  "$script_dir/smoke-installed-rmux.sh" "$tmpdir/root/usr/bin/rmux" >/dev/null
fi

printf 'package=%s\n' "$archive_abs"
printf 'sha256=%s\n' "$actual_hash"
printf 'binary_sha256=%s\n' "$packaged_binary_hash"
printf 'helper_binary_sha256=%s\n' "$packaged_helper_hash"
printf 'daemon_binary_sha256=%s\n' "$packaged_daemon_hash"
printf 'run_binary=%s\n' "$([ "$run_binary" -eq 1 ] && printf true || printf false)"
