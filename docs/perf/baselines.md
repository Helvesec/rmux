# RMUX Performance Baselines

This file pins the baseline context for release-branch performance work.
Update it only when intentionally changing the immutable release baseline.

## Immutable Release Baseline

- Branch base: `release/0.6.5`
- Baseline commit: `53d3e2b2ce5143c8a8fb201d9e5c5328ca2372cf`
- Baseline describe: `v0.6.1-114-g53d3e2b2`
- Target branch: `release/0.7.0`
- Committed schema-2 JSON: `benches/perf/baselines/release-0.7.0.json`

## Measurement Environment

- OS: `Linux`
- Kernel: `6.17.0-35-generic`
- Machine: `x86_64`
- Rust: `rustc 1.94.1 (e408947bf 2026-03-25)`
- Toolchain source: `rust-toolchain.toml`
- Allocator: system allocator
- Build profile: `release`
- CPU governor: record per run in generated baseline JSON

## Rules

- Compare every optimization PR against this baseline and the immediately
  preceding merged PR.
- Record the command line, binary path, git commit, and machine context in
  every generated JSON artifact.
- Do not publish a release-facing percentage unless the benchmark artifact
  includes sample count, baseline commit, and comparator method.
- Keep `capture_pane_sb10k` pending until it is sampled under `loadavg < 1`
  with the `scripts/perf-bench.sh` marker pre-fill verification passing.

## Deferred Release Contracts

### H7: Web Session Snapshot Lock Hoist

- Status for `0.7.0`: deferred with explicit operator caveat.
- Current behavior: web session snapshot composition can hold the daemon state
  lock while building the session render frame in
  `crates/rmux-server/src/handler_web.rs`.
- Operator caveat: high web-share viewer fan-out, large scrollback, or repeated
  resync snapshots can delay concurrent daemon requests while a snapshot is
  composed. Keep web-share fan-out bounded for latency-sensitive sessions until
  PR15B lands.
- Exit condition: split web snapshot target resolution from render composition
  so state lock scope ends before `web_session_snapshot_from_state` materializes
  pane frames, then add a production-style web-share fan-out smoke.
