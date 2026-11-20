use std::time::Duration;

use rmux_sdk::{PaneSet, PaneSetVisibleTextOutcome, Result, RmuxError, TerminalLoadState};

mod terminal_example_fixture;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let fixture = terminal_example_fixture::ready_pane("terminal-playwright").await?;
    let panes = PaneSet::new(vec![fixture.pane.clone()]);

    require_all_matched(
        panes.expect_all().visible_text_contains("Ready").await,
        "Ready",
    )?;
    panes
        .keyboard()
        .type_text(&terminal_example_fixture::print_command("multiplexer"))
        .await?;

    require_any_matched(
        panes
            .expect_any()
            .visible_text_contains("multiplexer")
            .timeout(Duration::from_secs(20))
            .await,
        "multiplexer",
    )?;

    let pane = fixture.pane.clone();
    pane.get_by_text("Ready").first().wait_for().await?;
    pane.wait_for_load_state(TerminalLoadState::Quiet).await?;
    fixture.session.kill().await?;
    Ok(())
}

fn require_all_matched(outcome: PaneSetVisibleTextOutcome, label: &str) -> Result<()> {
    let Some(batch) = outcome.all() else {
        return Err(protocol_error("expected an all-panes outcome"));
    };
    if batch.is_success() {
        Ok(())
    } else {
        Err(protocol_error(format!(
            "not all panes matched {label:?}; {} panes failed",
            batch.failures().len()
        )))
    }
}

fn require_any_matched(outcome: PaneSetVisibleTextOutcome, label: &str) -> Result<()> {
    let Some(any) = outcome.any() else {
        return Err(protocol_error("expected an any-pane outcome"));
    };
    if any.matched() {
        Ok(())
    } else {
        Err(protocol_error(format!(
            "no pane matched {label:?}; {} panes failed",
            any.failures().len()
        )))
    }
}

fn protocol_error(message: impl Into<String>) -> RmuxError {
    RmuxError::unsupported("example.paneset_expectation", message.into())
}
