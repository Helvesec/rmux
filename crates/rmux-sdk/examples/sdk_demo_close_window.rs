//! Demonstrates the Window-handle lifecycle: `exists()` + `close()`.
//!
//! The daemon refuses to remove a session's *only* window — it would
//! orphan the session — so this scenario also shows the typed error you
//! get in that case, which is the contract a real caller needs to handle.

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let (_rmux, session) = sdk_demo_helpers::demo_session("closewin").await?;
    sdk_demo_helpers::paint_idle_prompt(&session).await?;

    // example:start
    let window = session.window(0);
    let alive = window.exists().await?;
    println!("alive: {alive}");
    match window.close().await {
        Ok(outcome) => println!("closed: {outcome:?}"),
        Err(error) => println!("close refused: {error}"),
    }
    // example:end

    sdk_demo_helpers::cleanup(session).await
}
