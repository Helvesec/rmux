//! Demonstrates picking up an existing session by name.
//!
//! Pairs with `Run an app detached`. After leaving an app running in
//! a daemon-managed session, a caller uses `Rmux::session(name)` to
//! grab a handle and verifies the session is still alive with
//! `Session::exists()`. Reading the live state via `Pane::snapshot()`
//! belongs to the dedicated "Snapshot pane" example — keeping it out
//! of this snippet avoids duplicating the same pattern across docs.

use rmux_sdk::Result;

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> Result<()> {
    let (rmux, _demo) = sdk_demo_helpers::demo_session("reconnect").await?;
    // Pre-create the session the snippet will reconnect to so the
    // example is rejouable on its own (not dependent on `run-detached`
    // running first in the daemon).
    let _api_server = sdk_demo_helpers::throwaway_session(&rmux, "api-server").await?;

    // example:start
    use rmux_sdk::{Rmux, SessionName};
    let rmux = Rmux::builder().connect_or_start().await?;
    let session = rmux
        .session(SessionName::new("api-server").expect("valid static session name"))
        .await?;
    assert!(session.exists().await?);
    // example:end

    Ok(())
}
