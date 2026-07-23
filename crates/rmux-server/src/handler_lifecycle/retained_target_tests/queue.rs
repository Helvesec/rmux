use std::time::Duration;

use super::*;
use crate::handler::scripting_support::install_queue_exact_target_capture_pause;
use crate::pane_io::AttachControl;
use tokio::sync::mpsc;

fn create_session_in_state(
    state: &mut crate::pane_terminals::HandlerState,
    name: &str,
) -> SessionName {
    let name = session_name(name);
    state
        .sessions
        .create_session(name.clone(), terminal_size())
        .expect("create session");
    name
}

async fn retained_alert_binding(
    handler: &RequestHandler,
    session_name: &SessionName,
) -> (Option<Target>, std::sync::Arc<LifecycleTargetLease>) {
    let mut state = handler.state.lock().await;
    let event = super::super::super::prepare_lifecycle_event(
        &mut state,
        &rmux_core::LifecycleEvent::AlertActivity {
            target: WindowTarget::with_window(session_name.clone(), 0),
        },
    );
    (
        event.current_target,
        event
            .retained_current_target
            .expect("activity event captures a retained window"),
    )
}

async fn wait_for_pause(pause: &crate::handler::scripting_support::QueueExactTargetCapturePause) {
    tokio::time::timeout(Duration::from_secs(2), pause.reached.notified())
        .await
        .expect("queued command reaches the post-parse pause");
}

#[tokio::test]
async fn parsed_implicit_mutation_revalidates_after_slot_replacement() {
    let handler = RequestHandler::new();
    let session_name = {
        let mut state = handler.state.lock().await;
        create_session_in_state(&mut state, "retained-parse-mutation-race")
    };
    let (current_target, lease) = retained_alert_binding(&handler, &session_name).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "rename-window");
    let command_handler = handler.clone();
    let command = tokio::spawn(async move {
        command_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                "rename-window stale-retarget",
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    let replacement_id = {
        let mut state = handler.state.lock().await;
        let session = state
            .sessions
            .session_mut(&session_name)
            .expect("session exists");
        session
            .remove_window_allowing_empty(0)
            .expect("remove parsed target");
        session
            .insert_window_with_initial_pane(0, terminal_size())
            .expect("replace parsed target");
        let replacement_id = session.window_at(0).expect("replacement exists").id();
        state.retire_removed_lifecycle_targets();
        replacement_id
    };
    pause.release.notify_one();

    let error = command
        .await
        .expect("queued command task joins")
        .expect_err("replacement must fail closed");
    assert!(error.to_string().contains("replaced"), "{error}");
    let state = handler.state.lock().await;
    let replacement = state
        .sessions
        .session(&session_name)
        .and_then(|session| session.window_at(0))
        .expect("replacement survives");
    assert_eq!(replacement.id(), replacement_id);
    assert_ne!(replacement.name(), Some("stale-retarget"));
}

#[tokio::test]
async fn implicit_pane_role_revalidates_after_same_window_slot_replacement() {
    let handler = RequestHandler::new();
    let session_name = {
        let mut state = handler.state.lock().await;
        let session_name = create_session_in_state(&mut state, "retained-pane-role");
        state
            .sessions
            .session_mut(&session_name)
            .expect("session exists")
            .split_active_pane()
            .expect("create second pane");
        session_name
    };
    let (current_target, lease) = retained_alert_binding(&handler, &session_name).await;

    handler
        .execute_hook_command_with_target_binding(
            std::process::id(),
            "send-keys",
            current_target.clone(),
            Some(lease.clone()),
        )
        .await
        .expect("a live window lease resolves its implicit pane role");

    let pause = install_queue_exact_target_capture_pause(&handler, "send-keys");
    let command_handler = handler.clone();
    let command = tokio::spawn(async move {
        command_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                "send-keys",
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .session_mut(&session_name)
            .expect("session remains live")
            .swap_panes(
                SessionPaneTarget::new(0, 0),
                SessionPaneTarget::new(0, 1),
                PaneSwapOptions::new(true, false),
            )
            .expect("swap pane identities across numeric slots");
    }
    pause.release.notify_one();

    let error = command
        .await
        .expect("send-keys task joins")
        .expect_err("pane slot replacement must fail closed");
    assert!(error.to_string().contains("replaced"), "{error}");
}

#[tokio::test]
async fn implicit_pane_role_rejects_respawned_output_generation() {
    let handler = RequestHandler::new();
    let session_name = create_handler_session(&handler, "retained-pane-respawn").await;
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let (pane_id, initial_generation) = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(rmux_core::Pane::id)
            .expect("target pane exists");
        (
            pane_id,
            state.pane_output_generation_for_target(&target, pane_id),
        )
    };
    let (current_target, lease) = retained_alert_binding(&handler, &session_name).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "send-keys");
    let command_handler = handler.clone();
    let command = tokio::spawn(async move {
        command_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                "send-keys x",
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    let respawned = handler
        .handle(Request::RespawnPane(Box::new(
            rmux_proto::RespawnPaneRequest {
                target: target.clone(),
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(vec![crate::test_shell::stdin_discard_command()]),
                process_command: None,
            },
        )))
        .await;
    assert!(
        matches!(respawned, Response::RespawnPane(_)),
        "{respawned:?}"
    );
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .map(rmux_core::Pane::id),
            Some(pane_id)
        );
        assert_ne!(
            state.pane_output_generation_for_target(&target, pane_id),
            initial_generation
        );
    }
    pause.release.notify_one();

    let error = command
        .await
        .expect("send-keys task joins")
        .expect_err("the retained command must not reach the respawned process");
    assert!(
        error.to_string().contains("target identity changed"),
        "{error}"
    );
}

