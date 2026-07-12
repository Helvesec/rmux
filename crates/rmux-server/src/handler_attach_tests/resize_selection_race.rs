use super::{pane_terminal_size, session_name, RequestHandler};
use crate::pane_io::AttachControl;
use rmux_proto::{
    KillWindowRequest, NewSessionRequest, NewWindowRequest, OptionName, Request, Response,
    ScopeSelector, SetOptionMode, SetOptionRequest, TerminalSize, WindowTarget,
};
use tokio::sync::mpsc;

const LARGE_SIZE: TerminalSize = TerminalSize {
    cols: 132,
    rows: 43,
};
const SMALL_SIZE: TerminalSize = TerminalSize { cols: 72, rows: 19 };

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
