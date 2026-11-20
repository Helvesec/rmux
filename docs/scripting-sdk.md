# RMUX SDK

RMUX 0.10.0 ships a daemon-backed Rust SDK for terminal automation. The SDK talks
to the local RMUX daemon through the typed IPC contract; it is not a CLI parser
or a tmux control-mode wrapper.

Use the CLI for interactive tmux-compatible workflows, and use `rmux-sdk` when
code is the user: create or reuse sessions, address panes by handle, send input,
wait for rendered text, capture snapshots, inspect locators, stream output, and
start browser shares.

For the v28 pre-baseline changes from released 0.10, see the
[Rust SDK migration guide](../crates/rmux-sdk/README.md#migrating-from-released-010-toward-10)
and its combined compiled consumer. The guide covers public type/error changes,
bootstrap ownership, raw recovery, surface retry, render baseline and cancellation.

## Additional-language SDK scope

Python [`librmux 0.6.1`](https://pypi.org/project/librmux/0.6.1/) targets RMUX
0.6.1; TypeScript [`@rmux/sdk 0.6.5`](https://www.npmjs.com/package/@rmux/sdk/v/0.6.5)
targets RMUX 0.6.x. These historical packages invoke a separately installed CLI;
they do not use the Rust SDK's typed IPC transport. Historical publications and
`Rmux`/`Server` aliases are preserved. This disposition does not declare them
absent, broken or end-of-life, and no candidate regression was demonstrated.

Both additional SDKs are excluded from new RMUX 1.0 distributions, bundled
implementations and positive SDK support, artifact, build and package manifests.
The Rust SDK scope and accepted cutoffs are unchanged. The explicit milestone
[Post-1.0 Python/TypeScript SDK qualification](ROADMAP.md#post-10-pythontypescript-sdk-qualification)
follows RMUX 1.0, without an assigned date or minor version.

A new support promise requires package owners, an approved supported surface
mapped to the common schema, exact source/package/daemon bindings and installed
consumers. Qualification must establish an API baseline, 0.6 migration and
support policy, stable identity, promised stream/recovery semantics, bounded
lifecycle/timeout/cancellation behavior, platform/runtime evidence, release
ownership and independent review. Publication alone does not satisfy these gates.

Historical binary-contract 1, JSON shapes, IDs/targets, formats and control-mode
compatibility remain accountable independently of this exclusion. Any demonstrated
candidate regression needs its own reproduction and correction. Local artifact
policy checks do not qualify historical consumers against the final 1.0 daemon.

## Install

```toml
[dependencies]
rmux-sdk = "0.10.0"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

## Example

```rust
use std::time::Duration;

use rmux_sdk::{EnsureSession, Rmux, SessionName};

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let rmux = Rmux::builder().connect_or_start().await?;
    let session = rmux
        .ensure_session(
            EnsureSession::try_named(SessionName::new("ci")?)?
                .create_or_reuse()
                .detached(true),
        )
        .await?;

    let pane = session.pane(0, 0);
    pane.send_text("printf 'ready\\n'\n").await?;
    pane.expect_visible_text()
        .to_contain("ready")
        .timeout(Duration::from_secs(5))
        .await?;

    Ok(())
}
```

## Discovery

### CLI JSON output and errors

For commands that accept `--json`, results are written as JSON to stdout.
Command errors, such as invalid arguments, unresolved targets or transport
failures, use a nonzero exit status and a plain-text diagnostic on stderr,
with stdout empty. They do not emit partial JSON or a new JSON error object.

Some existing result schemas also describe unsuccessful outcomes: `wait-pane`
timeouts or output lag, unmet `expect-pane` assertions, and output lag from
`collect-pane-output`. These complete version 1 result reports remain JSON on
stdout with a nonzero exit status. Check both the exit status and the command's
documented result fields. This convention does not add a public error schema.

The Rust SDK exposes typed errors for facade operations. Its `Rmux::cmd` escape
hatch preserves the CLI's stdout bytes, stderr bytes and exit status in
`CommandRun`, including nonzero command exits.

### Capability discovery

SDK clients should call `rmux capabilities --json` or use
`Rmux::capabilities()` to negotiate daemon features. `rmux diagnose --json`
reports build, platform, and runtime support details for debugging.

Pane-local SDK metadata and state streams require the daemon capabilities
`sdk.pane.options`, `sdk.pane.state_events`, and, when foreground process data
is requested, `sdk.pane.foreground`. The foreground contract is best-effort:
Unix reports the foreground process group when available, while Windows reports
the ConPTY root process plus OSC7/process/profile cwd fallbacks. The executable
path is best-effort from the observed process, falling back to the configured
profile shell when process inspection is unavailable; RMUX does not try to
classify agent names.

Pane-state `Closed` events are terminal stream events: explicit kill/remove
operations, normal pane removal, and panes retained by `remain-on-exit` close
the stream. Retained panes use `PaneStateClosedReason::DiedKept`; the pane
remains addressable for snapshots and captures after the state stream closes.

## Examples

Run the crate examples from the repository:

```sh
cargo run -p rmux-sdk --example wait_for_text
cargo run -p rmux-sdk --example assert_visible_text
cargo run -p rmux-sdk --example sdk_demo_snapshot
cargo run -p rmux-sdk --example collect_until_exit
cargo run -p rmux-sdk --example pane_options
cargo run -p rmux-sdk --example pane_state_events
```