#[tokio::test]
async fn explicit_send_keys_revalidates_before_copy_mode_effects() {
    let handler = RequestHandler::new();
    let session_name = create_handler_session(&handler, "retained-send-keys-copy-mode").await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session_name.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let (current_target, lease) = retained_alert_binding(&handler, &session_name).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "send-keys");
    let command_handler = handler.clone();
    let command_text = format!("send-keys -t {target} q");
    let command = tokio::spawn(async move {
        command_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &command_text,
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .session_mut(&session_name)
            .expect("session remains live")
            .swap_panes(
                SessionPaneTarget::new(0, 0),
                SessionPaneTarget::new(0, 1),
                PaneSwapOptions::new(true, false),
            )
            .expect("replace the explicit numeric pane slot");
    }
    let entered = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: false,
        }))
        .await;
    assert!(matches!(entered, Response::CopyMode(_)), "{entered:?}");
    pause.release.notify_one();

    let error = command
        .await
        .expect("send-keys task joins")
        .expect_err("replacement must be rejected before copy-mode consumes q");
    assert!(error.to_string().contains("replaced"), "{error}");
    let state = handler.state.lock().await;
    let transcript = state
        .transcript_handle(&target)
        .expect("replacement transcript remains available");
    assert!(
        transcript
            .lock()
            .expect("replacement transcript lock")
            .copy_mode_state()
            .is_some(),
        "rejected send-keys must not cancel copy mode on the replacement pane"
    );
}

#[tokio::test]
async fn join_pane_explicit_source_keeps_retained_destination() {
    let handler = RequestHandler::new();
    let alpha = create_handler_session(&handler, "retained-join-destination").await;
    let beta = create_handler_session(&handler, "retained-join-source").await;
    let (current_target, lease) = retained_alert_binding(&handler, &alpha).await;

    handler
        .execute_hook_command_with_target_binding(
            std::process::id(),
            &format!("join-pane -s {beta}:0.0"),
            current_target,
            Some(lease),
        )
        .await
        .expect("explicit source and retained destination succeed");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .map(|window| window.panes().len()),
        Some(2)
    );
    assert!(state.sessions.session(&beta).is_none());
}

