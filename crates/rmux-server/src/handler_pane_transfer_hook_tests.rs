use super::pane_group_transfer_tests::{create_grouped_session, create_session, split_session};
use super::{QueuedLifecycleEvent, RequestHandler};
use rmux_proto::{
    BreakPaneRequest, HookLifecycle, HookName, JoinPaneRequest, LinkWindowRequest, MovePaneRequest,
    NewWindowRequest, PaneTarget, Request, Response, ScopeSelector, SetHookRequest, SplitDirection,
    SplitWindowRequest, SplitWindowTarget, SwapPaneRequest, WindowTarget,
};

async fn set_one_shot_hook(
    handler: &RequestHandler,
    scope: ScopeSelector,
    hook: HookName,
    command: &str,
) {
    let response = handler
        .handle(Request::SetHook(SetHookRequest {
            scope,
            hook,
            command: command.to_owned(),
            lifecycle: HookLifecycle::OneShot,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

fn join_request(source: PaneTarget, target: PaneTarget) -> JoinPaneRequest {
    JoinPaneRequest {
        source,
        target,
        direction: SplitDirection::Vertical,
        detached: true,
        before: false,
        full_size: false,
        size: None,
    }
}

async fn create_session_with_duplicate_window_alias(
    handler: &RequestHandler,
    label: &str,
    split_source: bool,
) -> rmux_proto::SessionName {
    let session_name = create_session(handler, label).await;
    if split_source {
        split_session(handler, &session_name).await;
    }
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(session_name.clone(), 0),
            target: WindowTarget::with_window(session_name.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    handler.wait_for_initial_panes_for_test().await;
    session_name
}

fn drain_layout_targets(
    events: &mut tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>,
) -> Vec<WindowTarget> {
    drain_layout_events(events)
        .into_iter()
        .filter_map(|event| match event.event {
            rmux_core::LifecycleEvent::WindowLayoutChanged { target } => Some(target),
            _ => None,
        })
        .collect()
}

fn drain_layout_events(
    events: &mut tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>,
) -> Vec<QueuedLifecycleEvent> {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| {
            matches!(
                &event.event,
                rmux_core::LifecycleEvent::WindowLayoutChanged { .. }
            )
        })
        .collect()
}

#[tokio::test]
async fn session_closed_one_shot_survives_success_without_session_destruction() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "one-shot-survives-non-emission-source").await;
    split_session(&handler, &source).await;
    let target = create_session(&handler, "one-shot-survives-non-emission-target").await;
    handler.wait_for_initial_panes_for_test().await;
    set_one_shot_hook(
        &handler,
        ScopeSelector::Session(source.clone()),
        HookName::SessionClosed,
        "display-message -p non-emitted-one-shot",
    )
    .await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::JoinPane(join_request(
            PaneTarget::with_window(source.clone(), 0, 1),
            PaneTarget::with_window(target, 0, 0),
        )))
        .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&source).is_some());
    assert_eq!(
        state
            .hooks
            .session_lifecycle(&source, HookName::SessionClosed),
        Some(HookLifecycle::OneShot)
    );
    drop(state);
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(
                event.event,
                rmux_core::LifecycleEvent::SessionClosed { session_name, .. }
                    if session_name == source
            ),
            "a surviving session must not emit session-closed"
        );
    }
}

#[tokio::test]
async fn session_closed_one_shot_survives_failed_transfer() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "one-shot-survives-error-source").await;
    let target = create_session(&handler, "one-shot-survives-error-target").await;
    handler.wait_for_initial_panes_for_test().await;
    set_one_shot_hook(
        &handler,
        ScopeSelector::Session(source.clone()),
        HookName::SessionClosed,
        "display-message -p failed-one-shot",
    )
    .await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::JoinPane(join_request(
            PaneTarget::with_window(source.clone(), 0, 0),
            PaneTarget::with_window(target, 0, 99),
        )))
        .await;
    assert!(matches!(response, Response::Error(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&source).is_some());
    assert_eq!(
        state
            .hooks
            .session_lifecycle(&source, HookName::SessionClosed),
        Some(HookLifecycle::OneShot)
    );
    drop(state);
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(
                event.event,
                rmux_core::LifecycleEvent::SessionClosed { session_name, .. }
                    if session_name == source
            ),
            "a failed transfer must not emit session-closed"
        );
    }
}

#[tokio::test]
async fn session_closed_one_shot_is_emitted_once_for_real_destruction() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "one-shot-real-destruction-source").await;
    let target = create_session(&handler, "one-shot-real-destruction-target").await;
    handler.wait_for_initial_panes_for_test().await;
    let command = "display-message -p destroyed-one-shot";
    set_one_shot_hook(
        &handler,
        ScopeSelector::Session(source.clone()),
        HookName::SessionClosed,
        command,
    )
    .await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::JoinPane(join_request(
            PaneTarget::with_window(source.clone(), 0, 0),
            PaneTarget::with_window(target, 0, 0),
        )))
        .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");

    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&source)
        .is_none());
    let mut closed_events = 0;
    let mut dispatched_hooks = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(
            event.event,
            rmux_core::LifecycleEvent::SessionClosed { ref session_name, .. }
                if session_name == &source
        ) {
            closed_events += 1;
            dispatched_hooks += event
                .hooks
                .iter()
                .filter(|hook| hook.command() == command)
                .count();
        }
    }
    assert_eq!(closed_events, 1);
    assert_eq!(dispatched_hooks, 1);
}

