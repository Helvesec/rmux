# rmux-sdk

Public, daemon-backed Rust SDK for the [RMUX](https://github.com/helvesec/rmux) terminal multiplexer.

Drives sessions, windows, and panes through typed handles over the local
RMUX daemon — no TTY scraping, no string-formatted commands. Snapshots,
send-keys, wait-for-text, output streams, and `EnsureSession` bootstrap are
the primary primitives.

## Usage

```toml
[dependencies]
rmux-sdk = "0.2"
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
    let snapshot = pane.snapshot().await?;
    println!("{}x{}", snapshot.cols, snapshot.rows);
    Ok(())
}
```

## Surface

- `Rmux::builder().connect_or_start()` — reuse a running daemon or spawn one.
- `EnsureSession` — idempotent session bootstrap (named, create-or-reuse, detached, process spec).
- Typed handles: `Session`, `Window`, `Pane`.
- `Pane::send_text`, `Pane::wait_for_text`, `Pane::snapshot`, plus streams and events for incremental output.

The SDK is a peer of the local IPC client, not a wrapper — no dependency on internal RMUX runtime crates.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