#[tokio::test]
async fn join_pane_explicit_destination_keeps_retained_source() {
    let handler = RequestHandler::new();
    let alpha = create_handler_session(&handler, "retained-join-source").await;
    let beta = create_handler_session(&handler, "retained-join-destination").await;
    let (current_target, lease) = retained_alert_binding(&handler, &alpha).await;

    handler
        .execute_hook_command_with_target_binding(
            std::process::id(),
            &format!("join-pane -t {beta}:0.0"),
            current_target,
            Some(lease),
        )
        .await
        .expect("retained source and explicit destination succeed");

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alpha).is_none());
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(0))
            .map(|window| window.panes().len()),
        Some(2)
    );
}

#[tokio::test]
async fn join_pane_implicit_source_revalidates_after_slot_replacement() {
    let handler = RequestHandler::new();
    let (alpha, beta) = {
        let mut state = handler.state.lock().await;
        (
            create_session_in_state(&mut state, "retained-join-race-source"),
            create_session_in_state(&mut state, "retained-join-race-destination"),
        )
    };
    let (current_target, lease) = retained_alert_binding(&handler, &alpha).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "join-pane");
    let command_handler = handler.clone();
    let command_text = format!("join-pane -t {beta}:0.0");
    let command = tokio::spawn(async move {
        command_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &command_text,
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    {
        let mut state = handler.state.lock().await;
        let session = state.sessions.session_mut(&alpha).expect("source exists");
        session
            .remove_window_allowing_empty(0)
            .expect("remove implicit source");
        session
            .insert_window_with_initial_pane(0, terminal_size())
            .expect("replace implicit source");
        state.retire_removed_lifecycle_targets();
    }
    pause.release.notify_one();

    let error = command
        .await
        .expect("join task joins")
        .expect_err("replaced implicit source must fail closed");
    assert!(error.to_string().contains("replaced"), "{error}");
    let state = handler.state.lock().await;
    for session_name in [&alpha, &beta] {
        assert_eq!(
            state
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(0))
                .map(|window| window.panes().len()),
            Some(1)
        );
    }
}

async fn assert_session_command_rejects_stale_window(command_name: &str, command: &str) {
    for replace in [false, true] {
        let handler = RequestHandler::new();
        let session_name = {
            let mut state = handler.state.lock().await;
            let session_name =
                create_session_in_state(&mut state, &format!("retained-{command_name}-{replace}"));
            state
                .sessions
                .session_mut(&session_name)
                .expect("session exists")
                .create_window(terminal_size())
                .expect("create surviving window");
            session_name
        };
        let (current_target, lease) = retained_alert_binding(&handler, &session_name).await;
        let pause = install_queue_exact_target_capture_pause(&handler, command_name);
        let command_handler = handler.clone();
        let command_text = command.to_owned();
        let queued = tokio::spawn(async move {
            command_handler
                .execute_hook_command_with_target_binding(
                    std::process::id(),
                    &command_text,
                    current_target,
                    Some(lease),
                )
                .await
        });
        wait_for_pause(&pause).await;

        {
            let mut state = handler.state.lock().await;
            let session = state
                .sessions
                .session_mut(&session_name)
                .expect("session survives");
            session
                .remove_window_allowing_empty(0)
                .expect("remove retained window");
            if replace {
                session
                    .insert_window_with_initial_pane(0, terminal_size())
                    .expect("replace retained window");
            }
            state.retire_removed_lifecycle_targets();
        }
        pause.release.notify_one();

        let error = queued
            .await
            .expect("session command task joins")
            .expect_err("stale lifecycle window must fail closed");
        let phase = if replace { "replaced" } else { "retired" };
        assert!(error.to_string().contains(phase), "{error}");
    }
}

#[tokio::test]
async fn queued_new_window_rejects_retired_and_replaced_implicit_window() {
    assert_session_command_rejects_stale_window(
        "new-window",
        "new-window -d -n stale-retained-window",
    )
    .await;
}

