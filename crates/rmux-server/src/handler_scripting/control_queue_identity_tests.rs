use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rmux_core::command_parser::CommandParser;
use rmux_proto::{
    AttachSessionRequest, ControlMode, HookLifecycle, HookName, KillSessionRequest,
    NewSessionRequest, Request, Response, ScopeSelector, SessionName, SetHookRequest, TerminalSize,
    WaitForMode, WaitForRequest,
};
use tokio::sync::mpsc;

use super::super::control_support::{
    with_control_queue_eof_cancellation, ControlQueueEofCancellation,
};
use super::*;
use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
use crate::outer_terminal::OuterTerminalContext;

#[tokio::test]
async fn control_queue_attach_rejects_recreated_same_name_session() {
    let handler = RequestHandler::new();
    let requester_pid = 93_771;
    let session_name = SessionName::new("control-queue-attach-aba").expect("valid session name");
    create_session(&handler, session_name.clone()).await;
    let original_session_id = session_id(&handler, &session_name).await;
    let request = Request::AttachSession(AttachSessionRequest {
        target: session_name.clone(),
    });
    let outcome = handler.dispatch(requester_pid, request.clone()).await;
    assert_eq!(
        outcome
            .attach
            .as_ref()
            .expect("unmanaged requester receives attach upgrade")
            .session_id,
        original_session_id
    );

    let (control_id, _control_events) = register_control(&handler, requester_pid).await;
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    create_session(&handler, session_name.clone()).await;
    let replacement_session_id = session_id(&handler, &session_name).await;
    assert_ne!(replacement_session_id, original_session_id);

    let error = handler
        .control_queue_action_from_outcome(requester_pid, control_id, request, outcome)
        .await
        .expect_err("stale attach identity must not bind the recreated session");
    assert_eq!(error, RmuxError::SessionNotFound(session_name.to_string()));
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&session_name)
                .expect("replacement session survives")
                .last_attached_at(),
            None,
            "the stale outcome must be rejected before touching the replacement"
        );
    }
    let active_control = handler.active_control.lock().await;
    let active = active_control
        .by_pid
        .get(&requester_pid)
        .expect("control client remains registered");
    assert_eq!(active.session_name, None);
    assert_eq!(active.session_id, None);
}

#[tokio::test]
async fn control_queue_stops_when_its_registration_disappears() {
    let handler = RequestHandler::new();
    let requester_pid = 93_772;
    let wait_channel = "control-queue-registration-disappears";
    let (control_id, _control_events) = register_control(&handler, requester_pid).await;
    let commands = CommandParser::new()
        .parse(&format!(
            "wait-for {wait_channel} ; set-environment -g CONTROL_QUEUE_GONE mutated"
        ))
        .expect("control commands parse");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_control_commands_identity(requester_pid, control_id, commands)
            .await
    });
    wait_for_waiter(&handler, wait_channel).await;

    handler.finish_control(requester_pid, control_id).await;
    signal_waiter(&handler, wait_channel).await;

    let result = queued.await.expect("control queue joins");
    assert!(
        result.error.is_some(),
        "missing registration must fail closed"
    );
    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("CONTROL_QUEUE_GONE"), None);
}

#[tokio::test]
async fn control_queue_stops_when_the_same_pid_is_registered_again() {
    let handler = RequestHandler::new();
    let requester_pid = 93_773;
    let wait_channel = "control-queue-registration-reused";
    let (old_control_id, _old_events) = register_control(&handler, requester_pid).await;
    let commands = CommandParser::new()
        .parse(&format!(
            "wait-for {wait_channel} ; set-environment -g CONTROL_QUEUE_REUSED mutated"
        ))
        .expect("control commands parse");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_control_commands_identity(requester_pid, old_control_id, commands)
            .await
    });
    wait_for_waiter(&handler, wait_channel).await;

    let (replacement_control_id, _replacement_events) =
        register_control(&handler, requester_pid).await;
    assert_ne!(replacement_control_id, old_control_id);
    signal_waiter(&handler, wait_channel).await;

    let result = queued.await.expect("control queue joins");
    assert!(
        result.error.is_some(),
        "replacement registration must fail closed"
    );
    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("CONTROL_QUEUE_REUSED"), None);
    drop(state);
    let active_control = handler.active_control.lock().await;
    let replacement = active_control
        .by_pid
        .get(&requester_pid)
        .expect("replacement remains registered");
    assert_eq!(replacement.id, replacement_control_id);
    assert_eq!(replacement.session_name, None);
}

