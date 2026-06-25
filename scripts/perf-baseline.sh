#!/usr/bin/env bash
set -euo pipefail

iterations=30
line_count=10000
binary=""
output_dir="target/perf-baseline"
skip_build=0

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
        -h|--help)
            cat <<'USAGE'
usage: scripts/perf-baseline.sh [--iterations N] [--line-count N]
                                [--binary PATH] [--output-dir DIR]
                                [--skip-build]

Runs the current Unix perf bench, records repository baseline metadata, and
writes a schema-2 JSON artifact for release/0.7.0 performance work.
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

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [ -z "$binary" ]; then
    binary="${CARGO_TARGET_DIR:-target}/release/rmux"
fi

mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"

legacy_dir="$output_dir/schema1"
mkdir -p "$legacy_dir"

legacy_args=(
    --iterations "$iterations"
    --line-count "$line_count"
    --binary "$binary"
    --output-dir "$legacy_dir"
)

if [ "$skip_build" -eq 1 ]; then
    legacy_args+=(--skip-build)
fi

legacy_output="$(scripts/perf-bench.sh "${legacy_args[@]}")"
legacy_json="$(printf '%s\n' "$legacy_output" | awk -F= '$1 == "json" { print $2 }' | tail -n 1)"
legacy_summary="$(printf '%s\n' "$legacy_output" | awk -F= '$1 == "summary" { print $2 }' | tail -n 1)"

if [ -z "$legacy_json" ] || [ ! -f "$legacy_json" ]; then
    echo "scripts/perf-bench.sh did not produce a JSON artifact" >&2
    exit 1
fi

if [ ! -x "$binary" ]; then
    echo "rmux binary was not found or is not executable: $binary" >&2
    exit 1
fi

binary="$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")"

rss_proxy_kib=null
rss_proxy_status="unavailable"
rss_proxy_note="/usr/bin/time -v diagnose --json proxy"
if [ -x /usr/bin/time ]; then
    rss_probe_out="$output_dir/rss-proxy-diagnose.json"
    rss_probe_time="$output_dir/rss-proxy-diagnose.time.txt"
    set +e
    /usr/bin/time -v "$binary" diagnose --json >"$rss_probe_out" 2>"$rss_probe_time"
    rss_probe_status=$?
    set -e
    if [ "$rss_probe_status" -eq 0 ]; then
        parsed_rss="$(
            awk -F: '/Maximum resident set size/ {
                value = $2
                gsub(/^[ \t]+|[ \t]+$/, "", value)
                print value
            }' "$rss_probe_time" | tail -n 1
        )"
        if is_positive_integer "$parsed_rss"; then
            rss_proxy_kib="$parsed_rss"
            rss_proxy_status="collected"
        else
            rss_proxy_status="parse-failed"
        fi
    else
        rss_proxy_status="probe-failed"
    fi
fi

json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

json_string() {
    printf '"%s"' "$(json_escape "$1")"
}

command_or_unknown() {
    local fallback="$1"
    shift
    "$@" 2>/dev/null || printf '%s' "$fallback"
}

git_commit="$(command_or_unknown unknown git rev-parse HEAD)"
git_branch="$(command_or_unknown unknown git rev-parse --abbrev-ref HEAD)"
git_describe="$(command_or_unknown unknown git describe --tags --always --dirty)"
rustc_version="$(command_or_unknown unknown rustc -V)"
rustc_verbose="$(command_or_unknown unknown rustc -Vv | tr '\n' ';' | sed 's/;*$//')"
toolchain="$(command_or_unknown unknown rustup show active-toolchain)"
platform="$(uname -s | tr '[:upper:]' '[:lower:]')"
kernel="$(uname -r)"
machine="$(uname -m)"
cpu_governor="unknown"
if [ -r /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor ]; then
    cpu_governor="$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor)"
fi
allocator="system"
binary_size_bytes="$(wc -c <"$binary" | tr -d ' ')"
timestamp="$(date -u +%Y%m%d-%H%M%S)"
json_path="$output_dir/baseline-$timestamp.json"

fixture_manifest="benches/perf/fixtures/MANIFEST.sha256"
baseline_file="docs/perf/baselines.md"

