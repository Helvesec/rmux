use super::*;

use super::super::lock_support::{install_lock_identity_pause, LockIdentityPausePoint};

async fn create_session(handler: &RequestHandler, session: &SessionName) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
}

async fn set_lock_command(handler: &RequestHandler) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::LockCommand,
            value: "lock-identity-probe".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn rename_session(
    handler: &RequestHandler,
    current_name: SessionName,
    new_name: SessionName,
) {
    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: current_name,
            new_name,
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
}

async fn switch_attached_client(handler: &RequestHandler, attach_pid: u32, target: SessionName) {
    let response = handler
        .dispatch(
            attach_pid,
            Request::SwitchClient(SwitchClientRequest {
                target: target.clone(),
            }),
        )
        .await
        .response;
    assert_eq!(
        response,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: target,
        })
    );
}

async fn expect_lock_control(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    context: &str,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(control) = control_rx.recv().await {
            if matches!(control, AttachControl::LockShellCommand(_)) {
                return;
            }
        }
        panic!("attach control channel closed while waiting for {context}");
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {context}"));
}

fn assert_replacement_is_unlocked(
    active_attach: &super::super::attach_support::ActiveAttachState,
    attach_pid: u32,
    replacement_id: u64,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) {
    while let Ok(control) = control_rx.try_recv() {
        assert!(
            !matches!(control, AttachControl::LockShellCommand(_)),
            "replacement attach must not receive the stale lock command"
        );
    }
    let replacement = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("replacement attach survives");
    assert_eq!(replacement.id, replacement_id);
    assert!(!replacement.suspended);
}

fn assert_attach_is_unlocked(
    active_attach: &super::super::attach_support::ActiveAttachState,
    attach_pid: u32,
    expected_session: &SessionName,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) {
    while let Ok(control) = control_rx.try_recv() {
        assert!(
            !matches!(control, AttachControl::LockShellCommand(_)),
            "attach outside the lock-session target must not receive a lock command"
        );
    }
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach survives");
    assert_eq!(&active.session_name, expected_session);
    assert!(!active.suspended);
}

#[tokio::test]
async fn lock_server_does_not_suspend_same_pid_attach_replacement_after_snapshot() {
    let handler = RequestHandler::new();
    let session = session_name("lock-server-attach-identity");
    let attach_pid = 920_060;
    create_session(&handler, &session).await;
    set_lock_command(&handler).await;

    let (old_tx, _old_rx) = mpsc::unbounded_channel();
    let old_id = handler
        .register_attach(attach_pid, session.clone(), old_tx)
        .await;
    let pause = install_lock_identity_pause(LockIdentityPausePoint::ServerClient(attach_pid));
    let lock_handler = handler.clone();
    let lock = tokio::spawn(async move {
        lock_handler
            .handle(Request::LockServer(rmux_proto::LockServerRequest))
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), pause.wait_until_reached())
        .await
        .expect("lock-server captures attached-client identities");
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    let replacement_id = handler
        .register_attach(attach_pid, session, replacement_tx)
        .await;
    assert_ne!(replacement_id, old_id);
    pause.release();

    let response = lock.await.expect("lock-server task joins");
    assert!(matches!(response, Response::LockServer(_)), "{response:?}");
    let active_attach = handler.active_attach.lock().await;
    assert_replacement_is_unlocked(
        &active_attach,
        attach_pid,
        replacement_id,
        &mut replacement_rx,
    );
}

