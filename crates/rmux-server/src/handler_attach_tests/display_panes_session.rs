use super::*;

fn request(session: SessionName, target_client: Option<String>) -> Request {
    Request::DisplayPanes(Box::new(rmux_proto::DisplayPanesRequest {
        target: session,
        duration_ms: Some(60_000),
        non_blocking: true,
        no_command: true,
        template: None,
        target_client,
    }))
}

fn assert_no_overlay(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) {
    while let Ok(control) = control_rx.try_recv() {
        assert!(!matches!(control, AttachControl::Overlay(_)), "{control:?}");
    }
}

#[tokio::test]
async fn display_panes_exact_session_rpc_never_arms_the_requesters_other_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("exact-alpha");
    let beta = session_name("exact-beta");
    let mut alpha_rx = create_attached_session(&handler, 101, &alpha).await;
    let mut beta_rx = create_attached_session(&handler, 202, &beta).await;
    drain_attach_controls(&mut alpha_rx);
    drain_attach_controls(&mut beta_rx);

    let response = handler
        .dispatch(202, request(alpha.clone(), None))
        .await
        .response;
    assert!(
        matches!(response, Response::DisplayPanes(ref value)
        if value.target.session_name() == &alpha),
        "{response:?}"
    );
    let active = handler.active_attach.lock().await;
    assert!(active.by_pid[&101].display_panes.is_some());
    assert!(active.by_pid[&202].display_panes.is_none());
    drop(active);
    assert_no_overlay(&mut beta_rx);
    let _ = recv_overlay_frame(&mut alpha_rx, "exact alpha overlay").await;
}

#[tokio::test]
async fn display_panes_exact_session_rpc_without_its_client_fails_before_mutation() {
    let handler = RequestHandler::new();
    let alpha = session_name("no-client-alpha");
    let beta = session_name("no-client-beta");
    let alpha_rx = create_attached_session(&handler, 101, &alpha).await;
    handler.active_attach.lock().await.by_pid.remove(&101);
    drop(alpha_rx);
    let mut beta_rx = create_attached_session(&handler, 202, &beta).await;
    drain_attach_controls(&mut beta_rx);

    let response = handler.dispatch(202, request(alpha, None)).await.response;
    assert!(matches!(response, Response::Error(_)), "{response:?}");
    assert!(handler.active_attach.lock().await.by_pid[&202]
        .display_panes
        .is_none());
    assert_no_overlay(&mut beta_rx);
}

#[tokio::test]
async fn display_panes_exact_session_rpc_rejects_a_discordant_explicit_client() {
    let handler = RequestHandler::new();
    let alpha = session_name("discordant-alpha");
    let beta = session_name("discordant-beta");
    let mut alpha_rx = create_attached_session(&handler, 101, &alpha).await;
    let mut beta_rx = create_attached_session(&handler, 202, &beta).await;
    drain_attach_controls(&mut alpha_rx);
    drain_attach_controls(&mut beta_rx);

    let response = handler
        .dispatch(202, request(alpha, Some("202".to_owned())))
        .await
        .response;
    assert!(matches!(response, Response::Error(_)), "{response:?}");
    let active = handler.active_attach.lock().await;
    assert!(active
        .by_pid
        .values()
        .all(|client| client.display_panes.is_none()));
    drop(active);
    assert_no_overlay(&mut alpha_rx);
    assert_no_overlay(&mut beta_rx);
}
