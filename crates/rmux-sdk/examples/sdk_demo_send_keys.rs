use rmux_sdk::Result;

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> Result<()> {
    let (_rmux, session) = sdk_demo_helpers::demo_session("keys").await?;
    sdk_demo_helpers::paint_uname(&session).await?;

    // example:start
    let pane = session.pane(0, 0);
    pane.send_text("uname -s").await?;
    pane.send_key("Enter").await?;
    pane.wait_for_text("Linux").await?;
    // example:end

    let _ = pane.snapshot().await?;
    sdk_demo_helpers::cleanup(session).await
}
