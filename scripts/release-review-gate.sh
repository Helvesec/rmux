#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/release-review-gate.sh [options]

Run the review-derived release gate for RMUX 0.9.0. This intentionally targets
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

Environment:
  RMUX_PERF_CURRENT_JSON  Required current-run perf JSON for release comparison.
  RMUX_PERF_AUTO_CURRENT  Set to 1 to generate current perf JSON locally.
  RMUX_PERF_EXPECTED_GIT_SHA
                          Commit that a current perf artifact must measure.
  RMUX_PERF_EXPECTED_PLATFORM
                          Platform that a current perf artifact must measure.
  RMUX_PERF_EXPECTED_PROVENANCE
                          Invocation identity stamped into a current perf artifact.
  RMUX_PERF_MAX_CURRENT_AGE_SECONDS
                          Maximum accepted current artifact age (default: 21600).
  RMUX_PERF_GATE_MODE     owner-comparison (default) or portable-budget.
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

generate_perf_current() {
  local fail_on_budget="$1"
  local perf_current_dir="$target_dir/perf-current"
  local perf_current_log="$target_dir/perf-current.log"
  local perf_args=(--output-dir "$perf_current_dir")
  if [ "$fail_on_budget" = 1 ]; then
    perf_args+=(--fail-on-budget)
  fi
  mkdir -p "$perf_current_dir"
  run_step "perf current benchmark" \
    bash -c 'set -o pipefail; scripts/perf-bench.sh "${@:2}" | tee "$1"' _ \
      "$perf_current_log" "${perf_args[@]}"
  perf_current="$(awk -F= '/^json=/{print $2}' "$perf_current_log" | tail -n1)"
  [ -n "$perf_current" ] || die "current perf benchmark did not report a JSON path"
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_id="$(printf '%s' "$repo_root" | cksum | awk '{print $1}')"
target_dir="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/rmux-release-review-target-${repo_id}}"
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
gate_platform="$(uname -s | tr '[:upper:]' '[:lower:]')"
gate_machine="$(uname -m | tr '[:upper:]' '[:lower:]')"
if [ "$gate_platform" = "darwin" ]; then
  perf_baseline="benches/perf/baselines/release-0.9.0.json"
else
  perf_baseline="benches/perf/baselines/release-0.9.0-${gate_platform}.json"
fi
perf_current="${RMUX_PERF_CURRENT_JSON:-}"
git_head="$(git rev-parse HEAD)"
perf_expected_git_sha="${RMUX_PERF_EXPECTED_GIT_SHA:-$git_head}"
perf_expected_platform="${RMUX_PERF_EXPECTED_PLATFORM:-$gate_platform}"
perf_expected_provenance="${RMUX_PERF_EXPECTED_PROVENANCE:-local:$git_head}"
perf_max_current_age="${RMUX_PERF_MAX_CURRENT_AGE_SECONDS:-21600}"
perf_gate_mode="${RMUX_PERF_GATE_MODE:-owner-comparison}"
package_version="$(awk '
  /^\[workspace\.package\]$/ { in_workspace = 1; next }
  /^\[/ { in_workspace = 0 }
  in_workspace && $1 == "version" { gsub(/"/, "", $3); print $3; exit }
' Cargo.toml)"
[ -n "$package_version" ] || die "unable to read workspace package version"
case "$perf_max_current_age" in
  ''|*[!0-9]*|0) die "RMUX_PERF_MAX_CURRENT_AGE_SECONDS must be a positive integer" ;;
esac
case "$perf_gate_mode" in
  owner-comparison|portable-budget) ;;
  *) die "RMUX_PERF_GATE_MODE must be owner-comparison or portable-budget" ;;
esac
[ "$perf_expected_git_sha" = "$git_head" ] ||
  die "RMUX_PERF_EXPECTED_GIT_SHA=$perf_expected_git_sha does not match checkout $git_head"
[ "$perf_expected_platform" = "$gate_platform" ] ||
  die "RMUX_PERF_EXPECTED_PLATFORM=$perf_expected_platform does not match host $gate_platform"
[ -n "$perf_expected_provenance" ] || die "RMUX_PERF_EXPECTED_PROVENANCE must not be empty"
export RMUX_PERF_EXPECTED_GIT_SHA="$perf_expected_git_sha"
export RMUX_PERF_EXPECTED_PLATFORM="$perf_expected_platform"
export RMUX_PERF_EXPECTED_PROVENANCE="$perf_expected_provenance"

case "$target_dir" in
  /*) ;;
  *) target_dir="$repo_root/$target_dir" ;;
esac
export CARGO_TARGET_DIR="$target_dir"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export RMUX_REQUIRE_TMUX=1

run_step "release versions" scripts/check-release-versions.sh
run_step "changelog release audit" python3 scripts/check-changelog-release.py CHANGELOG.md
run_step "tmux divergence ledger" python3 scripts/check-tmux-release-ledger.py
run_step "feature inventory" \
  cargo run --locked --package xtask -- feature-inventory --check
run_step "formatting" cargo fmt --all -- --check
run_step "perf comparator self-test" python3 scripts/perf-diff.py --self-test
run_step "perf current validator self-test" python3 scripts/check-perf-current.py --self-test
darwin_perf_baseline="benches/perf/baselines/release-0.9.0.json"
[ -f "$darwin_perf_baseline" ] ||
  die "missing required Darwin perf baseline at $darwin_perf_baseline"
run_step "committed Darwin perf baseline" \
  python3 scripts/check-perf-baseline.py "$darwin_perf_baseline" --expected-platform darwin
linux_perf_baseline="benches/perf/baselines/release-0.9.0-linux.json"
if [ -f "$linux_perf_baseline" ]; then
  run_step "committed Linux perf baseline" \
    python3 scripts/check-perf-baseline.py "$linux_perf_baseline" --expected-platform linux
fi
if [ ! -f "$perf_baseline" ]; then
  die "missing $gate_platform perf baseline at $perf_baseline; record one with scripts/perf-baseline.sh on the release machine before running the release gate"
else
  run_step "perf baseline JSON" \
    bash -c 'test -s "$1" && python3 -m json.tool "$1" >/dev/null' _ "$perf_baseline"
  run_step "perf baseline coverage" \
    python3 scripts/check-perf-baseline.py "$perf_baseline" --expected-platform "$gate_platform"
  # Relative timings only compare meaningfully on the owner host. Hosted CI
  # instead runs the explicit portable absolute-budget gate; it never spoofs
  # the owner fingerprint or turns a host mismatch into a green skip.
  baseline_fingerprint="$(python3 -c '
import json, sys
payload = json.load(open(sys.argv[1]))
environment = payload.get("environment") or {}
print(environment.get("host_fingerprint") or "")
' "$perf_baseline")"
  if [ -n "${RMUX_PERF_HOST_FINGERPRINT:-}" ]; then
    current_fingerprint="$RMUX_PERF_HOST_FINGERPRINT"
  elif [ -r /etc/machine-id ]; then
    current_fingerprint="$(printf 'machine-id:%s' "$(cat /etc/machine-id)" | sha256sum | cut -c1-16)"
  elif command -v sha256sum >/dev/null 2>&1; then
    current_fingerprint="$(printf 'hostname:%s' "$(hostname)" | sha256sum | cut -c1-16)"
  else
    current_fingerprint="$(printf 'hostname:%s' "$(hostname)" | shasum -a 256 | cut -c1-16)"
  fi
  if [ -z "$baseline_fingerprint" ]; then
    die "$gate_platform perf baseline $perf_baseline has no environment.host_fingerprint; re-record it cleanly with scripts/perf-baseline.sh on the release machine"
  elif [ "$perf_gate_mode" = portable-budget ]; then
    if [ -z "$perf_current" ]; then
      [ "${RMUX_PERF_AUTO_CURRENT:-0}" = 1 ] ||
        die "portable-budget mode requires RMUX_PERF_CURRENT_JSON or RMUX_PERF_AUTO_CURRENT=1"
      generate_perf_current 1
    fi
    run_step "portable perf artifact validation" \
      python3 scripts/check-perf-current.py "$perf_current" \
        --expected-current-commit "$perf_expected_git_sha" \
        --expected-current-platform "$perf_expected_platform" \
        --expected-current-machine "$gate_machine" \
        --expected-current-host-fingerprint "$current_fingerprint" \
        --expected-current-provenance "$perf_expected_provenance" \
        --expected-current-binary-version "rmux $package_version" \
        --max-current-age-seconds "$perf_max_current_age" \
        --require-absolute-budgets
    printf 'perf-gate=portable-budget enforcement=absolute-budgets\n'
  elif [ "$baseline_fingerprint" != "$current_fingerprint" ]; then
    die "perf baseline host fingerprint mismatch: baseline=$baseline_fingerprint current=$current_fingerprint; run this mandatory comparison on the baseline owner host"
  else
    if [ -z "$perf_current" ]; then
      if [ "${RMUX_PERF_AUTO_CURRENT:-0}" != "1" ]; then
        die "RMUX_PERF_CURRENT_JSON is required; set RMUX_PERF_AUTO_CURRENT=1 only for local regenerated-current runs"
      fi
      generate_perf_current 0
    fi
    run_step "perf comparator" \
      python3 scripts/perf-diff.py "$perf_baseline" "$perf_current" \
        --expected-current-commit "$perf_expected_git_sha" \
        --expected-current-platform "$perf_expected_platform" \
        --expected-current-machine "$gate_machine" \
        --expected-current-host-fingerprint "$current_fingerprint" \
        --expected-current-provenance "$perf_expected_provenance" \
        --expected-current-binary-version "rmux $package_version" \
        --max-current-age-seconds "$perf_max_current_age" \
        --fail-on-regression \
        --json-out "$target_dir/perf-current-diff.json"
  fi
fi
run_step "worktree hygiene" scripts/check-worktree-hygiene.sh
run_step "platform neutrality" scripts/check-platform-neutrality.sh
run_step "workspace clippy" \
  cargo clippy --workspace --all-targets --locked -- -D warnings
run_step "server lib tests" \
  cargo test -p rmux-server --lib --locked -- --test-threads=1
run_step "tiny parser filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 1 -- test -p rmux --features tiny-cli tiny_main --locked
run_step "tiny parser and boundary tests" \
  cargo test -p rmux --features tiny-cli tiny_main --locked
run_step "mutating target-action retry filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 1 -- test -p rmux --bin rmux --locked target_action_retry_is_limited
run_step "mutating target-action retry tests" \
  cargo test -p rmux --bin rmux --locked target_action_retry_is_limited
run_step "acceptance CLI matrix filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 1 -- test --locked --test acceptance_cli_matrix
run_step "acceptance CLI matrix" \
  cargo test --locked --test acceptance_cli_matrix -- --test-threads=1
run_step "source/config acceptance matrix filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 2 -- test --locked --test acceptance_source_config_matrix
run_step "source/config acceptance matrix" \
  cargo test --locked --test acceptance_source_config_matrix -- --test-threads=1
run_step "target/format acceptance matrix filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 2 -- test --locked --test acceptance_target_format_matrix
run_step "target/format acceptance matrix" \
  cargo test --locked --test acceptance_target_format_matrix -- --test-threads=1
run_step "config corpus smoke filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 2 -- test --locked --test config_corpus_script
run_step "config corpus parse-only smoke" \
  cargo test --locked --test config_corpus_script -- --test-threads=1
run_step "source-file tmux oracle filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 2 -- test --locked --test unix_source_file_tmux_oracle
run_step "source-file tmux oracle" \
  cargo test --locked --test unix_source_file_tmux_oracle -- --test-threads=1
run_step "tmux surface matrix oracle filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 45 -- test --locked --test tmux_compat_surface_matrix
run_step "tmux surface matrix oracle" \
  cargo test --locked --test tmux_compat_surface_matrix -- --test-threads=1
run_step "format tmux oracle filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 18 -- test --locked --test formats
run_step "format tmux oracle" \
  cargo test --locked --test formats -- --test-threads=1
run_step "capture tmux oracle filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 13 -- test --locked --test capture
run_step "capture tmux oracle" \
  cargo test --locked --test capture -- --test-threads=1
run_step "startup config acceptance filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 1 -- test --locked --test unix_startup_config_acceptance
run_step "startup config acceptance" \
  cargo test --locked --test unix_startup_config_acceptance -- --test-threads=1
run_step "diagnose config diagnostics acceptance filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 1 -- test --locked --test diagnose_acceptance
run_step "diagnose config diagnostics acceptance" \
  cargo test --locked --test diagnose_acceptance -- --test-threads=1
run_step "background lifecycle acceptance filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 3 -- test --locked --test unix_background_lifecycle_acceptance
run_step "background lifecycle acceptance" \
  cargo test --locked --test unix_background_lifecycle_acceptance -- --test-threads=1
run_step "control-mode shutdown acceptance filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 1 -- test --locked --test cli_surface control_mode_blocking_command_exits_on_server_shutdown
run_step "control-mode shutdown acceptance" \
  cargo test --locked --test cli_surface control_mode_blocking_command_exits_on_server_shutdown -- --test-threads=1
run_step "Unix PTY winsize acceptance filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 1 -- test --locked --test unix_pty_resize_acceptance
run_step "Unix PTY winsize acceptance" \
  cargo test --locked --test unix_pty_resize_acceptance -- --test-threads=1
if [ "$(uname -s)" = "Linux" ]; then
  run_step "Linux daemon resource acceptance filter selects tests" \
    scripts/assert-cargo-filter-nonempty.sh 1 -- test --locked --test unix_daemon_resource_acceptance
  run_step "Linux daemon resource acceptance" \
    env RMUX_RESOURCE_ACCEPTANCE_ITERATIONS="${RMUX_RESOURCE_ACCEPTANCE_ITERATIONS:-50}" \
      cargo test --locked --test unix_daemon_resource_acceptance -- --test-threads=1
else
  run_step "Linux daemon resource acceptance host skip" \
    cargo test --locked --test unix_daemon_resource_acceptance -- --test-threads=1
fi
run_step "SDK armed wait filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 8 -- test -p rmux-sdk --test armed_wait --locked
run_step "SDK armed wait smoke" \
  cargo test -p rmux-sdk --test armed_wait --locked -- --test-threads=1
run_step "SDK wait API filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 15 -- test -p rmux-sdk --test wait --locked
run_step "SDK wait API smoke" \
  cargo test -p rmux-sdk --test wait --locked -- --test-threads=1
run_step "SDK wait cancellation filter selects tests" \
  scripts/assert-cargo-filter-nonempty.sh 2 -- test -p rmux --test wait_for_cancel_after_server_crash --locked
run_step "SDK wait cancellation smoke" \
  cargo test -p rmux --test wait_for_cancel_after_server_crash --locked -- --test-threads=1

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