#[tokio::test]
async fn lock_server_follows_exact_session_identity_across_rename() {
    let handler = RequestHandler::new();
    let original_name = session_name("lock-server-before-rename");
    let renamed = session_name("lock-server-after-rename");
    let attach_pid = 920_064;
    create_session(&handler, &original_name).await;
    set_lock_command(&handler).await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let attach_id = handler
        .register_attach(attach_pid, original_name.clone(), control_tx)
        .await;
    let session_id = handler
        .active_attach_identity_for_test(attach_pid)
        .await
        .session_id();

    let pause = install_lock_identity_pause(LockIdentityPausePoint::ServerClient(attach_pid));
    let lock_handler = handler.clone();
    let lock = tokio::spawn(async move {
        lock_handler
            .handle(Request::LockServer(rmux_proto::LockServerRequest))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), pause.wait_until_reached())
        .await
        .expect("lock-server captures the exact attach identity");
    rename_session(&handler, original_name, renamed.clone()).await;
    pause.release();

    let response = lock.await.expect("lock-server task joins");
    assert!(matches!(response, Response::LockServer(_)), "{response:?}");
    expect_lock_control(&mut control_rx, "lock-server after rename").await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach survives");
    assert_eq!(active.id, attach_id);
    assert_eq!(active.session_id, session_id);
    assert_eq!(active.session_name, renamed);
    assert!(active.suspended);
}

#[tokio::test]
async fn lock_server_follows_same_attach_registration_across_session_switch() {
    let handler = RequestHandler::new();
    let alpha = session_name("lock-server-before-switch");
    let beta = session_name("lock-server-after-switch");
    let attach_pid = 920_067;
    create_session(&handler, &alpha).await;
    create_session(&handler, &beta).await;
    set_lock_command(&handler).await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let attach_id = handler.register_attach(attach_pid, alpha, control_tx).await;

    let pause = install_lock_identity_pause(LockIdentityPausePoint::ServerClient(attach_pid));
    let lock_handler = handler.clone();
    let lock = tokio::spawn(async move {
        lock_handler
            .handle(Request::LockServer(rmux_proto::LockServerRequest))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), pause.wait_until_reached())
        .await
        .expect("lock-server captures the attach registration");
    switch_attached_client(&handler, attach_pid, beta.clone()).await;
    pause.release();

    let response = lock.await.expect("lock-server task joins");
    assert!(matches!(response, Response::LockServer(_)), "{response:?}");
    expect_lock_control(&mut control_rx, "lock-server after session switch").await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach survives");
    assert_eq!(active.id, attach_id);
    assert_eq!(active.session_name, beta);
    assert!(active.suspended);
}

#[tokio::test]
async fn lock_client_does_not_suspend_same_pid_attach_replacement_after_resolution() {
    let handler = RequestHandler::new();
    let session = session_name("lock-client-attach-identity");
    let attach_pid = 920_061;
    create_session(&handler, &session).await;
    set_lock_command(&handler).await;

    let (old_tx, _old_rx) = mpsc::unbounded_channel();
    let old_id = handler
        .register_attach(attach_pid, session.clone(), old_tx)
        .await;
    let pause = install_lock_identity_pause(LockIdentityPausePoint::Client(attach_pid));
    let lock_handler = handler.clone();
    let lock = tokio::spawn(async move {
        lock_handler
            .dispatch(
                attach_pid,
                Request::LockClient(rmux_proto::LockClientRequest {
                    target_client: "=".to_owned(),
                }),
            )
            .await
            .response
    });

    tokio::time::timeout(Duration::from_secs(2), pause.wait_until_reached())
        .await
        .expect("lock-client captures the attached-client identity");
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    let replacement_id = handler
        .register_attach(attach_pid, session, replacement_tx)
        .await;
    assert_ne!(replacement_id, old_id);
    pause.release();

    let response = lock.await.expect("lock-client task joins");
    assert!(matches!(response, Response::LockClient(_)), "{response:?}");
    let active_attach = handler.active_attach.lock().await;
    assert_replacement_is_unlocked(
        &active_attach,
        attach_pid,
        replacement_id,
        &mut replacement_rx,
    );
}

