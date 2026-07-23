use std::time::Duration;

use super::RequestHandler;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    HookLifecycle, HookName, LinkWindowRequest, NewSessionRequest, NewWindowRequest, OptionName,
    RenameSessionRequest, Request, Response, ScopeSelector, SelectWindowRequest, SessionName,
    SetHookMutationRequest, SetOptionMode, SetOptionRequest, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, TerminalSize, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, name: &str) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name(name),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
}

async fn set_indexed_global_hook(
    handler: &RequestHandler,
    hook: HookName,
    index: u32,
    command: &str,
) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: Some(index),
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn wait_for_session_absent(handler: &RequestHandler, name: &SessionName) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if handler.state.lock().await.sessions.session(name).is_none() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for session {name} to disappear"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn session_renamed_hook_chain_keeps_target_identity_after_first_hook_renames_again() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    create_session(&handler, "keeper").await;
    set_indexed_global_hook(
        &handler,
        HookName::SessionRenamed,
        0,
        "rename-session -t beta gamma",
    )
    .await;
    set_indexed_global_hook(&handler, HookName::SessionRenamed, 1, "kill-session").await;

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name("alpha"),
            new_name: session_name("beta"),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );

    // "beta" exists after the rename unless the first hook renamed it away;
    // waiting on it (not only on "gamma", which never exists if the chain
    // never ran) proves the hook chain actually executed.
    wait_for_session_absent(&handler, &session_name("beta")).await;
    wait_for_session_absent(&handler, &session_name("gamma")).await;
    let state = handler.state.lock().await;
    assert!(state.sessions.session(&session_name("keeper")).is_some());
}

#[tokio::test]
async fn session_hook_chain_falls_back_to_stable_window_after_current_pane_is_killed() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session_name("alpha")),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
    set_indexed_global_hook(&handler, HookName::SessionRenamed, 0, "kill-pane").await;
    set_indexed_global_hook(
        &handler,
        HookName::SessionRenamed,
        1,
        "set-buffer -b stable-parent continued",
    )
    .await;

    let queued = {
        let mut state = handler.state.lock().await;
        super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::SessionRenamed {
                session_name: session_name("alpha"),
            },
        )
    };
    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name("alpha"))
            .and_then(|session| session.window_at(0))
            .expect("stable parent window survives")
            .panes()
            .len(),
        1
    );
    assert_eq!(
        state
            .buffers
            .show(Some("stable-parent"))
            .expect("second hook runs against the surviving parent")
            .1,
        b"continued"
    );
}

#[tokio::test]
async fn session_window_changed_hook_chain_follows_session_through_rename() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    create_session(&handler, "keeper").await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name("alpha"),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    set_indexed_global_hook(
        &handler,
        HookName::SessionWindowChanged,
        0,
        "rename-session -t alpha beta",
    )
    .await;
    set_indexed_global_hook(&handler, HookName::SessionWindowChanged, 1, "kill-session").await;

    let response = handler
        .handle(Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(session_name("alpha"), 1),
        }))
        .await;
    assert!(
        matches!(response, Response::SelectWindow(_)),
        "{response:?}"
    );

    // "alpha" survives unless the first hook renamed it away; waiting on it
    // (not only on "beta", which never exists if the chain never ran) proves
    // the hook chain actually executed.
    wait_for_session_absent(&handler, &session_name("alpha")).await;
    wait_for_session_absent(&handler, &session_name("beta")).await;
    let state = handler.state.lock().await;
    assert!(state.sessions.session(&session_name("keeper")).is_some());
}

#[tokio::test]
async fn window_hook_chain_follows_window_identity_across_session_move() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    set_indexed_global_hook(
        &handler,
        HookName::AlertActivity,
        0,
        "move-window -s alpha:0 -t beta:1",
    )
    .await;
    set_indexed_global_hook(
        &handler,
        HookName::AlertActivity,
        1,
        "rename-window second-ran",
    )
    .await;

    let queued = {
        let mut state = handler.state.lock().await;
        super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::AlertActivity {
                target: WindowTarget::with_window(session_name("alpha"), 0),
            },
        )
    };
    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&session_name("alpha")).is_none());
    let moved = state
        .sessions
        .session(&session_name("beta"))
        .and_then(|session| session.window_at(1))
        .expect("moved window survives in beta");
    assert_eq!(moved.name(), Some("second-ran"));
}

