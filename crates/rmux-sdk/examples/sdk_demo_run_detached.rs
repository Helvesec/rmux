//! Demonstrates spawning a long-running app detached.
//!
//! The captured snapshot pretends the session is hosting a web
//! server that just booted — that's the visual the docs page
//! shows. The snippet itself is the canonical `ensure_session`
//! call that bootstraps the daemon-managed app and returns
//! immediately, leaving the process running in the background
//! ready to be picked up by `Reconnect to an app`.

use rmux_sdk::Result;

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> Result<()> {
    let (_rmux, session) = sdk_demo_helpers::demo_session("detach").await?;
    // First line carries the colored prompt so the visual reads
    // "you typed something in your shell and the python server
    // started right after"; subsequent rows are pure log output.
    // `\033[..m` codes are interpreted by `printf` (single-quoted on
    // the shell side); the captured pane preserves the colors.
    sdk_demo_helpers::paint_lines(
        &session,
        &[
            "\\033[36muser@rmuxio\\033[0m:\\033[32m~/workspace\\033[0m$ [INFO]  starting workers...",
            "[INFO]  listening on http://0.0.0.0:8080",
            "[READY] server up — Ctrl+B d to detach",
        ],
    )
    .await?;

    // example:start
    use rmux_sdk::{EnsureSession, Rmux};
    let rmux = Rmux::builder().connect_or_start().await?;
    let _session = rmux
        .ensure_session(
            EnsureSession::try_named("api-server")?
                .create_or_reuse()
                .detached(true)
                .argv(["python3", "-m", "http.server", "8080"]),
        )
        .await?;
    // The handle is dropped here. The daemon keeps the python server
    // alive in the background — the caller can move on and reconnect
    // to inspect output later.
    // example:end

    Ok(())
}