#[tokio::test]
async fn stale_control_queue_cannot_apply_parse_time_assignments_to_reused_pid() {
    let handler = RequestHandler::new();
    let requester_pid = 93_774;
    let (old_control_id, _old_events) = register_control(&handler, requester_pid).await;
    let (replacement_control_id, _replacement_events) =
        register_control(&handler, requester_pid).await;
    assert_ne!(replacement_control_id, old_control_id);
    let commands = CommandParser::new()
        .parse("CONTROL_QUEUE_PARSE=mutated list-sessions")
        .expect("control commands parse");

    let result = handler
        .execute_control_commands_identity(requester_pid, old_control_id, commands)
        .await;

    assert!(result.error.is_some(), "stale assignment must fail closed");
    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("CONTROL_QUEUE_PARSE"), None);
}

#[tokio::test]
async fn stopped_control_frame_does_not_enter_a_nested_command_queue() {
    let handler = RequestHandler::new();
    let requester_pid = 93_780;
    let (control_id, _control_events) = register_control(&handler, requester_pid).await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    let cancellation = ControlQueueEofCancellation::new(identity);
    cancellation.cancel_for_eof();
    cancellation.mark_wait_cancelled();
    let nested_commands = CommandParser::new()
        .parse(
            "CONTROL_EOF_NESTED_PARSE=mutated set-environment -g CONTROL_EOF_NESTED_FIRST must-not-run",
        )
        .expect("nested command parses");

    // `run-shell -C`, command-form `if-shell`, source-file, and hooks enter
    // this detached parsed-command path while retaining the outer control
    // identity. A monotone StopFrame must be observed before its first item.
    let result = with_control_queue_eof_cancellation(
        cancellation,
        with_control_queue_identity(
            identity,
            handler.execute_parsed_commands_for_test(requester_pid, nested_commands),
        ),
    )
    .await;
    assert!(result.is_ok(), "EOF frame stop stays internal: {result:?}");

    let state = handler.state.lock().await;
    assert_eq!(
        state.environment.global_value("CONTROL_EOF_NESTED_FIRST"),
        None,
        "a nested queue entered after StopFrame must not execute its first mutation"
    );
    assert_eq!(
        state.environment.global_value("CONTROL_EOF_NESTED_PARSE"),
        None,
        "StopFrame must be checked before nested parse-time assignments"
    );
    drop(state);
    handler.finish_control(requester_pid, control_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn control_eof_ready_signal_wins_same_turn_cancellation() {
    let handler = RequestHandler::new();
    let requester_pid = 93_781;
    let channel = "control-eof-ready-signal-race";
    let (control_id, _control_events) = register_control(&handler, requester_pid).await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    let cancellation = ControlQueueEofCancellation::new(identity);
    let commands = CommandParser::new()
        .parse(&format!(
            "wait-for {channel} ; set-environment -g CONTROL_EOF_READY_SIGNAL won"
        ))
        .expect("control commands parse");
    let queued_handler = handler.clone();
    let queued_cancellation = cancellation.clone();
    let queued = tokio::spawn(async move {
        with_control_queue_eof_cancellation(
            queued_cancellation,
            queued_handler.execute_control_commands_identity(requester_pid, control_id, commands),
        )
        .await
    });
    wait_for_waiter(&handler, channel).await;

    // Current-thread runtime plus no await between these operations makes
    // both receiver and EOF ready before the waiter task can be repolled.
    handler
        .wait_for
        .lock()
        .expect("wait-for store")
        .signal(channel)
        .expect("signal waiter");
    cancellation.cancel_for_eof();

    let result = queued.await.expect("control queue joins");
    assert!(result.error.is_none(), "Ready signal stays successful");
    assert_eq!(handler.wait_for_counts(channel), (0, 0, false));
    let state = handler.state.lock().await;
    assert_eq!(
        state.environment.global_value("CONTROL_EOF_READY_SIGNAL"),
        Some("won"),
        "receiver-first bias must let the frame continue"
    );
    drop(state);
    handler.finish_control(requester_pid, control_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn control_eof_ready_lock_grant_wins_same_turn_cancellation() {
    let handler = RequestHandler::new();
    let requester_pid = 93_782;
    let channel = "control-eof-ready-lock-race";
    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: channel.to_owned(),
            mode: WaitForMode::Lock,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(_)), "{response:?}");
    let (control_id, _control_events) = register_control(&handler, requester_pid).await;
    let identity = ControlClientIdentity::new(requester_pid, control_id);
    let cancellation = ControlQueueEofCancellation::new(identity);
    let commands = CommandParser::new()
        .parse(&format!(
            "wait-for -L {channel} ; set-environment -g CONTROL_EOF_READY_LOCK won"
        ))
        .expect("control commands parse");
    let queued_handler = handler.clone();
    let queued_cancellation = cancellation.clone();
    let queued = tokio::spawn(async move {
        with_control_queue_eof_cancellation(
            queued_cancellation,
            queued_handler.execute_control_commands_identity(requester_pid, control_id, commands),
        )
        .await
    });
    wait_for_lock_waiter(&handler, channel).await;

    handler
        .wait_for
        .lock()
        .expect("wait-for store")
        .unlock(channel)
        .expect("grant lock waiter");
    cancellation.cancel_for_eof();

    let result = queued.await.expect("control queue joins");
    assert!(result.error.is_none(), "Ready lock grant stays successful");
    assert_eq!(
        handler.wait_for_counts(channel),
        (0, 0, true),
        "the receiver-first branch must accept and retain the lock grant"
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state.environment.global_value("CONTROL_EOF_READY_LOCK"),
        Some("won"),
        "accepted lock grant must let the frame continue"
    );
    drop(state);

    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: channel.to_owned(),
            mode: WaitForMode::Unlock,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(_)), "{response:?}");
    assert_eq!(handler.wait_for_counts(channel), (0, 0, false));
    handler.finish_control(requester_pid, control_id).await;
}

