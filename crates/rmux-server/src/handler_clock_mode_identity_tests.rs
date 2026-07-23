use std::sync::Arc;

use super::{clock_mode_tests::create_session, RequestHandler};
use crate::pane_io::AttachControl;
use rmux_proto::{
    ClockModeRequest, HookLifecycle, HookName, KillSessionRequest, PaneTarget, Request, Response,
    ScopeSelector, SessionId, SetHookRequest, ShowBufferRequest, SwitchClientRequest, TerminalSize,
};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{timeout, Duration};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

async fn enter_clock_mode(handler: &RequestHandler, target: &PaneTarget) {
    let response = handler
        .handle(Request::ClockMode(ClockModeRequest {
            target: Some(target.clone()),
        }))
        .await;
    assert!(matches!(response, Response::ClockMode(_)), "{response:?}");
}

async fn assert_clock_mode(handler: &RequestHandler, target: &PaneTarget, expected: bool) {
    assert_eq!(
        handler
            .target_is_in_clock_mode(target)
            .await
            .expect("clock-mode target resolves"),
        expected
    );
}

async fn session_id(handler: &RequestHandler, target: &PaneTarget) -> Option<SessionId> {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .map(rmux_core::Session::id)
}

async fn wait_for_session_absence(handler: &RequestHandler, target: &PaneTarget) {
    timeout(TEST_TIMEOUT, async {
        loop {
            if session_id(handler, target).await.is_none() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("old session removal becomes visible");
}

async fn wait_for_recreated_session(
    handler: &RequestHandler,
    target: &PaneTarget,
    old_session_id: SessionId,
) -> SessionId {
    timeout(TEST_TIMEOUT, async {
        loop {
            if let Some(current) = session_id(handler, target)
                .await
                .filter(|current| *current != old_session_id)
            {
                return current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same-name replacement session becomes visible")
}

async fn drain_mode_changed_hooks(
    handler: &RequestHandler,
    events: &mut broadcast::Receiver<super::QueuedLifecycleEvent>,
) -> Vec<String> {
    let mut snapshots = Vec::new();
    loop {
        match events.try_recv() {
            Ok(event) => {
                if matches!(
                    event.event,
                    rmux_core::LifecycleEvent::PaneModeChanged { .. }
                ) {
                    snapshots.push(
                        event
                            .formats
                            .iter()
                            .find(|(name, _)| name == "pane_in_mode")
                            .map(|(_, value)| value.clone())
                            .expect("pane-mode-changed snapshots pane_in_mode"),
                    );
                }
                handler.dispatch_lifecycle_hook(event).await;
            }
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                return snapshots;
            }
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                panic!("clock lifecycle events lagged during test: {skipped}");
            }
        }
    }
}

#[tokio::test]
async fn true_live_input_revalidates_attach_before_clock_exit() {
    let handler = RequestHandler::new();
    let alpha = create_session(
        &handler,
        "live-clock-identity-alpha",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let beta = create_session(
        &handler,
        "live-clock-identity-beta",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let requester_pid = std::process::id();
    let (alpha_tx, _alpha_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.session_name().clone(), alpha_tx)
        .await;
    enter_clock_mode(&handler, &alpha).await;

    let stale_identity = handler.active_attach_identity_for_test(requester_pid).await;
    let pause = handler.install_live_clock_mode_exit_pause_for_test(stale_identity);
    let input_handler = handler.clone();
    let input = tokio::spawn(async move {
        input_handler
            .handle_attached_live_input_for_test(requester_pid, b"x")
            .await
    });
    timeout(TEST_TIMEOUT, pause.wait_until_reached())
        .await
        .expect("live clock exit reaches identity pause");

    let (beta_tx, _beta_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, beta.session_name().clone(), beta_tx)
        .await;
    pause.release();
    let _result = timeout(TEST_TIMEOUT, input)
        .await
        .expect("live input completes")
        .expect("live input task joins");

    assert_clock_mode(&handler, &alpha, true).await;
}

#[tokio::test]
async fn delayed_escape_revalidates_attach_before_clock_exit() {
    let handler = RequestHandler::new();
    let alpha = create_session(
        &handler,
        "escape-clock-identity-alpha",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let beta = create_session(
        &handler,
        "escape-clock-identity-beta",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let requester_pid = std::process::id();
    let (alpha_tx, _alpha_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.session_name().clone(), alpha_tx)
        .await;
    enter_clock_mode(&handler, &alpha).await;

    let stale_identity = handler.active_attach_identity_for_test(requester_pid).await;
    let pause = handler.install_live_clock_mode_exit_pause_for_test(stale_identity);
    let input_handler = handler.clone();
    let input = tokio::spawn(async move {
        let mut pending_input = vec![b'\x1b'];
        input_handler
            .flush_attached_pending_escape_input_for_identity(stale_identity, &mut pending_input)
            .await
    });
    timeout(TEST_TIMEOUT, pause.wait_until_reached())
        .await
        .expect("delayed escape reaches identity pause");

    let (beta_tx, _beta_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, beta.session_name().clone(), beta_tx)
        .await;
    pause.release();
    let _result = timeout(TEST_TIMEOUT, input)
        .await
        .expect("delayed escape completes")
        .expect("delayed escape task joins");

    assert_clock_mode(&handler, &alpha, true).await;
}

#[tokio::test]
async fn switched_attach_uses_current_session_identity_for_clock_exit() {
    let handler = RequestHandler::new();
    let alpha = create_session(
        &handler,
        "switched-clock-identity-alpha",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let beta = create_session(
        &handler,
        "switched-clock-identity-beta",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let requester_pid = std::process::id();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.session_name().clone(), control_tx)
        .await;

    let response = handler
        .dispatch(
            requester_pid,
            Request::SwitchClient(SwitchClientRequest {
                target: beta.session_name().clone(),
            }),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );
    enter_clock_mode(&handler, &beta).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"x")
        .await
        .expect("live input exits clock mode after a session switch");
    assert_clock_mode(&handler, &beta, false).await;
}

#[tokio::test]
async fn reentered_clock_mode_supersedes_stale_exit_restore_in_commit_order() {
    let handler = RequestHandler::new();
    let target = create_session(
        &handler,
        "reentered-clock-effects",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let requester_pid = std::process::id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, target.session_name().clone(), control_tx)
        .await;
    enter_clock_mode(&handler, &target).await;
    while control_rx.try_recv().is_ok() {}

    let set_hook = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Pane(target.clone()),
            hook: HookName::PaneModeChanged,
            command: "if-shell -F '#{pane_in_mode}' { set-buffer -b clock-reentry-format 1 } { set-buffer -b clock-reentry-format 0 }".to_owned(),
            lifecycle: HookLifecycle::Persistent,
        }))
        .await;
    assert!(matches!(set_hook, Response::SetHook(_)), "{set_hook:?}");
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    let pause = handler.install_clock_mode_exit_commit_pause_for_test(target.clone());
    let exit_handler = handler.clone();
    let exit = tokio::spawn(async move {
        exit_handler
            .handle_attached_live_input_for_test(requester_pid, b"x")
            .await
    });
    timeout(TEST_TIMEOUT, pause.wait_until_reached())
        .await
        .expect("clock exit reaches committed-effect pause");

    let reentry_handler = handler.clone();
    let reentry_target = target.clone();
    let reentry = tokio::spawn(async move {
        enter_clock_mode(&reentry_handler, &reentry_target).await;
    });
    timeout(TEST_TIMEOUT, async {
        loop {
            if handler
                .target_is_in_clock_mode(&target)
                .await
                .expect("clock target resolves during reentry")
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("new clock mutation becomes visible before old exit publishes");

    pause.release();
    timeout(TEST_TIMEOUT, exit)
        .await
        .expect("clock exit completes")
        .expect("clock exit task joins")
        .expect("clock exit succeeds");
    timeout(TEST_TIMEOUT, reentry)
        .await
        .expect("clock reentry completes")
        .expect("clock reentry task joins");

    assert_eq!(
        drain_mode_changed_hooks(&handler, &mut lifecycle_events).await,
        ["0", "1"]
    );
    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("clock-reentry-format".to_owned()),
        }))
        .await;
    let Response::ShowBuffer(buffer) = shown else {
        panic!("expected hook buffer, got {shown:?}");
    };
    assert_eq!(buffer.command_output().stdout(), b"1");
    assert_clock_mode(&handler, &target, true).await;

    while let Ok(control) = control_rx.try_recv() {
        if let AttachControl::Overlay(frame) = control {
            assert!(
                frame.persistent,
                "the superseded exit must not send a transient restore overlay"
            );
        }
    }
}

#[tokio::test]
async fn restore_publication_serializes_a_later_clock_reentry() {
    let handler = RequestHandler::new();
    let target = create_session(
        &handler,
        "serialized-clock-restore",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let requester_pid = std::process::id();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, target.session_name().clone(), control_tx)
        .await;
    enter_clock_mode(&handler, &target).await;
    while control_rx.try_recv().is_ok() {}
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(&target)
            .expect("clock transcript exists")
    };

    let pause = handler.install_clock_mode_restore_commit_pause_for_test(target.clone());
    let exit_handler = handler.clone();
    let exit = tokio::spawn(async move {
        exit_handler
            .handle_attached_live_input_for_test(requester_pid, b"x")
            .await
    });
    timeout(TEST_TIMEOUT, pause.wait_until_reached())
        .await
        .expect("clock restore reaches atomic publication pause");

    let attempted = Arc::new(tokio::sync::Notify::new());
    let reentry_attempted = attempted.clone();
    let reentry_handler = handler.clone();
    let reentry_target = target.clone();
    let reentry = tokio::spawn(async move {
        reentry_attempted.notify_one();
        enter_clock_mode(&reentry_handler, &reentry_target).await;
    });
    attempted.notified().await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert!(
        transcript
            .lock()
            .expect("clock transcript lock")
            .clock_mode_generation()
            .is_none(),
        "reentry must remain behind the state-to-overlay transaction"
    );

    pause.release();
    timeout(TEST_TIMEOUT, exit)
        .await
        .expect("serialized clock exit completes")
        .expect("serialized clock exit task joins")
        .expect("serialized clock exit succeeds");
    timeout(TEST_TIMEOUT, reentry)
        .await
        .expect("serialized clock reentry completes")
        .expect("serialized clock reentry task joins");
    assert_clock_mode(&handler, &target, true).await;

    let overlays = std::iter::from_fn(|| control_rx.try_recv().ok())
        .filter_map(|control| match control {
            AttachControl::Overlay(frame) => Some(frame.persistent),
            _ => None,
        })
        .collect::<Vec<_>>();
    let restore_index = overlays
        .iter()
        .position(|persistent| !persistent)
        .expect("current exit publishes its transient restore");
    let reentry_index = overlays
        .iter()
        .rposition(|persistent| *persistent)
        .expect("later reentry publishes a persistent clock frame");
    assert!(restore_index < reentry_index);
}

#[tokio::test]
async fn clock_exit_effects_ignore_recreated_same_name_session() {
    let handler = RequestHandler::new();
    let alpha = create_session(
        &handler,
        "recreated-clock-effects",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let fallback = create_session(
        &handler,
        "recreated-clock-fallback",
        TerminalSize { cols: 20, rows: 8 },
    )
    .await;
    let requester_pid = std::process::id();
    let (old_tx, _old_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.session_name().clone(), old_tx)
        .await;
    enter_clock_mode(&handler, &alpha).await;

    let old_session_id = session_id(&handler, &alpha)
        .await
        .expect("old session identity exists");
    let pause = handler.install_clock_mode_exit_commit_pause_for_test(alpha.clone());
    let input_handler = handler.clone();
    let input = tokio::spawn(async move {
        input_handler
            .handle_attached_live_input_for_test(requester_pid, b"x")
            .await
    });
    timeout(TEST_TIMEOUT, pause.wait_until_reached())
        .await
        .expect("clock exit reaches post-clear pause");

    let kill_handler = handler.clone();
    let kill_target = alpha.session_name().clone();
    let kill = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: kill_target,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await
    });
    wait_for_session_absence(&handler, &alpha).await;

    let recreate_handler = handler.clone();
    let recreate_name = alpha.session_name().as_str().to_owned();
    let recreate = tokio::spawn(async move {
        create_session(
            &recreate_handler,
            &recreate_name,
            TerminalSize { cols: 20, rows: 8 },
        )
        .await
    });
    let replacement_session_id = wait_for_recreated_session(&handler, &alpha, old_session_id).await;
    assert_ne!(replacement_session_id, old_session_id);

    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, alpha.session_name().clone(), replacement_tx)
        .await;
    while replacement_rx.try_recv().is_ok() {}
    pause.release();

    timeout(TEST_TIMEOUT, input)
        .await
        .expect("clock exit completes")
        .expect("clock exit task joins")
        .expect("clock exit completes cleanly after target replacement");
    let killed = timeout(TEST_TIMEOUT, kill)
        .await
        .expect("kill-session completes")
        .expect("kill-session task joins");
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    let recreated = timeout(TEST_TIMEOUT, recreate)
        .await
        .expect("same-name recreation completes")
        .expect("same-name recreation task joins");
    assert_eq!(recreated, alpha);

    assert_clock_mode(&handler, &recreated, false).await;
    assert!(
        timeout(Duration::from_millis(100), async {
            loop {
                if matches!(replacement_rx.recv().await, Some(AttachControl::Overlay(_))) {
                    break;
                }
            }
        })
        .await
        .is_err(),
        "stale restore overlay must not target the recreated session"
    );
    assert_ne!(fallback.session_name(), recreated.session_name());
}
