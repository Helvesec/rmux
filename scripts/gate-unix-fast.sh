#!/usr/bin/env bash
set -euo pipefail

identity_args=("$@")

usage() {
  cat <<'USAGE'
Usage: scripts/gate-unix-fast.sh [options]

Run a fast local quality gate on Linux or macOS.

Options:
  --platform linux|macos       Platform override (default: host detection)
  --test-threads N             Test concurrency for cargo test or nextest
  --nextest                    Use cargo-nextest for workspace tests
  --skip-doc                   Skip doc tests and cargo doc
  --skip-source-gates          Skip source boundary checks: release policy coverage and shell scans
  --skip-tiny-routes           Skip the packaged tiny CLI caller-cwd routes
  --install-nextest            Install cargo-nextest if it is missing
  -h, --help                   Show this help

This gate sets RUST_TEST_THREADS from --test-threads and uses the same minimum
test-thread stack as CI. It uses a private Cargo target for the pinned source
HEAD and OS, and runs doc/source checks separately.

The tiny CLI route step needs a build with debug_assertions off, so its first
run on a host populates a second target directory; later runs are incremental.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

detect_platform() {
  case "$(uname -s)" in
    Linux) printf 'linux\n' ;;
    Darwin) printf 'macos\n' ;;
    *) die "unsupported Unix host: $(uname -s)" ;;
  esac
}

logical_cpus() {
  if command -v nproc >/dev/null 2>&1; then
    nproc
  elif command -v sysctl >/dev/null 2>&1; then
    sysctl -n hw.logicalcpu
  else
    printf '4\n'
  fi
}

run_step() {
  local label start status end
  label="$1"
  shift
  printf '\n[gate] %s\n' "$label"
  start="$(date +%s)"
  set +e
  "$@"
  status=$?
  set -e
  end="$(date +%s)"
  if [ "$status" -ne 0 ]; then
    printf '[gate] FAIL %s (%ss)\n' "$label" "$((end - start))" >&2
    exit "$status"
  fi
  printf '[gate] PASS %s (%ss)\n' "$label" "$((end - start))"
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
platform=""
test_threads=""
skip_doc=0
skip_source_gates=0
skip_tiny_routes=0
install_nextest=0
use_nextest=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --platform)
      [ "$#" -ge 2 ] || die "--platform requires a value"
      platform="$2"
      shift 2
      ;;
    --test-threads)
      [ "$#" -ge 2 ] || die "--test-threads requires a value"
      test_threads="$2"
      shift 2
      ;;
    --nextest)
      use_nextest=1
      shift
      ;;
    --skip-doc)
      skip_doc=1
      shift
      ;;
    --skip-source-gates)
      skip_source_gates=1
      shift
      ;;
    --skip-tiny-routes)
      skip_tiny_routes=1
      shift
      ;;
    --install-nextest)
      install_nextest=1
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

if [ -z "$platform" ]; then
  platform="$(detect_platform)"
fi
case "$platform" in linux|macos) ;; *) die "unsupported platform: $platform" ;; esac

if [ -z "$test_threads" ]; then
  test_threads="$(logical_cpus)"
fi
case "$test_threads" in
  ''|*[!0-9]*) die "--test-threads must be a positive integer" ;;
  0) die "--test-threads must be greater than zero" ;;
esac

cd "$repo_root"

if [ -z "${RMUX_IDENTITY_HEAD:-}" ]; then
  exec "${RMUX_PYTHON:-python3}" "$repo_root/scripts/candidate_identity.py" with -- /bin/bash "$repo_root/scripts/gate-unix-fast.sh" "${identity_args[@]}"
fi
"${RMUX_PYTHON:-python3}" "$repo_root/scripts/candidate_identity.py" validate-gate
run_step "candidate identity regression" "${RMUX_PYTHON:-python3}" -m unittest discover -s scripts -p 'test_candidate*.py'
if [ "$platform" = linux ]; then
  run_step "global surface manifest" "${RMUX_PYTHON:-python3}" -B scripts/surface_manifest.py --check
  run_step "global surface regressions" "${RMUX_PYTHON:-python3}" -B -m unittest discover -s scripts -p 'test_surface*.py'