#[tokio::test]
async fn window_hook_chain_follows_surviving_cross_session_alias_after_unlink() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name("alpha"),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(session_name("alpha"), 0),
            target: WindowTarget::with_window(session_name("beta"), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    set_indexed_global_hook(
        &handler,
        HookName::AlertActivity,
        0,
        "unlink-window -t alpha:0",
    )
    .await;
    set_indexed_global_hook(
        &handler,
        HookName::AlertActivity,
        1,
        "set-buffer -b stable-alias continued",
    )
    .await;

    let queued = {
        let mut state = handler.state.lock().await;
        super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::AlertActivity {
                target: WindowTarget::with_window(session_name("alpha"), 0),
            },
        )
    };
    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    assert!(state
        .sessions
        .session(&session_name("alpha"))
        .and_then(|session| session.window_at(0))
        .is_none());
    assert!(state
        .sessions
        .session(&session_name("beta"))
        .and_then(|session| session.window_at(1))
        .is_some());
    assert_eq!(
        state
            .buffers
            .show(Some("stable-alias"))
            .expect("second hook creates buffer")
            .1,
        b"continued"
    );
}

#[tokio::test]
async fn stable_alias_resolution_prefers_newest_surviving_session() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    create_session(&handler, "gamma").await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name("alpha"),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    for peer in ["gamma", "beta"] {
        let response = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(session_name("alpha"), 0),
                target: WindowTarget::with_window(session_name(peer), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await;
        assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    }
    let gamma_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&session_name("gamma"))
        .expect("gamma exists")
        .id();
    set_indexed_global_hook(
        &handler,
        HookName::AlertActivity,
        0,
        "unlink-window -t alpha:0",
    )
    .await;
    set_indexed_global_hook(
        &handler,
        HookName::AlertActivity,
        1,
        "rename-session chosen",
    )
    .await;

    let queued = {
        let mut state = handler.state.lock().await;
        super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::AlertActivity {
                target: WindowTarget::with_window(session_name("alpha"), 0),
            },
        )
    };
    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name("chosen"))
            .expect("newest surviving alias session is renamed")
            .id(),
        gamma_id
    );
    assert!(state.sessions.session(&session_name("beta")).is_some());
    assert!(state.sessions.session(&session_name("gamma")).is_none());
}

#[tokio::test]
async fn stable_alias_resolution_prefers_lowest_index_within_session() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name("alpha"),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    for window_index in [3, 2] {
        let response = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(session_name("alpha"), 0),
                target: WindowTarget::with_window(session_name("beta"), window_index),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await;
        assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    }
    set_indexed_global_hook(
        &handler,
        HookName::AlertActivity,
        0,
        "unlink-window -t alpha:0",
    )
    .await;
    set_indexed_global_hook(&handler, HookName::AlertActivity, 1, "unlink-window").await;

    let queued = {
        let mut state = handler.state.lock().await;
        super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::AlertActivity {
                target: WindowTarget::with_window(session_name("alpha"), 0),
            },
        )
    };
    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    let beta = state
        .sessions
        .session(&session_name("beta"))
        .expect("beta survives");
    assert!(beta.window_at(2).is_none());
    assert!(beta.window_at(3).is_some());
}

#[tokio::test]
async fn destroyed_session_hook_does_not_target_recreated_same_name() {
    let handler = RequestHandler::new();
    {
        let mut state = handler.state.lock().await;
        for name in ["alpha", "keeper"] {
            state
                .sessions
                .create_session(session_name(name), TerminalSize { cols: 80, rows: 24 })
                .expect("create in-memory session");
        }
    }
    set_indexed_global_hook(&handler, HookName::SessionClosed, 0, "kill-session").await;

    let queued = {
        let mut state = handler.state.lock().await;
        let alpha = session_name("alpha");
        let session_id = state
            .sessions
            .session(&alpha)
            .expect("original alpha exists")
            .id()
            .as_u32();
        let queued = super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::SessionClosed {
                session_name: alpha.clone(),
                session_id: Some(session_id),
            },
        );
        state
            .sessions
            .remove_session(&alpha)
            .expect("remove original alpha");
        queued
    };

    let replacement_id = {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .create_session(session_name("alpha"), TerminalSize { cols: 80, rows: 24 })
            .expect("create replacement alpha");
        state
            .sessions
            .session(&session_name("alpha"))
            .expect("replacement alpha exists")
            .id()
    };

    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name("alpha"))
            .expect("replacement alpha survives")
            .id(),
        replacement_id
    );
    assert!(state.sessions.session(&session_name("keeper")).is_some());
}

