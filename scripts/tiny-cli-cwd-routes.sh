#!/usr/bin/env bash
set -euo pipefail



# Exercises the caller-cwd contract through a real packaged `tiny-cli` client.
#
# A tiny build selects its dispatcher on `debug_assertions`, so the binary a
# normal cargo test profile produces is always the full CLI and the acceptance
# matrix's tiny-labelled commands would otherwise take the full path.  This
# builds the two binaries a package actually ships -- a tiny public client and
# the private full helper it execs for commands it does not implement -- and
# runs the caller-cwd test against them.  The test fails unless tiny's own
# routing trace shows `new-window` and `split-window` taking its direct seams.

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
if [ -z "${RMUX_IDENTITY_HEAD:-}" ]; then
  exec "${RMUX_PYTHON:-python3}" "$repo_root/scripts/candidate_identity.py" with -- /bin/bash "$repo_root/scripts/tiny-cli-cwd-routes.sh" "$@"
fi
"${RMUX_PYTHON:-python3}" "$repo_root/scripts/candidate_identity.py" validate

case "$(uname -s)" in
  Linux|Darwin) ;;
  *) die "tiny CLI route proof runs on Unix hosts" ;;
esac

: "${CARGO_TARGET_DIR:=${TMPDIR:-/tmp}/rmux-tiny-routes-target}"
# The tiny client needs `debug_assertions` off, which is a different profile
# from the one the workspace tests use.  Keeping it in its own target directory
# means neither build invalidates the other.
tiny_target="${CARGO_TARGET_DIR%/}/tiny-cli"

printf '[tiny-routes] target=%s\n' "$CARGO_TARGET_DIR"
printf '[tiny-routes] tiny_target=%s\n' "$tiny_target"

printf '[tiny-routes] building the full helper\n'
CARGO_TARGET_DIR="$CARGO_TARGET_DIR" cargo build --locked --bin rmux

printf '[tiny-routes] building the tiny client\n'
cargo build --target-dir "$tiny_target" --locked --bin rmux \
  --features tiny-cli --config profile.dev.debug-assertions=false

full_helper="$CARGO_TARGET_DIR/debug/rmux"
tiny_client="$tiny_target/debug/rmux"
[ -x "$full_helper" ] || die "full helper was not built at $full_helper"
[ -x "$tiny_client" ] || die "tiny client was not built at $tiny_client"

printf '[tiny-routes] full_helper=%s\n' "$full_helper"
printf '[tiny-routes] tiny_client=%s\n' "$tiny_client"

RMUX_ACCEPTANCE_TINY_BINARY="$tiny_client" \
RMUX_FULL_BINARY_PATH="$full_helper" \
CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
  cargo test --locked --test acceptance_cli_matrix \
  detached_window_spawns_without_c_use_non_attached_caller_cwd -- --exact

printf '[tiny-routes] PASS tiny new-window and split-window caller-cwd routes\n'
