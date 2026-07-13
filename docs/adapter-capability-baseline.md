# External Adapter Capability Baseline (R-001)

Status: completed baseline for RMUX `0.8.0` at commit `3b254fee1931`.

This document records only behavior available through the public Rust SDK or
typed IPC and backed by executable tests. It is the dependency boundary for a
future out-of-tree adapter. It does not define an AI-agent protocol, a global
message bus, or an input-delivery policy.

## Status vocabulary

- `supported`: the public surface is sufficient and has executable evidence.
- `partial`: useful public behavior exists, but a named semantic gap remains.
- `missing`: no public runtime surface provides the capability.
- `not-applicable`: the concern belongs to the adapter or message bus, not RMUX.

## Capability matrix

| Capability | Status | Public SDK / IPC entry | Semantics and scope | Failure behavior | Automated evidence | A-001 / A-002 impact | R-002 blocker |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Default and explicit endpoint selection | supported | `RmuxEndpoint`, `RmuxBuilder::endpoint`, `unix_socket`, `windows_pipe`, `Rmux::resolved_endpoint` | Default discovery is user-scoped. Explicit endpoints bypass the discovery allowlist but retain daemon-side permission checks. Unix/macOS use Unix sockets; Windows uses per-user named pipes. | Invalid or unreachable endpoints return `RmuxError::Transport`; a platform-incompatible explicit variant returns `Unsupported`. | `crates/rmux-sdk/tests/discovery.rs`; `adapter_baseline_observes_replacement_and_restart_boundaries` | Mock the resolved endpoint, not environment variables. A-002 may accept an explicit endpoint or use SDK discovery. | no |
| Daemon probe and capability negotiation | supported | `Rmux::connect`, `capabilities`, `has_capability`, `list_sessions` | `connect` probes without starting. The handshake exposes string capability identifiers for the connected daemon. | Connection, wire-version, and missing-capability failures are typed as `Transport` or `Unsupported`. | `crates/rmux-sdk/src/handles/rmux.rs`; adapter baseline smoke | A-001 needs capability sets and typed failures. A-002 must gate optional operations before use. | no |
| Disconnect detection and client reuse | partial | Every daemon-backed operation; `RmuxError::Transport` | A transport actor latches its terminal failure. A fresh `Rmux` and fresh child handles are required after daemon loss; existing handles do not reconnect. | Graceful shutdown and abrupt loss terminate old operations with `Transport`; output subscriptions do not migrate. | `failure_cleanup_uses_existing_typed_diagnostics`; adapter baseline smoke | A-002 must rebuild the client, discard all old handles, and rediscover. | no for conservative reconnect; epoch support is an R-002 candidate |
| Named session create / reuse | supported | `EnsureSession`, `EnsureSessionPolicy`, `Rmux::ensure_session` | `CreateOnly`, `CreateOrReuse`, and `ReuseOnly` are explicit. Concurrent `CreateOrReuse` calls converge on one session; exactly one reports creation. | Duplicate create and missing reuse are protocol errors. | `create_only_duplicate_is_error_but_attach_if_exists_reuses`; `concurrent_create_or_reuse_creates_one_named_session` | A-001 must model creation outcome. A-002 can safely ensure its named session. | no |
| Existing window and pane discovery | supported | `Session::window`, `Session::pane`, `Session::pane_by_id`, `Rmux::find_panes`, `Pane::id` | Slot handles resolve the current occupant on each call. By-id handles bind to one `PaneId` within the current daemon lifetime. | Missing slots return empty/default observations where documented; missing stable ids return `PaneNotFound` or stale observations. | `crates/rmux-sdk/tests/lifecycle.rs`; adapter baseline smoke | A-002 should cache `(endpoint, session name, PaneId)`, not a pane index alone. | no |
| Stable object identities | partial | `SessionId`, `WindowId`, `PaneId`; `InfoSnapshot` | IDs are stable only inside one daemon process. A restarted daemon can reuse the same numeric IDs. There is no public daemon epoch to qualify them. | Old handles fail with `Transport`; a new client can observe numerically equal IDs belonging to new objects. | `crates/rmux-sdk/tests/identity.rs`; adapter baseline smoke | A-001 must include an adapter connection epoch. A-002 must invalidate all cached identities on reconnect. | candidate: expose a daemon-instance identity if cross-restart comparison is required |
| Generation, revision, and sticky process state | partial | `SessionInfo`, `WindowInfo`, `PaneInfo`, `Pane::info` | `generation` counts observed mutations; `revision` is a coarser visible/layout counter; `output_sequence` is per-pane. They are sticky state counters, not incarnation tokens, and reset with the daemon. | Lag recovery can refresh current state, but cannot prove continuity across daemon restart or reconstruct lost bytes. | `crates/rmux-sdk/tests/info.rs`; `crates/rmux-sdk/tests/lifecycle.rs` | A-001 models counters separately from identity. A-002 must never use generation alone to detect replacement. | no for MVP; daemon epoch is the related candidate |
| Pane replacement and stale handles | supported | Slot `Pane`, `Session::pane_by_id`, `Pane::id`, `Pane::info` | Closing and recreating a pane in the same slot yields a new `PaneId`. The slot handle follows the replacement; the old by-id handle stays stale. | Stable operations fail or return stale/empty results rather than targeting the replacement. | `pane_close_and_respawn_preserve_slot_semantics`; adapter baseline smoke | A-002 must use by-id handles for critical observation and re-resolve after stale results. | no |
| Rendered snapshot | supported | `Pane::snapshot`, `PaneSnapshot` | Captures the current typed terminal grid, cells, dimensions, cursor, and revision. It is rendered state, not a byte transcript. IPC frame bounds apply. | A stale slot returns an empty revision-0 snapshot; transport/protocol failures remain typed. | `crates/rmux-sdk/tests/snapshot.rs`; `crates/rmux-sdk/tests/smoke_v1_full.rs` | A-001 needs a rendered snapshot DTO. A-002 can use it to re-anchor visible state after lag. | no |
| Observe-only raw output | supported | `Pane::output_stream_starting_at`, `PaneOutputStream`, `PaneOutputChunk` | Observation sends no pane input. Chunks preserve arbitrary bytes and carry a monotonic per-pane sequence. | Setup can fail for stale panes, transport loss, or capability refusal. | `streams_contract_tests.rs`; `adapter_baseline_observes_replacement_and_restart_boundaries` | This is the primary A-002 data path. | no |
| Stream start cursor | partial | `PaneOutputStart::{Now, Oldest}` | `Now` observes only future output; `Oldest` replays the retained ring. No public arbitrary sequence cursor is available. | Retention gaps become lag notices; reconnect cannot request an exact previously persisted sequence. | `streams_contract_tests.rs`; burst and oldest smoke tests | A-001 models only `Now` / `Oldest`. A-002 must document snapshot-plus-resubscribe recovery. | candidate for exact resumable delivery, not an observe-only MVP blocker |
| Sequence, arbitrary bytes, and bursts | supported | `PaneOutputChunk::Bytes { sequence, bytes }` | Sequence is monotonic per pane and per daemon lifetime. Bytes may contain NUL or invalid UTF-8. Burst events preserve order; line rendering is separately lossy. | Protocol response/subscription mismatches are rejected. | `output_stream_preserves_arbitrary_bytes_and_sequences`; `immediate_burst_output_is_available_from_oldest_cursor` | A-001 must preserve bytes and sequence without UTF-8 conversion. | no |
| EOF and subscription removal | partial | Empty `PaneOutputChunk::Bytes`; `PaneOutputStream::next -> Ok(None)` | An empty byte event is treated as pane EOF. A removed subscription also ends as `None`; the public stream does not expose a distinct terminal reason enum. Transport loss remains an error. | Callers can distinguish clean end from transport error, but not EOF from subscription-gone after the terminal item is consumed. | `collect_stream_until_eof`; `output_stream_returns_none_when_subscription_gone` | A-002 should map terminal completion conservatively and confirm sticky pane state with `info()`. | candidate: typed terminal reason |
| Lag visibility | supported | `PaneOutputChunk::Lag(PaneLagNotice)` | The notice carries expected, resume, missed, newest sequences and bounded recent raw bytes. The stream resumes at retained output; lag is not silent. | Bytes older than retention are unrecoverable. | `output_stream_surfaces_lag_notice_with_recent_bytes`; `output_stream_lag_after_buffered_bytes_returns_buffered_first` | A-002 can enter `degraded`, refresh info/snapshot, and continue. | no |
| Exact lag / reconnect recovery | partial | `Pane::info`, `Pane::snapshot`, new output subscription | Sticky state and rendered state can be refreshed. Exact dropped raw bytes cannot be reconstructed, and no arbitrary cursor exists after reconnect. | Recovery is explicitly lossy when retained output is gone. | `info_snapshot_lag_recovery_refreshes_sticky_state_and_output_anchor`; stream contract tests | A-002 health must expose degraded history rather than claiming exactly-once observation. | candidate only if exact transcript recovery is required |
| Graceful and abrupt daemon restart | partial | `Rmux::shutdown`, fresh `RmuxBuilder::connect_or_start` | Graceful and abrupt loss invalidate all clients, handles, streams, identities, counters, and subscriptions. A fresh client can recover the endpoint but not the prior runtime. | Old actors retain terminal `Transport` failure; stale socket startup is recovered by the bootstrap path. | `failure_cleanup_uses_existing_typed_diagnostics`; adapter baseline smoke | A-002 state transitions: `healthy -> disconnected -> reconnecting -> rediscovering`; never reuse old handles. | candidate: public daemon epoch; no blocker with conservative reset |
| Unified live `PaneEvent` lifecycle stream | missing | `PaneEvent` is a public DTO only | The Rust SDK does not expose one public subscription that emits output, close, lag, disconnect, permission, and command events together. Do not claim otherwise. | Consumers must combine output streams with sticky polling and transport errors. | Public API inventory; `crates/rmux-sdk/tests/events.rs` proves serialization only | A-001 may model adapter events independently. A-002 must not depend on a nonexistent producer. | candidate if polling proves insufficient |
| Literal/key input | supported | `Pane::send_text`, `Pane::send_key` | Calls enqueue one daemon request and return after its response. There is no delivery receipt from the child process and no idempotency key. Retrying after an uncertain transport failure can duplicate side effects. | Stale panes and transport loss are typed errors. | `crates/rmux-sdk/tests/pane_input.rs` | Inventory only for R-001; A-002 observe-only must not call these methods. | no |
| Broadcast input | partial | `Rmux::broadcast`, `PaneSet::broadcast`, `PartialBroadcastFailure` | Per-pane success/failure is reported, but the batch is not atomic and successful panes may already have consumed input. | Partial failure identifies successes and failures; blind retry can duplicate successful panes. | `broadcast_reports_partial_failures_by_pane`; `crates/rmux-sdk/tests/pane_set.rs` | Exclude from A-002 observe-only behavior. | no |
| Durable queued input / idempotent receipt | missing | none | RMUX has no public durable input queue, message id, acknowledgement ledger, or exactly-once retry contract. | Caller cannot safely infer whether an uncertain write reached the child process. | Public API inventory | Any future delivery integration needs a separate design; do not simulate it by retrying `send_text`. | not part of R-002 unless a generic terminal API is separately approved |
| Native inbox and structured hook | not-applicable | none | These are adapter/message-bus delivery modes, not terminal multiplexer capabilities. | Defined by the downstream transport. | downstream A-series tests | Implement outside RMUX. | no |
| Safe stdin | not-applicable | none | Safe stdin requires policy, readiness, deduplication, and operator controls above RMUX's raw input primitive. It is intentionally outside R-001. | Raw retries may duplicate side effects. | future dedicated goal only | Must remain the last delivery mode, after observe-only and native inbox. | no |