target_json_line() {
    local name="$1"
    local status="$2"
    local source_metric="$3"
    local note="$4"
    printf '    {"name":%s,"status":%s,"source_metric":%s,"note":%s}' \
        "$(json_string "$name")" \
        "$(json_string "$status")" \
        "$(json_string "$source_metric")" \
        "$(json_string "$note")"
}

{
    printf '{\n'
    printf '  "schema": 2,\n'
    printf '  "kind": "rmux-perf-baseline",\n'
    printf '  "timestamp": %s,\n' "$(json_string "$(date -u +%Y-%m-%dT%H:%M:%SZ)")"
    printf '  "git": {"branch":%s,"commit":%s,"describe":%s},\n' \
        "$(json_string "$git_branch")" "$(json_string "$git_commit")" "$(json_string "$git_describe")"
    printf '  "environment": {"platform":%s,"kernel":%s,"machine":%s,"rustc":%s,"rustc_verbose":%s,"toolchain":%s,"allocator":%s,"cpu_governor":%s},\n' \
        "$(json_string "$platform")" "$(json_string "$kernel")" "$(json_string "$machine")" \
        "$(json_string "$rustc_version")" "$(json_string "$rustc_verbose")" \
        "$(json_string "$toolchain")" "$(json_string "$allocator")" "$(json_string "$cpu_governor")"
    printf '  "binary": {"path":%s,"size_bytes":%s},\n' "$(json_string "$binary")" "$binary_size_bytes"
    printf '  "memory": {"rss_proxy_kib":%s,"status":%s,"note":%s},\n' \
        "$rss_proxy_kib" "$(json_string "$rss_proxy_status")" "$(json_string "$rss_proxy_note")"
    printf '  "parameters": {"iterations":%s,"line_count":%s},\n' "$iterations" "$line_count"
    printf '  "artifacts": {"schema1_json":%s,"schema1_summary":%s,"baseline_file":%s,"fixture_manifest":%s},\n' \
        "$(json_string "$legacy_json")" "$(json_string "$legacy_summary")" \
        "$(json_string "$baseline_file")" "$(json_string "$fixture_manifest")"
    printf '  "required_targets": [\n'
    target_json_line "attach_deep_scrollback_render" "pending" "" "requires attach render golden harness"
    printf ',\n'
    target_json_line "pane_snapshot_deep_scrollback_80x24_200x60" "proxy" "capture_pane_${line_count}_lines" "current CLI proxy until SDK snapshot bench lands"
    printf ',\n'
    target_json_line "capture_pane_sb10k" "pending" "" "requires loadavg < 1 run with --line-count 10000 and marker pre-fill verification before samples are release-facing"
    printf ',\n'
    target_json_line "copy_mode_search_10k" "pending" "" "requires copy-mode search harness"
    printf ',\n'
    target_json_line "resize_reflow_10k" "proxy" "resize_pane_round_trip" "current resize proxy does not yet preload deep scrollback"
    printf ',\n'
    target_json_line "codec_roundtrip_50B_8KiB_1MiB" "pending" "" "requires codec microbench"
    printf ',\n'
    target_json_line "pane_output_flood" "proxy" "pane_output_${line_count}_lines_ready" "line flood readiness proxy"
    printf ',\n'
    target_json_line "web_backpressure_resize" "pending" "" "requires web-share harness"
    printf ',\n'
    target_json_line "ws_outbound_frame" "pending" "" "requires web outbound microbench"
    printf ',\n'
    target_json_line "cold_path_size" "collected" "binary.size_bytes" "release binary byte size"
    printf '\n  ],\n'
    printf '  "notes": [\n'
    printf '    %s,\n' "$(json_string "PR0A records proxy/pending targets honestly; do not treat pending targets as measured.")"
    printf '    %s\n' "$(json_string "Use PR0B-PR0E to add byte goldens, protocol ledgers, comparator statistics, and CI gates.")"
    printf '  ],\n'
    printf '  "source": '
    cat "$legacy_json"
    printf '\n'
    printf '}\n'
} >"$json_path"

printf 'json=%s\n' "$json_path"
printf 'schema1_json=%s\n' "$legacy_json"
printf 'schema1_summary=%s\n' "$legacy_summary"