#[tokio::test]
async fn lock_client_follows_exact_session_identity_across_rename() {
    let handler = RequestHandler::new();
    let original_name = session_name("lock-client-before-rename");
    let renamed = session_name("lock-client-after-rename");
    let attach_pid = 920_065;
    create_session(&handler, &original_name).await;
    set_lock_command(&handler).await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let attach_id = handler
        .register_attach(attach_pid, original_name.clone(), control_tx)
        .await;
    let session_id = handler
        .active_attach_identity_for_test(attach_pid)
        .await
        .session_id();

    let pause = install_lock_identity_pause(LockIdentityPausePoint::Client(attach_pid));
    let lock_handler = handler.clone();
    let lock = tokio::spawn(async move {
        lock_handler
            .dispatch(
                attach_pid,
                Request::LockClient(rmux_proto::LockClientRequest {
                    target_client: "=".to_owned(),
                }),
            )
            .await
            .response
    });
    tokio::time::timeout(Duration::from_secs(2), pause.wait_until_reached())
        .await
        .expect("lock-client captures the exact attach identity");
    rename_session(&handler, original_name, renamed.clone()).await;
    pause.release();

    let response = lock.await.expect("lock-client task joins");
    assert!(matches!(response, Response::LockClient(_)), "{response:?}");
    expect_lock_control(&mut control_rx, "lock-client after rename").await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach survives");
    assert_eq!(active.id, attach_id);
    assert_eq!(active.session_id, session_id);
    assert_eq!(active.session_name, renamed);
    assert!(active.suspended);
}

#[tokio::test]
async fn lock_client_follows_same_attach_registration_across_session_switch() {
    let handler = RequestHandler::new();
    let alpha = session_name("lock-client-before-switch");
    let beta = session_name("lock-client-after-switch");
    let attach_pid = 920_068;
    create_session(&handler, &alpha).await;
    create_session(&handler, &beta).await;
    set_lock_command(&handler).await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let attach_id = handler.register_attach(attach_pid, alpha, control_tx).await;

    let pause = install_lock_identity_pause(LockIdentityPausePoint::Client(attach_pid));
    let lock_handler = handler.clone();
    let lock = tokio::spawn(async move {
        lock_handler
            .dispatch(
                attach_pid,
                Request::LockClient(rmux_proto::LockClientRequest {
                    target_client: "=".to_owned(),
                }),
            )
            .await
            .response
    });
    tokio::time::timeout(Duration::from_secs(2), pause.wait_until_reached())
        .await
        .expect("lock-client captures the attach registration");
    switch_attached_client(&handler, attach_pid, beta.clone()).await;
    pause.release();

    let response = lock.await.expect("lock-client task joins");
    assert!(matches!(response, Response::LockClient(_)), "{response:?}");
    expect_lock_control(&mut control_rx, "lock-client after session switch").await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach survives");
    assert_eq!(active.id, attach_id);
    assert_eq!(active.session_name, beta);
    assert!(active.suspended);
}

#[tokio::test]
async fn lock_session_does_not_suspend_same_pid_attach_replacement_after_snapshot() {
    let handler = RequestHandler::new();
    let session = session_name("lock-session-attach-identity");
    let attach_pid = 920_062;
    create_session(&handler, &session).await;
    set_lock_command(&handler).await;

    let (old_tx, _old_rx) = mpsc::unbounded_channel();
    let old_id = handler
        .register_attach(attach_pid, session.clone(), old_tx)
        .await;
    let pause =
        install_lock_identity_pause(LockIdentityPausePoint::SessionClients(session.clone()));
    let lock_handler = handler.clone();
    let lock_target = session.clone();
    let lock = tokio::spawn(async move {
        lock_handler
            .handle(Request::LockSession(rmux_proto::LockSessionRequest {
                target: lock_target,
            }))
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), pause.wait_until_reached())
        .await
        .expect("lock-session captures attached-client identities");
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    let replacement_id = handler
        .register_attach(attach_pid, session, replacement_tx)
        .await;
    assert_ne!(replacement_id, old_id);
    pause.release();

    let response = lock.await.expect("lock-session task joins");
    assert!(matches!(response, Response::LockSession(_)), "{response:?}");
    let active_attach = handler.active_attach.lock().await;
    assert_replacement_is_unlocked(
        &active_attach,
        attach_pid,
        replacement_id,
        &mut replacement_rx,
    );
}

