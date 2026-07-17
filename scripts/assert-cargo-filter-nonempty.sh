#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/assert-cargo-filter-nonempty.sh <min-tests> -- <cargo-test-args...>

Fail if a filtered `cargo test` command would select fewer than <min-tests>
tests. Pass cargo arguments after `--`, excluding the leading `cargo`.

Example:
  scripts/assert-cargo-filter-nonempty.sh 1 -- test -p rmux tiny_main --locked
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

[ "$#" -gt 0 ] || {
  usage >&2
  exit 2
}

case "$1" in
  -h|--help)
    usage
    exit 0
    ;;
esac

min_tests="$1"
shift
case "$min_tests" in
  ''|*[!0-9]*) die "<min-tests> must be a positive integer" ;;
  0) die "<min-tests> must be greater than zero" ;;
esac

[ "$#" -gt 0 ] && [ "$1" = "--" ] || die "expected -- before cargo arguments"
shift
[ "$#" -gt 0 ] || die "cargo arguments are required"
[ "$1" = "test" ] || die "cargo arguments must start with test"

if ! output="$(cargo "$@" -- --list 2>&1)"; then
  printf '%s\n' "$output" >&2
  die "cargo $* -- --list failed"
fi
count="$(printf '%s\n' "$output" | awk '/: test$/ { count += 1 } END { print count + 0 }')"

if [ "$count" -lt "$min_tests" ]; then
  printf '%s\n' "$output" >&2
  die "cargo $* selected $count tests; expected at least $min_tests"
fi

printf 'cargo-filter-nonempty=ok selected=%s min=%s command=cargo %s\n' \
  "$count" "$min_tests" "$*"
