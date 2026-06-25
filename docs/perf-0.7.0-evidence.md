# RMUX 0.7.0 Perf Execution Evidence

Date: 2026-06-20
Branch: `release/0.7.0`
Base: `release/0.6.5`

This file records implementation and exploration evidence for the 0.7.0
performance plan.

## Implemented Lots

- PR0A baseline foundation:
  - `docs/perf/baselines.md`
  - `scripts/perf-baseline.sh`
  - `benches/perf/fixtures/MANIFEST.sha256`
  - schema-2 baseline JSON with embedded schema-1 metrics and RSS proxy
- PR0B render/wire/crypto guards:
  - attach render goldens
  - web snapshot goldens
  - web-share wire ledger
  - `FrameSealer` non-Clone regression
- PR0D comparator:
  - `scripts/perf-diff.py`
  - self-test mode
  - schema-1 and schema-2 input support
  - default `auto` mode selects Welch or Mann-Whitney for tail-heavy samples
- PR0C instrumentation:
  - root and server `perf-instrument` features
  - `RMUX_PERF_TRACE` opt-in JSONL sink
  - spans/events for snapshot, render compose, attach refresh, web writer, state-lock hold time, and web queue/backpressure
  - default release builds remain instrumentation-free unless the feature is explicitly enabled
- PR0E partial CI gate:
  - Linux perf smoke already existed
  - issue34 responsiveness smoke is now wired into the Linux perf job
  - RSS/FD drift smoke is now wired into the Linux perf job
  - full release gating remains incomplete until bench-diff enforcement, allocation profiling, and release-branch metric policy are wired for all required metrics
- PR1 history stats split:
  - size/limit stats no longer force byte scans
- PR3 attach output metadata-only batch:
  - live render path skips output byte clones when bytes are not needed
- PR4 lazy status job profile:
  - status job profile is only built for templates containing `#(`
- PR5 partial CLI queue connection reuse:
  - safe command queues enable a thread-local detached RPC connection cache
  - attach/start-server/kill-server/source-file/web-share/unsupported commands keep the old per-command behavior
  - direct-connect call sites remain a later refactor
- PR6 lightweight pane screen metadata:
  - cursor position added to `PaneScreenState`
- PR7 copy-mode search hoist:
  - regex and lowercased needle are built outside the per-line loop
- PR8 format regex cache:
  - thread-local bounded regex cache for format/glob substitution paths
- PR9-pre:
  - matching-bracket ASCII oracle added
  - found and fixed reverse matching-bracket depth handling
- PR9 safe subset:
  - matching-bracket movement scans owner cells line by line without flattening
    the whole copy-mode buffer
  - scan remains bounded by `BRACKET_SCAN_LIMIT`
  - cross-line regression test added
- PR12B/PR12/PR14/PR19:
  - snapshot input resolution split from materialization
  - snapshot and web resnapshot use borrowed screen paths where practical
  - visible cells are collected without cloning full scrollback
- PR15 partial:
  - `styled_pane_screen` uses `Cow<Screen>`
  - attach full render uses borrowed screen paths where practical
  - full PR15B lock-hoist remains conditional
- PR17/PR17B:
  - TCP_NODELAY on accepted web sockets
  - WebSocket header read/unmask fast path
  - heartbeat missed-tick skip
- PR17C:
  - server-side web outbound payload builders avoid avoidable intermediate copies
  - protocol v1/opcodes/message shape unchanged
- PR17D:
  - per-IP pre-auth cap enforced with default cap 4
- PR10/PR11:
  - SGR append short-circuits unchanged style tuples
  - colour code emission uses stack arrays instead of per-cell `Vec<i32>`
- PR20:
  - web keyframes are budgeted before epoch advancement
  - oversized session resize frames are rejected
- PR20C:
  - additive AEAD `seal_into`
  - caller scratch buffer used by server writer
  - sequence advances only after successful seal
- PR21:
  - RPC frame decoder decodes from slice before draining
- PR21A:
  - client attach output adopts existing `next_data_payload_into`
- PR24-pre:
  - mixed style/wide/wrapped resize corpus added
- PR24A:
  - trivial unwrapped-line width resize fast path
- PR24B partial:
  - plain ASCII logical reflow emits compact `plain_text` lines directly
  - mixed styled/plain logical lines fall back to the existing cell path
