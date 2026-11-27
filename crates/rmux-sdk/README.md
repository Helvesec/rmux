# rmux-sdk

Public, daemon-backed Rust SDK for the [RMUX](https://github.com/Helvesec/rmux) terminal multiplexer.

Drives sessions, windows, and panes through typed handles over the local
RMUX daemon — no TTY scraping, no string-formatted commands. Snapshots,
send-keys, wait-for-text, output streams, and `EnsureSession` bootstrap are
the primary primitives.

## Usage

For a 0.11.0 daemon (wire 9), use the SDK from the same checkout. Replace
the example path with your local path to the crate. After the matching crate
is published, you can omit `path` and keep `version = "0.11.0"`.
The released 0.10.0 SDK uses wire 8.

For a path containing an apostrophe, use double quotes and forward slashes (for example, `"C:/src/team's/rmux/crates/rmux-sdk"`).

```toml
[dependencies]
rmux-sdk = { path = '/path/to/rmux/crates/rmux-sdk', version = "0.11.0" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use rmux_sdk::{EnsureSession, EnsureSessionPolicy, Rmux, SessionName};

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let rmux = Rmux::builder().connect_or_start().await?;
    let session = rmux
        .ensure_session(
            EnsureSession::named(SessionName::new("work").unwrap())
                .policy(EnsureSessionPolicy::CreateOrReuse)
                .detached(true),
        )
        .await?;

    let pane = session.pane(0, 0);
    pane.send_text("printf 'ready\\n'\n").await?;
    pane.wait_for_text("ready").await?;

    let logs = session.new_window_with().name("logs").detached(true).await?;
    assert!(logs.exists().await?);

    let snapshot = pane.snapshot().await?;
    println!("{}x{}", snapshot.cols, snapshot.rows);
    Ok(())
}
```

## Surface

- `Rmux::builder().connect_or_start()` — reuse a running daemon or spawn one.
- `EnsureSession` — idempotent session bootstrap (named, create-or-reuse, detached, process spec).
- Typed handles: `Session`, `Window`, `Pane`.
- `Session::window(index)` — lazy handle for a window slot; `Session::new_window()` creates one now.
- `Pane::send_text`, `Pane::wait_for_text`, `Pane::snapshot`, plus streams and events for incremental output.

The SDK and local IPC client share internal daemon startup coordination.

## Migrating from released 0.10 toward 1.0

This guide consolidates the pre-baseline SDK transport cutoff for RMUX v28.
The source baseline is released `v0.10.0`, commit
`dfd68c774ca0f4212139a21d37d09c90f75f8bd7`, not a later migration commit's
parent. At the historical cutoff, the checkout still declared package version
`0.10.0`; the current checkout declares `0.11.0`. The changes below describe
the candidate toward 1.0, not APIs already in the released `0.10.0` crate.
They do not declare a frozen 1.0 baseline or release qualification.
No additional 0.9→1.0 SDK source route is promised here; legacy daemon/wire
compatibility is a separate contract.

Import public values through `rmux_sdk` (or its documented modules). SDK
facades, snapshots, events and semantic errors use SDK-owned types or the
canonical leaf identities from `rmux-types`. `rmux-proto` contains internal
wire DTOs/codecs; its shared identity aliases do not make transport DTOs public
SDK values. Replace mixed-protocol arguments and public wire conversions with
SDK constructors, fields and builders. Do not add casts or a direct internal
crate dependency to restore a retired route. The SDK migrations below preserved
the then-current wire v8. RMUX 0.11.0 separately moved the daemon to wire 9.
The SDK command serialization exception is specified below.

The [combined downstream consumer](tests/fixtures/sdk-migration/src/lib.rs)
uses every migration family and the [three named streams](tests/fixtures/sdk-migration/src/streams.rs)
with only `rmux-sdk` as its RMUX dependency. Tokio supplies async cancellation;
no protocol or bootstrap types are imported. Build instructions and the
compile/runtime distinction are [below](#compiled-consumer).

### Bootstrap

The pre-1.0 cutoff removes the public `rmux_sdk::bootstrap` module, including
its Unix/Windows startup constructors, stream accessors and endpoint-returning
helpers. Use `Rmux::builder().connect_or_start().await?` to obtain an `Rmux`
handle. `RmuxBuilder::connect()` connects only; `build()` is inert. The existing
SDK error contract remains the public boundary.

Configure endpoint and timeout through `RmuxBuilder`; `resolved_endpoint()` and
`resolved_timeout()` retain discovery access. Explicit endpoint selection wins
over `RMUX_SDK_ENDPOINT`, which is validated before platform-default fallback.
The environment endpoint allowlist remains distinct from explicit caller-owned
endpoints. Timeout precedence remains per-operation override, builder default,
`RMUX_SDK_TIMEOUT_MS`, then five seconds. An explicit `Duration::MAX` resolves
to no timeout; zero is a zero budget, not an alias for unbounded.
`RMUX_SDK_DAEMON_BINARY` retains binary selection behavior.

Startup serialization, selected endpoint generation, readiness/configuration,
remaining operation budget, binary/version selection and upgrade behavior are
preserved. Cancellation is not a promise to stop an already-running blocking
OS probe immediately. Dropping the returned facade never kills the daemon;
`rmux.shutdown().await` is an explicit daemon-wide shutdown operation.

The shared implementation now lives in internal `rmux-bootstrap`, consumed by
the SDK and CLI client. Its versioned package dependency is an implementation
requirement, **not a stable SDK raw-stream escape hatch**. Migrate to the handle
facade; there is no replacement public bootstrap transport route.

### Canonical identity

`SessionName`, `SessionId`, `WindowId` and `PaneId` now have one canonical owner,
`rmux-types`. SDK users keep importing them from `rmux_sdk` or `rmux_sdk::types`.
Core and protocol reexports resolve to those same types; the existing
`rmux_proto::identity`, protocol root, and `rmux_proto::types::SessionName` paths
remain available. Passing identity values between crates needs no wrapper,
conversion or reparsing. Allocation and resolution remain core/server policies.
`rmux-types` has no protocol dependency and no default features; its optional
`serde` feature supplies the existing serialization implementations.

The deliberate pre-1.0 source/API change is the validation error:
`SessionName::new`, `FromStr::Err`, `TryFrom<&str>::Error` and
`TryFrom<String>::Error` now use `SessionNameError`, also reexported by the SDK
and protocol. This small, transport-independent unit error implements
`Error + Copy + Clone + Eq + Hash + Send + Sync + 'static`. Its Display remains
`invalid session name: session names must be non-empty`; Debug is now
`SessionNameError`, and its error source remains absent. Replace direct
`rmux_proto::RmuxError::EmptySessionName` constructor-error matches or associated
error annotations with `SessionNameError`.

```rust
use rmux_sdk::{SessionName, SessionNameError};

fn validate(raw: &str) -> Result<SessionName, SessionNameError> {
    SessionName::new(raw)
}

fn sdk_name(raw: &str) -> rmux_sdk::Result<SessionName> {
    Ok(SessionName::new(raw)?)
}
```

The semantic `From<SessionNameError> for rmux_sdk::RmuxError` supports `?` into
SDK errors; returning the validator result directly as an SDK `Result` uses
`.map_err(rmux_sdk::RmuxError::from)`. The public raw adapter
`From<SessionNameError> for rmux_proto::RmuxError` is intentionally removed
before 1.0, including its `Into`, `TryInto` and protocol `?` routes. Internal
protocol boundaries map validation errors explicitly to the unchanged
`EmptySessionName` variant, diagnostic and wire tag 0. SDK validation still
produces `RmuxError::Protocol` with the existing opaque SDK `ProtocolError`
source, diagnostic prefix and hint. Public raw protocol-error conversions into
the SDK remain unavailable.

Normalization is unchanged: constructors escape literal backslashes; serde
preserves canonical backslashes and normalizes raw colon/dot, C0/C1 controls
and the existing Unicode format-character set. Passing canonical values avoids
double sanitization. Plain Unicode bytes, ID zero/max values, display prefixes,
traits, and JSON/binary layouts are preserved. There is no wire-version or
target-parser change. This migration does not promise old associated-error
source compatibility for 0.10 or earlier callers.

### Errors and diagnostic helpers

`RmuxError::Protocol::source` is now an opaque SDK `ProtocolError` instead of a
raw protocol enum. `RmuxError::protocol` accepts that SDK type; use
`ProtocolError::new(message)` for an application diagnostic. The former public
`From<rmux_proto::RmuxError>` and `From<rmux_proto::ErrorResponse>` into SDK
errors, their `Into` routes, and raw source matches are removed. Internal
`from_proto`, `protocol_source`, `into_proto` and the wrapped value stay private.

Match SDK variants with `..` and a fallback for non-exhaustive types. For a
`Protocol` error, use `duplicate_session()`, `session_not_found()`,
`invalid_target()` or `server_message()` on its source when that distinction
matters. `ProtocolError::new` is a plain message, not a server failure.
`Error::source()` exposes the SDK leaf; it cannot be downcast to the raw
protocol error. Display/Debug diagnostics, protocol prefixes and recovery hints
are retained. `RmuxError`, `ProtocolError` and `CollectError` remain non-Clone;
use application-owned `Arc` if diagnostics must be shared.

Both root and `diagnostics` module versions of `protocol_diagnostic` and
`unsupported_feature_id` now take `&rmux_sdk::RmuxError`, not a raw protocol
error. `feature()` and `hint()` retain structured capability/recovery data.
Typed pane-not-found, process-running, spawn, lease, wait, broadcast and
aggregate outcomes remain SDK errors. `CollectError::errors()` preserves
individual diagnostics and ordering; `std::io::Error` and `CollectError`
conversions into SDK errors remain supported. Session validation's semantic
`?` conversion is described above; layout parsing has its separate error below.

### Named-layout

The pre-1.0 transport cutoff gives `rmux_sdk::LayoutName` its own SDK type.
Consumers already importing that path keep the same spelling and operation:

```rust
use rmux_sdk::{LayoutName, Window};

async fn arrange(window: &Window) -> rmux_sdk::Result<()> {
    window.select_layout(LayoutName::Tiled).await
}
```

This intentionally breaks type identity with the 0.10 reexport. Callers passing
`rmux_proto::LayoutName` directly to `Window::select_layout` must migrate to
`rmux_sdk::LayoutName`; there is no public transport conversion. This is not
0.10 mixed-proto source compatibility. `FromStr::Err` is now the public,
SDK-owned `rmux_sdk::ParseLayoutNameError`; callers naming or matching the old
protocol error must migrate too. Its display diagnostic is preserved.
Propagating a parse failure with `?` into an existing error type now requires
that type to accept `ParseLayoutNameError`; the downstream fixture uses an
application-level `Box<dyn std::error::Error>`.

All seven variants, `Clone`/`Copy`/`Debug`/`PartialEq`/`Eq`, exact lowercase
hyphenated display/parse names, and serde behavior are preserved. JSON uses
variant names such as `"Tiled"`, not display names such as `"tiled"`; binary
serde retains the original variant order. This SDK type change preserved the
protocol enums and their encoding.

### Web theme and palette

With feature `web`, `rmux_sdk::WebTerminalTheme` and `rmux_sdk::WebTerminalPalette`
are now SDK-owned types at the same import paths. Session and pane share builders
retain `theme`, `terminal_theme`, `user_theme`, `light_theme`, `dark_theme` and
`terminal_palette`, including unset defaults and last-call-wins behavior.
Theme variants, derives, const `as_url_value`, serde names and binary variant
order are unchanged. Palette fields, all 16 ANSI entries and serialization are
preserved; values are moved into the request without color normalization.

This intentional pre-1.0 type-identity break requires callers mixing
`rmux_proto` theme or palette values with SDK builders to construct the SDK
types instead. There is no public protocol conversion or claim of old
mixed-proto source compatibility. Protocol definitions and wire are unchanged.

### Web Share scope and response

The intentional pre-1.0 cutoff replaces protocol scope/target values in
`WebShareSummary`, `WebShareHandle` and `WebShareLookup` with
`rmux_sdk::WebShareScope` and `WebSharePaneTarget`. Match `Session(SessionName)`
or `Pane(WebSharePaneTarget)`; the latter retains `Slot(PaneRef)` and
`Id { session_name, pane_id }`. Use SDK constructors or variants, and the
existing canonical `SessionName`/`PaneId`. An ID is never an index-zero slot.
For `Slot`, read `PaneRef`'s public component fields. Scope/target display,
serde names/order and value traits are preserved.

Public protocol `From`/`Into` conversions to `WebShareSummary` and `WebConfigInfo`
are removed. List, lookup, config and create return SDK values; mixed-protocol
callers must construct their own values explicitly. Summary literals now also
include the daemon's `controls` flag. Handles expose `controls`, `tunnel_provider`
and `tunnel_public_url` reported at creation; lookup exposes `controls` too.
These are passive metadata, without authorization, URL or tunnel policy changes.
All optional values, redacted/full URLs, pairing codes, counts, caps and expiry
values are retained. Builder requests, identity resolution, deadlines, transport
reuse, stop behavior and protocol/wire definitions are unchanged.

### Foreground and pane-state

`rmux_sdk::ForegroundSource` and `rmux_sdk::PaneStateClosedReason` are now
SDK-owned, non-exhaustive enums at their existing public import paths. All six
source variants, all three close reasons, derives, serde names and binary
variant order are preserved. Every `ForegroundSources` field now uses
`Option<ForegroundSource>`. Query, snapshot and event values retain all optional
fields, exact strings, revisions and pane identities.

This deliberate pre-1.0 migration breaks type identity with the 0.10 protocol
reexports. Mixed-proto callers must construct SDK values instead. Public
`From`/`Into` conversions from protocol foreground states/sources, pane-state
events and pane-option entries have been removed; those DTO mappings are SDK
implementation details. This is not mixed-proto 0.10 source compatibility.
Protocol definitions, wire format and stream behavior are unchanged.

### Terminal-size conversion

The pre-1.0 transport cutoff removes public `From`/`Into` conversions between
`rmux_sdk::TerminalSizeSpec` and `rmux_proto::TerminalSize` (the reexport of
`rmux_types::TerminalSize`). Mixed-proto callers must construct SDK values with
`TerminalSizeSpec::new(cols, rows)` or its public fields. When working separately
with protocol values, copy their `cols` and `rows` explicitly. The SDK's transport
helpers are crate-private; this is an intentional API break, not 0.10 mixed-proto
source compatibility.

Fields, const construction, derives, default `0×0`, serde bytes and exact `u16`
values are unchanged, including zero and maximum dimensions. Ensure-session,
new/attach command sizes, info responses and resize retain their existing
behavior. Protocol definitions and wire format are unchanged.

### Structured-selector conversion

The deliberate pre-1.0 transport cutoff removes the public `WindowRef::to_proto`
and `PaneRef::to_proto` methods and the owned, borrowed and reverse protocol
`From`/`Into` conversions for `WindowRef`, `PaneRef` and `TargetRef`. Construct SDK
selectors using their constructors or public fields. For logging, replace
`pane.target().to_proto()` with `pane.target()`: SDK `Display` preserves the exact
`session`, `session:window` and `session:window.pane` spellings.

Fields, derives, serde names and binary field/variant order, window-zero helpers,
session names and exact `u32` indices are unchanged. Selectors remain structured
values without parsing, normalization or lookup. SDK transport callers use
crate-private helpers; protocol definitions and wire bytes are unchanged. This
is an intentional public API break, without old mixed-proto source compatibility.

### Command-spec value conversion

The intentional pre-1.0 transport cutoff removes public protocol `From`/`Into`
conversions for `ProcessCommandSpec`, `ClientTerminalSpec`, `SplitDirectionSpec`
and `SplitTargetSpec`, including borrowed split targets and reverse conversions.
Construct SDK values using their variants, public fields and `ProcessSpec::argv`
or `ProcessSpec::shell`, then pass them to SDK specs or builders. Code working
separately with protocol DTOs must explicitly construct those DTOs. There is no
old mixed-proto source compatibility promise.

These values retain their fields, traits, defaults, serde representation and
literal contents. Request construction preserves owned moves and borrowed
clones, without parsing or normalizing commands, environments or selectors.
The process mappings used by runtime builders remain crate-private. The
command/full-spec cutoff also removes the now-unused terminal, split
direction and split target mappings. Protocol and wire are unchanged.

### Command and full-spec

`RmuxCommand` and `RmuxCommandKind` remain inert SDK values. The intentional
pre-1.0 cutoff removes `RmuxCommandKind::Request`, both `into_request` methods,
all command/spec `From`/`Into` conversions to or from `rmux_proto::Request`, and
the four full-spec conversions to protocol payloads. Construct commands from
`NewSessionSpec`, `AttachSessionSpec`, `SplitSpec` or `RefreshClientSpec` using
SDK fields, variants, `From`, `new` and `with_endpoint`. These DTOs have no
execution API. Use the existing facade/builders for supported operations or
`Rmux::cmd(["display-message", "-p", "literal text"])` for the official CLI
escape hatch. `CommandRun` retains stdout/stderr bytes and an optional exit code; a nonzero
CLI exit is a command result, not an SDK transport error. Raw IPC is outside
the supported SDK migration route.

Typed JSON names and fields, optional/default values and literal contents are
unchanged. Binary serde retains `NewSession=1`, `AttachSession=2`,
`SplitWindow=3`, `RefreshClient=4`, including inside `RmuxCommand`. Index 0 and
the JSON name `Request` are retired and rejected before reading their payload;
old raw values must be migrated explicitly by their owning application. Typed
persisted values need no rewrite, and old typed bytes never acquire a different
meaning. This is an SDK API/serde boundary change; protocol definitions, request
tags, wire version and daemon/runtime behavior are unchanged.

### Recovery, surface and render streams

These delivered 0.10 APIs remain active SDK streams, with owned SDK events and
snapshots. The inert control-mode vocabulary in `events::types` is a separate
DTO surface. Open the streams from `Pane`; do not implement their wire protocol.
Use a stable-ID handle (`Rmux::pane_by_id`) when tracking a pane across slot
moves/rekeying. A `PaneRef` is a slot selector, not a stable identity.

| Entry point | Consume | Recovery and end behavior |
| --- | --- | --- |
| `Pane::recover_output()` / `recover_output_with(options)` | `PaneRecoveryEvent`; reset your emulator on `Rebase`, then feed exact bytes | In-band rebases repair lag/invalidation/generation changes. A rejected poll or broken continuation closes this raw subscription. |
| `Pane::surface_stream()` | `PaneSurfaceEvent`; `Reset` and `Patch` each carry a complete `PaneSurfaceFrame` | Projection/frame-size poll errors leave the subscription available for retry after pane state changes. A typed `End` is terminal. |
| `Pane::render_stream()` | `RenderUpdate::snapshot()`, `lag()`, or owned `into_snapshot()` | Debounced shared surface updates with correlated legacy output lag; child EOF alone does not end rendering. |

**Raw recovery.** The opening event is a complete initial ANSI keyframe, except
when observation permission is revoked while subscribe is in flight: the
opening event can instead be `End(AccessRevoked)`, with no bytes to render.
A rebase carries a stream-local `epoch`, process `generation`, cold-path
`invalidation_revision`, geometry and `next_sequence` (the first output event
not covered by the keyframe). Reset and resize your emulator to that state;
never concatenate across rebases. Feed subsequent `Bytes` unchanged, without
lossy UTF-8 conversion. Sequences count output events, not byte offsets.

The SDK validates identity, opening admission, advancing epochs, non-regressing
generation/invalidation/sequence, and exact byte continuation before delivery.
`PaneRecoveryState` exposes derived epoch/generation/cursor state and validates
replays; its `apply` does not copy payloads. Lag, resize, clear-history, parser
expiry, terminal reset, transcript mutation and process-generation change
produce in-band rebases on the same healthy subscription. Do not reopen just
because a rebase arrived. If a required raw keyframe cannot fit the bounded
transport, the stream ends with `SlowConsumer`; skipping it would corrupt
continuity. After a returned raw error, stop that subscription. An explicit
new `recover_output()` starts with fresh authoritative state, not a guessed
resume cursor; this does not replay application mutations.

`PaneRecoveryCoverage::history_complete()` reports retained scrollback coverage;
`metadata_complete` separately reports exact title, OSC7 path, title-stack and
hyperlink reconstruction. Coverage is bounded, not a full history guarantee.
To also receive an owned viewport on each rebase, start with
`PaneRecoveryOptions::default()` and set `include_snapshot = true`. The snapshot
is optional and supplements the required raw keyframe.

**Surface projection.** Each reset/patch includes the full grid/cursor, aligned
hyperlink IDs and URIs, title/path, dynamic colors, modes, alternate-screen,
scroll-region and history metadata. Respect `metadata_complete`.
`PaneSurfaceState::apply` validates shape, reset epoch advancement, patch epoch,
increasing revision and non-regressing `next_output_sequence`; use
`frame()`/`ended()` to inspect accepted state. Revisions need not be contiguous,
and a patch is a replacement frame, not a delta requiring old cells. Surface
epochs are local to that stream, not interchangeable with raw recovery epochs.

A non-transport daemon projection or frame-size error from `next`/`poll_once`
does not automatically close a surface subscription. Retain the stream and
retry after the relevant pane state changes, under an explicit application
retry policy. Do not busy-loop, parse error display strings, or treat every
protocol error as transient. Local reducer rejection is a separate validation
failure; stop or explicitly obtain a fresh baseline. A typed terminal reason,
including `ProjectionFailed` or `TransportLost`, is not a retryable poll error.

**Render baseline and debounce.** Opening `render_stream()` consumes the initial
surface reset internally; it is not emitted by the first `next()`. A quiet pane
may therefore leave `next()` waiting. For an immediate initial image, use
`surface_stream()` and its reset. If a render consumer needs a separate initial
grid, use a stable-ID pane handle, open render first, take one `pane.snapshot()`,
and accept only subsequent
snapshots with a greater revision; queued older updates must not overwrite that
initial grid. Keep handling `lag()` even when a duplicate image is discarded.
The combined consumer demonstrates this ordering.

`with_debounce(Duration)` changes the coalescing delay for pending surface
updates (default 16 ms). Rendering/fingerprinting is shared per pane in the
daemon, with non-visual output filtered there. Render does **not** issue a fresh
snapshot RPC for every output burst. Its legacy output subscription correlates
`PaneLagNotice` with the surface output boundary. Pending frames/lag survive
child EOF and debounce ordering; an observed process-exit→new-epoch reset can
reopen output lag tracking after respawn. The final pending update is delivered
before terminal `None`; use a surface/recovery stream if you need the end reason.

**Cancellation and ownership.** Race `next()` against an application cancellation
future while retaining the stream owner. Cancelling that wait preserves an
in-flight cursor reply for the next call; it is not an unsubscribe. Drop the
stream explicitly to release observation: its guard sends a best-effort
unsubscribe at most once, with no double cleanup after raw rejection or an
opening terminal response. Dropping render releases its output and surface
subscriptions. Cleanup is not an acknowledged synchronous shutdown, and dropping
a stream never kills the pane, child, session or daemon. Cancel or abort and await
any application polling task so its stream owner is dropped; dropping a task's
`JoinHandle` alone detaches the task. A cancelled operation after dispatch does not promise rollback
of a mutation, so do not replay uncertain writes automatically.

Child output EOF (`PaneOutputChunk::Bytes` with empty bytes in the legacy output
stream) and `PaneStreamLifecycleEvent::ProcessExited` do not mean logical pane
removal. Recovery/surface streams can stay alive for a kept pane and later
respawn. Their `PaneStreamEndReason` values are `PaneRemoved`, `AccessRevoked`,
`SlowConsumer`, `ProjectionFailed`, `SubscriptionExpired` and `TransportLost`;
consume `End(reason)` before `None`. On the pane-state metadata stream, handle both terminal events.
When the connection negotiates `sdk.pane.state_events.exit`, a process exit
produces `PaneStateEvent::Exited` with reason `Exited` or `DiedKept` and the
exit status or signal the daemon observed. Each detail is optional. Other
closures and connections without the capability use `PaneStateEvent::Closed`.
After `DiedKept`, snapshots, recovery and surface observation remain available.

Keep resource owners explicit. Ordinary `Session`/`Pane` handles do not own the
process lifetime. `OwnedSession` provides separate cleanup/lease policies:
`KillOnDrop` is best-effort, `KillOnOwnerExit` uses a daemon lease and
`Preserve` keeps the session. Await explicit cleanup when required. Likewise,
`Rmux::shutdown()` acts on the whole daemon, not just this consumer's streams.

### Compiled consumer

The [combined fixture](tests/fixtures/sdk-migration/Cargo.toml) reuses existing
locked dependencies and forwards SDK feature modes. Its public async bodies
exercise builders, canonical identities, semantic errors/helpers, layout,
pane-state, size/selector/spec/command values, Web Share and all three streams.
Web code is gated by `web`; default enables Web, no-default omits it, all also
forwards `regex` and `crossterm`. This is a source migration fixture against this
checkout, not an installed released-package or daemon compatibility test.

From the repository root, using the pinned Rust 1.96.1 toolchain:

```sh
cargo test --offline --locked --manifest-path crates/rmux-sdk/tests/fixtures/sdk-migration/Cargo.toml
cargo test --offline --locked --manifest-path crates/rmux-sdk/tests/fixtures/sdk-migration/Cargo.toml --no-default-features
cargo test --offline --locked --manifest-path crates/rmux-sdk/tests/fixtures/sdk-migration/Cargo.toml --all-features
```

These commands compile every enabled consumer body and run only the small pure
consumer decision tests. Functions that connect, start, mutate, subscribe or
wait are **not executed** by those tests. `cargo check` is type-checking;
`no_run` rustdoc examples are compiled but not executed. Neither is proof of
live daemon/native stream behavior. The original focused fixtures under
[`tests/fixtures`](tests/fixtures) retain their value/serialization controls
and precise removed-API compiler checks; this combined fixture complements them.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
