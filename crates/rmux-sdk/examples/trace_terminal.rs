use rmux_sdk::Result;

mod terminal_example_fixture;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let fixture = terminal_example_fixture::ready_pane("trace-terminal").await?;
    let pane = fixture.pane.clone();
    let trace = fixture.rmux.tracing().start().await?;
    let command = terminal_example_fixture::print_command("trace multiplexer");

    trace.record_action("ask terminal")?;
    trace.record_input(&pane, &command)?;
    pane.keyboard().type_text(&command).await?;
    trace.record_snapshot(&pane).await?;

    let path = trace.stop("rmux-trace").await?;
    println!("{}", path.display());
    fixture.session.kill().await?;
    Ok(())
}
