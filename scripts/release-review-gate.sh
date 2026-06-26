#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/release-review-gate.sh [options]

Run the review-derived release gate for RMUX 0.7.x. This intentionally targets
the bug classes that manual reviews kept finding: tiny CLI fallback boundaries,
tmux authority cases, package layout, version drift, platform-neutrality budget,
and mutating target-action retry safety.

On Windows, prefer scripts/release-review-gate-windows.ps1. Running this Bash
gate through WSL may require a healthy Linux Rust toolchain and network access.

Options:
  --target-dir DIR     Cargo target dir. Defaults to /tmp/rmux-release-review-target.
  --layout DIR         Reuse or populate a release layout directory.
  --skip-package       Skip release layout build and tiny package smoke.
  --skip-package-build Reuse --layout without rebuilding it.
  --no-tmux            Skip tmux authority checks inside the package smoke.
  -h, --help           Show this help.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

run_step() {
  local label="$1"
  shift
  printf '\n[release-review] %s\n' "$label"
  "$@"
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="${TMPDIR:-/tmp}/rmux-release-review-target"
layout=""
skip_package=0
skip_package_build=0
no_tmux=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --target-dir)
      [ "$#" -ge 2 ] || die "--target-dir requires a directory"
      target_dir="$2"
      shift 2
      ;;
    --layout)
      [ "$#" -ge 2 ] || die "--layout requires a directory"
      layout="$2"
      shift 2
      ;;
    --skip-package)
      skip_package=1
      shift
      ;;
    --skip-package-build)
      skip_package_build=1
      shift
      ;;
    --no-tmux)
      no_tmux=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

cd "$repo_root"
perf_baseline="benches/perf/baselines/release-0.7.0.json"

case "$target_dir" in
  /*) ;;
  *) target_dir="$repo_root/$target_dir" ;;
esac
export CARGO_TARGET_DIR="$target_dir"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"

run_step "release versions" scripts/check-release-versions.sh
run_step "formatting" cargo fmt --all -- --check
run_step "perf baseline JSON" \
  bash -c 'test -s "$1" && python3 -m json.tool "$1" >/dev/null' _ "$perf_baseline"
run_step "perf comparator" \
  python3 scripts/perf-diff.py "$perf_baseline" "$perf_baseline" \
    --fail-on-regression \
    --json-out "$target_dir/perf-baseline-self-diff.json"
run_step "perf comparator self-test" python3 scripts/perf-diff.py --self-test
run_step "platform neutrality" scripts/check-platform-neutrality.sh
run_step "workspace clippy" \
  cargo clippy --workspace --all-targets --locked -- -D warnings
run_step "tiny parser and boundary tests" \
  cargo test -p rmux --features tiny-cli tiny_main --locked
run_step "mutating target-action retry tests" \
  cargo test -p rmux --bin rmux --locked target_action_retry_is_limited

if [ "$skip_package" -eq 0 ]; then
  args=(--target-dir "$target_dir")
  if [ -n "$layout" ]; then
    args+=(--layout "$layout")
  fi
  if [ "$skip_package_build" -eq 1 ]; then
    [ -n "$layout" ] || die "--skip-package-build requires --layout"
    args+=(--skip-build)
  fi
  if [ "$no_tmux" -eq 1 ]; then
    args+=(--no-tmux)
  fi
  run_step "tiny release package smoke" scripts/smoke-tiny-release-review.sh "${args[@]}"
fi

printf '\nrelease-review-gate=ok\n'
