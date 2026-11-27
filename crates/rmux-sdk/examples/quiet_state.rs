use std::time::Duration;

use rmux_sdk::{Result, TerminalLoadState};

mod terminal_example_fixture;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let fixture = terminal_example_fixture::ready_pane("quiet-state").await?;
    let pane = fixture.pane.clone();

    pane.wait_for_load_state(TerminalLoadState::Quiet)
        .timeout(Duration::from_secs(10))
        .await?;
    pane.wait_until_stable_for(Duration::from_millis(500))
        .timeout(Duration::from_secs(10))
        .await?;
    fixture.session.kill().await?;
    Ok(())
}