#[tokio::test]
async fn stale_control_queue_cannot_resolve_the_reused_pid_as_its_client() {
    let handler = RequestHandler::new();
    let requester_pid = 93_777;
    let (old_control_id, _old_events) = register_control(&handler, requester_pid).await;
    let (replacement_control_id, _replacement_events) =
        register_control(&handler, requester_pid).await;
    assert_ne!(replacement_control_id, old_control_id);

    let error = with_control_queue_identity(
        ControlClientIdentity::new(requester_pid, old_control_id),
        handler.resolve_target_managed_client(requester_pid, Some("="), "switch-client"),
    )
    .await
    .expect_err("stale queue must not resolve the replacement as its own client");

    assert!(matches!(error, RmuxError::Server(_)));
    assert_replacement_control_is_unbound(&handler, requester_pid, replacement_control_id).await;
}

#[tokio::test]
async fn stale_attach_outcome_cannot_bind_a_reused_control_pid() {
    let handler = RequestHandler::new();
    let requester_pid = 93_775;
    let session_name =
        SessionName::new("control-outcome-attach-pid-aba").expect("valid session name");
    create_session(&handler, session_name.clone()).await;
    let request = Request::AttachSession(AttachSessionRequest {
        target: session_name.clone(),
    });
    let outcome = handler.dispatch(requester_pid, request.clone()).await;
    let (old_control_id, _old_events) = register_control(&handler, requester_pid).await;
    let (replacement_control_id, _replacement_events) =
        register_control(&handler, requester_pid).await;

    let error = handler
        .control_queue_action_from_outcome(requester_pid, old_control_id, request, outcome)
        .await
        .expect_err("stale attach outcome must reject the replacement registration");
    assert!(matches!(error, RmuxError::Server(_)));
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name)
            .expect("session survives")
            .last_attached_at(),
        None
    );
    drop(state);
    assert_replacement_control_is_unbound(&handler, requester_pid, replacement_control_id).await;
}

