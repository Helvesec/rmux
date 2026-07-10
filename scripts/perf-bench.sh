#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

iterations=30
line_count=10000
binary=""
output_dir="target/perf"
skip_build=0
fail_on_budget=0
source_command_count="${RMUX_PERF_SOURCE_COMMANDS:-1000}"
hook_storm_events="${RMUX_PERF_HOOK_STORM_EVENTS:-25}"
daemon_churn_cycles="${RMUX_PERF_DAEMON_CHURN_CYCLES:-10}"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --iterations)
            iterations="$2"
            shift 2
            ;;
        --line-count)
            line_count="$2"
            shift 2
            ;;
        --binary)
            binary="$2"
            shift 2
            ;;
        --output-dir)
            output_dir="$2"
            shift 2
            ;;
        --skip-build)
            skip_build=1
            shift
            ;;
        --fail-on-budget)
            fail_on_budget=1
            shift
            ;;
        -h|--help)
            cat <<'USAGE'
usage: scripts/perf-bench.sh [--iterations N] [--line-count N]
                             [--binary PATH] [--output-dir DIR]
                             [--skip-build] [--fail-on-budget]
USAGE
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            exit 2
            ;;
    esac
done

is_positive_integer() {
    case "$1" in
        ''|*[!0-9]*)
            return 1
            ;;
    esac
    [ "$1" -gt 0 ]
}

if ! is_positive_integer "$iterations"; then
    echo "--iterations must be a positive integer, got: $iterations" >&2
    exit 2
fi

if ! is_positive_integer "$line_count"; then
    echo "--line-count must be a positive integer, got: $line_count" >&2
    exit 2
fi

if ! is_positive_integer "$source_command_count"; then
    echo "RMUX_PERF_SOURCE_COMMANDS must be a positive integer, got: $source_command_count" >&2
    exit 2
fi

if ! is_positive_integer "$hook_storm_events"; then
    echo "RMUX_PERF_HOOK_STORM_EVENTS must be a positive integer, got: $hook_storm_events" >&2
    exit 2
fi

if ! is_positive_integer "$daemon_churn_cycles"; then
    echo "RMUX_PERF_DAEMON_CHURN_CYCLES must be a positive integer, got: $daemon_churn_cycles" >&2
    exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
source "$repo_root/scripts/perf-provenance.sh"

if [ -z "$binary" ]; then
    binary="${CARGO_TARGET_DIR:-target}/release/rmux"
fi

if [ "$skip_build" -eq 0 ]; then
    cargo build --locked --release
fi

if [ ! -x "$binary" ]; then
    echo "rmux binary was not found or is not executable: $binary" >&2
    exit 1
fi

binary="$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")"
case "$binary" in
    */release/rmux) ;;
    *)
        echo "perf bench requires the release-profile rmux binary, got: $binary" >&2
        exit 2
        ;;
esac

rmux_perf_capture_current_identity "$binary" "$skip_build"
git_commit="$RMUX_PERF_GIT_COMMIT"
git_describe="$RMUX_PERF_GIT_DESCRIBE"
platform="$RMUX_PERF_PLATFORM"
machine="$RMUX_PERF_MACHINE"
host_fingerprint="$RMUX_PERF_HOST_FINGERPRINT_VALUE"
expected_git_commit="$RMUX_PERF_EXPECTED_COMMIT_VALUE"
expected_platform="$RMUX_PERF_EXPECTED_PLATFORM_VALUE"
provenance_invocation="$RMUX_PERF_PROVENANCE_VALUE"
binary_sha256="$RMUX_PERF_BINARY_SHA256"
binary_version="$RMUX_PERF_BINARY_VERSION"
build_mode="$RMUX_PERF_BUILD_MODE"
mkdir -p "$output_dir"

metric_names=()
metric_p50=()
metric_p95=()
metric_mean=()
metric_min=()
metric_max=()
metric_budget=()
metric_status=()
metric_samples=()

unique_socket() {
    local metric="$1"
    printf 'rmux-perf-%s-%s-%s' "$metric" "$$" "$(date +%s%N)"
}

cleanup_socket() {
    local socket="$1"
    "$binary" -L "$socket" kill-server >/dev/null 2>&1 || true
}

