#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  printf 'usage: %s <repository> <expected-head>\n' "$0" >&2
  exit 2
fi

repository="$1"
expected_head="$2"

[ -d "$repository/.git" ] || {
  printf 'error: package repository is not a Git checkout: %s\n' "$repository" >&2
  exit 1
}

case "$expected_head" in
  *[!0-9a-fA-F]*|"")
    printf 'error: invalid expected package repository HEAD\n' >&2
    exit 1
    ;;
esac

case "${#expected_head}" in
  40|64) ;;
  *)
    printf 'error: invalid expected package repository HEAD length\n' >&2
    exit 1
    ;;
esac

actual_head="$(git -C "$repository" rev-parse --verify 'HEAD^{commit}')"
if [ "$actual_head" != "$expected_head" ]; then
  printf 'error: package repository advanced after release preparation\n' >&2
  exit 1
fi

printf 'package_repository_base=%s\n' "$actual_head"
