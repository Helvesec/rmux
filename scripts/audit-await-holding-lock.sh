#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if ! command -v rg >/dev/null 2>&1; then
    echo "[await-lock-audit] ERROR: ripgrep (rg) is required for lock-map crate audit" >&2
    exit 2
fi

echo "[await-lock-audit] clippy server default features"
cargo clippy -p rmux-server --all-targets --locked -- -D warnings -D clippy::await_holding_lock

echo "[await-lock-audit] clippy server perf-instrument feature"
cargo clippy -p rmux-server --features perf-instrument --all-targets --locked -- -D warnings -D clippy::await_holding_lock

echo "[await-lock-audit] checking deferred lock-map crates stay out of rmux-server/src"
if rg -n '\b(DashMap|parking_lot::)' crates/rmux-server/src; then
    echo "[await-lock-audit] ERROR: DashMap/parking_lot usage requires PR26C design audit first" >&2
    exit 1
fi

echo "[await-lock-audit] passed"