run_timed_ms() {
    local output_file="$1"
    shift
    local start_ns end_ns status
    start_ns="$(date +%s%N)"
    set +e
    "$binary" "$@" >"$output_file" 2>&1
    status=$?
    set -e
    end_ns="$(date +%s%N)"
    if [ "$status" -ne 0 ]; then
        cat "$output_file" >&2
        echo "rmux $* failed with exit code $status" >&2
        exit "$status"
    fi
    awk -v start="$start_ns" -v end="$end_ns" 'BEGIN { printf "%.3f", (end - start) / 1000000 }'
}

percentile() {
    local percentile_value="$1"
    shift
    printf '%s\n' "$@" | sort -n | awk -v p="$percentile_value" '
        NF { values[++count] = $1 }
        END {
            idx = int((p / 100) * count)
            if (((p / 100) * count) > idx) {
                idx++
            }
            if (idx < 1) {
                idx = 1
            }
            printf "%.3f", values[idx]
        }
    '
}

mean_value() {
    printf '%s\n' "$@" | awk '{ sum += $1; count++ } END { printf "%.3f", sum / count }'
}

min_value() {
    printf '%s\n' "$@" | sort -n | head -n 1
}

max_value() {
    printf '%s\n' "$@" | sort -n | tail -n 1
}

new_line_script() {
    local path
    path="$(mktemp "${TMPDIR:-/tmp}/rmux-perf-lines.XXXXXX")"
    cat >"$path" <<SCRIPT
#!/bin/sh
i=1
while [ "\$i" -le "$line_count" ]; do
    printf 'rmux-perf-line-%s\n' "\$i"
    i=\$((i + 1))
done
sleep 30
SCRIPT
    chmod +x "$path"
    printf '%s' "$path"
}

new_large_source_file() {
    local path
    path="$(mktemp "${TMPDIR:-/tmp}/rmux-perf-source.XXXXXX")"
    for ((index = 1; index <= source_command_count; index++)); do
        printf 'set-option -g @perf-source-%05d value-%05d\n' "$index" "$index"
    done >"$path"
    printf '%s' "$path"
}

new_hook_storm_source_file() {
    local path
    path="$(mktemp "${TMPDIR:-/tmp}/rmux-perf-hooks.XXXXXX")"
    for ((index = 1; index <= hook_storm_events; index++)); do
        printf 'set-option -g @perf-hook-storm-%03d value-%03d\n' "$index" "$index"
    done >"$path"
    printf '%s' "$path"
}

heavy_status_format() {
    local format=""
    for ((index = 1; index <= 80; index++)); do
        format="${format}#{session_name}:#{window_index}:#{pane_index}:#{window_panes}:#{?pane_active,active,inactive}|"
    done
    printf '%s' "$format"
}

wait_for_line_marker() {
    local socket="$1"
    local output_file="$2"
    local marker="rmux-perf-line-$line_count"
    local deadline=$((SECONDS + 15))

    while [ "$SECONDS" -lt "$deadline" ]; do
        "$binary" -L "$socket" capture-pane -t perf -p -S "-$line_count" >"$output_file" 2>&1
        if grep -Fq "$marker" "$output_file"; then
            return 0
        fi
        sleep 0.1
    done

    echo "timed out waiting for pane output marker $marker" >&2
    return 1
}

sample_diagnose() {
    local output_file
    output_file="$(mktemp)"
    run_timed_ms "$output_file" diagnose --json
    rm -f "$output_file"
}

sample_daemon_startup() {
    local socket output_file
    socket="$(unique_socket daemon-startup)"
    output_file="$(mktemp)"
    run_timed_ms "$output_file" -L "$socket" start-server
    cleanup_socket "$socket"
    rm -f "$output_file"
}

sample_new_session_sh() {
    local socket output_file
    socket="$(unique_socket new-sh)"
    output_file="$(mktemp)"
    run_timed_ms "$output_file" -L "$socket" new-session -d -s perf /bin/sh
    cleanup_socket "$socket"
    rm -f "$output_file"
}