fi
run_step "publication identity regression" "${RMUX_PYTHON:-python3}" -B -m unittest discover -s scripts/release -p 'test_publication*.py'
if [ "$skip_source_gates" -eq 0 ]; then
  # Before Cargo, so an unlisted file fails in seconds; a partial source tree
  # (--skip-source-gates) has no complete policy to check.
  run_step "release policy coverage" "${RMUX_PYTHON:-python3}" -B -m unittest discover -s scripts -p test_release_policy_coverage.py
fi

export RUST_TEST_THREADS="$test_threads"
export RUST_MIN_STACK=8388608
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$(logical_cpus)}"
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  export CARGO_TARGET_DIR="${TMPDIR:-/tmp}/rmux-gate-${platform}-target"
fi

if [ "$install_nextest" -eq 1 ] && ! cargo nextest --version >/dev/null 2>&1; then
  run_step "install cargo-nextest" cargo install cargo-nextest --locked
fi

printf '[gate] platform=%s\n' "$platform"
printf '[gate] test_threads=%s\n' "$test_threads"
printf '[gate] rust_test_threads=%s\n' "$RUST_TEST_THREADS"
printf '[gate] rust_min_stack=%s\n' "$RUST_MIN_STACK"
printf '[gate] cargo_build_jobs=%s\n' "$CARGO_BUILD_JOBS"
printf '[gate] cargo_incremental=%s\n' "$CARGO_INCREMENTAL"
printf '[gate] cargo_target_dir=%s\n' "$CARGO_TARGET_DIR"

command -v "${RMUX_XTERM_NODE:-node}" >/dev/null 2>&1 || die "Node is required for the copy viewport oracle (RMUX_XTERM_NODE selects Node)"
run_step "pinned xterm viewport prerequisite" "${RMUX_XTERM_NODE:-node}" tests/xterm-oracle/viewport-oracle.mjs --check

run_step "cargo fmt" cargo fmt --all --check
run_step "feature compilation gate regression" "${RMUX_PYTHON:-python3}" -m unittest discover -s scripts -p test_feature_powerset.py
run_step "workspace and isolated feature powerset" "${RMUX_PYTHON:-python3}" scripts/check_feature_powerset.py
run_step "cargo clippy" cargo clippy --workspace --all-targets --locked -- -D warnings
run_step "closed-line regression registry" "${RMUX_PYTHON:-python3}" scripts/regression_registry.py --platform "$platform"

if [ "$use_nextest" -eq 1 ]; then
  cargo nextest --version >/dev/null 2>&1 || die "cargo-nextest not found; use --install-nextest or omit --nextest"
  run_step "cargo nextest workspace" \
    cargo nextest run --workspace --all-targets --locked --no-fail-fast --test-threads "$test_threads"
else
  run_step "cargo test workspace" cargo test --workspace --all-targets --locked --no-fail-fast
fi

if [ "$skip_tiny_routes" -eq 0 ]; then
  run_step "tiny CLI caller-cwd routes" scripts/tiny-cli-cwd-routes.sh
fi

run_step "queue inventory regression" "${RMUX_PYTHON:-python3}" -B -m unittest discover -s scripts/queue-inventory -p test_inventory.py
run_step "queue limits and referenced tests" "${RMUX_PYTHON:-python3}" -B scripts/queue-inventory/check_inventory.py --source . --execute

if [ "$skip_doc" -eq 0 ]; then
  run_step "cargo doc tests" cargo test --workspace --doc --locked
  run_step "cargo doc" cargo doc --workspace --locked --no-deps
fi

if [ "$skip_source_gates" -eq 0 ]; then
  run_step "release documentation allowlist" "${RMUX_PYTHON:-python3}" scripts/release_docs_allowlist.py
  run_step "worktree hygiene" scripts/check-worktree-hygiene.sh
  run_step "runtime network source scan" scripts/no-network-in-runtime.sh
  run_step "platform neutrality source scan" scripts/check-platform-neutrality.sh
  run_step "debug_assert side-effect scan" scripts/no-debug-assert-side-effects.sh
fi

printf '\n[gate] PASS fast %s gate\n' "$platform"
