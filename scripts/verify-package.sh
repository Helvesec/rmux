#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/verify-package.sh <archive.tar.gz> [options]

Verify a local-first RMUX Unix package.

Options:
  --checksums <path>     SHA256SUMS file (default: archive directory)
  --expected-commit <sha> Require metadata to identify this full source commit
  --run-binary           Execute rmux -V plus helper fallback and daemon smokes
  --require-release-artifact
                         Fail unless metadata marks this as a release artifact
  -h, --help             Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    die "no SHA256 tool found"
  fi
}

verify_checksum_manifest() {
  local root manifest line hash relative path actual
  root="$1"
  manifest="$2"

  while IFS= read -r line || [ -n "$line" ]; do
    [ -n "$line" ] || continue
    hash="${line%%  *}"
    relative="${line#*  }"
    [ "$hash" != "$line" ] || die "invalid checksum line: $line"
    case "$hash" in
      *[!0-9a-fA-F]*|"") die "invalid checksum hash: $line" ;;
    esac
    [ "${#hash}" -eq 64 ] || die "invalid checksum hash length: $line"
    case "$relative" in
      /*|../*|*/../*|*\\*|*[A-Za-z]:*) die "non-portable checksum path: $relative" ;;
    esac

    path="$root/$relative"
    [ -f "$path" ] || die "checksum target is missing: $relative"
    actual="$(sha256_file "$path")"
    [ "$actual" = "$(printf '%s' "$hash" | tr 'A-F' 'a-f')" ] ||
      die "checksum mismatch for $relative"
  done < "$manifest"
}

verify_package_hygiene() {
  local root forbidden
  root="$1"

  forbidden="$(
    find "$root" \( \
      -name ".?*" -o \
      -name '*.sock' -o \
      -name '*.tmp' -o \
      -name '*.bak' -o \
      -name '*.orig' -o \
      -name '*~' \
    \) -print -quit
  )"
  [ -z "$forbidden" ] || die "forbidden package entry: ${forbidden#$root/}"

  forbidden="$(find "$root" -type s -print -quit 2>/dev/null || true)"
  [ -z "$forbidden" ] || die "package contains a Unix socket: ${forbidden#$root/}"
}

archive=""
checksums=""
run_binary=0
require_release_artifact=0
expected_commit=""

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
    --expected-commit)
      [ "$#" -ge 2 ] || die "--expected-commit requires a value"
      [ -n "$2" ] || die "--expected-commit requires a non-empty value"
      expected_commit="$2"
      shift 2
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

[ -n "$archive" ] || die "archive path is required"
[ -f "$archive" ] || die "archive not found: $archive"
case "$archive" in
  *.tar.gz) ;;
  *) die "unsupported archive extension, expected .tar.gz: $archive" ;;
esac

archive_dir="$(cd "$(dirname "$archive")" && pwd)"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
archive_name="$(basename "$archive")"
archive_abs="$archive_dir/$archive_name"

if [ -z "$checksums" ]; then
  checksums="$archive_dir/SHA256SUMS.txt"
fi
[ -f "$checksums" ] || die "checksum manifest not found: $checksums"

expected_hash="$(awk -v name="$archive_name" '{ hash = $1; file = $2; sub(/\r$/, "", hash); sub(/\r$/, "", file); if (file == name) { print hash; exit } }' "$checksums")"
[ -n "$expected_hash" ] || die "archive is missing from checksum manifest: $archive_name"
actual_hash="$(sha256_file "$archive_abs")"
[ "$expected_hash" = "$actual_hash" ] || die "checksum mismatch for $archive_name"

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/rmux-package-verify.XXXXXX")"
trap 'rm -rf "$tmpdir"' EXIT
tar -xzf "$archive_abs" -C "$tmpdir"

package_root="$tmpdir/${archive_name%.tar.gz}"
[ -d "$package_root" ] || die "archive root directory is missing: ${archive_name%.tar.gz}"
verify_package_hygiene "$package_root"

# The layout, its modes and the absence of anything else come from the one
# descriptor every builder and installer projects.
"$script_dir/packaging/layout" --for-package "$package_root" check-tree --variant unix-archive --root "$package_root" ||
  die "package layout does not match the descriptor"
# The plan the package carries for its own installer is the projection of that
# same descriptor, so an installer cannot be handed a layout nobody validated.
"$script_dir/packaging/layout" --for-package "$package_root" manifest --variant unix-archive > "$tmpdir/package-layout.expected" ||
  die "could not project the package manifest"
cmp -s "$tmpdir/package-layout.expected" "$package_root/share/rmux/package-layout.tsv" ||
  die "packaged layout manifest does not match the layout descriptor"
[ -x "$package_root/install.sh" ] || die "packaged install.sh is not executable"
verify_checksum_manifest "$package_root" "$package_root/SHA256SUMS.txt"

metadata="$package_root/share/rmux/artifact-metadata.json"
metadata_commit="$(python3 - "$metadata" "$expected_commit" "$require_release_artifact" <<'PYTHON'
import json
from pathlib import Path
import re
import sys

try:
    metadata = json.loads(Path(sys.argv[1]).read_text())
    commit = metadata.get("git_commit")
    if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("git_commit must be a full lowercase SHA")
    expected = sys.argv[2]
    if expected and (not re.fullmatch(r"[0-9a-f]{40}", expected) or commit != expected):
        raise ValueError("git_commit differs from expected commit")
    if sys.argv[3] == "1":
        if metadata.get("release_artifact") is not True:
            raise ValueError("metadata release_artifact is not true")
        if metadata.get("configuration") != "release":
            raise ValueError("release artifact metadata configuration is not release")
        if metadata.get("git_dirty") is not False or metadata.get("skip_build") is not False:
            raise ValueError("release artifact requires a clean source and a fresh release build")
    print(commit)
except (OSError, ValueError, AttributeError) as error:
    sys.exit("source provenance: " + str(error))
PYTHON
)" || die "invalid package source provenance"
metadata_binary_hash="$(sed -n 's/.*"binary_sha256"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F]\{64\}\)".*/\1/p' "$metadata" | head -n 1 | tr 'A-F' 'a-f')"
[ -n "$metadata_binary_hash" ] || die "metadata binary_sha256 is missing or invalid"
packaged_binary_hash="$(sha256_file "$package_root/bin/rmux")"
[ "$metadata_binary_hash" = "$packaged_binary_hash" ] || die "metadata binary_sha256 does not match packaged binary"
metadata_helper_hash="$(sed -n 's/.*"helper_binary_sha256"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F]\{64\}\)".*/\1/p' "$metadata" | head -n 1 | tr 'A-F' 'a-f')"
[ -n "$metadata_helper_hash" ] || die "metadata helper_binary_sha256 is missing or invalid"
packaged_helper_hash="$(sha256_file "$package_root/libexec/rmux/rmux")"
[ "$metadata_helper_hash" = "$packaged_helper_hash" ] || die "metadata helper_binary_sha256 does not match packaged private helper"
metadata_daemon_hash="$(sed -n 's/.*"daemon_binary_sha256"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F]\{64\}\)".*/\1/p' "$metadata" | head -n 1 | tr 'A-F' 'a-f')"
[ -n "$metadata_daemon_hash" ] || die "metadata daemon_binary_sha256 is missing or invalid"
packaged_daemon_hash="$(sha256_file "$package_root/bin/rmux-daemon")"
[ "$metadata_daemon_hash" = "$packaged_daemon_hash" ] || die "metadata daemon_binary_sha256 does not match packaged daemon binary"

