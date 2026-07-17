use super::pane_group_transfer_tests::{
    create_grouped_session, create_session, pane_id, split_session,
};
use super::{prepare_lifecycle_event, RequestHandler};
use rmux_core::LifecycleEvent;
use rmux_proto::{
    BreakPaneRequest, HookLifecycle, HookName, JoinPaneRequest, KillPaneRequest, LinkWindowRequest,
    MovePaneRequest, PaneKillRequest, PaneTarget, PaneTargetRef, Request, Response,
    RotateWindowDirection, RotateWindowRequest, ScopeSelector, SetHookMutationRequest,
    SetHookRequest, ShowHooksRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget,
    SwapPaneRequest, WindowTarget,
};

async fn set_hook(
    handler: &RequestHandler,
    scope: ScopeSelector,
    hook: HookName,
    command: &str,
    lifecycle: HookLifecycle,
) {
    let response = handler
        .handle(Request::SetHook(SetHookRequest {
            scope,
            hook,
            command: command.to_owned(),
            lifecycle,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn unset_hook(handler: &RequestHandler, scope: ScopeSelector, hook: HookName) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope,
            hook,
            command: None,
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: true,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn shown_hook(handler: &RequestHandler, scope: ScopeSelector, hook: HookName) -> Vec<u8> {
    let (window, pane) = match scope {
        ScopeSelector::Window(_) => (true, false),
        ScopeSelector::Pane(_) => (false, true),
        ScopeSelector::Global | ScopeSelector::Session(_) => (false, false),
    };
    handler
        .handle(Request::ShowHooks(ShowHooksRequest {
            scope,
            window,
            pane,
            hook: Some(hook),
        }))
        .await
        .command_output()
        .expect("show-hooks output")
        .stdout()
        .to_vec()
}

async fn pane_target_for_id(
    handler: &RequestHandler,
    session_name: &rmux_proto::SessionName,
    pane_id: rmux_core::PaneId,
) -> PaneTarget {
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(session_name)
        .expect("session exists");
    session
        .windows()
        .iter()
        .find_map(|(window_index, window)| {
            window.panes().iter().find_map(|pane| {
                (pane.id() == pane_id).then(|| {
                    PaneTarget::with_window(session_name.clone(), *window_index, pane.index())
                })
            })
        })
        .expect("pane identity remains reachable")
}

#[tokio::test]
async fn grouped_pane_aliases_share_set_show_unset_and_one_shot() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "hook-id-pane-owner").await;
    split_session(&handler, &owner).await;
    let peer = create_grouped_session(&handler, "hook-id-pane-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    let owner_target = PaneTarget::with_window(owner.clone(), 0, 1);
    let peer_target = PaneTarget::with_window(peer.clone(), 0, 1);
    let command = "display-message identity-pane";

    set_hook(
        &handler,
        ScopeSelector::Pane(peer_target.clone()),
        HookName::PaneExited,
        command,
        HookLifecycle::Persistent,
    )
    .await;
    let expected = format!("pane-exited[0] {command}\n").into_bytes();
    assert_eq!(
        shown_hook(
            &handler,
            ScopeSelector::Pane(owner_target.clone()),
            HookName::PaneExited,
        )
        .await,
        expected
    );
    assert_eq!(
        shown_hook(
            &handler,
            ScopeSelector::Pane(peer_target.clone()),
            HookName::PaneExited,
        )
        .await,
        expected
    );

    unset_hook(
        &handler,
        ScopeSelector::Pane(owner_target.clone()),
        HookName::PaneExited,
    )
    .await;
    assert!(shown_hook(
        &handler,
        ScopeSelector::Pane(peer_target.clone()),
        HookName::PaneExited,
    )
    .await
    .is_empty());

    set_hook(
        &handler,
        ScopeSelector::Pane(owner_target.clone()),
        HookName::PaneDied,
        "display-message identity-once",
        HookLifecycle::OneShot,
    )
    .await;
    let (pane_id, window_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&owner).expect("owner exists");
        let window = session.window_at(0).expect("window exists");
        (window.pane(1).expect("pane exists").id(), window.id())
    };
    let mut state = handler.state.lock().await;
    let first = prepare_lifecycle_event(
        &mut state,
        &LifecycleEvent::PaneDied {
            target: peer_target,
            pane_id: Some(pane_id.as_u32()),
            window_id: Some(window_id.as_u32()),
            window_name: Some(String::new()),
        },
    );
    let second = prepare_lifecycle_event(
        &mut state,
        &LifecycleEvent::PaneDied {
            target: owner_target,
            pane_id: Some(pane_id.as_u32()),
            window_id: Some(window_id.as_u32()),
            window_name: Some(String::new()),
        },
    );
    assert_eq!(first.hooks.len(), 1);
    assert!(second.hooks.is_empty());
}

#[tokio::test]
async fn linked_and_grouped_window_aliases_share_one_binding() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "hook-id-window-owner").await;
    let linked = create_session(&handler, "hook-id-window-linked").await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(linked.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    let peer = create_grouped_session(&handler, "hook-id-window-peer", &owner).await;
    let command = "display-message identity-window";

    set_hook(
        &handler,
        ScopeSelector::Window(WindowTarget::with_window(peer.clone(), 1)),
        HookName::WindowLayoutChanged,
        command,
        HookLifecycle::Persistent,
    )
    .await;
    let expected = format!("window-layout-changed[0] {command}\n").into_bytes();
    for target in [
        WindowTarget::with_window(owner.clone(), 1),
        WindowTarget::with_window(peer, 1),
        WindowTarget::with_window(linked.clone(), 0),
    ] {
        assert_eq!(
            shown_hook(
                &handler,
                ScopeSelector::Window(target),
                HookName::WindowLayoutChanged,
            )
            .await,
            expected
        );
    }

    unset_hook(
        &handler,
        ScopeSelector::Window(WindowTarget::with_window(linked, 0)),
        HookName::WindowLayoutChanged,
    )
    .await;
    assert!(shown_hook(
        &handler,
        ScopeSelector::Window(WindowTarget::with_window(owner, 1)),
        HookName::WindowLayoutChanged,
    )
    .await
    .is_empty());
}

#[tokio::test]
async fn explicit_pane_scope_for_window_hook_uses_containing_window_identity() {
    let handler = RequestHandler::new();
    let session_name = create_session(&handler, "hook-id-pane-window-class").await;
    let pane_target = PaneTarget::with_window(session_name.clone(), 0, 0);
    let command = "display-message pane-addressed-window-hook";

    set_hook(
        &handler,
        ScopeSelector::Pane(pane_target.clone()),
        HookName::WindowLayoutChanged,
        command,
        HookLifecycle::Persistent,
    )
    .await;
    assert_eq!(
        shown_hook(
            &handler,
            ScopeSelector::Pane(pane_target),
            HookName::WindowLayoutChanged,
        )
        .await,
        format!("window-layout-changed[0] {command}\n").into_bytes()
    );

    let mut state = handler.state.lock().await;
    let event = prepare_lifecycle_event(
        &mut state,
        &LifecycleEvent::WindowLayoutChanged {
            target: WindowTarget::with_window(session_name, 0),
        },
    );
    assert_eq!(event.hooks.len(), 1);
    assert_eq!(event.hooks[0].command(), command);
}

#[tokio::test]
async fn filtered_session_show_uses_the_same_natural_identity_as_set() {
    let handler = RequestHandler::new();
    let session_name = create_session(&handler, "hook-id-natural-session-show").await;

    for (hook, command) in [
        (HookName::SessionRenamed, "display-message natural-session"),
        (
            HookName::WindowLayoutChanged,
            "display-message natural-window",
        ),
        (HookName::PaneModeChanged, "display-message natural-pane"),
    ] {
        let set = handler
            .handle(Request::SetHookMutation(SetHookMutationRequest {
                scope: ScopeSelector::Session(session_name.clone()),
                hook,
                command: Some(command.to_owned()),
                lifecycle: HookLifecycle::Persistent,
                append: false,
                unset: false,
                run_immediately: false,
                index: None,
            }))
            .await;
        assert!(matches!(set, Response::SetHook(_)), "{set:?}");
        assert_eq!(
            shown_hook(&handler, ScopeSelector::Session(session_name.clone()), hook).await,
            format!("{}[0] {command}\n", hook.as_str()).into_bytes()
        );
    }

    let unfiltered = handler
        .handle(Request::ShowHooks(ShowHooksRequest {
            scope: ScopeSelector::Session(session_name),
            window: false,
            pane: false,
            hook: None,
        }))
        .await
        .command_output()
        .expect("unfiltered show-hooks output")
        .stdout()
        .to_vec();
    assert_eq!(
        unfiltered,
        b"session-renamed[0] display-message natural-session\n"
    );
}

#[tokio::test]
async fn pane_bindings_follow_identity_through_swap_rotate_break_join_and_move() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "hook-id-transfer-source").await;
    split_session(&handler, &source).await;
    let first_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &source, 0, 0)
    };
    let second_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &source, 0, 1)
    };
    set_hook(
        &handler,
        ScopeSelector::Pane(PaneTarget::with_window(source.clone(), 0, 0)),
        HookName::PaneExited,
        "display-message first-pane",
        HookLifecycle::Persistent,
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Pane(PaneTarget::with_window(source.clone(), 0, 1)),
        HookName::PaneExited,
        "display-message second-pane",
        HookLifecycle::Persistent,
    )
    .await;

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: PaneTarget::with_window(source.clone(), 0, 0),
            target: PaneTarget::with_window(source.clone(), 0, 1),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapPane(_)), "{response:?}");
    let response = handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(source.clone(), 0),
            direction: RotateWindowDirection::Down,
            restore_zoom: false,
        }))
        .await;
    assert!(
        matches!(response, Response::RotateWindow(_)),
        "{response:?}"
    );
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .hooks
                .pane_command_by_id(first_id, HookName::PaneExited),
            Some("display-message first-pane")
        );
        assert_eq!(
            state
                .hooks
                .pane_command_by_id(second_id, HookName::PaneExited),
            Some("display-message second-pane")
        );
    }

    let second_target = pane_target_for_id(&handler, &source, second_id).await;
    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: second_target,
            target: Some(WindowTarget::with_window(source.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    let join_destination = create_session(&handler, "hook-id-transfer-join").await;
    let moved = pane_target_for_id(&handler, &source, second_id).await;
    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: moved,
            target: PaneTarget::with_window(join_destination.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");
    let joined = pane_target_for_id(&handler, &join_destination, second_id).await;
    assert_eq!(
        shown_hook(
            &handler,
            ScopeSelector::Pane(joined.clone()),
            HookName::PaneExited,
        )
        .await,
        b"pane-exited[0] display-message second-pane\n"
    );

    let move_destination = create_session(&handler, "hook-id-transfer-move").await;
    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: joined,
            target: PaneTarget::with_window(move_destination.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::MovePane(_)), "{response:?}");
    let moved = pane_target_for_id(&handler, &move_destination, second_id).await;
    assert_eq!(
        shown_hook(&handler, ScopeSelector::Pane(moved), HookName::PaneExited,).await,
        b"pane-exited[0] display-message second-pane\n"
    );
}

#[tokio::test]
async fn pane_binding_survives_before_split_and_new_pane_kill() {
    let handler = RequestHandler::new();
    let session_name = create_session(&handler, "hook-id-split-kill").await;
    let original_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &session_name, 0, 0)
    };
    set_hook(
        &handler,
        ScopeSelector::Pane(PaneTarget::with_window(session_name.clone(), 0, 0)),
        HookName::PaneExited,
        "display-message original-pane",
        HookLifecycle::Persistent,
    )
    .await;

    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(session_name.clone(), 0, 0)),
            direction: SplitDirection::Vertical,
            before: true,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
    let new_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .expect("session exists")
            .window_at(0)
            .expect("window exists")
            .panes()
            .iter()
            .find(|pane| pane.id() != original_id)
            .expect("split pane exists")
            .id()
    };
    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: pane_target_for_id(&handler, &session_name, new_id).await,
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let original_target = pane_target_for_id(&handler, &session_name, original_id).await;
    assert_eq!(
        shown_hook(
            &handler,
            ScopeSelector::Pane(original_target),
            HookName::PaneExited,
        )
        .await,
        b"pane-exited[0] display-message original-pane\n"
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state.hooks.pane_command_by_id(new_id, HookName::PaneExited),
        None,
        "destroyed pane identity must be purged"
    );
}

