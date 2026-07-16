use super::*;

async fn register_control_for_session(
    handler: &RequestHandler,
    requester_pid: u32,
    session_name: &SessionName,
) -> mpsc::Receiver<crate::control::ControlServerEvent> {
    let (event_tx, event_rx) = mpsc::channel(8);
    handler
        .register_control_with_closing(
            requester_pid,
            crate::control_mode::ControlModeUpgrade {
                mode: rmux_proto::ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
                initial_command_count: 0,
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(requester_pid, Some(session_name.clone()))
        .await
        .expect("control client attaches to the session");
    event_rx
}

async fn set_attached_count_status(handler: &RequestHandler, session_name: &SessionName) {
    for (option, value) in [
        (OptionName::StatusLeft, "attached=#{session_attached}"),
        (OptionName::StatusRight, ""),
    ] {
        let response = handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(session_name.clone()),
                option,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await;
        assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    }
}

#[tokio::test]
async fn identity_refresh_counts_attach_and_control_clients() {
    let handler = RequestHandler::new();
    let session_name = session_name("identity-refresh-attached-count");
    create_quiet_session(&handler, &session_name).await;
    set_attached_count_status(&handler, &session_name).await;

    let (attach_tx, mut attach_rx) = mpsc::unbounded_channel();
    let attach_pid = 91_701;
    handler
        .register_attach(attach_pid, session_name.clone(), attach_tx)
        .await;
    let _control_rx = register_control_for_session(&handler, 91_702, &session_name).await;

    let session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session_name)
        .expect("session survives")
        .id();
    let identity = handler.active_attach_identity_for_test(attach_pid).await;
    assert!(
        handler
            .refresh_attached_client_base_for_session_identity(identity, &session_name, session_id,)
            .await
    );

    let target = recv_switch_target(&mut attach_rx, "identity-aware attached count").await;
    let frame = String::from_utf8(target.render_frame).expect("render frame is utf-8");
    assert!(frame.contains("attached=2"), "render frame: {frame:?}");
}

#[tokio::test]
async fn identity_attached_count_rejects_a_reused_session_name() {
    let handler = RequestHandler::new();
    let original_name = session_name("identity-count-reused");
    let renamed = session_name("identity-count-renamed");
    create_quiet_session(&handler, &original_name).await;

    let (old_attach_tx, _old_attach_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(91_711, original_name.clone(), old_attach_tx)
        .await;
    let _old_control_rx = register_control_for_session(&handler, 91_712, &original_name).await;
    let original_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&original_name)
        .expect("original session exists")
        .id();

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: original_name.clone(),
            new_name: renamed.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );

    create_quiet_session(&handler, &original_name).await;
    let (new_attach_tx, _new_attach_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(91_713, original_name.clone(), new_attach_tx)
        .await;
    let _new_control_rx = register_control_for_session(&handler, 91_714, &original_name).await;
    let replacement_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&original_name)
        .expect("replacement session exists")
        .id();
    assert_ne!(replacement_id, original_id);

    assert_eq!(
        handler
            .attached_count_for_session_identity(&original_name, replacement_id)
            .await,
        2
    );
    assert_eq!(
        handler
            .attached_count_for_session_identity(&renamed, original_id)
            .await,
        2
    );
    assert_eq!(
        handler
            .attached_count_for_session_identity(&original_name, original_id)
            .await,
        0,
        "a reused name must not make clients from the replacement count for the old identity"
    );
}