package_glibc_min=""
metadata_target="$(sed -n 's/.*"target"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$metadata" | head -n 1)"
case "$metadata_target" in
  *-unknown-linux-gnu)
    glibc_floor_script="$script_dir/glibc-symbol-floor.sh"
    binary_glibc_min="$("$glibc_floor_script" "$package_root/bin/rmux")"
    helper_binary_glibc_min="$("$glibc_floor_script" "$package_root/libexec/rmux/rmux")"
    daemon_binary_glibc_min="$("$glibc_floor_script" "$package_root/bin/rmux-daemon")"
    package_glibc_min="$("$glibc_floor_script" \
      "$package_root/bin/rmux" \
      "$package_root/libexec/rmux/rmux" \
      "$package_root/bin/rmux-daemon")"
    for field_and_value in \
      "binary_glibc_min:$binary_glibc_min" \
      "helper_binary_glibc_min:$helper_binary_glibc_min" \
      "daemon_binary_glibc_min:$daemon_binary_glibc_min" \
      "package_glibc_min:$package_glibc_min"
    do
      field="${field_and_value%%:*}"
      expected="${field_and_value#*:}"
      grep -q "\"$field\"[[:space:]]*:[[:space:]]*\"$expected\"" "$metadata" ||
        die "metadata $field does not match imported GLIBC symbols ($expected)"
    done
    max_supported_glibc="$(sed -n 's/.*"max_supported_glibc"[[:space:]]*:[[:space:]]*"\([0-9.]*\)".*/\1/p' "$metadata" | head -n 1)"
    [ -n "$max_supported_glibc" ] || die "metadata max_supported_glibc is missing"
    if [ "$(printf '%s\n%s\n' "$package_glibc_min" "$max_supported_glibc" | LC_ALL=C sort -V | tail -n 1)" != "$max_supported_glibc" ]; then
      die "packaged binaries require GLIBC_$package_glibc_min, newer than supported GLIBC_$max_supported_glibc"
    fi
    ;;
