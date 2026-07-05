#!/usr/bin/env bash
set -euo pipefail

RMUX_BIN="${RMUX_BIN:-target/release/rmux}"
FD_DRIFT_MAX="${RMUX_FD_DRIFT_MAX:-8}"
RSS_DRIFT_KIB_MAX="${RMUX_RSS_DRIFT_KIB_MAX:-32768}"
ITERATIONS="${RMUX_DRIFT_ITERATIONS:-30}"
CHURN_ITERATIONS="${RMUX_DAEMON_CHURN_ITERATIONS:-$ITERATIONS}"

log() {
    printf '[rss-fd-smoke] %s\n' "$*"
}

fail() {
    printf '[rss-fd-smoke] ERROR: %s\n' "$*" >&2
    exit 1
}

is_non_negative_integer() {
    case "$1" in
        ''|*[!0-9]*)
            return 1
            ;;
    esac
}

for value in "$FD_DRIFT_MAX" "$RSS_DRIFT_KIB_MAX" "$ITERATIONS" "$CHURN_ITERATIONS"; do
    is_non_negative_integer "$value" || fail "expected numeric threshold, got: $value"
done

if [ ! -x "$RMUX_BIN" ]; then
    fail "rmux binary is not executable: $RMUX_BIN"
fi
case "$RMUX_BIN" in
    */debug/rmux|target/debug/rmux)
        fail "resource smoke must run against a release artifact, got debug binary: $RMUX_BIN"
        ;;
esac

if [ ! -d /proc ]; then
    log "skipping: /proc is unavailable"
    exit 0
fi

SMOKE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rmux-rss-fd-smoke.XXXXXX")"
SOCKET_PATH="$SMOKE_ROOT/rmux.sock"

cleanup() {
    "$RMUX_BIN" -S "$SOCKET_PATH" kill-server >/dev/null 2>&1 || true
    rm -rf "$SMOKE_ROOT"
}
trap cleanup EXIT

find_daemon_pid() {
    local socket="$1"
    ps -eo pid=,args= | awk -v socket="$socket" '
        index($0, "rmux-daemon") && index($0, socket) {
            print $1
            exit
        }
    '
}

find_daemon_pids_under_root() {
    ps -eo pid=,args= | awk -v root="$SMOKE_ROOT" '
        index($0, "rmux-daemon") && index($0, root) {
            print $1
        }
    '
}

wait_for_no_daemons_under_root() {
    local deadline
    deadline=$((SECONDS + 10))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if [ -z "$(find_daemon_pids_under_root)" ]; then
            return 0
        fi
        sleep 0.1
    done
    fail "daemon process leak under $SMOKE_ROOT: $(find_daemon_pids_under_root | tr '\n' ' ')"
}

wait_for_path_absent() {
    local path="$1"
    local deadline
    deadline=$((SECONDS + 10))
    while [ "$SECONDS" -lt "$deadline" ]; do
        [ ! -e "$path" ] && return 0
        sleep 0.1
    done
    fail "path still exists after daemon shutdown: $path"
}

fd_count() {
    find "/proc/$1/fd" -maxdepth 1 -type l 2>/dev/null | wc -l | tr -d ' '
}

rss_kib() {
    awk '/^VmRSS:/ { print $2; exit }' "/proc/$1/status" 2>/dev/null
}

measure_pair() {
    local pid="$1"
    local fd rss
    fd="$(fd_count "$pid")"
    rss="$(rss_kib "$pid")"
    if [ -z "$fd" ] || [ -z "$rss" ]; then
        fail "failed to measure daemon pid=$pid"
    fi
    printf '%s %s\n' "$fd" "$rss"
}

log "using $RMUX_BIN"
"$RMUX_BIN" -S "$SOCKET_PATH" start-server >/dev/null
sleep 0.2

DAEMON_PID="$(find_daemon_pid "$SOCKET_PATH")"
if [ -z "$DAEMON_PID" ]; then
    fail "could not find daemon for socket $SOCKET_PATH"
fi

"$RMUX_BIN" -S "$SOCKET_PATH" new-session -d -s drift /bin/sh >/dev/null
sleep 0.2

read -r BASE_FD BASE_RSS < <(measure_pair "$DAEMON_PID")

i=1
while [ "$i" -le "$ITERATIONS" ]; do
    "$RMUX_BIN" -S "$SOCKET_PATH" display-message -p -t drift '#{session_name}:#{window_panes}' >/dev/null
    "$RMUX_BIN" -S "$SOCKET_PATH" send-keys -t drift "echo drift-$i" Enter >/dev/null
    "$RMUX_BIN" -S "$SOCKET_PATH" capture-pane -t drift -p >/dev/null
    i=$((i + 1))
done

sleep 0.2
read -r FINAL_FD FINAL_RSS < <(measure_pair "$DAEMON_PID")

FD_DRIFT=$((FINAL_FD - BASE_FD))
RSS_DRIFT=$((FINAL_RSS - BASE_RSS))

log "pid=$DAEMON_PID fd_base=$BASE_FD fd_final=$FINAL_FD fd_drift=$FD_DRIFT"
log "rss_base_kib=$BASE_RSS rss_final_kib=$FINAL_RSS rss_drift_kib=$RSS_DRIFT"

if [ "$FD_DRIFT" -gt "$FD_DRIFT_MAX" ]; then
    fail "fd drift $FD_DRIFT exceeds max $FD_DRIFT_MAX"
fi

if [ "$RSS_DRIFT" -gt "$RSS_DRIFT_KIB_MAX" ]; then
    fail "RSS drift ${RSS_DRIFT}KiB exceeds max ${RSS_DRIFT_KIB_MAX}KiB"
fi

log "steady RSS/FD drift smoke passed"

"$RMUX_BIN" -S "$SOCKET_PATH" kill-server >/dev/null 2>&1 || true
wait_for_no_daemons_under_root

i=1
while [ "$i" -le "$CHURN_ITERATIONS" ]; do
    churn_socket="$SMOKE_ROOT/churn-$i.sock"
    "$RMUX_BIN" -S "$churn_socket" new-session -d -s "churn$i" /bin/sh >/dev/null
    sleep 0.05
    if [ -z "$(find_daemon_pid "$churn_socket")" ]; then
        fail "could not find churn daemon for socket $churn_socket"
    fi
    "$RMUX_BIN" -S "$churn_socket" kill-server >/dev/null
    wait_for_path_absent "$churn_socket"
    i=$((i + 1))
done

wait_for_no_daemons_under_root
log "daemon churn process/socket smoke passed iterations=$CHURN_ITERATIONS"
log "RSS/FD/process drift smoke passed"