- PR24C safe subset:
  - alternate-screen restore after width-only resize reflows only the saved viewport instead of reflowing the retained history to the old width and back
  - height-changing alternate-screen restore keeps the existing path to preserve history movement semantics
- PR25:
  - ICH/DCH use in-place `GridLine` mutation APIs
- PR26-pre:
  - parser transition tables are tested for exactly one transition per state/byte
- PR26A:
  - parser transition lookup uses a derived 17x256 LUT
  - exhaustive LUT-vs-linear-table equivalence test added
- PR26B safe subset:
  - `InputParser::parse` and the parser helper/dispatch surfaces are generic
    over `W: ScreenWriter + ?Sized`
  - the concrete `Screen` writer path can now be monomorphized without
    changing parser state tables, command dispatch semantics, or public wire
    behavior
  - parser unit tests and parser trace goldens remain green after the change
- PR26C-pre:
  - `clippy::await_holding_lock` is denied at the `rmux-server` crate root
  - `scripts/audit-await-holding-lock.sh` runs server clippy in default and `perf-instrument` modes with the lint denied
  - the audit script rejects `DashMap`/`parking_lot` usage under `crates/rmux-server/src` until PR26C has a design audit
  - CI runs the audit in the Linux quality job
- PR6E safe subset:
  - tmux shim symlink short-circuit
  - requester environment is extracted once for source-file target/depth paths
- PR6F-format:
  - format literal-run scan
  - time-token no-percent fast path
- PR6F-options:
  - exact option metadata/name/alias lookups use `OnceLock<HashMap>` indexes built from `OPTIONS`
  - no serialized enum discriminant assumptions are introduced
- PR11A:
  - ASCII identity width/truncation fast path when no ASCII width override is configured
- Transport/storage memory:
  - rare output cursor gaps are boxed to reduce cursor item size
- PR6A-measure partial:
  - schema-2 perf baselines record release binary size
  - schema-2 perf baselines record `/usr/bin/time -v diagnose --json` RSS proxy when available
  - clean base/current release binaries were compared from separate build outputs under `/tmp`
- PR6G request/response:
  - large `Request` payload variants are boxed while preserving enum order and bincode wire bytes
  - `Request` size is bounded at 88 bytes in the PR6G unit test, down from the measured 264-byte baseline
  - large `Response` payload variants `WebShare` and `PaneOutputLag` are boxed while preserving bincode wire bytes
  - `Response` size is bounded at 72 bytes in the PR6G unit test, down from the measured 264-byte baseline
  - `wire_ledger_v1` remains byte-for-byte green after the representation change
  - workspace test targets compile after updating direct request constructors and patterns
  - `RmuxError` remains unboxed at 56 bytes; named-field variant boxing was deferred because it would add churn without driving the `Response` size bound
- PR-F2-05A/05B:
  - `split-window`, `resize-pane`, and `capture-pane` have additive target-action request variants with raw target text resolved server-side
  - old request variants remain available, and `RMUX_DISABLE_CLI_TARGET_ACTIONS=1` keeps the CLI on the legacy client-side resolution path
  - current daemons advertise `commands.cli_target_actions` and `commands.cli_capture_target_action`; the CLI uses an optimistic fast path and retries the legacy path on protocol decode/EOF from older active daemons, avoiding a handshake RPC on the current fast path
  - `resize-pane` percentage dimensions keep the legacy path because they still require a window-size lookup before constructing the absolute adjustment
  - wire ledger entries were added without reusing old tags
  - server regression tests cover raw target resolution for split, resize, and capture
  - CLI regression tests cover the new path and preserve tmux-shaped `can't find pane` errors

## Explored And Not Implemented In This Diff

- PR0E full CI gate:
  - issue34 and RSS/FD smokes are wired into the existing Linux perf job
  - bench-diff enforcement, allocation profiling, and release-branch metric gates remain separate CI work
- PR15B full attach refresh lock-hoist:
  - partial clone removal landed
  - full lock-hoist remains conditional because refresh ordering spans prelude, borders, status, overlays, control frames, and live pane output
- H7 web session snapshot lock-hoist:
  - explicitly deferred in `docs/perf/baselines.md` for 0.7.0 with an operator-facing
    web-share fan-out caveat
  - exit condition is PR15B-style split of snapshot target resolution from
    render composition before materializing pane frames
