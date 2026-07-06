use rmux_sdk::{EnsureSession, PaneStateEvent, PaneStateEventsOptions, Rmux, SessionName};

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let rmux = Rmux::builder().connect_or_start().await?;
    let session = rmux
        .ensure_session(
            EnsureSession::try_named(SessionName::new("pane-state-events")?)?
                .create_or_reuse()
                .detached(true),
        )
        .await?;

    let pane = session.pane(0, 0);
    let mut stream = pane
        .state_events(PaneStateEventsOptions {
            include_foreground: true,
            ..PaneStateEventsOptions::default()
        })
        .await?;

    if let Some(PaneStateEvent::Snapshot {
        title, foreground, ..
    }) = stream.next().await?
    {
        println!("initial title={title:?} foreground={foreground:?}");
    }

    pane.set_option("@agent.state", "running").await?;
    if let Some(event) = stream.next().await? {
        println!("next event={event:?}");
    }

    Ok(())
}
