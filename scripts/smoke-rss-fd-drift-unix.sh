#!/usr/bin/env bash
set -euo pipefail

RMUX_BIN="${RMUX_BIN:-target/release/rmux}"
FD_DRIFT_MAX="${RMUX_FD_DRIFT_MAX:-8}"
RSS_DRIFT_KIB_MAX="${RMUX_RSS_DRIFT_KIB_MAX:-32768}"
ITERATIONS="${RMUX_DRIFT_ITERATIONS:-30}"

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

for value in "$FD_DRIFT_MAX" "$RSS_DRIFT_KIB_MAX" "$ITERATIONS"; do
    is_non_negative_integer "$value" || fail "expected numeric threshold, got: $value"
done

if [ ! -x "$RMUX_BIN" ]; then
    fail "rmux binary is not executable: $RMUX_BIN"
fi

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
    ps -eo pid=,args= | awk -v socket="$SOCKET_PATH" '
        index($0, "rmux-daemon") && index($0, socket) {
            print $1
            exit
        }
    '
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

DAEMON_PID="$(find_daemon_pid)"
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

log "RSS/FD drift smoke passed"
