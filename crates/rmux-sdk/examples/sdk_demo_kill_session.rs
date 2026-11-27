//! Demonstrates the Session lifecycle: `exists()` + `kill()`.
//!
//! The fixture pipeline records the demo session in the background, so
//! killing *that* session would yank the recorder mid-capture. The
//! snippet shown to readers is the canonical pattern; the runtime
//! exercises the same calls on a throwaway companion session so the
//! recorded fixture stays intact.

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let (rmux, demo) = sdk_demo_helpers::demo_session("killdemo").await?;
    sdk_demo_helpers::paint_idle_prompt(&demo).await?;
    let session = sdk_demo_helpers::throwaway_session(&rmux, "killdemo-target").await?;

    // example:start
    let alive = session.exists().await?;
    println!("alive before kill: {alive}");
    session.kill().await?;
    // example:end

    Ok(())
}