- PR22 SDK wait endpoint:
  - not implemented because existing bincode request has no timeout field
  - requires new request/capability to preserve compatibility
- PR23 bounded control fan-in:
  - not implemented; needs workload-specific backpressure semantics and control-mode compatibility checks
- PR9 copy-mode motion follow-ups:
  - matching-bracket flatten removal landed
  - owner-position caching and broader word-motion rewrites remain conditional on a
    copy-mode motion benchmark and invalidation design
- PR6F-options:
  - discriminant-indexed table was rejected because `OptionName` is serialized and not `repr`
  - exact lookup indexing landed; generated exhaustive static table remains optional
- PR6G error/cursor/stack boxing:
  - request and response boxing landed
  - `RmuxError` named-field boxing and cursor/stack boxing remain separate follow-up sub-PRs with full wire ledger fixtures
  - `RmuxError` exploration found broad direct construction/matching across CLI, SDK, server, and tests; at 56 bytes it does not currently justify public field-type churn without a dedicated profile
- PR24 resize reflow:
  - PR24B plain ASCII direct reflow landed
  - PR24C width-only alternate-screen double-reflow avoidance landed
  - PR24C height-changing alternate-screen restore and wide padding template reuse remain conditional on deeper corpus/profiling
- PR26B parser follow-ups:
  - generic parser/writer dispatch landed
  - additional `#[inline]` tuning, parser throughput claims, and wider trace
    corpus expansion remain conditional on a dedicated parser benchmark
- PR26C runtime lock stack:
  - not implemented; PR26C-pre audit/lint enforcement landed
  - lock-stack experiments still require PR15B and a dedicated state-lock design pass
- Wave 6 RSS/packaging/storage:
  - PR6A measurement hooks improved and clean `/tmp` baseline/current artifacts were produced
  - allocator, interning, and GridCell storage remain separate research tracks
  - PR6A-dev exploration found no existing dev/test profile override to safely tune without a measured rebuild/test baseline
  - PR6A-panic remains a product/diagnostics decision: release currently uses
    `panic = "unwind"` and `rmux-web-crypto` is both `rlib` and `cdylib`, so a
    workspace-wide panic strategy change needs explicit WASM/cdylib gating
  - PR6B allocator exploration found existing Linux/glibc daemon tuning in
    `rmux_os::memory::configure_daemon_allocator()` with
    `RMUX_DAEMON_ARENA_MAX`; no allocator dependency swap was introduced
  - PR6H exploration found multiple typed-id maps, but no profile currently proving `DefaultHasher` dominates enough to justify a new hasher dependency or broad map alias churn
  - PR6I cold-path outlining/deferred init remains conditional on `cargo bloat`
    and startup profile data
  - PR6J string interning remains conditional on status/format allocation
    traces; `CompactString` is already used in grid line storage
  - PR26E staged `GridCell` storage changes remain a separate design track;
    the current grid already has compact `plain_text` storage and boxed
    extended text
  - PR6D hardlink/multicall was rejected for this diff: the workspace already
    has a dedicated `rmux-daemon` binary and Unix/Debian/RPM/Windows packaging
    scripts install it, so hardlinking back to the CLI would risk reversing the
    daemon RSS isolation
- PR27-30 Windows performance:
  - not implemented without Windows perf data
- 2026-06-21 idle rebaseline request:
  - not rerun in this pass because `/proc/loadavg` reported `2.40 3.45 2.91`
    on a 24-core workstation with active browser/agent processes
  - do not use a new cold-path percentage from this environment as
    release-facing evidence for the `loadavg < 1` condition
- WASM provenance refresh:
  - verified on 2026-06-21 with
    `RMUX_SOURCE_DIR=<rmux-checkout> RMUX_WASM_DIRS=wasm,wasm-test node scripts/verify-wasm-from-source.mjs`
    from the adjacent `rmux-web-share` checkout
  - production `wasm/rmux_web_crypto_wasm_bg.wasm` reproduced
    `sha256:af2cd3c8ef49e9b5e1a7191e2943acdf0c6d9c911e342d3606ea562a0d853c7b`
    from source commit `2c65f0efbfa73f3f84807711e6afd63bbd322a3b`
  - production JS glue reproduced
    `sha256:ee961372d759c18c318c655fdf078e20380840ac8ce265cf161f5f4566e203cc`
  - test WASM and JS glue also reproduced byte-for-byte from the same source
    commit

