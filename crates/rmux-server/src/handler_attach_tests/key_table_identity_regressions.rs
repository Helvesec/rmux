use std::time::Instant;

use super::*;
use crate::handler::attach_support::ActiveAttachIdentity;

async fn current_identity(handler: &RequestHandler, attach_pid: u32) -> ActiveAttachIdentity {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .expect("attached client remains registered")
        .identity(attach_pid)
}

async fn table_references(handler: &RequestHandler, table_name: &str) -> Option<usize> {
    handler
        .state
        .lock()
        .await
        .key_bindings
        .table(table_name)
        .map(|table| table.references())
}

#[tokio::test]
async fn prefix_timer_follows_stable_session_identity_across_rename() {
    let handler = RequestHandler::new();
    let original = session_name("key-timer-rename-original");
    let renamed = session_name("key-timer-rename-current");
    let attach_pid = u32::MAX - 811;
    let _control_rx = create_attached_session(&handler, attach_pid, &original).await;
    let identity = current_identity(&handler, attach_pid).await;
    let key_table_set_at = Instant::now();
    handler
        .set_attached_key_table_for_client_session_identity(
            identity,
            &original,
            identity.session_id(),
            Some("prefix".to_owned()),
            Some(key_table_set_at),
        )
        .await
        .expect("prefix table is armed before rename");
    let key_table_generation = handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .expect("attach remains registered before rename")
        .key_table_generation;
    assert_eq!(table_references(&handler, "prefix").await, Some(1));

    let expiry_pause = handler.install_attached_key_table_timer_expiry_pause();
    let timer = handler
        .schedule_attached_prefix_timeout_for_test(
            identity,
            key_table_set_at,
            key_table_generation,
            Duration::ZERO,
        )
        .expect("normal lane admits rename timer probe");
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, expiry_pause.reached.notified())
        .await
        .expect("timer reaches expiry boundary before rename");

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: original,
            new_name: renamed.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );

    expiry_pause.release.notify_one();
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, timer)
        .await
        .expect("renamed timer finishes promptly")
        .expect("renamed timer task joins");

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("renamed attach remains registered");
    assert_eq!(active.session_name, renamed);
    assert_eq!(active.session_id, identity.session_id());
    assert_eq!(active.key_table_name, None);
    assert_eq!(active.key_table_set_at, None);
    assert!(!active.repeat_active);
    drop(active_attach);
    assert_eq!(table_references(&handler, "prefix").await, Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_live_dispatch_rejects_same_pid_same_generation_replacement() {
    let handler = RequestHandler::new();
    let session = session_name("non-live-dispatch-attach-aba");
    let attach_pid = std::process::id();
    let _original_rx = create_attached_session(&handler, attach_pid, &session).await;
    let original_identity = current_identity(&handler, attach_pid).await;
    assert_eq!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .expect("original attach remains registered")
            .key_table_generation,
        0
    );

    let dispatch_pause = handler.install_attached_key_dispatch_commit_pause(attach_pid);
    let dispatch_handler = handler.clone();
    let dispatch_session = session.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_handler
            .handle(Request::SendKeysExt(rmux_proto::SendKeysExtRequest {
                target: Some(PaneTarget::new(dispatch_session, 0)),
                keys: vec!["C-b".to_owned()],
                expand_formats: false,
                hex: false,
                literal: false,
                dispatch_key_table: true,
                copy_mode_command: false,
                forward_mouse_event: false,
                reset_terminal: false,
                repeat_count: None,
            }))
            .await
    });
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, dispatch_pause.reached.notified())
        .await
        .expect("non-live dispatch pauses after generation-zero lookup");

    let (replacement_tx, _replacement_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session, replacement_tx)
        .await;
    let replacement_identity = current_identity(&handler, attach_pid).await;
    assert_ne!(replacement_identity, original_identity);
    assert_eq!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .expect("replacement attach remains registered")
            .key_table_generation,
        0
    );

    dispatch_pause.release.notify_one();
    let response = tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, dispatch)
        .await
        .expect("stale non-live dispatch finishes promptly")
        .expect("stale non-live dispatch task joins");
    assert!(matches!(response, Response::Error(_)), "{response:?}");

    let active_attach = handler.active_attach.lock().await;
    let replacement = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("replacement remains registered after stale dispatch");
    assert_eq!(replacement.identity(attach_pid), replacement_identity);
    assert_eq!(replacement.key_table_generation, 0);
    assert_eq!(replacement.key_table_name, None);
    assert_eq!(replacement.repeat_deadline, None);
    assert!(!replacement.repeat_active);
    drop(active_attach);
    assert_eq!(table_references(&handler, "prefix").await, Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn switch_table_apply_rejects_attach_rehomed_after_session_resolution() {
    let handler = RequestHandler::new();
    let beta = session_name("switch-table-rehome-beta");
    let alpha = session_name("switch-table-rehome-alpha");
    let attach_pid = std::process::id();
    let _beta_rx = create_attached_session(&handler, u32::MAX - 812, &beta).await;
    let _alpha_rx = create_attached_session(&handler, attach_pid, &alpha).await;
    let alpha_identity = current_identity(&handler, attach_pid).await;

    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::DetachOnDestroy,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    handler
        .set_attached_key_table_for_client_session_identity(
            alpha_identity,
            &alpha,
            alpha_identity.session_id(),
            Some("pre-rehome-table".to_owned()),
            Some(Instant::now()),
        )
        .await
        .expect("pre-rehome table is installed");
    assert_eq!(
        table_references(&handler, "pre-rehome-table").await,
        Some(1)
    );

    let apply_pause = handler.install_attached_key_table_switch_apply_pause(attach_pid);
    let switch_handler = handler.clone();
    let stale_apply = tokio::spawn(async move {
        switch_handler
            .handle(Request::SwitchClientExt(
                rmux_proto::SwitchClientExtRequest {
                    target: None,
                    key_table: Some("prefix".to_owned()),
                },
            ))
            .await
    });
    tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, apply_pause.reached.notified())
        .await
        .expect("switch-client -T pauses after resolving alpha identity");

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: alpha,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    {
        let active_attach = handler.active_attach.lock().await;
        let rehomed = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("attach survives automatic rehome");
        assert_eq!(rehomed.session_name, beta);
        assert_ne!(rehomed.session_id, alpha_identity.session_id());
        assert_eq!(rehomed.key_table_name, None);
        assert_eq!(rehomed.repeat_deadline, None);
        assert!(!rehomed.repeat_active);
    }
    assert_eq!(table_references(&handler, "pre-rehome-table").await, None);

    apply_pause.release.notify_one();
    let response = tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, stale_apply)
        .await
        .expect("stale switch table application finishes promptly")
        .expect("stale switch table task joins");
    assert!(matches!(response, Response::Error(_)), "{response:?}");

    let active_attach = handler.active_attach.lock().await;
    let rehomed = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("rehomed attach remains registered");
    assert_eq!(rehomed.session_name, beta);
    assert_eq!(rehomed.key_table_name, None);
    assert_eq!(rehomed.repeat_deadline, None);
    assert!(!rehomed.repeat_active);
    drop(active_attach);
    assert_eq!(table_references(&handler, "prefix").await, Some(0));
}
