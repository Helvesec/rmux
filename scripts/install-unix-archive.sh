#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: ./install.sh [options]

Install an extracted RMUX Unix archive while preserving the tiny/full layout.

Options:
  --prefix DIR   Installation prefix (default: $RMUX_INSTALL_PREFIX or ~/.local)
  --no-verify    Skip the post-install CLI layout smoke
  -h, --help     Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

archive_root() {
  local script_dir
  script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]:-$0}")" && pwd)"
  printf '%s\n' "$script_dir"
}

install_file() {
  local mode source target target_dir tmp
  mode="$1"
  source="$2"
  target="$3"
  target_dir="$(dirname "$target")"
  mkdir -p "$target_dir"
  tmp="$target_dir/.rmux-install-$(basename "$target").$$"
  install -m "$mode" "$source" "$tmp"
  mv -f "$tmp" "$target"
}

install_tree() {
  local source target
  source="$1"
  target="$2"
  [ -d "$source" ] || return 0
  mkdir -p "$target"
  cp -R "$source/." "$target/"
}

verify_layout() {
  local rmux stdout stderr status
  rmux="$1"
  stdout="$(mktemp "${TMPDIR:-/tmp}/rmux-install-stdout.XXXXXX")"
  stderr="$(mktemp "${TMPDIR:-/tmp}/rmux-install-stderr.XXXXXX")"
  if "$rmux" --help >"$stdout" 2>"$stderr"; then
    status=0
  else
    status=$?
  fi
  if { [ "$status" -ne 0 ] && [ "$status" -ne 1 ]; } || ! grep -q 'usage: rmux' "$stdout" "$stderr"; then
    cat "$stderr" >&2
    rm -f "$stdout" "$stderr"
    die "installed rmux could not reach its full CLI helper"
  fi
  rm -f "$stdout" "$stderr"
}

prefix="${RMUX_INSTALL_PREFIX:-${HOME:-}/.local}"
verify=1

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix)
      [ "$#" -ge 2 ] || die "--prefix requires a value"
      prefix="$2"
      shift 2
      ;;
    --no-verify)
      verify=0
      shift
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

[ -n "$prefix" ] || die "prefix must not be empty"
root="$(archive_root)"

for required in bin/rmux bin/rmux-daemon libexec/rmux/rmux; do
  [ -x "$root/$required" ] || die "archive is missing executable $required"
done

# Install private targets first. The public tiny dispatcher is installed last so
# upgrades never expose a new rmux that cannot reach its matching full helper.
install_file 0755 "$root/libexec/rmux/rmux" "$prefix/libexec/rmux/rmux"
install_file 0755 "$root/bin/rmux-daemon" "$prefix/bin/rmux-daemon"
install_tree "$root/share" "$prefix/share"
install_file 0755 "$root/bin/rmux" "$prefix/bin/rmux"

if [ "$verify" -eq 1 ]; then
  verify_layout "$prefix/bin/rmux"
fi

printf 'rmux installed to %s\n' "$prefix/bin/rmux"
