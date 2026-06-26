#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/smoke-installed-rmux.sh <rmux-binary> [options]

Smoke-test an installed or packaged RMUX binary through the user-facing CLI.

Options:
  --skip-daemon              Only test helper fallback and diagnose output
  --require-daemon-command   Require rmux-daemon to be discoverable on PATH
  -h, --help                 Show this help
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

assert_helper_fallback() {
  local stdout stderr status
  stdout="$(mktemp "${TMPDIR:-/tmp}/rmux-help-stdout.XXXXXX")"
  stderr="$(mktemp "${TMPDIR:-/tmp}/rmux-help-stderr.XXXXXX")"
  if "$rmux" --help >"$stdout" 2>"$stderr"; then
    status=0
  else
    status=$?
  fi
  if [ "$status" -ne 0 ] && [ "$status" -ne 1 ]; then
    cat "$stderr" >&2
    rm -f "$stdout" "$stderr"
    die "rmux --help failed with unexpected exit code $status"
  fi
  if ! grep -q 'usage: rmux' "$stdout" "$stderr"; then
    cat "$stderr" >&2
    rm -f "$stdout" "$stderr"
    die "rmux --help did not reach the private helper"
  fi
  rm -f "$stdout" "$stderr"
}

rmux=""
skip_daemon=0
require_daemon_command=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --skip-daemon)
      skip_daemon=1
      shift
      ;;
    --require-daemon-command)
      require_daemon_command=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [ -n "$rmux" ]; then
        die "unexpected extra argument: $1"
      fi
      rmux="$1"
      shift
      ;;
  esac
done

[ -n "$rmux" ] || die "rmux binary is required"

# `--help` is intentionally outside the tiny direct path. It proves that the
# installed public CLI can reach the complete command surface: directly for full
# CLIs, or through the private helper for tiny dispatchers.
assert_helper_fallback
"$rmux" diagnose --json >/dev/null

if [ "$require_daemon_command" -eq 1 ]; then
  command -v rmux-daemon >/dev/null 2>&1 ||
    die "rmux-daemon is not discoverable on PATH"
fi

if [ "$skip_daemon" -eq 0 ]; then
  label="installed-smoke-$$-$(date +%s)"
  session="installed_smoke_$$"
  "$rmux" -L "$label" kill-server >/dev/null 2>&1 || true
  cleanup() {
    "$rmux" -L "$label" kill-server >/dev/null 2>&1 || true
  }
  trap cleanup EXIT

  "$rmux" -L "$label" new-session -d -s "$session" >/dev/null ||
    die "rmux failed to create a session through its daemon"
  "$rmux" -L "$label" has-session -t "$session" >/dev/null ||
    die "rmux failed to find the session created through its daemon"
  cleanup
  trap - EXIT
fi

printf 'rmux=%s\n' "$rmux"
printf 'helper_fallback=ok\n'
printf 'daemon_smoke=%s\n' "$([ "$skip_daemon" -eq 0 ] && printf ok || printf skipped)"