#[tokio::test]
async fn queued_lock_session_rejects_retired_and_replaced_implicit_window() {
    assert_session_command_rejects_stale_window("lock-session", "lock-session").await;
}

#[tokio::test]
async fn queued_new_window_relative_target_keeps_its_captured_anchor() {
    let handler = RequestHandler::new();
    let session_name = create_handler_session(&handler, "queued-new-window-relative").await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            target_window_index: Some(2),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    {
        let mut state = handler.state.lock().await;
        let session = state
            .sessions
            .session_mut(&session_name)
            .expect("session exists");
        session.select_window(0).expect("select relative anchor");
    }
    let (current_target, lease) = retained_alert_binding(&handler, &session_name).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "new-window");
    let queued_handler = handler.clone();
    let command = format!("new-window -d -t {session_name}:+1 -n captured-relative");
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &command,
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    handler
        .state
        .lock()
        .await
        .sessions
        .session_mut(&session_name)
        .expect("session exists")
        .select_window(2)
        .expect("move the live active cursor after capture");
    pause.release.notify_one();
    queued
        .await
        .expect("relative new-window task joins")
        .expect("captured relative destination remains valid");

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&session_name)
        .expect("session exists");
    assert_eq!(
        session.window_at(1).and_then(rmux_core::Window::name),
        Some("captured-relative")
    );
    assert!(session.window_at(3).is_none());
}

#[tokio::test]
async fn queued_new_window_insert_rejects_replaced_before_and_after_anchors() {
    for flag in ["-a", "-b"] {
        let handler = RequestHandler::new();
        let session_name = {
            let mut state = handler.state.lock().await;
            let session_name =
                create_session_in_state(&mut state, &format!("queued-new-window-anchor-{flag}"));
            state
                .sessions
                .session_mut(&session_name)
                .expect("session exists")
                .insert_window_with_initial_pane(1, terminal_size())
                .expect("create neighboring window");
            session_name
        };
        let (current_target, lease) = retained_alert_binding(&handler, &session_name).await;
        let pause = install_queue_exact_target_capture_pause(&handler, "new-window");
        let queued_handler = handler.clone();
        let command = format!("new-window -d {flag} -t {session_name}:0 -n stale-placement-anchor");
        let queued = tokio::spawn(async move {
            queued_handler
                .execute_hook_command_with_target_binding(
                    std::process::id(),
                    &command,
                    current_target,
                    Some(lease),
                )
                .await
        });
        wait_for_pause(&pause).await;

        let replacement_id = {
            let mut state = handler.state.lock().await;
            let session = state
                .sessions
                .session_mut(&session_name)
                .expect("session exists");
            session
                .remove_window_allowing_empty(0)
                .expect("remove captured anchor");
            session
                .insert_window_with_initial_pane(0, terminal_size())
                .expect("replace captured anchor");
            let replacement_id = session.window_at(0).expect("replacement exists").id();
            state.retire_removed_lifecycle_targets();
            replacement_id
        };
        pause.release.notify_one();

        let error = queued
            .await
            .expect("placement new-window task joins")
            .expect_err("replacement anchor must invalidate queued placement");
        assert!(error.to_string().contains("changed"), "{error}");
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        assert_eq!(
            session.window_at(0).expect("replacement survives").id(),
            replacement_id
        );
        assert!(session
            .windows()
            .values()
            .all(|window| window.name() != Some("stale-placement-anchor")));
    }
}