#[tokio::test]
async fn lock_session_follows_exact_session_identity_across_rename() {
    let handler = RequestHandler::new();
    let original_name = session_name("lock-session-before-rename");
    let renamed = session_name("lock-session-after-rename");
    let attach_pid = 920_066;
    create_session(&handler, &original_name).await;
    set_lock_command(&handler).await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let attach_id = handler
        .register_attach(attach_pid, original_name.clone(), control_tx)
        .await;
    let session_id = handler
        .active_attach_identity_for_test(attach_pid)
        .await
        .session_id();

    let pause = install_lock_identity_pause(LockIdentityPausePoint::Session(original_name.clone()));
    let lock_handler = handler.clone();
    let lock_target = original_name.clone();
    let lock = tokio::spawn(async move {
        lock_handler
            .handle(Request::LockSession(rmux_proto::LockSessionRequest {
                target: lock_target,
            }))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), pause.wait_until_reached())
        .await
        .expect("lock-session captures the exact session identity");
    rename_session(&handler, original_name, renamed.clone()).await;
    pause.release();

    let response = lock.await.expect("lock-session task joins");
    assert!(matches!(response, Response::LockSession(_)), "{response:?}");
    expect_lock_control(&mut control_rx, "lock-session after rename").await;
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach survives");
    assert_eq!(active.id, attach_id);
    assert_eq!(active.session_id, session_id);
    assert_eq!(active.session_name, renamed);
    assert!(active.suspended);
}

#[tokio::test]
async fn lock_session_does_not_follow_attach_that_switches_out_of_target_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("lock-session-before-switch");
    let beta = session_name("lock-session-after-switch");
    let attach_pid = 920_069;
    create_session(&handler, &alpha).await;
    create_session(&handler, &beta).await;
    set_lock_command(&handler).await;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;

    let pause = install_lock_identity_pause(LockIdentityPausePoint::SessionClients(alpha.clone()));
    let lock_handler = handler.clone();
    let lock_target = alpha;
    let lock = tokio::spawn(async move {
        lock_handler
            .handle(Request::LockSession(rmux_proto::LockSessionRequest {
                target: lock_target,
            }))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), pause.wait_until_reached())
        .await
        .expect("lock-session captures target-session attach registrations");
    switch_attached_client(&handler, attach_pid, beta.clone()).await;
    pause.release();

    let response = lock.await.expect("lock-session task joins");
    assert!(matches!(response, Response::LockSession(_)), "{response:?}");
    let active_attach = handler.active_attach.lock().await;
    assert_attach_is_unlocked(&active_attach, attach_pid, &beta, &mut control_rx);
}

#[tokio::test]
async fn lock_session_does_not_suspend_attach_in_same_name_recreated_session() {
    let handler = RequestHandler::new();
    let session = session_name("lock-session-recreated-identity");
    let keeper = session_name("lock-session-recreated-keeper");
    let replacement_pid = 920_063;
    create_session(&handler, &session).await;
    create_session(&handler, &keeper).await;
    set_lock_command(&handler).await;
    let original_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .expect("original session exists")
        .id();

    let pause = install_lock_identity_pause(LockIdentityPausePoint::Session(session.clone()));
    let lock_handler = handler.clone();
    let lock_target = session.clone();
    let lock = tokio::spawn(async move {
        lock_handler
            .handle(Request::LockSession(rmux_proto::LockSessionRequest {
                target: lock_target,
            }))
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), pause.wait_until_reached())
        .await
        .expect("lock-session captures the target session identity");
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    create_session(&handler, &session).await;
    let replacement_session_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .expect("replacement session exists")
        .id();
    assert_ne!(replacement_session_id, original_session_id);
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    let replacement_id = handler
        .register_attach(replacement_pid, session, replacement_tx)
        .await;
    pause.release();

    let response = lock.await.expect("lock-session task joins");
    assert!(matches!(response, Response::LockSession(_)), "{response:?}");
    let active_attach = handler.active_attach.lock().await;
    let replacement = active_attach
        .by_pid
        .get(&replacement_pid)
        .expect("replacement session attach survives");
    assert_eq!(replacement.id, replacement_id);
    assert_eq!(replacement.session_id, replacement_session_id);
    assert!(!replacement.suspended);
    while let Ok(control) = replacement_rx.try_recv() {
        assert!(!matches!(control, AttachControl::LockShellCommand(_)));
    }
}
