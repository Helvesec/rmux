#!/usr/bin/env sh
set -eu

fail=0

check_budget() {
  crate="$1"
  path="$2"
  budget="$3"
  if [ ! -d "$path" ]; then
    count=0
  else
    count="$(grep -R --include='*.rs' -E '#[[:space:]]*\[[[:space:]]*cfg[[:space:]]*\([[:space:]]*target_os[[:space:]]*=' "$path" 2>/dev/null | wc -l | tr -d ' ')"
  fi
  printf '%-14s %4s / %s\n' "$crate" "$count" "$budget"
  if [ "$count" -gt "$budget" ]; then
    fail=1
  fi
}

check_budget rmux-types crates/rmux-types/src 0
check_budget rmux-core crates/rmux-core/src 0
check_budget rmux-proto crates/rmux-proto/src 0
# Exact inventory and layer rationale: docs/platform-neutrality-budget.md
check_budget rmux-server crates/rmux-server/src 9
check_budget rmux-client crates/rmux-client/src 10
check_budget rmux-ipc crates/rmux-ipc/src 15
check_budget rmux-pty crates/rmux-pty/src 20
check_budget rmux-os crates/rmux-os/src 30
check_budget rmux-bin src 10

if [ "$fail" -ne 0 ]; then
  echo "cfg(target_os) budget exceeded." >&2
  exit 1
fi

echo "cfg(target_os) check passed."
