#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/glibc-symbol-floor.sh <elf> [<elf> ...]

Print the newest GLIBC symbol version imported by the supplied ELF binaries.
READELF may override the readelf executable for cross-toolchains and tests.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

if [ "$#" -eq 0 ]; then
  usage >&2
  exit 2
fi
case "$1" in
  -h|--help)
    usage
    exit 0
    ;;
esac

readelf_bin="${READELF:-readelf}"
command -v "$readelf_bin" >/dev/null 2>&1 || die "missing required command: $readelf_bin"

versions=""
for binary in "$@"; do
  [ -f "$binary" ] || die "ELF binary not found: $binary"
  binary_versions="$($readelf_bin --version-info "$binary" 2>/dev/null | sed -n 's/.*Name: GLIBC_\([0-9][0-9.]*\).*/\1/p')"
  [ -n "$binary_versions" ] || die "no imported GLIBC symbol versions found in $binary"
  versions="${versions}${versions:+
}${binary_versions}"
done

printf '%s\n' "$versions" | LC_ALL=C sort -Vu | tail -n 1
