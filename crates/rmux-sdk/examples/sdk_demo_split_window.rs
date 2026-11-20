use rmux_sdk::SplitDirection;

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let (_rmux, session) = sdk_demo_helpers::demo_session("split").await?;
    sdk_demo_helpers::paint_idle_prompt(&session).await?;

    // example:start
    let pane = session.pane(0, 0);
    let new_pane = pane.split(SplitDirection::Right).await?;
    // example:end

    let panes = session.window(0).panes().await?;
    println!("split pane: {}", new_pane.target());
    println!("visible panes: {}", panes.len());
    sdk_demo_helpers::cleanup(session).await
}
