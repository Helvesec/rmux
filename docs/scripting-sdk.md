# RMUX SDK

RMUX 0.9.0 ships a daemon-backed Rust SDK for terminal automation. The SDK talks
to the local RMUX daemon through the typed IPC contract; it is not a CLI parser
or a tmux control-mode wrapper.

Use the CLI for interactive tmux-compatible workflows, and use `rmux-sdk` when
code is the user: create or reuse sessions, address panes by handle, send input,
wait for rendered text, capture snapshots, inspect locators, stream output, and
start browser shares.

## Install

```toml
[dependencies]
rmux-sdk = "0.9.0"
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

SDK clients should call `rmux capabilities --json` or use
`Rmux::capabilities()` to negotiate daemon features. `rmux diagnose --json`
reports build, platform, and runtime support details for debugging.

Pane-local SDK metadata and state streams require the daemon capabilities
`sdk.pane.options`, `sdk.pane.state_events`, and, when foreground process data
is requested, `sdk.pane.foreground`. The foreground contract is best-effort:
Unix reports the foreground process group when available, while Windows reports
the ConPTY root process plus OSC7/process/profile cwd fallbacks without trying
to classify agent names.

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