#[tokio::test]
async fn kill_pane_last_pane_dispatches_session_local_session_closed_hook() {
    let handler = RequestHandler::new();
    let session_name = create_session(&handler, "hook-id-kill-pane-session").await;
    handler.wait_for_initial_panes_for_test().await;
    let command = "display-message kill-pane-session-closed";
    set_hook(
        &handler,
        ScopeSelector::Session(session_name.clone()),
        HookName::SessionClosed,
        command,
        HookLifecycle::Persistent,
    )
    .await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(session_name.clone(), 0, 0),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let closed = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| {
            matches!(
                &event.event,
                LifecycleEvent::SessionClosed { session_name: closed, .. }
                    if closed == &session_name
            )
        })
        .expect("kill-pane emits session-closed");
    assert_eq!(
        closed
            .hooks
            .iter()
            .filter(|hook| hook.command() == command)
            .count(),
        1
    );
}

#[tokio::test]
async fn pane_kill_by_id_last_pane_dispatches_session_local_session_closed_hook() {
    let handler = RequestHandler::new();
    let session_name = create_session(&handler, "hook-id-pane-kill-session").await;
    handler.wait_for_initial_panes_for_test().await;
    let pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &session_name, 0, 0)
    };
    let command = "display-message pane-kill-session-closed";
    set_hook(
        &handler,
        ScopeSelector::Session(session_name.clone()),
        HookName::SessionClosed,
        command,
        HookLifecycle::Persistent,
    )
    .await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(session_name.clone(), pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let closed = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| {
            matches!(
                &event.event,
                LifecycleEvent::SessionClosed { session_name: closed, .. }
                    if closed == &session_name
            )
        })
        .expect("pane-kill emits session-closed");
    assert_eq!(
        closed
            .hooks
            .iter()
            .filter(|hook| hook.command() == command)
            .count(),
        1
    );
}

