//! Demonstrates `Pane::output_stream()` — a live stream of pane output
//! chunks driven by `next().await`.
//!
//! The stream terminates when the pane's shell exits, so the snippet asks
//! the shell to `exit`. That would yank the recorder mid-capture if we ran
//! it on the demo session, so the runtime uses a throwaway companion
//! session while the demo session continues to be recorded.

use rmux_sdk::PaneOutputChunk;

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let (rmux, demo) = sdk_demo_helpers::demo_session("outstr").await?;
    sdk_demo_helpers::paint_idle_prompt(&demo).await?;
    let session = sdk_demo_helpers::throwaway_session(&rmux, "outstr-x").await?;

    // example:start
    let pane = session.pane(0, 0);
    let mut stream = pane.output_stream().await?;
    pane.send_text("printf 'A\\nB\\nC\\n'; exit\n").await?;
    while let Some(chunk) = stream.next().await? {
        if let PaneOutputChunk::Bytes { bytes, .. } = chunk {
            println!("chunk: {} bytes", bytes.len());
        }
    }
    // example:end

    Ok(())
}
