//! Demonstrates `Pane::wait_for_text_next()` — arm a wait *before* the
//! producing action so there is no race window where the text could appear
//! before we start watching.

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let (_rmux, session) = sdk_demo_helpers::demo_session("waitnext").await?;

    // example:start
    let pane = session.pane(0, 0);
    let armed = pane.wait_for_text_next("DONE").await?;
    pane.send_text("printf 'DONE\\n'\n").await?;
    armed.await?;
    // example:end

    sdk_demo_helpers::cleanup(session).await
}
