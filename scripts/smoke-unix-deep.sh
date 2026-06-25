#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in
    /*) ;;
    *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac
RMUX="$TARGET_DIR/debug/rmux"
RMUX_BINARY="$RMUX"
unset RMUX TMUX RMUX_PANE TMUX_PANE TERM_PROGRAM TERM_PROGRAM_VERSION
RMUX="$RMUX_BINARY"
SMOKE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rmux-smoke-deep.XXXXXX")"
export RMUX_TMPDIR="$SMOKE_ROOT"

log() {
    printf '[deep-smoke] %s\n' "$*"
}

fail() {
    printf '[deep-smoke] ERROR: %s\n' "$*" >&2
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
        "$RMUX" -L deep-label kill-server >/dev/null 2>&1 || true
        "$RMUX" -S "$SMOKE_ROOT/custom.sock" kill-server >/dev/null 2>&1 || true
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

expect_failure() {
    local label="$1"
    shift

    local output status
    set +e
    output="$("$@" 2>&1)"
    status=$?
    set -e

    [[ "$status" -ne 0 ]] || fail "$label unexpectedly succeeded"
    [[ "$output" != *panicked* ]] || fail "$label exposed a panic: $output"
    [[ "$output" != *"No such file or directory"* ]] || fail "$label exposed a raw OS error: $output"
    log "$label failed cleanly"
}

pane_field() {
    local target="$1"
    local format="$2"
    "$RMUX" display-message -p -t "$target" "$format"
}

capture_contains() {
    local target="$1"
    local needle="$2"
    "$RMUX" capture-pane -p -t "$target" 2>/dev/null | grep -q "$needle"
}

pane_path_is_tmp() {
    local target="$1"
    local expected="$2"
    [[ "$(pane_field "$target" '#{pane_current_path}')" == "$expected" ]]
}

pane_command_is() {
    local target="$1"
    local expected="$2"
    [[ "$(pane_field "$target" '#{pane_current_command}')" == "$expected" ]]
}

assert_process_formats() {
    local target="$1"
    local fields
    fields="$(pane_field "$target" '#{pane_pid}|#{pane_current_command}|#{pane_current_path}|#{pane_tty}')"
    IFS='|' read -r pane_pid pane_command pane_path pane_tty <<<"$fields"

    [[ "$pane_pid" =~ ^[0-9]+$ ]] || fail "pane_pid must be numeric, got $pane_pid"
    [[ -n "$pane_command" ]] || fail "pane_current_command must be populated"
    [[ -n "$pane_path" ]] || fail "pane_current_path must be populated"
    [[ "$pane_tty" == /dev/* ]] || fail "pane_tty must be under /dev, got $pane_tty"
}

assert_no_rmux_processes_for_root() {
    command -v pgrep >/dev/null 2>&1 || return 0
    local matches
    matches="$(pgrep -af -- "$RMUX" | grep -F "$SMOKE_ROOT" || true)"
    if [[ -n "$matches" ]]; then
        printf '%s\n' "$matches" >&2
        fail "rmux process still references $SMOKE_ROOT"
    fi
}

no_rmux_processes_for_root() {
    command -v pgrep >/dev/null 2>&1 || return 0
    ! pgrep -af -- "$RMUX" | grep -Fq "$SMOKE_ROOT"
}

attach_mode_tree_smoke() {
    command -v expect >/dev/null 2>&1 || {
        log 'SKIP interactive deep smoke: expect not found'
        return 0
    }

    log 'attach, open mode tree, close it, then detach'
    RMUX_BIN="$RMUX" expect <<'EXPECT'
set timeout 6
spawn $env(RMUX_BIN) attach-session -t s1
expect {
    "RIGHT_PANE_READY" {}
    timeout { exit 2 }
}
send "\002w"
expect {
    "s1" {}
    timeout { exit 3 }
}
send "q"
send "\002d"
expect {
    eof {}
    timeout { exit 4 }
}
EXPECT
}

cd "$ROOT"

run cargo build --locked

expect_failure 'list-sessions without server' "$RMUX" list-sessions

run "$RMUX" new-session -d -s s1
run "$RMUX" split-window -h -t s1:0.0
run "$RMUX" new-session -d -s s2
run "$RMUX" new-window -d -t s2 -n logs

s1_panes="$("$RMUX" list-panes -t s1 -F '#{session_name}:#{window_index}.#{pane_index}:#{pane_tty}')"
assert_contains "$s1_panes" 's1:0.0:/dev/' 's1 panes'
assert_contains "$s1_panes" 's1:0.1:/dev/' 's1 panes'

s2_windows="$("$RMUX" list-windows -t s2 -F '#{window_index}:#{window_name}')"
assert_contains "$s2_windows" '1:logs' 's2 windows'

run "$RMUX" send-keys -t s1:0.1 'printf RIGHT_PANE_READY' Enter
wait_until 'right pane capture' 5 capture_contains s1:0.1 RIGHT_PANE_READY

assert_process_formats s1:0.0
tmp_realpath="$(cd /tmp && pwd -P)"
run "$RMUX" send-keys -t s1:0.0 'cd /tmp && pwd' Enter
wait_until 'pane cwd update' 5 pane_path_is_tmp s1:0.0 "$tmp_realpath"

run "$RMUX" send-keys -t s1:0.0 'sleep 5' Enter
wait_until 'foreground sleep command' 5 pane_command_is s1:0.0 sleep
run "$RMUX" send-keys -t s1:0.0 C-c

before_width="$(pane_field s1:0.0 '#{pane_width}')"
run "$RMUX" resize-pane -t s1:0.0 -x 35
after_width="$(pane_field s1:0.0 '#{pane_width}')"
[[ "$after_width" != "$before_width" ]] || fail "resize-pane -x did not change pane_width"

config="$SMOKE_ROOT/rmux.conf"
cat >"$config" <<'CONFIG'
set-option -g status off
set-environment -g RMUX_DEEP_ENV ok
bind-key -T prefix X display-message deep-bound
CONFIG
run "$RMUX" source-file "$config"
status_value="$("$RMUX" show-options -g -v status)"
[[ "$status_value" == 'off' ]] || fail "source-file did not set status off: $status_value"
environment_value="$("$RMUX" show-environment -g RMUX_DEEP_ENV)"
assert_contains "$environment_value" 'RMUX_DEEP_ENV=ok' 'show-environment output'
key_rows="$("$RMUX" list-keys -T prefix)"
assert_contains "$key_rows" 'X       display-message deep-bound' 'list-keys output'

expect_failure 'attach missing target' "$RMUX" attach-session -t missing
expect_failure 'split missing target' "$RMUX" split-window -t missing
expect_failure 'kill missing session' "$RMUX" kill-session -t missing

attach_mode_tree_smoke

run "$RMUX" kill-server
wait_until 'default socket shutdown' 5 expect_failure 'list-sessions after kill-server' "$RMUX" list-sessions
wait_until 'default daemon process cleanup' 5 no_rmux_processes_for_root
assert_no_rmux_processes_for_root

run "$RMUX" -L deep-label new-session -d -s label_socket
label_sessions="$("$RMUX" -L deep-label list-sessions)"
assert_contains "$label_sessions" 'label_socket' '-L list-sessions output'
run "$RMUX" -L deep-label kill-server

custom_socket="$SMOKE_ROOT/custom.sock"
run "$RMUX" -S "$custom_socket" new-session -d -s custom_socket
custom_sessions="$("$RMUX" -S "$custom_socket" list-sessions)"
assert_contains "$custom_sessions" 'custom_socket' '-S list-sessions output'
run "$RMUX" -S "$custom_socket" kill-server

wait_until 'all daemon process cleanup' 5 no_rmux_processes_for_root
assert_no_rmux_processes_for_root
log 'deep unix smoke passed'
