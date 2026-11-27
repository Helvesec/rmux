use super::*;
use crate::pane_io::AttachControl;

async fn register_delegated_attach(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &rmux_proto::SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    create_send_keys_test_session(handler, session).await;
    register_existing_delegated_attach(handler, requester_pid, session).await
}

async fn register_existing_delegated_attach(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &rmux_proto::SessionName,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    let mut active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get_mut(&requester_pid)
        .expect("delegated attach is active");
    active.can_write = false;
    active.flags = active.flags.with_read_only();
    control_rx
}

async fn bind_read_only_key(
    handler: &RequestHandler,
    table_name: &str,
    key: &str,
    command: &[&str],
) {
    let response = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: table_name.to_owned(),
            key: key.to_owned(),
            note: Some("read-only client action test".to_owned()),
            repeat: false,
            command: Some(
                command
                    .iter()
                    .map(|argument| (*argument).to_owned())
                    .collect(),
            ),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)), "{response:?}");
}

async fn active_session_name(
    handler: &RequestHandler,
    requester_pid: u32,
) -> rmux_proto::SessionName {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&requester_pid)
        .expect("delegated attach remains active")
        .session_name
        .clone()
}

async fn active_key_table(handler: &RequestHandler, requester_pid: u32) -> Option<String> {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&requester_pid)
        .expect("delegated attach remains active")
        .key_table_name
        .clone()
}

async fn recv_detach(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(control) = control_rx.recv().await {
            if matches!(control, AttachControl::Detach) {
                return;
            }
        }
        panic!("attach control channel closed before detach");
    })
    .await
    .expect("delegated detach arrives");
}

fn assert_no_detach(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) {
    while let Ok(control) = control_rx.try_recv() {
        assert!(
            !matches!(control, AttachControl::Detach),
            "unauthorized input must not detach the client"
        );
    }
}

fn assert_no_detach_variant(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) {
    while let Ok(control) = control_rx.try_recv() {
        assert!(
            !matches!(
                control,
                AttachControl::Detach
                    | AttachControl::DetachKill
                    | AttachControl::DetachExecShellCommand(_)
            ),
            "unauthorized input must not terminate or replace a client"
        );
    }
}

