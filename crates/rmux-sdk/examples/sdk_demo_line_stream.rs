//! Demonstrates `Pane::line_stream()` — same as `output_stream` but yields
//! complete rendered lines instead of raw byte chunks.
//!
//! The stream terminates when the pane's shell exits, so the snippet asks
//! the shell to `exit`. That would yank the recorder mid-capture if we ran
//! it on the demo session, so the runtime uses a throwaway companion
//! session while the demo session continues to be recorded.

use rmux_sdk::PaneLineItem;

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let (rmux, demo) = sdk_demo_helpers::demo_session("linestr").await?;
    sdk_demo_helpers::paint_idle_prompt(&demo).await?;
    let session = sdk_demo_helpers::throwaway_session(&rmux, "linestr-x").await?;

    // example:start
    let pane = session.pane(0, 0);
    let mut lines = pane.line_stream().await?;
    pane.send_text("printf 'one\\ntwo\\nthree\\n'; exit\n")
        .await?;
    while let Some(item) = lines.next().await? {
        if let PaneLineItem::Line { text } = item {
            println!("line: {text:?}");
        }
    }
    // example:end

    Ok(())
}