#[tokio::test]
async fn removed_window_hook_does_not_target_recreated_same_slot() {
    let handler = RequestHandler::new();
    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .create_session(session_name("alpha"), TerminalSize { cols: 80, rows: 24 })
            .expect("create original alpha");
    }
    set_indexed_global_hook(&handler, HookName::WindowUnlinked, 0, "kill-window").await;

    let (queued, replacement_window_id) = {
        let mut state = handler.state.lock().await;
        let alpha = session_name("alpha");
        let removed = state
            .sessions
            .remove_session(&alpha)
            .expect("remove original alpha");
        let removed_window = removed.window_at(0).expect("removed window exists");
        let queued = super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::WindowUnlinked {
                session_name: alpha.clone(),
                target: Some(WindowTarget::with_window(alpha.clone(), 0)),
                window_id: Some(removed_window.id().as_u32()),
                window_name: removed_window.name().map(str::to_owned),
            },
        );
        state
            .sessions
            .create_session(alpha.clone(), TerminalSize { cols: 80, rows: 24 })
            .expect("create replacement alpha");
        let replacement_window_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("replacement window exists")
            .id();
        (queued, replacement_window_id)
    };

    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name("alpha"))
            .and_then(|session| session.window_at(0))
            .expect("replacement window survives")
            .id(),
        replacement_window_id
    );
}

#[tokio::test]
async fn window_unlinked_hook_tracks_the_selected_winlink_occurrence_through_slot_aba() {
    let handler = RequestHandler::new();
    let session = session_name("window-unlinked-occurrence-aba");
    create_session(&handler, session.as_str()).await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(session.clone(), 0),
            target: WindowTarget::with_window(session.clone(), 2),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;
    for (index, command) in [
        (0, format!("move-window -s {session}:0 -t {session}:5")),
        (1, format!("link-window -s {session}:2 -t {session}:0")),
        (
            2,
            "set-option -g -F @window-unlinked-occurrence-aba '#{window_index}'".to_owned(),
        ),
    ] {
        set_indexed_global_hook(&handler, HookName::WindowUnlinked, index, &command).await;
    }

    let queued = {
        let mut state = handler.state.lock().await;
        let window_id = state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .expect("selected winlink occurrence exists")
            .id()
            .as_u32();
        super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::WindowUnlinked {
                session_name: session.clone(),
                target: Some(WindowTarget::with_window(session.clone(), 0)),
                window_id: Some(window_id),
                window_name: Some("occurrence-aba".to_owned()),
            },
        )
    };

    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .options
            .resolve_name(Some(&session), "@window-unlinked-occurrence-aba"),
        Some("5".to_owned()),
        "hook follows the original occurrence",
    );
}

#[tokio::test]
async fn window_unlinked_hook_fails_closed_after_the_selected_occurrence_is_relinked() {
    let handler = RequestHandler::new();
    let session = session_name("window-unlinked-occurrence-replaced");
    create_session(&handler, session.as_str()).await;
    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(session.clone(), 0),
                target: WindowTarget::with_window(session.clone(), 2),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await,
        Response::LinkWindow(_)
    ));
    handler.wait_for_initial_panes_for_test().await;
    set_indexed_global_hook(
        &handler,
        HookName::WindowUnlinked,
        0,
        "set-buffer -b window-unlinked-occurrence-replaced unexpected",
    )
    .await;

    let queued = {
        let mut state = handler.state.lock().await;
        let window_id = state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .expect("selected occurrence exists")
            .id()
            .as_u32();
        super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::WindowUnlinked {
                session_name: session.clone(),
                target: Some(WindowTarget::with_window(session.clone(), 0)),
                window_id: Some(window_id),
                window_name: Some("occurrence-replaced".to_owned()),
            },
        )
    };

    {
        let mut state = handler.state.lock().await;
        state
            .unlink_window(WindowTarget::with_window(session.clone(), 0), false)
            .expect("selected occurrence unlinks without dispatching its hook");
        state.retire_removed_lifecycle_targets();
        state
            .link_window(LinkWindowRequest {
                source: WindowTarget::with_window(session.clone(), 2),
                target: WindowTarget::with_window(session.clone(), 0),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            })
            .expect("selected window relinks without reserving a later lifecycle ticket");
    }

    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    assert!(
        state
            .buffers
            .show(Some("window-unlinked-occurrence-replaced"))
            .is_err(),
        "replacement winlink must not inherit the removed occurrence's hook target"
    );
}

