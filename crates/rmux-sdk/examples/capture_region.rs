use rmux_sdk::{Rect, Result};

mod terminal_example_fixture;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let fixture = terminal_example_fixture::ready_pane("capture-region").await?;
    let pane = fixture.pane.clone();

    let prompt = pane
        .get_by_text("Ready")
        .first()
        .capture()
        .preserve_style(true)
        .await?;
    println!("{}", prompt.text);

    let top_left = pane.capture_region(Rect::new(0, 0, 5, 80)).await?;
    println!("{}", top_left.text);
    fixture.session.kill().await?;
    Ok(())
}