#[tokio::test]
async fn global_session_closed_one_shot_is_not_duplicated_across_destroyed_group() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "global-one-shot-group-owner").await;
    let peer = create_grouped_session(&handler, "global-one-shot-group-peer", &owner).await;
    let target = create_session(&handler, "global-one-shot-group-target").await;
    handler.wait_for_initial_panes_for_test().await;
    let command = "display-message -p global-destroyed-one-shot";
    set_one_shot_hook(
        &handler,
        ScopeSelector::Global,
        HookName::SessionClosed,
        command,
    )
    .await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::JoinPane(join_request(
            PaneTarget::with_window(owner.clone(), 0, 0),
            PaneTarget::with_window(target, 0, 0),
        )))
        .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&owner).is_none());
    assert!(state.sessions.session(&peer).is_none());
    assert_eq!(state.hooks.global_lifecycle(HookName::SessionClosed), None);
    drop(state);
    let mut closed_events = 0;
    let mut dispatched_hooks = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(
            event.event,
            rmux_core::LifecycleEvent::SessionClosed { ref session_name, .. }
                if session_name == &owner || session_name == &peer
        ) {
            closed_events += 1;
            dispatched_hooks += event
                .hooks
                .iter()
                .filter(|hook| hook.command() == command)
                .count();
        }
    }
    assert_eq!(closed_events, 2);
    assert_eq!(dispatched_hooks, 1);
}

#[tokio::test]
async fn join_last_pane_emits_only_destination_layout_contexts_like_tmux() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "join-last-layout-source").await;
    let target = create_session(&handler, "join-last-layout-target").await;
    handler.wait_for_initial_panes_for_test().await;
    let target_window = WindowTarget::with_window(target.clone(), 0);
    let command = "display-message -p join-last-layout-once";
    set_one_shot_hook(
        &handler,
        ScopeSelector::Window(target_window.clone()),
        HookName::WindowLayoutChanged,
        command,
    )
    .await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::JoinPane(join_request(
            PaneTarget::with_window(source.clone(), 0, 0),
            PaneTarget::with_window(target, 0, 0),
        )))
        .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");

    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&source)
        .is_none());
    let layouts = drain_layout_events(&mut events);
    assert_eq!(
        layouts
            .iter()
            .filter_map(|event| match &event.event {
                rmux_core::LifecycleEvent::WindowLayoutChanged { target } => {
                    Some(target.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![target_window.clone(), target_window]
    );
    assert_eq!(
        layouts
            .iter()
            .flat_map(|event| &event.hooks)
            .filter(|hook| hook.command() == command)
            .count(),
        1,
        "the target one-shot hook must be consumed once"
    );
}

#[tokio::test]
async fn move_last_pane_emits_only_destination_layout_contexts() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "move-last-layout-source").await;
    let target = create_session(&handler, "move-last-layout-target").await;
    handler.wait_for_initial_panes_for_test().await;
    let target_window = WindowTarget::with_window(target.clone(), 0);
    let command = "display-message -p move-last-layout-once";
    set_one_shot_hook(
        &handler,
        ScopeSelector::Window(target_window.clone()),
        HookName::WindowLayoutChanged,
        command,
    )
    .await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: PaneTarget::with_window(source.clone(), 0, 0),
            target: PaneTarget::with_window(target, 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::MovePane(_)), "{response:?}");

    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&source)
        .is_none());
    let layouts = drain_layout_events(&mut events);
    assert_eq!(
        layouts
            .iter()
            .filter_map(|event| match &event.event {
                rmux_core::LifecycleEvent::WindowLayoutChanged { target } => {
                    Some(target.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![target_window.clone(), target_window]
    );
    assert_eq!(
        layouts
            .iter()
            .flat_map(|event| &event.hooks)
            .filter(|hook| hook.command() == command)
            .count(),
        1,
        "the target one-shot hook must be consumed once"
    );
}

#[tokio::test]
async fn swap_group_aliases_emits_one_target_layout_and_consumes_one_shot_once() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "swap-alias-layout-owner").await;
    split_session(&handler, &owner).await;
    let peer = create_grouped_session(&handler, "swap-alias-layout-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    let target_window = WindowTarget::with_window(peer.clone(), 0);
    let command = "display-message -p swap-alias-layout-once";
    set_one_shot_hook(
        &handler,
        ScopeSelector::Window(target_window.clone()),
        HookName::WindowLayoutChanged,
        command,
    )
    .await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: PaneTarget::with_window(owner, 0, 0),
            target: PaneTarget::with_window(peer, 0, 1),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapPane(_)), "{response:?}");

    let layouts = drain_layout_events(&mut events);
    assert_eq!(layouts.len(), 1);
    assert!(matches!(
        &layouts[0].event,
        rmux_core::LifecycleEvent::WindowLayoutChanged { target }
            if target == &target_window
    ));
    assert_eq!(
        layouts[0]
            .hooks
            .iter()
            .filter(|hook| hook.command() == command)
            .count(),
        1
    );
}

