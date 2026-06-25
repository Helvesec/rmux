#!/usr/bin/env bash
set -euo pipefail

iterations=15
out=""
binary=""
skip_build=0
layout=""
quiet=0
sample_progress=0
only_operations=""
only_tools="rmux,tmux,zellij"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --iterations)
            iterations="$2"
            shift 2
            ;;
        --out)
            out="$2"
            shift 2
            ;;
        --binary)
            binary="$2"
            shift 2
            ;;
        --layout)
            layout="$2"
            shift 2
            ;;
        --skip-build)
            skip_build=1
            shift
            ;;
        --quiet)
            quiet=1
            shift
            ;;
        --sample-progress)
            sample_progress=1
            shift
            ;;
        --only-operations)
            only_operations="$2"
            shift 2
            ;;
        --only-tools)
            only_tools="$2"
            shift 2
            ;;
        -h|--help)
            cat <<'USAGE'
usage: tools/bench/run-unix.sh --out target/benchmarks/<linux|macos>.json
                                [--iterations N] [--layout DIR]
                                [--binary PATH] [--skip-build]
                                [--sample-progress] [--quiet]
                                [--only-tools rmux,tmux,zellij]
                                [--only-operations OP1,OP2]

Runs a local Unix benchmark and writes a JSON artifact consumed by
tools/bench/render.py. The script clears RMUX/TMUX/TERM_PROGRAM so benchmark
servers do not target the orchestrating terminal.

By default, this builds and measures a package-like tiny release layout:
  <layout>/bin/rmux
  <layout>/bin/rmux-daemon
  <layout>/libexec/rmux/rmux

Pass --binary only for local debug/smoke runs; public benchmark artifacts should
use the package-like layout.
USAGE
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            exit 2
            ;;
    esac
done

if [ -z "$out" ]; then
    echo "--out is required" >&2
    exit 2
fi

case "$iterations" in
    ''|*[!0-9]*)
        echo "--iterations must be a positive integer" >&2
        exit 2
        ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

unset RMUX
unset TMUX
unset TERM_PROGRAM
unset ZELLIJ
if [ -d "$HOME/.local/bin" ]; then
    PATH="$HOME/.local/bin:$PATH"
    export PATH
fi

mkdir -p "$(dirname "$out")"

target_dir="${CARGO_TARGET_DIR:-target}"
if [ -z "$layout" ]; then
    layout="$target_dir/benchmarks/layout"
fi

rmux_layout="custom-binary"
helper_binary=""
daemon_binary=""

if [ -z "$binary" ]; then
    rmux_layout="packaged-tiny"
    binary="$layout/bin/rmux"
    helper_binary="$layout/libexec/rmux/rmux"
    daemon_binary="$layout/bin/rmux-daemon"

    if [ "$skip_build" -eq 0 ]; then
        cargo build --locked --release --package rmux --bin rmux
        rm -rf "$layout"
        mkdir -p "$layout/bin" "$layout/libexec/rmux"
        install -m 755 "$target_dir/release/rmux" "$helper_binary"

        cargo build --locked --release --package rmux --features tiny-cli --bin rmux
        install -m 755 "$target_dir/release/rmux" "$binary"

        cargo build --locked --release --package rmux --bin rmux-daemon
        install -m 755 "$target_dir/release/rmux-daemon" "$daemon_binary"
    fi

    [ -x "$binary" ] || { echo "rmux binary not found or not executable: $binary" >&2; exit 2; }
    [ -x "$helper_binary" ] || { echo "rmux helper not found or not executable: $helper_binary" >&2; exit 2; }
    [ -x "$daemon_binary" ] || { echo "rmux daemon not found or not executable: $daemon_binary" >&2; exit 2; }
elif [ "$skip_build" -eq 0 ]; then
    cargo build --locked --release --package rmux --bin rmux
fi

args=(
    --out "$out"
    --iterations "$iterations"
    --binary "$binary"
    --rmux-layout "$rmux_layout"
    --rmux-public-binary "$binary"
)
if [ "$quiet" -eq 1 ]; then
    args+=(--quiet)
fi
if [ "$sample_progress" -eq 1 ]; then
    args+=(--sample-progress)
fi
args+=(--only-tools "$only_tools")
if [ -n "$only_operations" ]; then
    args+=(--only-operations "$only_operations")
fi
if [ -n "$helper_binary" ]; then
    args+=(--rmux-helper-binary "$helper_binary")
fi
if [ -n "$daemon_binary" ]; then
    args+=(--rmux-daemon-binary "$daemon_binary")
fi

python3 tools/bench/bench_unix.py "${args[@]}"
