#!/usr/bin/env sh
# Checks that rmux-client still compiles when rustix resolves to a newer
# semver-compatible release than the one pinned in Cargo.lock. rustix 1.1.5
# made its `runtime` module private, which broke the Linux attach build for
# downstream users who do not inherit our lockfile (GH #230).
#
# Usage: scripts/check-rustix-latest.sh [rustix-version]
# The committed Cargo.lock is restored on exit.
set -eu

version="${1:-1.1.6}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

backup="$(mktemp "${TMPDIR:-/tmp}/rmux-cargo-lock.XXXXXX")"
cp Cargo.lock "$backup"
restore_lock() {
  cp "$backup" Cargo.lock
  rm -f "$backup"
}
trap restore_lock EXIT INT TERM

cargo update -p rustix --precise "$version"
cargo check -p rmux-client --all-targets --locked
echo "rmux-client compiles against rustix $version."
