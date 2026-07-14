use super::{pane_terminal_size, session_name, RequestHandler};
use crate::pane_io::AttachControl;
use rmux_proto::{
    KillSessionRequest, KillWindowRequest, NewSessionRequest, NewWindowRequest, OptionName,
    Request, Response, ScopeSelector, SetOptionMode, SetOptionRequest, TerminalGeometry,
    TerminalPixels, TerminalSize, WindowTarget,
};
use tokio::sync::mpsc;

const LARGE_SIZE: TerminalSize = TerminalSize {
    cols: 132,
    rows: 43,
};
const SMALL_SIZE: TerminalSize = TerminalSize { cols: 72, rows: 19 };

#[tokio::test]
async fn live_resize_aborts_if_the_attach_switches_after_geometry_capture() {
    let handler = RequestHandler::new();
    let alpha = session_name("attached-resize-switch-alpha");
    let beta = session_name("attached-resize-switch-beta");
    for name in [&alpha, &beta] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: name.clone(),
                detached: true,
                size: Some(SMALL_SIZE),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    }
    handler.wait_for_initial_panes_for_test().await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel::<AttachControl>();
    let attach_pid = 7_509;
    handler
        .register_attach(attach_pid, alpha.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(attach_pid).await;
    let beta_id = {
        let state = handler.state.lock().await;
        state.sessions.session(&beta).expect("beta session").id()
    };
    let pixels = TerminalPixels::new(1_320, 860);
    let geometry = TerminalGeometry {
        size: LARGE_SIZE,
        pixels: Some(pixels),
    };

    // Hold state so resize can capture and publish its client geometry but
    // cannot yet commit session pixels. Switch the same registration while it
    // is blocked; the stale resize must then abandon every session mutation.
    let state_guard = handler.state.lock().await;
    let resize_handler = handler.clone();
    let resize = tokio::spawn(async move {
        resize_handler
            .handle_attached_resize_geometry_for_identity(identity, geometry)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let mut active_attach = handler.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .expect("attached client survives");
            if active.client_size == LARGE_SIZE && active.client_pixels == Some(pixels) {
                active.session_name = beta.clone();
                active.session_id = beta_id;
                break;
            }
            drop(active_attach);
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resize reaches the post-capture state boundary");
    handler.bump_active_attach_epoch();
    drop(state_guard);

    resize
        .await
        .expect("resize task join")
        .expect("stale resize exits cleanly");
    let state = handler.state.lock().await;
    assert_eq!(state.attached_terminal_pixels_for_test(&alpha), None);
    assert_eq!(state.attached_terminal_pixels_for_test(&beta), None);
}

#[tokio::test]
async fn live_resize_never_mutates_a_recreated_same_name_session() {
    let handler = RequestHandler::new();
    let session_name = session_name("attached-resize-session-identity");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(SMALL_SIZE),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel::<AttachControl>();
    let attach_pid = 7_510;
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    let identity = handler.active_attach_identity_for_test(attach_pid).await;
    let original_session_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .expect("original session")
            .id()
    };

    let pause = handler.install_attached_size_selection_pause();
    let resize = handler.handle_attached_resize_for_identity(identity, LARGE_SIZE);
    let replace = async {
        pause.reached.notified().await;
        let killed = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: session_name.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
        let recreated = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(SMALL_SIZE),
                environment: None,
            }))
            .await;
        assert!(
            matches!(recreated, Response::NewSession(_)),
            "{recreated:?}"
        );
        pause.release.notify_one();
    };
    let (resized, ()) = tokio::join!(resize, replace);
    resized.expect("stale resize exits without touching the replacement");

    let state = handler.state.lock().await;
    let replacement = state
        .sessions
        .session(&session_name)
        .expect("replacement session survives");
    assert_ne!(replacement.id(), original_session_id);
    assert_eq!(replacement.window().size(), SMALL_SIZE);
}

#[tokio::test]
async fn attached_size_selection_retries_after_the_captured_window_is_killed() {
    let handler = RequestHandler::new();
    let session_name = session_name("attached-size-window-race");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(SMALL_SIZE),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: Some("captured-window".to_owned()),
            detached: false,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel::<AttachControl>();
    let attach_pid = 7511;
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    handler
        .handle_attached_resize(attach_pid, SMALL_SIZE)
        .await
        .expect("initial attached resize succeeds");
    {
        let mut active_attach = handler.active_attach.lock().await;
        let size_sequence = active_attach.next_size_sequence;
        active_attach.next_size_sequence = size_sequence.saturating_add(1);
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .expect("attached client exists");
        active.client_size = LARGE_SIZE;
        active.size_sequence = size_sequence;
    }
    handler.bump_active_attach_epoch();

    let pause = handler.install_attached_size_selection_pause();
    let reconcile = handler.reconcile_attached_session_size(&session_name);
    let kill_captured_window = async {
        pause.reached.notified().await;
        let response = handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(session_name.clone(), 1),
                kill_all_others: false,
            }))
            .await;
        pause.release.notify_one();
        response
    };
    let (reconciled, killed) = tokio::join!(reconcile, kill_captured_window);
    assert!(matches!(killed, Response::KillWindow(_)), "{killed:?}");
    let reconciled = reconciled.expect("reconciliation succeeds");
    let surviving_target = WindowTarget::with_window(session_name.clone(), 0);
    assert!(
        reconciled.is_none() || reconciled == Some(surviving_target),
        "the concurrent reconcile must either observe kill-window's completed resize or target the surviving active window: {reconciled:?}"
    );

    {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("session survives window kill");
        assert_eq!(session.active_window_index(), 0);
        assert!(session.window_at(1).is_none());
        assert_eq!(session.window().size(), LARGE_SIZE);
    }
    let pty_size = pane_terminal_size(&handler, &session_name, 0, 0).await;
    assert_eq!(pty_size.cols, LARGE_SIZE.cols);
    assert!(
        pty_size.rows == LARGE_SIZE.rows || pty_size.rows == LARGE_SIZE.rows.saturating_sub(1),
        "surviving PTY follows the retried active-window resize: {pty_size:?}"
    );
}

