#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in
    /*) ;;
    *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac
RMUX="${RMUX_BIN:-$TARGET_DIR/debug/rmux}"
SMOKE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rmux-paste-runtime.XXXXXX")"
SESSION="p4c"
PANE="$SESSION:0.0"
export RMUX_TMPDIR="$SMOKE_ROOT/socket"

log() {
    printf '[paste-smoke] %s\n' "$*"
}

fail() {
    printf '[paste-smoke] ERROR: %s\n' "$*" >&2
    exit 1
}

run() {
    log "$*"
    "$@"
}

shell_quote() {
    printf "'%s'" "${1//\'/\'\\\'\'}"
}

wait_until() {
    local description="$1"
    local timeout="$2"
    shift 2

    local deadline=$((SECONDS + timeout))
    until "$@"; do
        if ((SECONDS >= deadline)); then
            fail "timed out waiting for $description"
        fi
        sleep 0.1
    done
}

cleanup() {
    if [[ -x "$RMUX" ]]; then
        "$RMUX" kill-server >/dev/null 2>&1 || true
    fi
    rm -rf "$SMOKE_ROOT"
}
trap cleanup EXIT

require_tool() {
    command -v "$1" >/dev/null 2>&1 || fail "$1 is required for paste runtime smoke"
}

capture_contains() {
    local needle="$1"
    "$RMUX" capture-pane -p -t "$PANE" 2>/dev/null | grep -Fq "$needle"
}

server_is_absent() {
    ! "$RMUX" list-sessions >/dev/null 2>&1
}

write_payload() {
    local payload_file="$1"

    {
        printf '\033[200~'
        printf 'RMUX_P4C_BEGIN\n'
        printf 'ASCII line survives attach paste\n'
        printf 'UTF8: \346\235\261\344\272\254 | \355\225\234\352\270\200 | cafe\314\201\n'
        printf '\002 prefix byte stays payload\n'
        printf '\033[<64;2;2M mouse-looking bytes stay payload\n'
        printf '\033[9;2u csi-u-looking bytes stay payload\n'
        printf '\033[200~ nested-start-looking bytes stay payload\n'
        printf 'RMUX_P4C_END\n'
        printf '\033[201~'
    } >"$payload_file"
}

attach_paste_and_detach() {
    local payload_file="$1"

    log 'attach-session, paste bracketed payload, EOF cat, then detach'
    python3 - "$RMUX" "$SESSION" "$payload_file" <<'PY'
import os
import pty
import select
import signal
import sys
import time

rmux_bin, session, payload_path = sys.argv[1:4]
with open(payload_path, "rb") as payload_file:
    payload = payload_file.read()

pid, fd = pty.fork()
if pid == 0:
    os.execlp(rmux_bin, rmux_bin, "attach-session", "-t", session)

seen = bytearray()


def pump_until(needle: bytes, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        remaining = max(0.0, deadline - time.monotonic())
        readable, _, _ = select.select([fd], [], [], min(0.1, remaining))
        if fd in readable:
            try:
                data = os.read(fd, 8192)
            except OSError:
                return needle in seen
            if not data:
                return needle in seen
            os.write(1, data)
            seen.extend(data)
            if needle in seen:
                return True
        try:
            child_pid, _status = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return needle in seen
        if child_pid == pid:
            return needle in seen
    return False


try:
    if not pump_until(session.encode(), 8):
        sys.exit(2)
    os.write(fd, payload)
    os.write(fd, b"\x04\x04")
    if not pump_until(b"RMUX_P4C_CAT_DONE", 8):
        sys.exit(3)
    os.write(fd, b"\x02d")
    if not pump_until(b"[detached", 8):
        sys.exit(4)
    sys.exit(0)
finally:
    try:
        os.close(fd)
    except OSError:
        pass
    try:
        os.kill(pid, signal.SIGTERM)
    except OSError:
        pass
    try:
        os.waitpid(pid, os.WNOHANG)
    except (ChildProcessError, OSError):
        pass
PY
}

cd "$ROOT"

require_tool shasum
require_tool cmp
require_tool python3

run cargo build --locked

payload_file="$SMOKE_ROOT/payload.bin"
captured_file="$SMOKE_ROOT/captured.bin"
write_payload "$payload_file"
expected_sha="$(shasum -a 256 "$payload_file" | awk '{print $1}')"

run "$RMUX" new-session -d -s "$SESSION"

captured_quoted="$(shell_quote "$captured_file")"
collector_command="cat > $captured_quoted; printf 'RMUX_P4C_CAT_DONE\\n'; shasum -a 256 $captured_quoted | awk '{print \"RMUX_P4C_SHA \" \$1}'"
run "$RMUX" send-keys -t "$PANE" "$collector_command" Enter
wait_until 'cat capture file creation' 5 test -f "$captured_file"

attach_paste_and_detach "$payload_file"

wait_until 'capture marker' 5 capture_contains 'RMUX_P4C_CAT_DONE'
wait_until 'capture sha marker' 5 capture_contains 'RMUX_P4C_SHA'

cmp -s "$payload_file" "$captured_file" || {
    log "expected sha: $expected_sha"
    log "actual sha: $(shasum -a 256 "$captured_file" | awk '{print $1}')"
    fail 'captured pane input did not match bracketed paste payload'
}

run "$RMUX" kill-server
wait_until 'server shutdown' 5 server_is_absent

log 'paste runtime unix smoke passed'