#[tokio::test]
async fn queued_new_window_kill_revalidates_the_destination_at_replacement() {
    let handler = RequestHandler::new();
    let session_name = create_handler_session(&handler, "queued-new-window-kill-occupant").await;
    let created_spare = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: Some("replacement".to_owned()),
            detached: true,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(created_spare, Response::NewWindow(_)));
    let (current_target, lease) = retained_alert_binding(&handler, &session_name).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "new-window");
    let queued_handler = handler.clone();
    let command = format!("new-window -dk -t {session_name}:0 -n stale-kill-window");
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &command,
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    let moved = handler
        .handle_move_window(rmux_proto::MoveWindowRequest {
            source: Some(WindowTarget::with_window(session_name.clone(), 1)),
            target: rmux_proto::MoveWindowTarget::Window(WindowTarget::with_window(
                session_name.clone(),
                0,
            )),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        })
        .await;
    let replacement_id = match moved {
        Response::MoveWindow(response) => {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&session_name)
                .and_then(|session| {
                    session.window_at(response.target.expect("move target").window_index())
                })
                .expect("replacement exists")
                .id()
        }
        response => panic!("replacement move failed: {response:?}"),
    };
    let hook = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AfterNewWindow,
            command: Some("set-buffer -b rejected-new-window-hook ran".to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(hook, Response::SetHook(_)), "{hook:?}");
    let mut lifecycle = handler.subscribe_lifecycle_events();
    pause.release.notify_one();

    let error = queued
        .await
        .expect("kill-existing new-window task joins")
        .expect_err("replacement destination must not be killed");
    assert!(error.to_string().contains("changed"), "{error}");
    let state = handler.state.lock().await;
    let replacement = state
        .sessions
        .session(&session_name)
        .and_then(|session| session.window_at(0))
        .expect("replacement destination survives");
    assert_eq!(replacement.id(), replacement_id);
    assert_ne!(replacement.name(), Some("stale-kill-window"));
    let session = state
        .sessions
        .session(&session_name)
        .expect("destination session survives");
    assert_eq!(session.windows().len(), 1);
    assert!(session
        .windows()
        .values()
        .all(|window| window.name() != Some("stale-kill-window")));
    assert!(state
        .buffers
        .show(Some("rejected-new-window-hook"))
        .is_err());
    drop(state);
    assert!(
        lifecycle.try_recv().is_err(),
        "rejected transaction must publish no window lifecycle event"
    );
}

#[tokio::test]
async fn queued_new_window_kill_publishes_only_the_committed_window_identity() {
    let handler = RequestHandler::new();
    let session_name = create_handler_session(&handler, "queued-new-window-transaction").await;
    let hook = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AfterNewWindow,
            command: Some("set-buffer -b committed-new-window-hook ran".to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(hook, Response::SetHook(_)), "{hook:?}");
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let (current_target, lease) = retained_alert_binding(&handler, &session_name).await;
    let lifecycle_pause = handler.install_window_lifecycle_emit_pause();
    let queued_handler = handler.clone();
    let command = format!("new-window -dk -t {session_name}:0 -n committed-window");
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &command,
                current_target,
                Some(lease),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), lifecycle_pause.reached.notified())
        .await
        .expect("committed move reaches lifecycle publication");
    {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("session survives");
        assert_eq!(
            session.windows().len(),
            1,
            "temporary window must be gone before lifecycle publication"
        );
        assert_eq!(
            session.window_at(0).and_then(rmux_core::Window::name),
            Some("committed-window")
        );
        assert!(
            state
                .buffers
                .show(Some("committed-new-window-hook"))
                .is_err(),
            "after-new-window must wait for the committed lifecycle effects"
        );
    }
    lifecycle_pause.release.notify_one();
    queued
        .await
        .expect("transactional replacement task joins")
        .expect("transactional replacement succeeds");

    let (committed_target, committed_window_id) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("session survives");
        assert_eq!(session.windows().len(), 1);
        let committed = session.window_at(0).expect("committed window exists");
        assert_eq!(committed.name(), Some("committed-window"));
        assert_eq!(
            state
                .buffers
                .show(Some("committed-new-window-hook"))
                .expect("after-new-window runs once")
                .1,
            b"ran"
        );
        (
            WindowTarget::with_window(session_name.clone(), 0),
            committed.id(),
        )
    };
    let mut observed = Vec::new();
    while let Ok(event) = lifecycle.try_recv() {
        match event.event {
            rmux_core::LifecycleEvent::WindowLinked { target, .. } => {
                assert_eq!(target, Some(committed_target.clone()));
                observed.push("linked");
            }
            rmux_core::LifecycleEvent::WindowUnlinked { window_id, .. } => {
                assert_eq!(window_id, Some(committed_window_id.as_u32()));
                observed.push("unlinked");
            }
            _ => {}
        }
    }
    assert_eq!(
        observed,
        ["linked", "unlinked"],
        "tmux 3.7b publishes the final WindowId as linked then unlinked before after-new-window"
    );
}

