use super::*;
use crate::pane_io::PaneOutputSender;

pub(super) async fn session_pane(handler: &RequestHandler, name: &str) -> (u32, PaneOutputSender) {
    let session_name = SessionName::new(name).expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    handler
        .control_session_panes(&session_name)
        .await
        .expect("session output exists")
        .into_iter()
        .next()
        .expect("session has a pane")
}

async fn assert_demand(sender: &PaneOutputSender, pane_id: u32, expected: bool, reason: &str) {
    let result = tokio::time::timeout(
        Duration::from_millis(50),
        sender.wait_for_reader_demand(pane_id, None),
    )
    .await;
    assert_eq!(result.ok(), expected.then_some(true), "{reason}");
}

#[tokio::test]
async fn global_off_paused_demand_survives_direct_actions_and_bulk_refresh() {
    let handler = Arc::new(RequestHandler::new());
    session_pane(&handler, "global-demand-a").await;
    let (pane_id, sender) = session_pane(&handler, "global-demand-b").await;
    let mut control = OpenControlTestClient::spawn(Arc::clone(&handler), 42_520).await;
    control.command("attach-session -t global-demand-a").await;
    control
        .command(format!("refresh-client -A '%{pane_id}:off'"))
        .await;
    assert_demand(
        &sender,
        pane_id,
        false,
        &format!("global OFF blocks reading: {}", control.rendered()),
    )
    .await;

    control
        .command(format!("refresh-client -A '%{pane_id}:pause'"))
        .await;
    assert_demand(&sender, pane_id, true, "global OFF+PAUSED permits reading").await;
    control.command("refresh-client").await;
    assert_demand(
        &sender,
        pane_id,
        true,
        "bulk refresh preserves paused demand",
    )
    .await;
    sender.send(b"GLOBAL_HIDDEN\r\n".to_vec());
    control.yield_and_drain_available().await;
    assert!(!control.rendered().contains("GLOBAL_HIDDEN"));

    control
        .command(format!("refresh-client -A '%{pane_id}:on'"))
        .await;
    assert_demand(&sender, pane_id, true, "ON leaves PAUSED readable").await;
    control
        .command(format!("refresh-client -A '%{pane_id}:off'"))
        .await;
    assert_demand(&sender, pane_id, true, "OFF leaves PAUSED readable").await;
    control
        .command(format!("refresh-client -A '%{pane_id}:continue'"))
        .await;
    assert_demand(&sender, pane_id, false, "CONTINUE leaves OFF blocking").await;
    let _ = control.close().await;
    assert_demand(
        &sender,
        pane_id,
        true,
        "last detach restores headless demand",
    )
    .await;
}

#[tokio::test]
async fn off_paused_demand_survives_switch_away_and_back() {
    let handler = Arc::new(RequestHandler::new());
    let (pane_id, sender) = session_pane(&handler, "switch-demand-a").await;
    session_pane(&handler, "switch-demand-b").await;
    let mut control = OpenControlTestClient::spawn(Arc::clone(&handler), 42_521).await;
    control.command("attach-session -t switch-demand-a").await;
    control
        .command(format!(
            "refresh-client -A '%{pane_id}:off' -A '%{pane_id}:pause'"
        ))
        .await;
    assert_demand(&sender, pane_id, true, "local OFF+PAUSED permits reading").await;
    control.command("switch-client -t switch-demand-b").await;
    assert_demand(
        &sender,
        pane_id,
        true,
        "switch-away must retain global paused demand",
    )
    .await;
    control.command("refresh-client").await;
    assert_demand(
        &sender,
        pane_id,
        true,
        "bulk replacement must exclude paused OFF panes",
    )
    .await;
    control.command("switch-client -t switch-demand-a").await;
    sender.send(b"LOCAL_HIDDEN\r\n".to_vec());
    control.yield_and_drain_available().await;
    assert!(
        !control.rendered().contains("LOCAL_HIDDEN"),
        "{}",
        control.rendered()
    );
    control
        .command(format!("refresh-client -A '%{pane_id}:continue'"))
        .await;
    assert_demand(
        &sender,
        pane_id,
        false,
        "CONTINUE must retain OFF after switching",
    )
    .await;
    control
        .command(format!("refresh-client -A '%{pane_id}:on'"))
        .await;
    assert_demand(&sender, pane_id, true, "ON restores local reading").await;
    let _ = control.close().await;
}