## Local Evidence

Commands that passed after the implementation pass:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked --no-fail-fast`
- `cargo build --locked --release`
- `cargo check --features perf-instrument --locked`
- `cargo check -p rmux-server --features perf-instrument --locked`
- `cargo clippy -p rmux-server --features perf-instrument --all-targets --locked -- -D warnings`
- `cargo test -p rmux-server --features perf-instrument perf_instrument -- --nocapture`
- `scripts/audit-await-holding-lock.sh`
- `RMUX_SOURCE_DIR=<rmux-checkout> RMUX_WASM_DIRS=wasm,wasm-test node scripts/verify-wasm-from-source.mjs`
- `RMUX_BIN=target/release/rmux scripts/smoke-issue34-responsiveness-unix.sh`
- `RMUX_BIN=target/release/rmux scripts/smoke-rss-fd-drift-unix.sh`
- `git diff --check`
- `(cd benches/perf/fixtures && sha256sum -c MANIFEST.sha256)`
- `scripts/perf-diff.py --self-test`
- `bash -n scripts/smoke-rss-fd-drift-unix.sh scripts/perf-baseline.sh scripts/perf-bench.sh`

Targeted tests added or rerun after later lots:

- `cargo check -p rmux-proto -p rmux-client -p rmux-server -p rmux-sdk --locked`
- `cargo test -p rmux-proto pr6g_ -- --nocapture`
- `cargo test -p rmux-proto --test wire_ledger_v1 -- --nocapture`
- `cargo test -p rmux-server --locked target_action -- --nocapture`
- `cargo test --locked --test cli_target_actions -- --nocapture`
- `cargo test -p rmux-proto capabilities --locked -- --nocapture`
- `cargo test --locked -p rmux target_action_retry_is_limited_to_protocol_decode_failures -- --nocapture`
- `cargo test --workspace --locked --no-run`
- `cargo check -p rmux-proto -p rmux-server -p rmux-sdk -p rmux --locked`
- `cargo test --locked --test subscriptions_concurrent -- --nocapture`
- `cargo test -p rmux-sdk --locked pane_output -- --nocapture`
- `cargo test -p rmux-server --locked web_share -- --nocapture`
- `cargo test -p rmux-server --locked handler_targets -- --nocapture`
- `cargo test -p rmux-server --locked source_file -- --nocapture`
- `cargo test -p rmux-server --locked copy_mode -- --nocapture`
- `RUST_MIN_STACK=33554432 cargo test -p rmux-server --locked copy_mode -- --nocapture`
- `cargo test -p rmux-core --locked width_resize -- --nocapture`
- `cargo test -p rmux-core --locked alternate_screen_restore_after_width_resize_preserves_history_and_main_view -- --nocapture`
- `cargo test -p rmux-core --locked transition_tables_cover_every_state_byte_once -- --nocapture`
- `cargo test -p rmux-core --locked transition_ -- --nocapture`
- `cargo test -p rmux-core --locked parser_traces -- --nocapture`
- `cargo test -p rmux-core --locked input -- --nocapture`
- `cargo test -p rmux-core --locked ascii_width -- --nocapture`
- `cargo test -p rmux-server --locked styled_pane_screen_borrows_when_no_overlay_is_needed -- --nocapture`
- `cargo test -p rmux-server --locked attach_render_golden_normal_idle_pane_is_byte_stable -- --nocapture`
- `cargo test --locked -p rmux --test request_end_to_end cli_command_surface_matches_public_help_enum_and_dispatch -- --nocapture`
- `cargo test --locked -p rmux --test cli_surface queue -- --nocapture`
- `cargo test --locked -p rmux --test cli_window_surface source_file -- --nocapture`

Perf smoke artifacts under `/tmp`:

- `/tmp/rmux-perf-base/unix-20260620-151017.json`
- `/tmp/rmux-perf-current/unix-20260620-151230.json`
- `/tmp/rmux-perf-current/diff-20260620-151017-vs-151230.json`
- `/tmp/rmux-perf-baseline-current-v4/baseline-20260620-151912.json`
- `/tmp/rmux-perf-current-final/baseline-20260620-154301.json`
- `/tmp/rmux-perf-current-final/schema1/unix-20260620-154301.json`
- `/tmp/rmux-perf-current-final/diff-base-vs-final.json`
- `/tmp/rmux-perf-base-clean-30/baseline-20260620-160046.json`
- `/tmp/rmux-perf-current-clean-30/baseline-20260620-160046.json`
- `/tmp/rmux-perf-current-clean-30/diff-clean-base-vs-current.json`
- `/tmp/rmux-perf-current-post-pr9-pr26b/baseline-20260620-165421.json`
- `/tmp/rmux-perf-current-post-pr9-pr26b-5000/baseline-20260620-165443.json`
- `/tmp/rmux-perf-current-post-pr9-pr26b-5000/diff-clean-base-vs-post-pr9-pr26b.json`

Earlier 5-sample `/tmp` comparison against the first captured `release/0.6.5` baseline:

| Metric | p50 delta | p95 delta | p approx | Status |
|---|---:|---:|---:|---|
| `capture_pane_5000_lines` | -29.8% | -35.5% | 0.0061 | improvement |
| `daemon_startup` | -12.1% | -13.6% | 0.0530 | neutral |
| `diagnose_json_cold` | +0.7% | +29.8% | 0.4798 | neutral |
| `new_session_detached_sh` | -2.3% | -8.9% | 0.4871 | neutral |
| `pane_output_5000_lines_ready` | -2.8% | -8.7% | 0.0316 | neutral |
| `resize_pane_round_trip` | -18.0% | -21.5% | 0.1132 | neutral |
| `send_keys_detached_round_trip` | -17.5% | +3.7% | 0.0467 | neutral |
| `split_window_detached_sh` | +13.4% | +15.9% | 0.1488 | neutral |

The comparator classified only `capture_pane_5000_lines` as statistically significant at alpha 0.01.
The `split_window_detached_sh` increase stayed neutral with the current 5-run sample and must be watched
with larger samples before making release-facing claims.

Final clean `/tmp` comparison used a detached `release/0.6.5` worktree at
`53d3e2b2ce5143c8a8fb201d9e5c5328ca2372cf`, separate base/current release binaries,
and 30 samples per metric:

| Metric | p50 delta | p95 delta | p approx | Status |
|---|---:|---:|---:|---|
| `capture_pane_5000_lines` | +4.1% | -7.8% | 0.6409 | neutral |
| `daemon_startup` | +0.3% | -19.0% | 0.3848 | neutral |
| `diagnose_json_cold` | +0.6% | -17.6% | 0.3673 | neutral |
| `new_session_detached_sh` | +0.5% | +10.0% | 0.8429 | neutral |
| `pane_output_5000_lines_ready` | -1.2% | -1.3% | 0.0018 | neutral |
| `resize_pane_round_trip` | +1.5% | +13.5% | 0.3024 | neutral |
| `send_keys_detached_round_trip` | +6.9% | +2.9% | 0.6139 | neutral |
| `split_window_detached_sh` | +10.8% | +22.8% | 0.0546 | neutral |

No metric crossed the configured regression classification threshold in the clean n=30 comparison.
The `split_window_detached_sh` p50/p95 increase remains non-significant and should stay on the watch list.

Post-PR9/PR26B clean `/tmp` comparison reused the detached `release/0.6.5`
baseline at `53d3e2b2ce5143c8a8fb201d9e5c5328ca2372cf` and compared against
the current release binary after the parser monomorphization and copy-mode
matching-bracket scan changes, with 30 samples per metric:

| Metric | p50 delta | p95 delta | p approx | Status |
|---|---:|---:|---:|---|
| `capture_pane_5000_lines` | +10.8% | +15.8% | 0.4898 | neutral |
| `daemon_startup` | -16.2% | -30.2% | 0.0000 | improvement |
| `diagnose_json_cold` | -14.4% | -24.1% | 0.0001 | improvement |
| `new_session_detached_sh` | -19.4% | -9.2% | 0.0000 | neutral |
| `pane_output_5000_lines_ready` | +3.9% | +7.0% | 0.0000 | neutral |
| `resize_pane_round_trip` | -11.6% | +7.8% | 0.0182 | neutral |
| `send_keys_detached_round_trip` | -8.2% | -25.7% | 0.0006 | improvement |
| `split_window_detached_sh` | -11.2% | -14.8% | 0.0003 | improvement |

No metric crossed the configured regression classification threshold in the
post-PR9/PR26B comparison. `capture_pane_5000_lines` remains noisy and should be
watched because its current p50/p95 increased without statistical support.