#[tokio::test]
async fn stale_new_session_outcome_cannot_bind_a_reused_control_pid() {
    let handler = RequestHandler::new();
    let requester_pid = 93_776;
    let session_name = SessionName::new("control-outcome-new-pid-aba").expect("valid session name");
    let request = Request::NewSession(NewSessionRequest {
        session_name: session_name.clone(),
        detached: false,
        size: Some(TerminalSize { cols: 80, rows: 24 }),
        environment: None,
    });
    let outcome = handler.dispatch(requester_pid, request.clone()).await;
    let (old_control_id, _old_events) = register_control(&handler, requester_pid).await;
    let (replacement_control_id, _replacement_events) =
        register_control(&handler, requester_pid).await;

    let error = handler
        .control_queue_action_from_outcome(requester_pid, old_control_id, request, outcome)
        .await
        .expect_err("stale new-session outcome must reject the replacement registration");
    assert!(matches!(error, RmuxError::Server(_)));
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name)
            .expect("new session survives")
            .last_attached_at(),
        None
    );
    drop(state);
    assert_replacement_control_is_unbound(&handler, requester_pid, replacement_control_id).await;
}

#[tokio::test]
async fn control_queue_new_session_rejects_same_name_replacement_before_attach() {
    let handler = RequestHandler::new();
    let requester_pid = 93_778;
    let session_name =
        SessionName::new("control-queue-new-session-aba").expect("valid session name");
    let hook_response = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AfterNewSession,
            command: "set-environment -g CONTROL_NEW_SESSION_HOOK ran".to_owned(),
            lifecycle: HookLifecycle::Persistent,
        }))
        .await;
    assert!(
        matches!(hook_response, Response::SetHook(_)),
        "{hook_response:?}"
    );
    let pause = handler.install_created_session_control_attach_pause(session_name.clone());
    let (control_id, mut control_events) = register_control(&handler, requester_pid).await;
    let commands = CommandParser::new()
        .parse(&format!("new-session -s {session_name}"))
        .expect("new-session command parses");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_control_commands_identity(requester_pid, control_id, commands)
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("new-session reaches the pre-attach pause");
    let original_session_id = session_id(&handler, &session_name).await;
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    crate::hook_runtime::with_hook_execution(
        Vec::new(),
        create_session(&handler, session_name.clone()),
    )
    .await;
    let replacement_session_id = session_id(&handler, &session_name).await;
    let replacement_window_id = active_window_id(&handler, &session_name).await;
    assert_ne!(replacement_session_id, original_session_id);

    pause.release.notify_one();
    let result = queued.await.expect("control queue joins");
    assert!(
        result.error.is_some(),
        "stale new-session identity must fail closed"
    );

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&session_name)
                .expect("replacement session survives")
                .last_attached_at(),
            None,
            "the stale new-session must not touch the replacement"
        );
        assert_eq!(
            state.environment.global_value("CONTROL_NEW_SESSION_HOOK"),
            None,
            "the exact inline hook must not run against the replacement"
        );
    }
    {
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&requester_pid)
            .expect("control client remains registered");
        assert_eq!(active.id, control_id);
        assert_eq!(active.session_name, None);
        assert_eq!(active.session_id, None);
    }
    while let Ok(event) = control_events.try_recv() {
        match event {
            ControlServerEvent::SessionChanged(Some(changed_session)) => {
                assert_ne!(
                    changed_session, session_name,
                    "replacement must stay unbound"
                );
            }
            ControlServerEvent::Notification(line) => {
                assert_ne!(
                    line,
                    format!("%window-add @{replacement_window_id}"),
                    "replacement window must not be announced by the stale new-session"
                );
            }
            ControlServerEvent::SessionChanged(None)
            | ControlServerEvent::Refresh
            | ControlServerEvent::Exit(_) => {}
        }
    }
}

