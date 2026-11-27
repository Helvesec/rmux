use super::*;
use crate::pane_io::AttachControl;
#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;

async fn register_read_only_attach(
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
        .expect("read-only attach is active");
    active.can_write = false;
    active.flags = active.flags.with_read_only();
    control_rx
}

async fn bind_key(handler: &RequestHandler, key: &str, command: &[&str]) {
    let response = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: key.to_owned(),
            note: Some("read-only security boundary test".to_owned()),
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

async fn active_session(handler: &RequestHandler, requester_pid: u32) -> rmux_proto::SessionName {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&requester_pid)
        .expect("read-only attach remains active")
        .session_name
        .clone()
}

fn assert_no_terminal_control(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) {
    while let Ok(control) = control_rx.try_recv() {
        assert!(
            !matches!(
                control,
                AttachControl::Detach
                    | AttachControl::DetachKill
                    | AttachControl::DetachExecShellCommand(_)
            ),
            "read-only unsafe form emitted terminal client control: {control:?}"
        );
    }
}

#[tokio::test]
async fn read_only_switch_allowlist_rejects_privilege_targeting_and_session_mutation() {
    let handler = RequestHandler::new();
    let alpha = session_name("read-only-switch-security-alpha");
    let beta = session_name("read-only-switch-security-beta");
    create_send_keys_test_session(&handler, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;
    let requester_pid = std::process::id();
    let witness_pid = requester_pid.wrapping_add(1);
    let mut requester_rx = register_read_only_attach(&handler, requester_pid, &alpha).await;
    let mut witness_rx = register_read_only_attach(&handler, witness_pid, &alpha).await;
    let witness_name = handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&witness_pid)
        .expect("witness attach is active")
        .client_name
        .clone();
    let target_window = {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .session_setup(&beta)
            .expect("beta exists")
            .create_window(TerminalSize { cols: 80, rows: 24 })
            .expect("inactive beta window is created")
            .0
    };

    bind_key(&handler, "r", &["switch-client", "-Er"]).await;
    bind_key(
        &handler,
        "c",
        &[
            "switch-client",
            "-c",
            witness_name.as_str(),
            "-E",
            "-t",
            beta.as_str(),
        ],
    )
    .await;
    let window_target = format!("{beta}:{target_window}");
    bind_key(
        &handler,
        "w",
        &["switch-client", "-t", window_target.as_str()],
    )
    .await;
    bind_key(&handler, "z", &["switch-client", "-Z", "-t", beta.as_str()]).await;

    for key in [b"r", b"c", b"w", b"z"] {
        handler
            .handle_attached_live_input_for_test(requester_pid, key)
            .await
            .expect("unsafe read-only switch input is consumed");
        assert_eq!(active_session(&handler, requester_pid).await, alpha);
        assert_eq!(active_session(&handler, witness_pid).await, alpha);
        assert!(
            handler
                .active_attach
                .lock()
                .await
                .by_pid
                .get(&requester_pid)
                .expect("requester remains active")
                .flags
                .contains(crate::client_flags::ClientFlags::READONLY),
            "switch-client -r must never clear read-only"
        );
        assert_eq!(
            handler
                .state
                .lock()
                .await
                .sessions
                .session(&beta)
                .expect("beta exists")
                .active_window_index(),
            0,
            "window or zoom target must not mutate the session"
        );
        assert_no_terminal_control(&mut requester_rx);
        assert_no_terminal_control(&mut witness_rx);
    }
}

#[tokio::test]
async fn read_only_detach_allowlist_rejects_every_nonlocal_form() {
    let handler = RequestHandler::new();
    let alpha = session_name("read-only-detach-security");
    create_send_keys_test_session(&handler, &alpha).await;
    let requester_pid = std::process::id();
    let witness_pid = requester_pid.wrapping_add(1);
    let mut requester_rx = register_read_only_attach(&handler, requester_pid, &alpha).await;
    let mut witness_rx = register_read_only_attach(&handler, witness_pid, &alpha).await;
    let witness_name = handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&witness_pid)
        .expect("witness attach is active")
        .client_name
        .clone();

    bind_key(&handler, "p", &["detach-client", "-P"]).await;
    bind_key(&handler, "e", &["detach-client", "-E", "printf unsafe"]).await;
    bind_key(&handler, "a", &["detach-client", "-aP"]).await;
    bind_key(
        &handler,
        "t",
        &["detach-client", "-t", witness_name.as_str()],
    )
    .await;
    bind_key(&handler, "s", &["detach-client", "-s", alpha.as_str()]).await;

    for key in [b"p", b"e", b"a", b"t", b"s"] {
        handler
            .handle_attached_live_input_for_test(requester_pid, key)
            .await
            .expect("unsafe read-only detach input is consumed");
        assert_eq!(active_session(&handler, requester_pid).await, alpha);
        assert_eq!(active_session(&handler, witness_pid).await, alpha);
        assert_no_terminal_control(&mut requester_rx);
        assert_no_terminal_control(&mut witness_rx);
    }
}

#[tokio::test]
async fn read_only_safe_switch_cannot_lead_a_composed_mutation() {
    let handler = RequestHandler::new();
    let alpha = session_name("read-only-switch-chain-alpha");
    let beta = session_name("read-only-switch-chain-beta");
    create_send_keys_test_session(&handler, &alpha).await;
    create_send_keys_test_session(&handler, &beta).await;
    let requester_pid = std::process::id();
    let mut control_rx = register_read_only_attach(&handler, requester_pid, &alpha).await;
    bind_key(
        &handler,
        "x",
        &["switch-client", "-t", beta.as_str(), ";", "new-window"],
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"x")
        .await
        .expect("composed read-only switch input is consumed");

    assert_eq!(active_session(&handler, requester_pid).await, alpha);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .expect("beta exists")
            .windows()
            .count(),
        1,
        "the mutation after a safe-looking first command must not execute"
    );
    assert_no_terminal_control(&mut control_rx);
}

#[tokio::test]
async fn read_only_key_table_switch_cannot_lead_a_composed_mutation() {
    let handler = RequestHandler::new();
    let alpha = session_name("read-only-key-table-chain");
    create_send_keys_test_session(&handler, &alpha).await;
    let requester_pid = std::process::id();
    let mut control_rx = register_read_only_attach(&handler, requester_pid, &alpha).await;
    bind_key(
        &handler,
        "x",
        &["switch-client", "-T", "custom", ";", "new-window"],
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"x")
        .await
        .expect("composed read-only key-table input is consumed");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("requester remains active");
    assert_eq!(active.key_table_name, None);
    drop(active_attach);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&alpha)
            .expect("alpha exists")
            .windows()
            .count(),
        1
    );
    assert_no_terminal_control(&mut control_rx);
}
