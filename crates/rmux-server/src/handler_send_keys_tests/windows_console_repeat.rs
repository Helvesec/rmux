use super::*;

fn ctrl_semicolon_keystroke() -> rmux_proto::AttachedKeystroke {
    rmux_proto::AttachedKeystroke::new(b";".to_vec()).with_windows_console_key(
        rmux_proto::AttachedWindowsConsoleKey::new(0xba, 0x27, b';' as u16, 0x0008, 1),
    )
}

async fn dispatch_ctrl_semicolon(
    handler: &RequestHandler,
    requester_pid: u32,
    pending_input: &mut Vec<u8>,
) -> bool {
    handler
        .handle_attached_keystroke_input(requester_pid, pending_input, &ctrl_semicolon_keystroke())
        .await
        .expect("Ctrl+; attached input succeeds")
}

#[tokio::test]
async fn repeated_ctrl_semicolon_dispatches_each_root_binding() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "C-;".to_owned(),
            note: Some("repeated-windows-ctrl-semicolon-root".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "R".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));

    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "repeat-windows-c-semicolon-root", 3).await;
    let mut pending_input = Vec::new();
    for _ in 0..3 {
        assert!(!dispatch_ctrl_semicolon(&handler, requester_pid, &mut pending_input).await);
        assert!(pending_input.is_empty());
    }

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"RRR").await;
}

#[tokio::test]
async fn repeated_ctrl_semicolon_preserves_prefix_table_semantics() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let set_prefix = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Prefix,
            value: "C-;".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_prefix, Response::SetOption(_)));
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "C-;".to_owned(),
            note: Some("repeated-windows-ctrl-semicolon-prefix".to_owned()),
            repeat: false,
            command: Some(vec![
                "send-keys".to_owned(),
                "-l".to_owned(),
                "P".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)));
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "repeat-windows-c-semicolon-prefix", 1).await;
    let mut pending_input = Vec::new();
    assert!(!dispatch_ctrl_semicolon(&handler, requester_pid, &mut pending_input).await);
    assert!(!dispatch_ctrl_semicolon(&handler, requester_pid, &mut pending_input).await);
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"P").await;
}

#[tokio::test]
async fn repeated_unbound_ctrl_semicolon_forwards_each_original_byte() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "repeat-windows-c-semicolon-forward", 3).await;

    let mut pending_input = Vec::new();
    for _ in 0..3 {
        assert!(dispatch_ctrl_semicolon(&handler, requester_pid, &mut pending_input).await);
        assert!(pending_input.is_empty());
    }

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b";;;").await;
}