#[tokio::test]
async fn deferred_window_unlinked_hook_does_not_follow_a_replaced_active_survivor_slot() {
    let handler = RequestHandler::new();
    let alpha = session_name("window-unlinked-survivor-aba");
    create_session(&handler, alpha.as_str()).await;
    for window_index in [1, 2] {
        let response = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: Some(format!("window-{window_index}")),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: Some(window_index),
                insert_at_target: false,
            })))
            .await;
        assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    }
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(alpha.clone()),
            option: OptionName::RenumberWindows,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    set_indexed_global_hook(
        &handler,
        HookName::WindowUnlinked,
        0,
        "rename-window should-not-run",
    )
    .await;

    let (queued, replacement_window_id) = {
        let mut state = handler.state.lock().await;
        let removed_window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(1))
            .expect("first removed window exists");
        let event = LifecycleEvent::WindowUnlinked {
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 1)),
            window_id: Some(removed_window.id().as_u32()),
            window_name: removed_window.name().map(str::to_owned),
        };
        let deferred = super::defer_lifecycle_event(&state, &event);
        let mut hook_snapshot = state.hooks.clone();

        state
            .kill_window(WindowTarget::with_window(alpha.clone(), 1), false)
            .expect("first kill leaves active window 0 alive");
        let queued =
            super::prepare_deferred_lifecycle_event(&mut state, &mut hook_snapshot, deferred);

        state
            .kill_window(WindowTarget::with_window(alpha.clone(), 0), false)
            .expect("second kill replaces active slot 0 through renumbering");
        let replacement_window_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .expect("replacement window occupies slot 0")
            .id();
        (queued, replacement_window_id)
    };

    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    let replacement = state
        .sessions
        .session(&alpha)
        .and_then(|session| session.window_at(0))
        .expect("replacement window survives stale hook dispatch");
    assert_eq!(replacement.id(), replacement_window_id);
    assert_ne!(replacement.name(), Some("should-not-run"));
}

#[tokio::test]
async fn exited_pane_hook_does_not_target_recreated_same_slot() {
    let handler = RequestHandler::new();
    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .create_session(session_name("alpha"), TerminalSize { cols: 80, rows: 24 })
            .expect("create original alpha");
    }
    set_indexed_global_hook(&handler, HookName::PaneExited, 0, "kill-pane").await;

    let (queued, replacement_pane_id) = {
        let mut state = handler.state.lock().await;
        let alpha = session_name("alpha");
        let removed = state
            .sessions
            .remove_session(&alpha)
            .expect("remove original alpha");
        let removed_window = removed.window_at(0).expect("removed window exists");
        let removed_pane = removed_window.pane(0).expect("removed pane exists");
        let queued = super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::PaneExited {
                target: rmux_proto::PaneTarget::with_window(alpha.clone(), 0, 0),
                pane_id: Some(removed_pane.id().as_u32()),
                window_id: Some(removed_window.id().as_u32()),
                window_name: removed_window.name().map(str::to_owned),
            },
        );
        state
            .sessions
            .create_session(alpha.clone(), TerminalSize { cols: 80, rows: 24 })
            .expect("create replacement alpha");
        let replacement_pane_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("replacement pane exists")
            .id();
        (queued, replacement_pane_id)
    };

    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name("alpha"))
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("replacement pane survives")
            .id(),
        replacement_pane_id
    );
}
