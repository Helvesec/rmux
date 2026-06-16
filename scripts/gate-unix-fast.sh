#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/gate-unix-fast.sh [options]

Run a fast local quality gate on Linux or macOS.

Options:
  --platform linux|macos       Platform override (default: host detection)
  --test-threads N             Test concurrency for cargo test or nextest
  --nextest                    Use cargo-nextest for workspace tests
  --skip-doc                   Skip doc tests and cargo doc
  --skip-source-gates          Skip source boundary shell checks
  --install-nextest            Install cargo-nextest if it is missing
  -h, --help                   Show this help

This gate sets RUST_TEST_THREADS from --test-threads for cargo test. It keeps
the Cargo target directory outside the repo by default and runs doc/source
checks separately.
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

export RUST_TEST_THREADS="$test_threads"
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
printf '[gate] cargo_build_jobs=%s\n' "$CARGO_BUILD_JOBS"
printf '[gate] cargo_incremental=%s\n' "$CARGO_INCREMENTAL"
printf '[gate] cargo_target_dir=%s\n' "$CARGO_TARGET_DIR"

run_step "cargo fmt" cargo fmt --all --check
run_step "cargo clippy" cargo clippy --workspace --all-targets --locked -- -D warnings

if [ "$use_nextest" -eq 1 ]; then
  cargo nextest --version >/dev/null 2>&1 || die "cargo-nextest not found; use --install-nextest or omit --nextest"
  run_step "cargo nextest workspace" \
    cargo nextest run --workspace --all-targets --locked --no-fail-fast --test-threads "$test_threads"
else
  run_step "cargo test workspace" cargo test --workspace --all-targets --locked --no-fail-fast
fi

if [ "$skip_doc" -eq 0 ]; then
  run_step "cargo doc tests" cargo test --workspace --doc --locked
  run_step "cargo doc" cargo doc --workspace --locked --no-deps
fi

if [ "$skip_source_gates" -eq 0 ]; then
  run_step "runtime network source scan" scripts/no-network-in-runtime.sh
  run_step "platform neutrality source scan" scripts/check-platform-neutrality.sh
  run_step "debug_assert side-effect scan" scripts/no-debug-assert-side-effects.sh
fi

printf '\n[gate] PASS fast %s gate\n' "$platform"
