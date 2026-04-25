#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
RMUX="$ROOT/target/debug/rmux"
SMOKE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rmux-smoke.XXXXXX")"
export RMUX_TMPDIR="$SMOKE_ROOT"

log() {
    printf '[smoke] %s\n' "$*"
}

fail() {
    printf '[smoke] ERROR: %s\n' "$*" >&2
    exit 1
}

run() {
    log "$*"
    "$@"
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

assert_contains() {
    local haystack="$1"
    local needle="$2"
    local label="$3"

    if [[ "$haystack" != *"$needle"* ]]; then
        fail "$label did not contain $needle; got: $haystack"
    fi
}

assert_panes() {
    local panes="$1"
    local pane_count=0

    while IFS='|' read -r pane_index pane_command pane_path pane_tty; do
        [[ -n "$pane_index" ]] || fail "pane index must be populated"
        [[ -n "$pane_command" ]] || fail "pane command must be populated"
        [[ -n "$pane_path" ]] || fail "pane current path must be populated"
        [[ -n "$pane_tty" ]] || fail "pane tty must be populated"
        [[ "$pane_tty" == /dev/* ]] || fail "pane tty must be under /dev, got $pane_tty"
        pane_count=$((pane_count + 1))
    done <<<"$panes"

    [[ "$pane_count" -eq 2 ]] || fail "expected 2 panes after split, got $pane_count: $panes"
}

capture_contains_ready() {
    "$RMUX" capture-pane -p -t smoke:0.0 2>/dev/null | grep -q 'RMUX_SMOKE_READY'
}

server_is_absent() {
    ! "$RMUX" list-sessions >/dev/null 2>&1
}

attach_and_detach() {
    command -v expect >/dev/null 2>&1 || {
        log 'SKIP attach smoke: expect not found'
        return 0
    }

    log 'attach-session -t smoke, then detach with Ctrl-b d'
    RMUX_BIN="$RMUX" expect <<'EXPECT'
set timeout 5
spawn $env(RMUX_BIN) attach-session -t smoke
expect {
    "smoke" {}
    timeout { exit 2 }
}
send "\002d"
expect {
    eof {}
    timeout { exit 3 }
}
EXPECT
}

cd "$ROOT"

run cargo build --locked

run "$RMUX" new-session -d -s smoke
sessions="$("$RMUX" list-sessions)"
assert_contains "$sessions" 'smoke' 'list-sessions output'

run "$RMUX" split-window -h -t smoke:0.0
panes="$("$RMUX" list-panes -t smoke -F '#{pane_index}|#{pane_current_command}|#{pane_current_path}|#{pane_tty}')"
assert_panes "$panes"

run "$RMUX" resize-pane -t smoke:0.0 -x 40
run "$RMUX" resize-pane -t smoke:0.0 -y 10

session_shape="$("$RMUX" display-message -p -t smoke '#{session_name}:#{session_windows}:#{window_panes}')"
[[ "$session_shape" == 'smoke:1:2' ]] || fail "unexpected session shape: $session_shape"

run "$RMUX" send-keys -t smoke:0.0 'printf RMUX_SMOKE_READY' Enter
wait_until 'captured pane output' 5 capture_contains_ready

attach_and_detach
attach_and_detach

sessions="$("$RMUX" list-sessions)"
assert_contains "$sessions" 'smoke' 'list-sessions after detach'

run "$RMUX" kill-server
wait_until 'server shutdown' 5 server_is_absent

run "$RMUX" new-session -d -s smoke2
sessions="$("$RMUX" list-sessions)"
assert_contains "$sessions" 'smoke2' 'list-sessions after restart'
run "$RMUX" kill-server

log 'unix smoke passed'