#[tokio::test]
async fn control_queue_new_session_attach_existing_rejects_same_name_replacement() {
    let handler = RequestHandler::new();
    let requester_pid = 93_779;
    let session_name =
        SessionName::new("control-queue-new-session-attach-aba").expect("valid session name");
    create_session(&handler, session_name.clone()).await;
    let original_session_id = session_id(&handler, &session_name).await;
    let pause = handler.install_created_session_control_attach_pause(session_name.clone());
    let (control_id, mut control_events) = register_control(&handler, requester_pid).await;
    let commands = CommandParser::new()
        .parse(&format!("new-session -A -D -s {session_name}"))
        .expect("new-session attach command parses");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_control_commands_identity(requester_pid, control_id, commands)
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("attach-if-exists reaches the pre-attach pause");
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    create_session(&handler, session_name.clone()).await;
    let replacement_session_id = session_id(&handler, &session_name).await;
    let replacement_window_id = active_window_id(&handler, &session_name).await;
    assert_ne!(replacement_session_id, original_session_id);
    let replacement_attach_pid = requester_pid + 1;
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    let replacement_attach_id = handler
        .register_attach(replacement_attach_pid, session_name.clone(), replacement_tx)
        .await;

    pause.release.notify_one();
    let result = queued.await.expect("control queue joins");
    assert!(
        result.error.is_some(),
        "stale attach-if-exists identity must fail closed"
    );

    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&session_name)
            .map(rmux_core::Session::id),
        Some(replacement_session_id),
        "replacement session survives"
    );
    {
        let active_attach = handler.active_attach.lock().await;
        let replacement = active_attach
            .by_pid
            .get(&replacement_attach_pid)
            .expect("replacement attach survives stale -D");
        assert_eq!(replacement.id, replacement_attach_id);
        assert_eq!(replacement.session_id, replacement_session_id);
        assert!(!replacement
            .closing
            .load(std::sync::atomic::Ordering::SeqCst));
    }
    while let Ok(control) = replacement_rx.try_recv() {
        assert!(
            !matches!(
                control,
                crate::pane_io::AttachControl::Detach | crate::pane_io::AttachControl::DetachKill
            ),
            "stale attach-if-exists must not detach the replacement client"
        );
    }
    {
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&requester_pid)
            .expect("control client remains registered");
        assert_eq!(active.id, control_id);
        assert_eq!(active.session_name, None);
        assert_eq!(active.session_id, None);
    }
    while let Ok(event) = control_events.try_recv() {
        match event {
            ControlServerEvent::SessionChanged(Some(changed_session)) => {
                assert_ne!(
                    changed_session, session_name,
                    "replacement must stay unbound"
                );
            }
            ControlServerEvent::Notification(line) => {
                assert_ne!(
                    line,
                    format!("%window-add @{replacement_window_id}"),
                    "replacement window must not be announced by stale attach-if-exists"
                );
            }
            ControlServerEvent::SessionChanged(None)
            | ControlServerEvent::Refresh
            | ControlServerEvent::Exit(_) => {}
        }
    }
}

#[tokio::test]
async fn control_queue_new_session_binds_captured_identity() {
    let handler = RequestHandler::new();
    let requester_pid = 93_780;
    let session_name =
        SessionName::new("control-queue-new-session-bind").expect("valid session name");
    let (control_id, _control_events) = register_control(&handler, requester_pid).await;
    let commands = CommandParser::new()
        .parse(&format!("new-session -s {session_name}"))
        .expect("new-session command parses");

    let result = handler
        .execute_control_commands_identity(requester_pid, control_id, commands)
        .await;
    assert_eq!(result.error, None, "{result:?}");

    let expected_session_id = session_id(&handler, &session_name).await;
    let active_control = handler.active_control.lock().await;
    let active = active_control
        .by_pid
        .get(&requester_pid)
        .expect("control client remains registered");
    assert_eq!(active.id, control_id);
    assert_eq!(active.session_name.as_ref(), Some(&session_name));
    assert_eq!(active.session_id, Some(expected_session_id));
}

