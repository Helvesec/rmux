use rmux_sdk::{EnsureSession, Rmux, SessionName};

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let rmux = Rmux::builder().connect_or_start().await?;
    let session = rmux
        .ensure_session(
            EnsureSession::try_named(SessionName::new("pane-options")?)?
                .create_or_reuse()
                .detached(true),
        )
        .await?;

    let pane = session.pane(0, 0);
    let mutation = pane.set_option("@agent.state", "waiting").await?;
    println!(
        "{}: {:?} -> {:?}",
        mutation.name, mutation.old_value, mutation.new_value
    );

    println!("@agent.state = {:?}", pane.option("@agent.state").await?);
    pane.unset_option("@agent.state").await?;
    Ok(())
}