#[tokio::test]
async fn implicit_session_role_rejects_a_post_parse_alias_move() {
    let handler = RequestHandler::new();
    let (alpha, beta) = {
        let mut state = handler.state.lock().await;
        let alpha = create_session_in_state(&mut state, "retained-alias-move-alpha");
        let beta = create_session_in_state(&mut state, "retained-alias-move-beta");
        state
            .sessions
            .session_mut(&alpha)
            .expect("source session exists")
            .create_window(terminal_size())
            .expect("source session keeps a spare window");
        let retained_window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .cloned()
            .expect("retained window exists");
        state
            .sessions
            .session_mut(&beta)
            .expect("alias session exists")
            .link_window(1, retained_window, false, false)
            .expect("link retained window into alias session");
        (alpha, beta)
    };
    let (current_target, lease) = retained_alert_binding(&handler, &alpha).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "new-window");
    let command_handler = handler.clone();
    let queued = tokio::spawn(async move {
        command_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                "new-window -d -n stale-alias",
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .session_mut(&alpha)
            .expect("source session survives")
            .remove_window_allowing_empty(0)
            .expect("unlink parsed alias while stable window survives elsewhere");
        state.retire_removed_lifecycle_targets();
    }
    pause.release.notify_one();

    let error = queued
        .await
        .expect("alias move task joins")
        .expect_err("post-parse alias move must invalidate the immutable witness");
    assert!(error.to_string().contains("changed"), "{error}");
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&alpha)
            .expect("source session remains")
            .windows()
            .len(),
        1
    );
    assert!(state
        .sessions
        .session(&beta)
        .and_then(|session| session.window_at(1))
        .is_some());
}

async fn assert_read_rejects_replaced_target(command_name: &str, command: &str) {
    let handler = RequestHandler::new();
    let session_name = {
        let mut state = handler.state.lock().await;
        create_session_in_state(&mut state, &format!("retained-read-{command_name}"))
    };
    let (current_target, lease) = retained_alert_binding(&handler, &session_name).await;
    let pause = install_queue_exact_target_capture_pause(&handler, command_name);
    let command_handler = handler.clone();
    let command_text = command.to_owned();
    let queued = tokio::spawn(async move {
        command_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &command_text,
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    {
        let mut state = handler.state.lock().await;
        let session = state
            .sessions
            .session_mut(&session_name)
            .expect("session exists");
        session
            .remove_window_allowing_empty(0)
            .expect("remove parsed read target");
        session
            .insert_window_with_initial_pane(0, terminal_size())
            .expect("replace parsed read target");
        state.retire_removed_lifecycle_targets();
    }
    pause.release.notify_one();

    let error = queued
        .await
        .expect("read task joins")
        .expect_err("stale read must fail closed");
    assert!(error.to_string().contains("replaced"), "{error}");
}

#[tokio::test]
async fn display_message_read_revalidates_after_slot_replacement() {
    assert_read_rejects_replaced_target(
        "display-message",
        "display-message -p '#{window_id}:#{pane_id}'",
    )
    .await;
}

#[tokio::test]
async fn target_client_display_follows_the_captured_registration_not_the_lifecycle_target() {
    let handler = RequestHandler::new();
    let lifecycle = create_handler_session(&handler, "display-client-lifecycle").await;
    let before_switch = create_handler_session(&handler, "display-client-before-switch").await;
    let after_switch = create_handler_session(&handler, "display-client-after-switch").await;
    let attach_pid = 91_941;
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, before_switch, control_tx)
        .await;
    let (current_target, lease) = retained_alert_binding(&handler, &lifecycle).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "display-message");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &format!("display-message -c {attach_pid} 'format #{{session_name}}'"),
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    let response = handler
        .dispatch(
            attach_pid,
            Request::SwitchClient(rmux_proto::SwitchClientRequest {
                target: after_switch.clone(),
            }),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::SwitchClient(_)),
        "{response:?}"
    );
    while control_rx.try_recv().is_ok() {}
    pause.release.notify_one();
    queued
        .await
        .expect("queued display task joins")
        .expect("the same attach registration remains valid after switching");

    let frame = std::iter::from_fn(|| control_rx.try_recv().ok())
        .find_map(|control| match control {
            AttachControl::Overlay(overlay) => String::from_utf8(overlay.frame).ok(),
            _ => None,
        })
        .expect("target client receives its display overlay");
    assert!(
        frame.contains("format display-client-after-switch"),
        "{frame:?}"
    );
    assert!(!frame.contains(lifecycle.as_str()), "{frame:?}");
}

