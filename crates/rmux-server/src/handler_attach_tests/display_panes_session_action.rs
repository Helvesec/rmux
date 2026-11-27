use super::*;

#[tokio::test]
async fn display_panes_exact_session_action_uses_only_the_requested_sessions_label() {
    let handler = RequestHandler::new();
    let alpha = session_name("action-alpha");
    let beta = session_name("action-beta");
    let mut alpha_rx = create_attached_session(&handler, 101, &alpha).await;
    let mut beta_rx = create_attached_session(&handler, 202, &beta).await;
    drain_attach_controls(&mut alpha_rx);
    drain_attach_controls(&mut beta_rx);
    let response = handler
        .dispatch(
            202,
            Request::DisplayPanes(Box::new(rmux_proto::DisplayPanesRequest {
                target: alpha.clone(),
                duration_ms: Some(60_000),
                non_blocking: true,
                no_command: false,
                template: Some("set-buffer -b exact-session-action '%%'".to_owned()),
                target_client: None,
            })),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::DisplayPanes(ref value)
        if value.target.session_name() == &alpha),
        "{response:?}"
    );
    let label = {
        let active = handler.active_attach.lock().await;
        assert!(active.by_pid[&202].display_panes.is_none());
        active.by_pid[&101].display_panes.as_ref().unwrap().labels[0]
            .label
            .clone()
    };
    while let Ok(control) = beta_rx.try_recv() {
        assert!(!matches!(control, AttachControl::Overlay(_)), "{control:?}");
    }
    handler
        .handle_attached_live_input_for_test(101, label.as_bytes())
        .await
        .expect("alpha label executes");
    let state = handler.state.lock().await;
    let (_, bytes) = state.buffers.show(Some("exact-session-action")).unwrap();
    assert!(String::from_utf8_lossy(bytes).starts_with(&format!("={alpha}:")));
}