#[tokio::test]
async fn attached_size_selection_retries_after_the_candidate_epoch_changes() {
    let handler = RequestHandler::new();
    let session_name = session_name("attached-size-candidate-race");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(LARGE_SIZE),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel::<AttachControl>();
    let attach_pid = 7512;
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    set_attached_candidate_size(&handler, attach_pid, SMALL_SIZE).await;

    let pause = handler.install_attached_size_selection_pause();
    let reconcile = handler.reconcile_attached_session_size(&session_name);
    let replace_candidate = async {
        pause.reached.notified().await;
        set_attached_candidate_size(&handler, attach_pid, LARGE_SIZE).await;
        pause.release.notify_one();
    };
    let (reconciled, ()) = tokio::join!(reconcile, replace_candidate);
    assert_eq!(reconciled.expect("reconciliation succeeds"), None);

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name)
            .expect("session survives")
            .window()
            .size(),
        LARGE_SIZE,
        "a stale selection must not overwrite the latest attached candidate"
    );
}

#[tokio::test]
async fn attached_size_selection_retries_after_the_policy_changes() {
    let handler = RequestHandler::new();
    let session_name = session_name("attached-size-policy-race");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(LARGE_SIZE),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel::<AttachControl>();
    let attach_pid = 7513;
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    set_attached_candidate_size(&handler, attach_pid, SMALL_SIZE).await;

    let pause = handler.install_attached_size_selection_pause();
    let reconcile = handler.reconcile_attached_session_size(&session_name);
    let change_policy = async {
        pause.reached.notified().await;
        let response = handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(session_name.clone(), 0)),
                option: OptionName::WindowSize,
                value: "manual".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await;
        pause.release.notify_one();
        response
    };
    let (reconciled, policy_response) = tokio::join!(reconcile, change_policy);
    assert!(
        matches!(policy_response, Response::SetOption(_)),
        "{policy_response:?}"
    );
    assert_eq!(reconciled.expect("reconciliation succeeds"), None);

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name)
            .expect("session survives")
            .window()
            .size(),
        LARGE_SIZE,
        "a stale selection must not override a newly-manual window"
    );
}

#[tokio::test]
async fn attached_candidate_cannot_change_between_final_validation_and_apply() {
    let handler = RequestHandler::new();
    let session_name = session_name("attached-size-final-apply-race");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(LARGE_SIZE),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel::<AttachControl>();
    let attach_pid = 7514;
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    set_attached_candidate_size(&handler, attach_pid, SMALL_SIZE).await;

    let pause = handler.install_attached_size_apply_pause();
    let reconcile = handler.reconcile_attached_session_size(&session_name);
    let race_candidate = async {
        pause.reached.notified().await;
        let acquired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mutation_handler = handler.clone();
        let acquired_after_lock = acquired.clone();
        let mutation = tokio::spawn(async move {
            let mut active_attach = mutation_handler.active_attach.lock().await;
            acquired_after_lock.store(true, std::sync::atomic::Ordering::Release);
            let size_sequence = active_attach.next_size_sequence;
            active_attach.next_size_sequence = size_sequence.saturating_add(1);
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .expect("attached client exists");
            active.client_size = LARGE_SIZE;
            active.size_sequence = size_sequence;
            drop(active_attach);
            mutation_handler.bump_active_attach_epoch();
        });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(
            !acquired.load(std::sync::atomic::Ordering::Acquire),
            "final validation must retain the attached-client lock through apply"
        );
        pause.release.notify_one();
        mutation.await.expect("candidate mutation task succeeds");
        handler
            .reconcile_attached_session_size(&session_name)
            .await
            .expect("latest candidate reconciliation succeeds")
    };
    let (first_reconcile, final_reconcile) = tokio::join!(reconcile, race_candidate);
    assert!(first_reconcile
        .expect("first reconciliation succeeds")
        .is_some());
    assert!(final_reconcile.is_some());

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name)
            .expect("session survives")
            .window()
            .size(),
        LARGE_SIZE,
        "the serialized follow-on candidate is applied after the first reconciliation"
    );
}

async fn set_attached_candidate_size(
    handler: &RequestHandler,
    attach_pid: u32,
    size: TerminalSize,
) {
    let mut active_attach = handler.active_attach.lock().await;
    let size_sequence = active_attach.next_size_sequence;
    active_attach.next_size_sequence = size_sequence.saturating_add(1);
    let active = active_attach
        .by_pid
        .get_mut(&attach_pid)
        .expect("attached client exists");
    active.client_size = size;
    active.size_sequence = size_sequence;
    drop(active_attach);
    handler.bump_active_attach_epoch();
}
