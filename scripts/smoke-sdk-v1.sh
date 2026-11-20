#!/usr/bin/env sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"
if [ -z "${RMUX_IDENTITY_HEAD:-}" ]; then
  exec python3 "$repo_root/scripts/candidate_identity.py" with -- sh "$0" "$@"
fi
python3 "$repo_root/scripts/candidate_identity.py" validate

root="${RMUX_SDK_SMOKE_ROOT:-/tmp/rmux-sdk-v1-smoke-$$}"
socket="$root/daemon.sock"
lock="$socket.startup-lock"

case "$root" in
  /tmp/rmux-sdk-v1-smoke-*) ;;
  *)
    echo "SDK smoke root must be /tmp/rmux-sdk-v1-smoke-*, got: $root" >&2
    exit 2
    ;;
esac

case "$root" in
  *"/../"*|*"/.."|*"/./"*|*"/.")
    echo "SDK smoke root must not contain relative path components: $root" >&2
    exit 2
    ;;
esac

cleanup() {
  if [ -S "$socket" ]; then
    cargo run --quiet --locked --bin rmux -- -S "$socket" kill-server >/dev/null 2>&1 || true
  fi
  rm -rf "$root"
}
trap cleanup EXIT HUP INT TERM

rm -rf "$root"
export TMPDIR="${TMPDIR:-/tmp}"
export RMUX_TMPDIR="${RMUX_TMPDIR:-/tmp}"
export RMUX_SDK_SMOKE_ROOT="$root"

cargo test -p rmux-sdk --locked --test smoke_v1 -- --nocapture
python3 "$repo_root/scripts/check_candidate_cleanup.py"

if [ -e "$socket" ]; then
  echo "SDK smoke left socket behind: $socket" >&2
  exit 1
fi

if [ -e "$lock" ]; then
  echo "SDK smoke left startup lock behind: $lock" >&2
  exit 1
fi

if [ -d "$root" ]; then
  echo "SDK smoke left endpoint root behind: $root" >&2
  exit 1
fi

echo "SDK v1 daemon smoke passed with /tmp-scoped endpoint cleanup."