#[tokio::test]
async fn linked_group_last_pane_kill_preserves_local_hooks_and_consumes_global_once() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "hook-id-kill-family-owner").await;
    let peer = create_grouped_session(&handler, "hook-id-kill-family-peer", &owner).await;
    let alias = create_session(&handler, "hook-id-kill-family-alias").await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(alias.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;

    let owner_command = "display-message kill-family-owner";
    let peer_command = "display-message kill-family-peer";
    let global_command = "display-message kill-family-global-once";
    for (session_name, command) in [(&owner, owner_command), (&peer, peer_command)] {
        set_hook(
            &handler,
            ScopeSelector::Session(session_name.clone()),
            HookName::SessionClosed,
            command,
            HookLifecycle::Persistent,
        )
        .await;
    }
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::SessionClosed,
        global_command,
        HookLifecycle::OneShot,
    )
    .await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(alias.clone(), 0, 0),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let closed = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(&event.event, LifecycleEvent::SessionClosed { .. }))
        .collect::<Vec<_>>();
    assert_eq!(closed.len(), 3);
    for (session_name, command) in [(&owner, owner_command), (&peer, peer_command)] {
        let event = closed
            .iter()
            .find(|event| {
                matches!(
                    &event.event,
                    LifecycleEvent::SessionClosed { session_name: closed, .. }
                        if closed == session_name
                )
            })
            .expect("group member emits session-closed");
        assert_eq!(
            event
                .hooks
                .iter()
                .filter(|hook| hook.command() == command)
                .count(),
            1
        );
    }
    assert_eq!(
        closed
            .iter()
            .flat_map(|event| &event.hooks)
            .filter(|hook| hook.command() == global_command)
            .count(),
        1
    );
    let state = handler.state.lock().await;
    for session_name in [&owner, &peer, &alias] {
        assert!(state.sessions.session(session_name).is_none());
    }
    assert_eq!(state.hooks.global_lifecycle(HookName::SessionClosed), None);
}

#[tokio::test]
async fn failed_linked_pane_kill_preserves_session_closed_one_shot() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "hook-id-kill-error-owner").await;
    split_session(&handler, &owner).await;
    let alias = create_session(&handler, "hook-id-kill-error-alias").await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(alias.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;
    set_hook(
        &handler,
        ScopeSelector::Session(alias.clone()),
        HookName::SessionClosed,
        "display-message failed-kill-one-shot",
        HookLifecycle::OneShot,
    )
    .await;
    {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
    }

    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(alias.clone(), 0, 1),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::Error(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .hooks
            .session_lifecycle(&alias, HookName::SessionClosed),
        Some(HookLifecycle::OneShot)
    );
}
