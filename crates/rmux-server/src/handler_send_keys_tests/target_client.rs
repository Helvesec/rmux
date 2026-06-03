use super::*;

#[tokio::test]
async fn send_keys_target_client_uses_that_clients_current_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler.register_attach(77, alpha.clone(), control_tx).await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "target-client", 1).await;

    let response = handler
        .handle(Request::SendKeysExt2(rmux_proto::SendKeysExt2Request {
            target: None,
            keys: vec!["A".to_owned()],
            expand_formats: false,
            hex: false,
            literal: true,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
            target_client: Some("77".to_owned()),
        }))
        .await;

    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );
    capture.assert_contents(&handler, b"A").await;
}

#[tokio::test]
async fn send_keys_missing_target_client_is_successful_noop() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_send_keys_test_session(&handler, &alpha).await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "target-client-missing", 0).await;

    let response = handler
        .handle(Request::SendKeysExt2(rmux_proto::SendKeysExt2Request {
            target: Some(PaneTarget::new(alpha, 0)),
            keys: vec!["A".to_owned()],
            expand_formats: false,
            hex: false,
            literal: true,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
            target_client: Some("999999".to_owned()),
        }))
        .await;

    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 0 })
    );
    capture.assert_contents(&handler, b"").await;
}

#[tokio::test]
async fn send_keys_target_client_key_dispatch_uses_target_client_context() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_send_keys_test_session(&handler, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler.register_attach(77, alpha.clone(), control_tx).await;

    let bound = handler
        .handle(Request::BindKey(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "x".to_owned(),
            note: Some("target-client-context".to_owned()),
            repeat: false,
            command: Some(vec![
                "switch-client".to_owned(),
                "-t".to_owned(),
                beta.to_string(),
            ]),
        }))
        .await;
    assert!(matches!(bound, Response::BindKey(_)));

    let response = handler
        .handle(Request::SendKeysExt2(rmux_proto::SendKeysExt2Request {
            target: None,
            keys: vec!["C-b".to_owned(), "x".to_owned()],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: true,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
            target_client: Some("77".to_owned()),
        }))
        .await;

    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&77)
        .expect("target attached client remains registered");
    assert_eq!(active.session_name, beta);
}
