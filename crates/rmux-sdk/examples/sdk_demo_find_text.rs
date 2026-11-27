//! Demonstrates `Pane::find_text()` — locate a literal in the visible buffer.

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let (_rmux, session) = sdk_demo_helpers::demo_session("find").await?;
    sdk_demo_helpers::paint_idle_prompt(&session).await?;

    // example:start
    let pane = session.pane(0, 0);
    if let Some(m) = pane.find_text("workspace").await? {
        println!("found at row={} col={}", m.start_row, m.start_col);
    }
    // example:end

    sdk_demo_helpers::cleanup(session).await
}