## Adapter readiness decision

The Rust SDK is sufficient for A-001 mocks and an A-002 observe-only adapter:
endpoint resolution, capability negotiation, idempotent named-session ensure,
stable in-epoch pane identity, sticky info, rendered snapshots, sequenced raw
bytes, explicit lag, and typed transport failures are all public.

A-002 must use a conservative recovery rule: any transport failure creates a
new adapter connection epoch, drops every RMUX handle and cached identity,
reconnects with a new `Rmux`, rediscovers the named session and pane, refreshes
`info()` and `snapshot()`, then opens a new `Now` or `Oldest` stream. It must
report degraded history after lag or disconnect rather than claiming an exact
transcript.

The baseline does not block native-inbox work in the downstream adapter. A
native inbox does not require terminal input. It still needs the independent
adapter's endpoint/context mapping, health model, and message-bus delivery
contract; those are deliberately not RMUX product features.

## Differences from earlier integration assumptions

1. `generation` is a mutation counter, not a cross-restart incarnation token.
2. `PaneId` is stable only within one daemon process; numeric ids can be reused
   after restart.
3. Output subscriptions support `Now` and `Oldest`, not an arbitrary persisted
   sequence cursor.
4. `PaneEvent` is a typed vocabulary, not a public unified live event source.
5. EOF, subscription removal, and transport loss do not share one typed
   terminal-reason enum.

## Python binding comparison

The separately released `librmux 0.6.1` Python SDK is a CLI/control-mode
wrapper, not a binding to the Rust detached-IPC client. It exposes binary
contract capability JSON, named session ensure, pane ids as strings, captures,
and synchronous control-mode output. It does not expose the Rust SDK's typed
`SessionId`/`WindowId`/`PaneId` DTOs, sticky `InfoSnapshot` generations and
revisions, per-pane output sequence, `Now`/`Oldest`, structured lag notices, or
detached-IPC terminal failure semantics. Its control stream has raw bytes and
EOF, but no exact parity with `PaneOutputStream`.

The first adapter should therefore target the Rust SDK. Python parity is a
separate versioned compatibility project, not an assumption of R-001.

## R-002 candidates

R-002 should be opened only if A-001/A-002 proves one of these generic gaps is
necessary:

1. A public daemon-instance identity that qualifies object ids and counters.
2. An arbitrary sequence start cursor for exact reconnect recovery.
3. A typed output-stream terminal reason distinguishing pane EOF,
   subscription removal, and daemon shutdown.
4. A public lifecycle stream, if output plus sticky polling is insufficient.

None is required to start the conservative observe-only implementation.
