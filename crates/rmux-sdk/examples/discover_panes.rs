use rmux_sdk::Result;

mod terminal_example_fixture;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let fixture = terminal_example_fixture::ready_pane("discover-panes").await?;
    let panes = fixture
        .rmux
        .find_panes()
        .session(fixture.session.name().as_str())
        .running()
        .all()
        .await?;

    for pane in panes {
        println!(
            "{} {} {:?}",
            pane.session_name,
            pane.pane_id,
            pane.title.as_deref().unwrap_or("<untitled>")
        );
    }
    fixture.session.kill().await?;
    Ok(())
}