sample_split_window_sh() {
    local socket output_file
    socket="$(unique_socket split-sh)"
    output_file="$(mktemp)"
    "$binary" -L "$socket" new-session -d -s perf /bin/sh >/dev/null
    run_timed_ms "$output_file" -L "$socket" split-window -d -t perf /bin/sh
    cleanup_socket "$socket"
    rm -f "$output_file"
}

sample_send_keys() {
    local socket output_file
    socket="$(unique_socket send-keys)"
    output_file="$(mktemp)"
    "$binary" -L "$socket" new-session -d -s perf /bin/sh >/dev/null
    run_timed_ms "$output_file" -L "$socket" send-keys -t perf RMUX_PERF Enter
    cleanup_socket "$socket"
    rm -f "$output_file"
}

sample_resize_pane() {
    local socket output_file
    socket="$(unique_socket resize)"
    output_file="$(mktemp)"
    "$binary" -L "$socket" new-session -d -s perf /bin/sh >/dev/null
    run_timed_ms "$output_file" -L "$socket" resize-pane -t perf -x 100
    cleanup_socket "$socket"
    rm -f "$output_file"
}

sample_pane_output_ready() {
    local socket output_file script start_ns end_ns
    socket="$(unique_socket output-ready)"
    output_file="$(mktemp)"
    script="$(new_line_script)"
    "$binary" -L "$socket" new-session -d -s perf /bin/sh "$script" >/dev/null
    start_ns="$(date +%s%N)"
    wait_for_line_marker "$socket" "$output_file"
    end_ns="$(date +%s%N)"
    cleanup_socket "$socket"
    rm -f "$output_file" "$script"
    awk -v start="$start_ns" -v end="$end_ns" 'BEGIN { printf "%.3f", (end - start) / 1000000 }'
}

sample_capture_pane() {
    local socket output_file script marker ms
    socket="$(unique_socket capture)"
    output_file="$(mktemp)"
    script="$(new_line_script)"
    marker="rmux-perf-line-$line_count"
    "$binary" -L "$socket" new-session -d -s perf /bin/sh "$script" >/dev/null
    wait_for_line_marker "$socket" "$output_file"
    ms="$(run_timed_ms "$output_file" -L "$socket" capture-pane -t perf -p -S "-$line_count")"
    if ! grep -Fq "$marker" "$output_file"; then
        echo "capture output did not include $marker" >&2
        exit 1
    fi
    cleanup_socket "$socket"
    rm -f "$output_file" "$script"
    printf '%s' "$ms"
}

sample_attach_render() {
    local socket output_file script marker ms status
    socket="$(unique_socket attach-render)"
    output_file="$(mktemp)"
    script="$(new_line_script)"
    marker="rmux-perf-line-$line_count"
    "$binary" -L "$socket" new-session -d -s perf /bin/sh "$script" >/dev/null
    wait_for_line_marker "$socket" "$output_file"
    set +e
    ms="$(
        python3 - "$binary" "$socket" "$marker" <<'PY'
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time

rmux, socket, marker = sys.argv[1], sys.argv[2], sys.argv[3].encode()
env = os.environ.copy()
env.setdefault("TERM", "xterm-256color")
env["COLUMNS"] = "80"
env["LINES"] = "24"
started = time.perf_counter()
pid, fd = pty.fork()
if pid == 0:
    os.execve(rmux, [rmux, "-L", socket, "attach-session", "-t", "perf"], env)

try:
    winsize = struct.pack("HHHH", 24, 80, 0, 0)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, winsize)
    tail = bytearray()
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        readable, _, _ = select.select([fd], [], [], 0.1)
        if fd not in readable:
            continue
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        if not data:
            break
        tail.extend(data)
        if len(tail) > 65536:
            del tail[:-65536]
        if marker in tail:
            print(f"{(time.perf_counter() - started) * 1000.0:.3f}")
            raise SystemExit(0)
    raise SystemExit("attach render marker was not observed")
finally:
    try:
        os.write(fd, b"\x02d")
    except OSError:
        pass
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
    except OSError:
        pass
PY
    )"
    status=$?
    set -e
    cleanup_socket "$socket"
    rm -f "$output_file" "$script"
    if [ "$status" -ne 0 ]; then
        exit "$status"
    fi
    printf '%s' "$ms"
}