#[tokio::test]
async fn target_client_display_rejects_a_recreated_attach_registration() {
    let handler = RequestHandler::new();
    let lifecycle = create_handler_session(&handler, "display-client-recreate-lifecycle").await;
    let original = create_handler_session(&handler, "display-client-recreate-original").await;
    let replacement = create_handler_session(&handler, "display-client-recreate-replacement").await;
    let attach_pid = 91_942;
    let (original_tx, _original_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, original, original_tx)
        .await;
    let (current_target, lease) = retained_alert_binding(&handler, &lifecycle).await;
    let pause = install_queue_exact_target_capture_pause(&handler, "display-message");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &format!("display-message -c {attach_pid} 'stale #{{session_name}}'"),
                current_target,
                Some(lease),
            )
            .await
    });
    wait_for_pause(&pause).await;

    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, replacement, replacement_tx)
        .await;
    pause.release.notify_one();

    let error = queued
        .await
        .expect("queued display task joins")
        .expect_err("a replacement registration must not inherit queued client work");
    assert!(error.to_string().contains("disappeared"), "{error}");
    assert!(
        std::iter::from_fn(|| replacement_rx.try_recv().ok())
            .all(|control| !matches!(control, AttachControl::Overlay(_))),
        "replacement attach must not receive the stale overlay"
    );
}

#[tokio::test]
async fn capture_pane_read_revalidates_after_slot_replacement() {
    assert_read_rejects_replaced_target("capture-pane", "capture-pane -p").await;
}

#[tokio::test]
async fn explicit_target_bypasses_retired_lifecycle_role_and_remains_pinned() {
    for replace in [false, true] {
        let handler = RequestHandler::new();
        let alpha = create_handler_session(&handler, &format!("retained-explicit-{replace}")).await;
        let beta =
            create_handler_session(&handler, &format!("explicit-destination-{replace}")).await;
        let (current_target, lease) = retained_alert_binding(&handler, &alpha).await;
        {
            let mut state = handler.state.lock().await;
            let session = state.sessions.session_mut(&alpha).expect("source exists");
            session
                .remove_window_allowing_empty(0)
                .expect("remove implicit lifecycle window");
            if replace {
                session
                    .insert_window_with_initial_pane(0, terminal_size())
                    .expect("replace implicit lifecycle window");
            }
            state.retire_removed_lifecycle_targets();
        }

        handler
            .execute_hook_command_with_target_binding(
                std::process::id(),
                &format!("rename-window -t {beta}:0 explicit-target"),
                current_target,
                Some(lease),
            )
            .await
            .expect("explicit target bypasses stale implicit lifecycle role");

        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&beta)
                .and_then(|session| session.window_at(0))
                .and_then(rmux_core::Window::name),
            Some("explicit-target")
        );
    }
}
