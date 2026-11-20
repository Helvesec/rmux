use std::time::Duration;

use rmux_sdk::Result;

mod terminal_example_fixture;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let fixture = terminal_example_fixture::ready_pane("assert-visible-text").await?;
    let pane = fixture.pane.clone();

    pane.get_by_text("Ready")
        .first()
        .expect()
        .to_be_visible()
        .timeout(Duration::from_secs(5))
        .await?;
    pane.expect_visible_text()
        .to_contain("Ready")
        .timeout(Duration::from_secs(5))
        .await?;
    fixture.session.kill().await?;
    Ok(())
}