sample_source_file_large() {
    local socket output_file source_file
    socket="$(unique_socket source-large)"
    output_file="$(mktemp)"
    source_file="$(new_large_source_file)"
    "$binary" -L "$socket" new-session -d -s perf /bin/sh >/dev/null
    run_timed_ms "$output_file" -L "$socket" source-file "$source_file"
    cleanup_socket "$socket"
    rm -f "$output_file" "$source_file"
}

sample_status_format_heavy() {
    local socket output_file format
    socket="$(unique_socket status-format)"
    output_file="$(mktemp)"
    format="$(heavy_status_format)"
    "$binary" -L "$socket" new-session -d -s perf /bin/sh >/dev/null
    run_timed_ms "$output_file" -L "$socket" display-message -p -F "$format" -t perf
    cleanup_socket "$socket"
    rm -f "$output_file"
}

sample_hook_storm() {
    local socket output_file source_file
    socket="$(unique_socket hook-storm)"
    output_file="$(mktemp)"
    source_file="$(new_hook_storm_source_file)"
    "$binary" -L "$socket" new-session -d -s perf /bin/sh >/dev/null
    "$binary" -L "$socket" set-hook -g after-set-option 'display-message -p "#{hook}:#{option_name}"' >/dev/null
    run_timed_ms "$output_file" -L "$socket" source-file "$source_file"
    cleanup_socket "$socket"
    rm -f "$output_file" "$source_file"
}

sample_daemon_churn() {
    local root start_ns end_ns status socket
    root="$(mktemp -d "${TMPDIR:-/tmp}/rmux-perf-daemon-churn.XXXXXX")"
    status=0
    start_ns="$(date +%s%N)"
    for ((index = 1; index <= daemon_churn_cycles; index++)); do
        socket="$root/churn-$index.sock"
        "$binary" -S "$socket" new-session -d -s "churn$index" /bin/sh >/dev/null 2>&1 || {
            status=$?
            break
        }
        "$binary" -S "$socket" kill-server >/dev/null 2>&1 || {
            status=$?
            break
        }
    done
    end_ns="$(date +%s%N)"
    rm -rf "$root"
    if [ "$status" -ne 0 ]; then
        echo "daemon churn failed with exit code $status" >&2
        exit "$status"
    fi
    awk -v start="$start_ns" -v end="$end_ns" 'BEGIN { printf "%.3f", (end - start) / 1000000 }'
}

record_metric() {
    local name="$1"
    local budget="$2"
    local sampler="$3"
    local samples=()

    echo "measuring $name ($iterations runs)" >&2
    for ((run = 1; run <= iterations; run++)); do
        samples+=("$("$sampler")")
    done

    local p50 p95 mean min max status
    p50="$(percentile 50 "${samples[@]}")"
    p95="$(percentile 95 "${samples[@]}")"
    mean="$(mean_value "${samples[@]}")"
    min="$(min_value "${samples[@]}")"
    max="$(max_value "${samples[@]}")"
    status="informational"
    if [ "$budget" != "null" ]; then
        status="$(awk -v p95="$p95" -v budget="$budget" 'BEGIN { if (p95 <= budget) print "pass"; else print "fail" }')"
    fi

    metric_names+=("$name")
    metric_p50+=("$p50")
    metric_p95+=("$p95")
    metric_mean+=("$mean")
    metric_min+=("$min")
    metric_max+=("$max")
    metric_budget+=("$budget")
    metric_status+=("$status")
    metric_samples+=("$(IFS=,; echo "${samples[*]}")")
}

record_metric "diagnose_json_cold" "null" sample_diagnose
record_metric "daemon_startup" "750" sample_daemon_startup
record_metric "new_session_detached_sh" "500" sample_new_session_sh
record_metric "split_window_detached_sh" "150" sample_split_window_sh
record_metric "send_keys_detached_round_trip" "20" sample_send_keys
record_metric "resize_pane_round_trip" "100" sample_resize_pane
record_metric "pane_output_${line_count}_lines_ready" "null" sample_pane_output_ready
record_metric "capture_pane_${line_count}_lines" "75" sample_capture_pane
record_metric "attach_render_${line_count}_line_scrollback" "null" sample_attach_render
record_metric "source_file_${source_command_count}_commands" "null" sample_source_file_large
record_metric "status_format_heavy_expand" "null" sample_status_format_heavy
record_metric "hook_storm_${hook_storm_events}_after_set_option" "null" sample_hook_storm
record_metric "daemon_churn_${daemon_churn_cycles}_create_kill" "null" sample_daemon_churn