esac

grep -q '"artifact_kind"[[:space:]]*:[[:space:]]*"unix-package-binary"' "$metadata" || die "metadata artifact_kind is not unix-package-binary"
package_layout_id="$("$script_dir/packaging/layout" --for-package "$package_root" layout-id --variant unix-archive)" ||
  die "could not project the package layout identity"
grep -q "\"package_layout\"[[:space:]]*:[[:space:]]*\"$package_layout_id\"" "$metadata" ||
  die "metadata package_layout is not $package_layout_id"
if [ "$run_binary" -eq 1 ]; then
  "$package_root/bin/rmux" -V >/dev/null
  mkdir -p "$tmpdir/home"
  env \
    HOME="$tmpdir/home" \
    PATH="$package_root/bin:$package_root/libexec/rmux:/usr/bin:/bin" \
    "$script_dir/smoke-installed-rmux.sh" "$package_root/bin/rmux" >/dev/null
  install_prefix="$tmpdir/install-prefix"
  "$package_root/install.sh" --prefix "$install_prefix" >/dev/null
  # The same payload, after installation: every projected destination exists.
  "$script_dir/packaging/layout" --for-package "$package_root" check-installed \
    --variant unix-archive --package-root "$package_root" --prefix "$install_prefix" ||
    die "installed layout does not match the descriptor"
  env \
    HOME="$tmpdir/home" \
    PATH="$install_prefix/bin:$install_prefix/libexec/rmux:/usr/bin:/bin" \
    "$script_dir/smoke-installed-rmux.sh" "$install_prefix/bin/rmux" >/dev/null
fi

printf 'archive=%s\n' "$archive_abs"
printf 'sha256=%s\n' "$actual_hash"
printf 'git_commit=%s\n' "$metadata_commit"
printf 'binary_sha256=%s\n' "$packaged_binary_hash"
printf 'daemon_binary_sha256=%s\n' "$packaged_daemon_hash"
[ -z "$package_glibc_min" ] || printf 'glibc_min=%s\n' "$package_glibc_min"
printf 'run_binary=%s\n' "$([ "$run_binary" -eq 1 ] && printf true || printf false)"
printf 'require_release_artifact=%s\n' "$([ "$require_release_artifact" -eq 1 ] && printf true || printf false)"