#[tokio::test]
async fn delegated_read_only_attach_can_switch_its_own_session_from_prefix_binding() {
    let handler = RequestHandler::new();
    let alpha = session_name("delegated-read-only-switch-prefix-alpha");
    let beta = session_name("delegated-read-only-switch-prefix-beta");
    let requester_pid = std::process::id();
    let mut control_rx = register_delegated_attach(&handler, requester_pid, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;
    {
        let mut state = handler.state.lock().await;
        state.environment.set(
            ScopeSelector::Session(beta.clone()),
            "DISPLAY".to_owned(),
            "read-only-target-sentinel".to_owned(),
        );
    }
    bind_read_only_key(
        &handler,
        "prefix",
        "s",
        &["switch-client", "-t", beta.as_str()],
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02s")
        .await
        .expect("read-only prefix switch input");

    assert_eq!(active_session_name(&handler, requester_pid).await, beta);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .session_value(&beta, "DISPLAY"),
        Some("read-only-target-sentinel"),
        "read-only navigation must force the no-environment-update form"
    );
    assert_no_detach_variant(&mut control_rx);
}

#[tokio::test]
async fn delegated_read_only_attach_can_switch_its_own_session_from_root_binding() {
    let handler = RequestHandler::new();
    let alpha = session_name("delegated-read-only-switch-root-alpha");
    let beta = session_name("delegated-read-only-switch-root-beta");
    let requester_pid = std::process::id();
    let mut control_rx = register_delegated_attach(&handler, requester_pid, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;
    bind_read_only_key(
        &handler,
        "root",
        "s",
        &["switch-client", "-E", "-t", beta.as_str()],
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"s")
        .await
        .expect("read-only root switch input");

    assert_eq!(active_session_name(&handler, requester_pid).await, beta);
    assert_no_detach_variant(&mut control_rx);
}

#[tokio::test]
async fn delegated_read_only_attach_can_enter_and_use_a_custom_safe_table() {
    let handler = RequestHandler::new();
    let alpha = session_name("delegated-read-only-switch-table-alpha");
    let beta = session_name("delegated-read-only-switch-table-beta");
    let requester_pid = std::process::id();
    let mut control_rx = register_delegated_attach(&handler, requester_pid, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;
    bind_read_only_key(
        &handler,
        "root",
        "q",
        &["switch-client", "-T", "readonly-custom"],
    )
    .await;
    bind_read_only_key(
        &handler,
        "readonly-custom",
        "s",
        &["switch-client", "-t", beta.as_str()],
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"q")
        .await
        .expect("read-only key-table switch input");
    assert_eq!(
        active_key_table(&handler, requester_pid).await.as_deref(),
        Some("readonly-custom")
    );
    assert_eq!(active_session_name(&handler, requester_pid).await, alpha);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"s")
        .await
        .expect("read-only custom-table navigation input");
    assert_eq!(active_session_name(&handler, requester_pid).await, beta);
    assert_eq!(active_key_table(&handler, requester_pid).await, None);
    assert_no_detach_variant(&mut control_rx);
}

#[tokio::test]
async fn delegated_read_only_attach_allows_safe_session_order_navigation_repeatedly() {
    for (label, command) in [
        ("last", vec!["switch-client", "-l"]),
        ("next", vec!["switch-client", "-n"]),
        ("previous", vec!["switch-client", "-p"]),
        ("next-order", vec!["switch-client", "-En", "-O", "name"]),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name(&format!("delegated-read-only-{label}-alpha"));
        let beta = session_name(&format!("delegated-read-only-{label}-beta"));
        let requester_pid = std::process::id();
        let mut control_rx = register_delegated_attach(&handler, requester_pid, &alpha).await;
        let forwarder_identity = handler.active_attach_identity_for_test(requester_pid).await;
        let mut pending_input = Vec::new();
        create_send_keys_test_session(&handler, &beta).await;
        if label == "last" {
            let beta_id = handler
                .state
                .lock()
                .await
                .sessions
                .session(&beta)
                .expect("beta exists")
                .id();
            let mut active_attach = handler.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&requester_pid)
                .expect("delegated attach remains active");
            active.last_session = Some(beta.clone());
            active.last_session_id = Some(beta_id);
        }
        bind_read_only_key(&handler, "root", "h", &command).await;

        handler
            .handle_attached_live_input_inner_for_identity(
                forwarder_identity,
                &mut pending_input,
                b"h",
            )
            .await
            .expect("first read-only ordered switch");
        assert_eq!(
            active_session_name(&handler, requester_pid).await,
            beta,
            "{label} first application"
        );
        handler
            .handle_attached_live_input_inner_for_identity(
                forwarder_identity,
                &mut pending_input,
                b"h",
            )
            .await
            .expect("repeated read-only ordered switch");
        assert_eq!(
            active_session_name(&handler, requester_pid).await,
            alpha,
            "{label} repeated application"
        );
        assert_no_detach_variant(&mut control_rx);
    }
}

#[tokio::test]
async fn delegated_read_only_attach_safe_noop_and_invalid_target_stay_local() {
    let handler = RequestHandler::new();
    let alpha = session_name("delegated-read-only-switch-noop");
    let requester_pid = std::process::id();
    let mut control_rx = register_delegated_attach(&handler, requester_pid, &alpha).await;
    bind_read_only_key(
        &handler,
        "root",
        "n",
        &["switch-client", "-t", alpha.as_str()],
    )
    .await;
    bind_read_only_key(
        &handler,
        "root",
        "i",
        &["switch-client", "-t", "does-not-exist"],
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"n")
        .await
        .expect("read-only no-op switch");
    assert_eq!(active_session_name(&handler, requester_pid).await, alpha);
    handler
        .handle_attached_live_input_for_test(requester_pid, b"i")
        .await
        .expect("read-only invalid-target switch");
    assert_eq!(active_session_name(&handler, requester_pid).await, alpha);
    assert!(
        handler
            .state
            .lock()
            .await
            .message_log
            .back()
            .is_some_and(|entry| entry.msg.contains("does-not-exist")),
        "an admitted invalid target must report its local command error"
    );
    assert_no_detach_variant(&mut control_rx);
}

#[tokio::test]
async fn delegated_read_only_attach_can_detach_with_split_prefix_binding() {
    let handler = RequestHandler::new();
    let alpha = session_name("delegated-read-only-detach");
    let requester_pid = std::process::id();
    let mut control_rx = register_delegated_attach(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("read-only prefix input");
    assert_no_detach(&mut control_rx);
    handler
        .handle_attached_live_input_for_test(requester_pid, b"d")
        .await
        .expect("read-only detach input");

    recv_detach(&mut control_rx).await;
}

#[tokio::test]
async fn delegated_read_only_attach_still_rejects_content_and_mutating_bindings() {
    let handler = RequestHandler::new();
    let alpha = session_name("delegated-read-only-denied");
    let requester_pid = std::process::id();
    let mut control_rx = register_delegated_attach(&handler, requester_pid, &alpha).await;
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "delegated-read-only-content", 0).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"content")
        .await
        .expect("read-only content input");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("read-only prefix input");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"c")
        .await
        .expect("read-only new-window binding");

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"").await;
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&alpha)
            .expect("session remains")
            .windows()
            .count(),
        1,
        "read-only prefix c must not create a window"
    );
    assert_no_detach(&mut control_rx);
}

// tmux 3.7b executes both commands in this binding for an `attach-session -r`
// client. RMUX delegated access deliberately grants only the local detach
// capability, never the mutation chained after it.
#[tokio::test]
async fn delegated_read_only_attach_rejects_chained_detach_binding_product_divergence() {
    let handler = RequestHandler::new();
    let alpha = session_name("delegated-read-only-chained");
    let requester_pid = std::process::id();
    let mut control_rx = register_delegated_attach(&handler, requester_pid, &alpha).await;
    let rebound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "d".to_owned(),
            note: Some("delegated detach must remain local".to_owned()),
            repeat: false,
            command: Some(vec![
                "detach-client".to_owned(),
                ";".to_owned(),
                "new-window".to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(rebound, Response::BindKey(_)), "{rebound:?}");

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02d")
        .await
        .expect("chained read-only binding input");

    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&alpha)
            .expect("session remains")
            .windows()
            .count(),
        1,
        "the chained new-window command must not execute"
    );
    assert_no_detach(&mut control_rx);
}

#[tokio::test]
async fn delegated_read_only_attach_does_not_interpret_detach_keys_inside_fragmented_paste() {
    let handler = RequestHandler::new();
    let alpha = session_name("delegated-read-only-paste");
    let requester_pid = std::process::id();
    let mut control_rx = register_delegated_attach(&handler, requester_pid, &alpha).await;
    let mut pending_input = Vec::new();

    for chunk in [
        b"\x1b[20".as_slice(),
        b"0~\x02".as_slice(),
        b"d\x1b[201~".as_slice(),
    ] {
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, chunk)
            .await
            .expect("read-only fragmented paste input");
    }

    assert!(pending_input.is_empty(), "complete paste must be consumed");
    assert_no_detach(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02d")
        .await
        .expect("explicit detach after paste");
    recv_detach(&mut control_rx).await;
}
