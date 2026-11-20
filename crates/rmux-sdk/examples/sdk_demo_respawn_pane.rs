//! Demonstrates `Pane::respawn()` — restart the process inside a pane slot
//! while preserving the stable pane id.

use rmux_sdk::{PaneRespawnOptions, ProcessSpec};

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let (_rmux, session) = sdk_demo_helpers::demo_session("respawn").await?;
    sdk_demo_helpers::paint_idle_prompt(&session).await?;

    // example:start
    let pane = session.pane(0, 0);
    pane.respawn(PaneRespawnOptions {
        kill: true,
        start_directory: None,
        process: ProcessSpec::default(),
        keep_alive_on_exit: None,
    })
    .await?;
    // example:end

    sdk_demo_helpers::cleanup(session).await
}
