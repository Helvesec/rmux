//! Demonstrates `Pane::wait_exit()` — block until the pane's process exits.
//!
//! The fixture pipeline records the demo session in the background; if the
//! snippet exits its sole pane the recorder loses its source mid-capture.
//! The snippet shown to readers is the canonical pattern; the runtime
//! exercises the same calls on a throwaway companion session so the
//! recorded fixture stays intact.

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let (rmux, demo) = sdk_demo_helpers::demo_session("waitexit").await?;
    sdk_demo_helpers::paint_idle_prompt(&demo).await?;
    let session = sdk_demo_helpers::throwaway_session(&rmux, "waitexit-target").await?;

    // example:start
    let pane = session.pane(0, 0);
    pane.send_text("exit\n").await?;
    let state = pane.wait_exit().await?;
    println!("exit state: {state:?}");
    // example:end

    Ok(())
}