timestamp="$(date -u +%Y%m%d-%H%M%S)"
json_path="$output_dir/unix-$timestamp.json"
markdown_path="$output_dir/unix-$timestamp.txt"

{
    printf '{\n'
    printf '  "schema": 2,\n'
    printf '  "kind": "rmux-perf-current",\n'
    printf '  "timestamp": %s,\n' "$(json_string "$(date -u +%Y-%m-%dT%H:%M:%SZ)")"
    printf '  "git": {"commit":%s,"describe":%s,"dirty":false},\n' \
        "$(json_string "$git_commit")" "$(json_string "$git_describe")"
    printf '  "environment": {"platform":%s,"machine":%s,"host_fingerprint":%s},\n' \
        "$(json_string "$platform")" "$(json_string "$machine")" "$(json_string "$host_fingerprint")"
    printf '  "binary": {"path":%s,"sha256":%s,"version":%s,"configuration":"release"},\n' \
        "$(json_string "$binary")" "$(json_string "$binary_sha256")" "$(json_string "$binary_version")"
    printf '  "provenance": {"generator":"scripts/perf-bench.sh","invocation":%s,"expected_git_commit":%s,"expected_platform":%s,"build_mode":%s},\n' \
        "$(json_string "$provenance_invocation")" "$(json_string "$expected_git_commit")" \
        "$(json_string "$expected_platform")" "$(json_string "$build_mode")"
    printf '  "parameters": {"iterations":%s,"line_count":%s},\n' "$iterations" "$line_count"
    printf '  "metrics": [\n'
    for ((i = 0; i < ${#metric_names[@]}; i++)); do
        [ "$i" -gt 0 ] && printf ',\n'
        printf '    {"name":"%s","p50_ms":%s,"p95_ms":%s,"mean_ms":%s,"min_ms":%s,"max_ms":%s,"budget_p95_ms":%s,"status":"%s","samples_ms":[%s]}' \
            "${metric_names[$i]}" "${metric_p50[$i]}" "${metric_p95[$i]}" \
            "${metric_mean[$i]}" "${metric_min[$i]}" "${metric_max[$i]}" \
            "${metric_budget[$i]}" "${metric_status[$i]}" "${metric_samples[$i]}"
    done
    printf '\n  ]\n'
    printf '}\n'
} >"$json_path"

{
    printf '# RMUX Unix Performance Bench\n\n'
    printf -- '- Binary: `%s`\n' "$binary"
    printf -- '- Binary SHA-256: `%s`\n' "$binary_sha256"
    printf -- '- Git commit: `%s`\n' "$git_commit"
    printf -- '- Provenance: `%s`\n' "$provenance_invocation"
    printf -- '- Iterations: `%s`\n' "$iterations"
    printf -- '- Line count: `%s`\n\n' "$line_count"
    printf '| Metric | p50 ms | p95 ms | Budget p95 ms | Status |\n'
    printf '|---|---:|---:|---:|---|\n'
    for ((i = 0; i < ${#metric_names[@]}; i++)); do
        budget="${metric_budget[$i]}"
        [ "$budget" = "null" ] && budget=""
        printf '| %s | %s | %s | %s | %s |\n' \
            "${metric_names[$i]}" "${metric_p50[$i]}" "${metric_p95[$i]}" \
            "$budget" "${metric_status[$i]}"
    done
} >"$markdown_path"

printf 'json=%s\n' "$json_path"
printf 'summary=%s\n' "$markdown_path"

if [ "$fail_on_budget" -eq 1 ]; then
    failed=0
    for status in "${metric_status[@]}"; do
        [ "$status" = "fail" ] && failed=1
    done
    if [ "$failed" -eq 1 ]; then
        echo "performance budget failed" >&2
        exit 1
    fi
fi
