//! Demonstrates `Rmux::list_sessions()` + `Rmux::has_session()`.
//!
//! The snippet enumerates session names and checks for one by name.
//! The capture pipeline records the demo session's pane state, so
//! the snippet must run against a separate throwaway session (so the
//! demo session's snapshot remains the canonical thing the docs
//! show).

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let (rmux, demo) = sdk_demo_helpers::demo_session("listsess").await?;
    sdk_demo_helpers::paint_idle_prompt(&demo).await?;
    // A second session so `list_sessions` returns more than one name
    // (otherwise the demo would show a list with a single entry, which
    // doesn't communicate the API's intent).
    let _ = sdk_demo_helpers::throwaway_session(&rmux, "listsess-x").await?;

    // example:start
    let names = rmux.list_sessions().await?;
    for name in &names {
        println!("session: {}", name.as_str());
    }
    let alive = rmux
        .has_session(rmux_sdk::SessionName::new("listsess").expect("valid static session name"))
        .await?;
    println!("listsess alive: {alive}");
    // example:end

    Ok(())
}