#[tokio::test]
async fn control_queue_new_session_attach_existing_binds_captured_identity() {
    let handler = RequestHandler::new();
    let requester_pid = 93_781;
    let session_name =
        SessionName::new("control-queue-new-session-attach").expect("valid session name");
    create_session(&handler, session_name.clone()).await;
    let hook_response = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AfterNewSession,
            command: "set-environment -g CONTROL_ATTACH_EXISTING_HOOK ran".to_owned(),
            lifecycle: HookLifecycle::Persistent,
        }))
        .await;
    assert!(
        matches!(hook_response, Response::SetHook(_)),
        "{hook_response:?}"
    );
    let expected_session_id = session_id(&handler, &session_name).await;
    let expected_window_id = active_window_id(&handler, &session_name).await;
    let (control_id, mut control_events) = register_control(&handler, requester_pid).await;
    let commands = CommandParser::new()
        .parse(&format!("new-session -A -s {session_name}"))
        .expect("new-session attach command parses");

    let result = handler
        .execute_control_commands_identity(requester_pid, control_id, commands)
        .await;
    assert_eq!(result.error, None, "{result:?}");

    {
        let state = handler.state.lock().await;
        assert!(
            state
                .sessions
                .session(&session_name)
                .expect("session survives")
                .last_attached_at()
                .is_some(),
            "the selected session is touched exactly during the stable bind"
        );
        assert_eq!(
            state
                .environment
                .global_value("CONTROL_ATTACH_EXISTING_HOOK"),
            None,
            "new-session -A must not run after-new-session for an existing session"
        );
    }
    {
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&requester_pid)
            .expect("control client remains registered");
        assert_eq!(active.id, control_id);
        assert_eq!(active.session_name.as_ref(), Some(&session_name));
        assert_eq!(active.session_id, Some(expected_session_id));
    }
    let mut saw_session_changed = false;
    let mut saw_window_add = false;
    while let Ok(event) = control_events.try_recv() {
        match event {
            ControlServerEvent::SessionChanged(Some(changed_session)) => {
                saw_session_changed |= changed_session == session_name;
            }
            ControlServerEvent::Notification(line) => {
                saw_window_add |= line == format!("%window-add @{expected_window_id}");
            }
            ControlServerEvent::SessionChanged(None)
            | ControlServerEvent::Refresh
            | ControlServerEvent::Exit(_) => {}
        }
    }
    assert!(
        saw_session_changed,
        "control binding event must be delivered"
    );
    assert!(saw_window_add, "selected window must be announced");
}

async fn create_session(handler: &RequestHandler, session_name: SessionName) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
}

async fn session_id(handler: &RequestHandler, session_name: &SessionName) -> rmux_proto::SessionId {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session_name)
        .expect("session exists")
        .id()
}

async fn active_window_id(handler: &RequestHandler, session_name: &SessionName) -> u32 {
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(session_name)
        .expect("session exists");
    session
        .window_at(session.active_window_index())
        .expect("session has an active window")
        .id()
        .as_u32()
}

async fn register_control(
    handler: &RequestHandler,
    requester_pid: u32,
) -> (u64, mpsc::Receiver<ControlServerEvent>) {
    let (event_tx, event_rx) = mpsc::channel::<ControlServerEvent>(CONTROL_SERVER_EVENT_CAPACITY);
    let control_id = handler
        .register_control_with_closing(
            requester_pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    (control_id, event_rx)
}

async fn wait_for_waiter(handler: &RequestHandler, channel: &str) {
    for _ in 0..200 {
        if handler.wait_for_counts(channel).0 == 1 {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(handler.wait_for_counts(channel).0, 1);
}

async fn wait_for_lock_waiter(handler: &RequestHandler, channel: &str) {
    for _ in 0..200 {
        if handler.wait_for_counts(channel).1 == 1 {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(handler.wait_for_counts(channel).1, 1);
}

async fn signal_waiter(handler: &RequestHandler, channel: &str) {
    let response = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: channel.to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(response, Response::WaitFor(_)), "{response:?}");
}

async fn assert_replacement_control_is_unbound(
    handler: &RequestHandler,
    requester_pid: u32,
    replacement_control_id: u64,
) {
    let active_control = handler.active_control.lock().await;
    let replacement = active_control
        .by_pid
        .get(&requester_pid)
        .expect("replacement remains registered");
    assert_eq!(replacement.id, replacement_control_id);
    assert_eq!(replacement.session_name, None);
    assert_eq!(replacement.session_id, None);
}