#[tokio::test]
async fn break_before_emits_layout_only_for_reindexed_source_window_identity() {
    let handler = RequestHandler::new();
    let session_name = create_session(&handler, "break-before-layout-source").await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: Some("source".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(session_name.clone(), 1, 0)),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let source_window_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(1))
            .map(rmux_core::Window::id)
            .expect("source window exists")
    };
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(session_name.clone(), 1, 1),
            target: Some(WindowTarget::with_window(session_name.clone(), 0)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    let layout_targets = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event.event {
            rmux_core::LifecycleEvent::WindowLayoutChanged { target } => Some(target),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        layout_targets,
        vec![WindowTarget::with_window(session_name.clone(), 2)]
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(2))
            .map(rmux_core::Window::id),
        Some(source_window_id)
    );
}

#[tokio::test]
async fn break_before_from_second_alias_updates_aliases_and_emits_first_surviving_layout() {
    let handler = RequestHandler::new();
    let session_name =
        create_session_with_duplicate_window_alias(&handler, "break-before-second-alias", true)
            .await;

    let (source_window_id, remaining_pane_id, moved_pane_id) = {
        let state = handler.state.lock().await;
        let source_window = state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(1))
            .expect("second linked occurrence exists");
        (
            source_window.id(),
            source_window.pane(0).expect("first pane exists").id(),
            source_window.pane(1).expect("second pane exists").id(),
        )
    };
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(session_name.clone(), 1, 1),
            target: Some(WindowTarget::with_window(session_name.clone(), 0)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected break-pane success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(session_name.clone(), 0, 0)
    );

    let layout_targets = drain_layout_targets(&mut events);
    assert_eq!(
        layout_targets,
        vec![WindowTarget::with_window(session_name.clone(), 1)]
    );

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&session_name)
        .expect("session survives break-pane");
    let broken_window = session.window_at(0).expect("new window exists");
    assert_eq!(broken_window.pane_count(), 1);
    assert_eq!(
        broken_window.pane(0).expect("moved pane exists").id(),
        moved_pane_id
    );
    for window_index in [1, 2] {
        let alias = session
            .window_at(window_index)
            .expect("source alias survives");
        assert_eq!(alias.id(), source_window_id);
        assert_eq!(alias.pane_count(), 1);
        assert_eq!(
            alias.pane(0).expect("remaining pane exists").id(),
            remaining_pane_id
        );
    }
}

#[tokio::test]
async fn break_after_from_second_alias_emits_first_surviving_layout() {
    let handler = RequestHandler::new();
    let session_name =
        create_session_with_duplicate_window_alias(&handler, "break-after-second-alias", true)
            .await;
    let source_window_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(1))
            .map(rmux_core::Window::id)
            .expect("second linked occurrence exists")
    };
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(session_name.clone(), 1, 1),
            target: Some(WindowTarget::with_window(session_name.clone(), 0)),
            name: None,
            detached: true,
            after: true,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected break-pane success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(session_name.clone(), 1, 0)
    );
    assert_eq!(
        drain_layout_targets(&mut events),
        vec![WindowTarget::with_window(session_name.clone(), 0)]
    );

    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(&session_name)
        .expect("session survives break-pane");
    assert_eq!(
        session
            .window_at(1)
            .expect("new window exists")
            .pane_count(),
        1
    );
    for window_index in [0, 2] {
        let alias = session
            .window_at(window_index)
            .expect("source alias survives");
        assert_eq!(alias.id(), source_window_id);
        assert_eq!(alias.pane_count(), 1);
    }
}

#[tokio::test]
async fn break_before_from_distinct_single_pane_window_emits_no_layout() {
    let handler = RequestHandler::new();
    let session_name = create_session(&handler, "break-single-distinct").await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: Some("single-source".to_owned()),
            detached: true,
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
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(session_name.clone(), 1, 0),
            target: Some(WindowTarget::with_window(session_name.clone(), 0)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");
    assert!(drain_layout_targets(&mut events).is_empty());
}

#[tokio::test]
async fn break_before_from_duplicate_single_pane_alias_emits_no_layout() {
    let handler = RequestHandler::new();
    let session_name =
        create_session_with_duplicate_window_alias(&handler, "break-single-duplicate", false).await;
    let mut events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(session_name.clone(), 1, 0),
            target: Some(WindowTarget::with_window(session_name.clone(), 0)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");
    assert!(drain_layout_targets(&mut events).is_empty());
}
