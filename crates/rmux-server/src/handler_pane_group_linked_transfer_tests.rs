use super::pane_group_transfer_tests::{
    create_grouped_session, create_session, pane_id, pane_ids, pane_option, set_pane_option,
    split_session,
};
use super::RequestHandler;
use rmux_proto::{
    BreakPaneRequest, CapturePaneRequest, HookLifecycle, HookName, JoinPaneRequest,
    KillPaneRequest, LinkWindowRequest, MovePaneRequest, NewWindowRequest, OptionScopeSelector,
    PaneTarget, Request, Response, ScopeSelector, SelectWindowRequest, SendKeysRequest,
    SetHookRequest, SetOptionByNameRequest, SetOptionMode, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, SwapPaneRequest, TerminalPixels, WindowTarget,
};

async fn create_group_with_linked_runtime_window(
    handler: &RequestHandler,
    label: &str,
) -> (
    rmux_proto::SessionName,
    rmux_proto::SessionName,
    rmux_proto::SessionName,
) {
    let owner = create_session(handler, &format!("{label}-owner")).await;
    split_session(handler, &owner).await;
    let linked_owner = create_session(handler, &format!("{label}-linked")).await;
    split_session(handler, &linked_owner).await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(linked_owner.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let peer = create_grouped_session(handler, &format!("{label}-peer"), &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    (owner, peer, linked_owner)
}

async fn create_group_with_duplicate_two_pane_linked_window(
    handler: &RequestHandler,
    label: &str,
) -> (
    rmux_proto::SessionName,
    rmux_proto::SessionName,
    rmux_proto::SessionName,
) {
    let owner = create_session(handler, &format!("{label}-owner")).await;
    let linked_owner = create_session(handler, &format!("{label}-linked")).await;
    split_session(handler, &linked_owner).await;
    for target_window_index in [1, 2] {
        let response = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(linked_owner.clone(), 0),
                target: WindowTarget::with_window(owner.clone(), target_window_index),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await;
        assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    }
    let peer = create_grouped_session(handler, &format!("{label}-peer"), &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    (owner, peer, linked_owner)
}

async fn create_group_with_single_pane_linked_window(
    handler: &RequestHandler,
    label: &str,
) -> (
    rmux_proto::SessionName,
    rmux_proto::SessionName,
    rmux_proto::SessionName,
) {
    let owner = create_session(handler, &format!("{label}-owner")).await;
    split_session(handler, &owner).await;
    let linked_owner = create_session(handler, &format!("{label}-linked")).await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(linked_owner.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let peer = create_grouped_session(handler, &format!("{label}-peer"), &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    (owner, peer, linked_owner)
}

async fn create_fully_linked_single_window_group(
    handler: &RequestHandler,
    label: &str,
) -> (
    rmux_proto::SessionName,
    rmux_proto::SessionName,
    rmux_proto::SessionName,
    rmux_proto::SessionName,
) {
    let destination = create_session(handler, &format!("{label}-destination")).await;
    let owner = create_session(handler, &format!("{label}-owner")).await;
    let linked_owner = create_session(handler, &format!("{label}-linked")).await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(linked_owner.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    let peer = create_grouped_session(handler, &format!("{label}-peer"), &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    (destination, owner, peer, linked_owner)
}

async fn assert_destroyed_group_runtime_names_are_reusable(
    handler: &RequestHandler,
    destroyed_sessions: &[rmux_proto::SessionName],
    recreated_name: &rmux_proto::SessionName,
) {
    {
        let state = handler.state.lock().await;
        for session_name in destroyed_sessions {
            assert!(state.sessions.session(session_name).is_none());
            assert!(
                !state.contains_session_terminals(session_name),
                "destroyed linked group member {session_name} must not retain a runtime"
            );
            assert_eq!(
                state.attached_terminal_pixels_for_test(session_name),
                None,
                "destroyed linked group member {session_name} must not retain pixel geometry"
            );
        }
    }

    let recreated = create_session(handler, recreated_name.as_str()).await;
    handler.wait_for_initial_panes_for_test().await;
    let state = handler.state.lock().await;
    assert!(state.contains_session_terminals(&recreated));
    assert_eq!(state.attached_terminal_pixels_for_test(&recreated), None);
    state
        .pane_profile_in_window(&recreated, 0, 0)
        .expect("recreated group peer receives a fresh runtime");
}

async fn link_external_window(
    handler: &RequestHandler,
    external: &rmux_proto::SessionName,
    owner: &rmux_proto::SessionName,
    owner_window_index: u32,
    kill_destination: bool,
) {
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(external.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), owner_window_index),
            after: false,
            before: false,
            kill_destination,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
}

async fn create_group_with_individually_linked_windows(
    handler: &RequestHandler,
    label: &str,
    window_count: usize,
) -> (
    rmux_proto::SessionName,
    rmux_proto::SessionName,
    Vec<rmux_proto::SessionName>,
) {
    assert!(window_count > 0);
    let owner = create_session(handler, &format!("{label}-owner")).await;
    let mut externals = Vec::with_capacity(window_count);
    for window_index in 0..window_count as u32 {
        let external = create_session(handler, &format!("{label}-external-{window_index}")).await;
        link_external_window(handler, &external, &owner, window_index, window_index == 0).await;
        externals.push(external);
    }
    let peer = create_grouped_session(handler, &format!("{label}-peer"), &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    (owner, peer, externals)
}

async fn create_group_with_duplicate_linked_winlinks(
    handler: &RequestHandler,
    label: &str,
) -> (
    rmux_proto::SessionName,
    rmux_proto::SessionName,
    rmux_core::WindowId,
    rmux_core::WindowId,
) {
    let (owner, peer, linked_owner) =
        create_group_with_single_pane_linked_window(handler, label).await;
    let base_window_id = {
        let state = handler.state.lock().await;
        window_id(&state, &owner, 0)
    };
    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(linked_owner, 0, 0),
            target: Some(WindowTarget::with_window(owner.clone(), 2)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");
    let linked_window_id = {
        let state = handler.state.lock().await;
        let linked_window_id = window_id(&state, &owner, 1);
        assert_eq!(window_id(&state, &owner, 2), linked_window_id);
        assert_eq!(window_id(&state, &peer, 1), linked_window_id);
        assert_eq!(window_id(&state, &peer, 2), linked_window_id);
        linked_window_id
    };
    (owner, peer, base_window_id, linked_window_id)
}

async fn set_window_marker(handler: &RequestHandler, target: WindowTarget, value: &str) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Window(target),
            name: "@break-slot".to_owned(),
            value: Some(value.to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
}

fn window_id(
    state: &super::HandlerState,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
) -> rmux_core::WindowId {
    state
        .sessions
        .session(session_name)
        .and_then(|session| session.window_at(window_index))
        .map(rmux_core::Window::id)
        .expect("window exists")
}

fn explicit_window_marker(
    state: &super::HandlerState,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
) -> Option<String> {
    state
        .options
        .explicit_value_by_name(
            &OptionScopeSelector::Window(WindowTarget::with_window(
                session_name.clone(),
                window_index,
            )),
            "@break-slot",
        )
        .expect("valid user option")
        .1
}

fn assert_linked_window_models_and_runtimes_match(
    state: &super::HandlerState,
    owner: &rmux_proto::SessionName,
    peer: &rmux_proto::SessionName,
    linked_owner: &rmux_proto::SessionName,
) {
    let expected = pane_ids(state, owner, 1);
    assert_eq!(pane_ids(state, peer, 1), expected);
    assert_eq!(pane_ids(state, linked_owner, 0), expected);
    for pane_index in 0..expected.len() as u32 {
        for (session_name, window_index) in [(owner, 1), (peer, 1), (linked_owner, 0)] {
            state
                .pane_profile_in_window(session_name, window_index, pane_index)
                .expect("every linked alias resolves the synchronized pane runtime");
        }
    }
}

async fn select_window(
    handler: &RequestHandler,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
) {
    let response = handler
        .handle(Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(session_name.clone(), window_index),
        }))
        .await;
    assert!(
        matches!(response, Response::SelectWindow(_)),
        "{response:?}"
    );
}

fn capture_request(target: PaneTarget) -> CapturePaneRequest {
    CapturePaneRequest {
        target,
        start: None,
        end: None,
        print: true,
        buffer_name: None,
        alternate: false,
        escape_ansi: false,
        escape_sequences: false,
        include_format: false,
        hyperlinks: false,
        line_numbers: false,
        join_wrapped: false,
        use_mode_screen: false,
        preserve_trailing_spaces: false,
        do_not_trim_spaces: false,
        pending_input: false,
        quiet: false,
        start_is_absolute: false,
        end_is_absolute: false,
    }
}

async fn assert_linked_source_was_removed_and_moved_pane_is_live(
    handler: &RequestHandler,
    owner: &rmux_proto::SessionName,
    peer: &rmux_proto::SessionName,
    linked_owner: &rmux_proto::SessionName,
    moved_target: PaneTarget,
    moved_pane_id: rmux_core::PaneId,
) {
    {
        let state = handler.state.lock().await;
        assert!(state.sessions.session(linked_owner).is_none());
        assert!(!state.contains_session_terminals(linked_owner));
        for group_member in [owner, peer] {
            assert!(
                state
                    .sessions
                    .session(group_member)
                    .and_then(|session| session.window_at(1))
                    .is_none(),
                "destroyed linked window must leave grouped member {group_member}"
            );
        }
        assert_eq!(
            pane_id(
                &state,
                moved_target.session_name(),
                moved_target.window_index(),
                moved_target.pane_index(),
            ),
            moved_pane_id
        );
        state
            .pane_profile_in_window(
                moved_target.session_name(),
                moved_target.window_index(),
                moved_target.pane_index(),
            )
            .expect("moved pane remains backed by its transferred runtime");
    }

    let stale_target = PaneTarget::with_window(linked_owner.clone(), 0, 0);
    let stale_send = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: stale_target.clone(),
            keys: vec!["x".to_owned()],
        }))
        .await;
    assert!(matches!(stale_send, Response::Error(_)), "{stale_send:?}");
    let stale_capture = handler
        .handle(Request::CapturePane(Box::new(capture_request(
            stale_target,
        ))))
        .await;
    assert!(
        matches!(stale_capture, Response::Error(_)),
        "{stale_capture:?}"
    );

    let live_send = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: moved_target.clone(),
            keys: vec!["x".to_owned()],
        }))
        .await;
    assert!(matches!(live_send, Response::SendKeys(_)), "{live_send:?}");
    let live_capture = handler
        .handle(Request::CapturePane(Box::new(capture_request(
            moved_target,
        ))))
        .await;
    assert!(
        matches!(live_capture, Response::CapturePane(_)),
        "{live_capture:?}"
    );
}

#[tokio::test]
async fn join_last_linked_pane_through_group_alias_removes_stale_family_and_keeps_runtime_live() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_single_pane_linked_window(&handler, "last-linked-join").await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &peer, 1, 0)
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(peer.clone(), 1, 0),
        "@last-linked-join",
        "tracked",
    )
    .await;
    let hook = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Session(linked_owner.clone()),
            hook: HookName::SessionClosed,
            command: "display-message -p linked-closed".to_owned(),
            lifecycle: HookLifecycle::Persistent,
        }))
        .await;
    assert!(matches!(hook, Response::SetHook(_)), "{hook:?}");
    let mut lifecycle_events = handler.subscribe_lifecycle_events();

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(peer.clone(), 1, 0),
            target: PaneTarget::with_window(owner.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::JoinPane(response) = response else {
        panic!("expected last linked pane join success, got {response:?}");
    };

    assert_linked_source_was_removed_and_moved_pane_is_live(
        &handler,
        &owner,
        &peer,
        &linked_owner,
        response.target.clone(),
        moved_pane_id,
    )
    .await;
    assert_eq!(
        pane_option(&handler, response.target, "@last-linked-join").await,
        Some("tracked".to_owned())
    );
    let mut saw_prepared_session_hook = false;
    while let Ok(event) = lifecycle_events.try_recv() {
        if matches!(
            &event.event,
            rmux_core::LifecycleEvent::SessionClosed { session_name, .. }
                if session_name == &linked_owner
        ) {
            assert!(
                !event.hooks.is_empty(),
                "destroyed linked session hook must be captured before scope removal"
            );
            saw_prepared_session_hook = true;
        }
    }
    assert!(saw_prepared_session_hook);
}

#[tokio::test]
async fn join_last_linked_pane_into_linked_target_synchronizes_destination_family() {
    let handler = RequestHandler::new();
    let (source_owner, source_peer, source_linked) =
        create_group_with_single_pane_linked_window(&handler, "last-linked-into-linked-source")
            .await;
    let (destination_owner, destination_peer, destination_linked) =
        create_group_with_duplicate_two_pane_linked_window(
            &handler,
            "last-linked-into-linked-target",
        )
        .await;
    let source_target = PaneTarget::with_window(source_peer.clone(), 1, 0);
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &source_peer, 1, 0)
    };
    set_pane_option(
        &handler,
        source_target.clone(),
        "@last-linked-into-linked",
        "moved",
    )
    .await;

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: source_target,
            target: PaneTarget::with_window(destination_owner.clone(), 1, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::JoinPane(response) = response else {
        panic!("expected linked-last join into linked target success, got {response:?}");
    };

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&source_linked).is_none());
    for surviving_source in [&source_owner, &source_peer] {
        assert!(state
            .sessions
            .session(surviving_source)
            .is_some_and(|session| session.window_at(1).is_none()));
    }
    for (session_name, window_index) in [
        (&destination_owner, 1),
        (&destination_owner, 2),
        (&destination_peer, 1),
        (&destination_peer, 2),
        (&destination_linked, 0),
    ] {
        assert!(pane_ids(&state, session_name, window_index).contains(&moved_pane_id));
        let pane_index = state
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .and_then(|window| {
                window
                    .panes()
                    .iter()
                    .find(|pane| pane.id() == moved_pane_id)
                    .map(rmux_core::Pane::index)
            })
            .expect("moved pane exists in destination alias");
        state
            .pane_profile_in_window(session_name, window_index, pane_index)
            .expect("moved pane runtime resolves through destination alias");
    }
    drop(state);
    assert_eq!(
        pane_option(&handler, response.target, "@last-linked-into-linked").await,
        Some("moved".to_owned())
    );
}

#[tokio::test]
async fn break_last_linked_pane_before_linked_target_remaps_destination_family() {
    let handler = RequestHandler::new();
    let (_source_owner, source_peer, _source_linked) =
        create_group_with_single_pane_linked_window(&handler, "last-linked-break-before-source")
            .await;
    let (destination_owner, destination_peer, destination_linked) =
        create_group_with_linked_runtime_window(&handler, "last-linked-break-before-target").await;
    let old_destination_window_id = {
        let state = handler.state.lock().await;
        window_id(&state, &destination_owner, 1)
    };
    for target in [
        WindowTarget::with_window(destination_owner.clone(), 1),
        WindowTarget::with_window(destination_peer.clone(), 1),
    ] {
        set_window_marker(&handler, target, "shifted-linked-window").await;
    }
    let source_target = PaneTarget::with_window(source_peer.clone(), 1, 0);
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &source_peer, 1, 0)
    };
    set_pane_option(
        &handler,
        source_target.clone(),
        "@last-linked-break-before",
        "moved",
    )
    .await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: source_target,
            target: Some(WindowTarget::with_window(destination_owner.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected linked-last break-before success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(destination_owner.clone(), 1, 0)
    );

    let state = handler.state.lock().await;
    for destination_group_member in [&destination_owner, &destination_peer] {
        assert_eq!(
            window_id(&state, destination_group_member, 2),
            old_destination_window_id
        );
        assert_eq!(
            explicit_window_marker(&state, destination_group_member, 2).as_deref(),
            Some("shifted-linked-window")
        );
        assert_eq!(
            explicit_window_marker(&state, destination_group_member, 1),
            None
        );
        assert_eq!(
            pane_id(&state, destination_group_member, 1, 0),
            moved_pane_id
        );
        state
            .pane_profile_in_window(destination_group_member, 1, 0)
            .expect("broken linked-last pane resolves in destination group");
    }
    assert_eq!(
        window_id(&state, &destination_linked, 0),
        old_destination_window_id
    );
    state
        .pane_profile_in_window(&destination_linked, 0, 0)
        .expect("shifted linked destination keeps its original runtime");
    drop(state);
    assert_eq!(
        pane_option(&handler, response.target, "@last-linked-break-before").await,
        Some("moved".to_owned())
    );
}

#[tokio::test]
async fn move_last_linked_pane_through_group_alias_removes_stale_family_and_keeps_runtime_live() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_single_pane_linked_window(&handler, "last-linked-move").await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &peer, 1, 0)
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(peer.clone(), 1, 0),
        "@last-linked-move",
        "tracked",
    )
    .await;

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: PaneTarget::with_window(peer.clone(), 1, 0),
            target: PaneTarget::with_window(owner.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::MovePane(response) = response else {
        panic!("expected last linked pane move success, got {response:?}");
    };

    assert_linked_source_was_removed_and_moved_pane_is_live(
        &handler,
        &owner,
        &peer,
        &linked_owner,
        response.target.clone(),
        moved_pane_id,
    )
    .await;
    assert_eq!(
        pane_option(&handler, response.target, "@last-linked-move").await,
        Some("tracked".to_owned())
    );
}

#[tokio::test]
async fn join_last_linked_pane_cleans_every_destroyed_group_runtime_before_name_reuse() {
    let handler = RequestHandler::new();
    let (destination, owner, peer, linked_owner) =
        create_fully_linked_single_window_group(&handler, "destroyed-group-join").await;
    {
        let mut state = handler.state.lock().await;
        state.set_attached_terminal_pixels(&peer, Some(TerminalPixels::new(111, 222)));
    }

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(peer.clone(), 0, 0),
            target: PaneTarget::with_window(destination, 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");

    assert_destroyed_group_runtime_names_are_reusable(
        &handler,
        &[owner, peer.clone(), linked_owner],
        &peer,
    )
    .await;
}

#[tokio::test]
async fn move_last_linked_pane_cleans_every_destroyed_group_runtime_before_name_reuse() {
    let handler = RequestHandler::new();
    let (destination, owner, peer, linked_owner) =
        create_fully_linked_single_window_group(&handler, "destroyed-group-move").await;
    {
        let mut state = handler.state.lock().await;
        state.set_attached_terminal_pixels(&peer, Some(TerminalPixels::new(333, 444)));
    }

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: PaneTarget::with_window(peer.clone(), 0, 0),
            target: PaneTarget::with_window(destination, 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::MovePane(_)), "{response:?}");

    assert_destroyed_group_runtime_names_are_reusable(
        &handler,
        &[owner, peer.clone(), linked_owner],
        &peer,
    )
    .await;
}

#[tokio::test]
async fn runtime_owner_transfer_preserves_existing_next_owner_pixel_geometry() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "pixel-owner").await;
    let peer = create_grouped_session(&handler, "pixel-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    let owner_pixels = TerminalPixels::new(100, 200);
    let peer_pixels = TerminalPixels::new(300, 400);

    let mut state = handler.state.lock().await;
    state.set_attached_terminal_pixels(&owner, Some(owner_pixels));
    state.set_attached_terminal_pixels(&peer, Some(peer_pixels));
    let current_runtime_owner = state.sessions.runtime_owner(&owner);
    let next_runtime_owner = state.sessions.runtime_owner_transfer_target(&owner);
    let _ = state
        .sessions
        .remove_session(&owner)
        .expect("runtime owner exists");
    state
        .remove_session_terminals(
            &owner,
            current_runtime_owner.as_ref(),
            next_runtime_owner.as_ref(),
        )
        .expect("runtime ownership transfers to peer");

    assert_eq!(state.attached_terminal_pixels_for_test(&owner), None);
    assert_eq!(
        state.attached_terminal_pixels_for_test(&peer),
        Some(peer_pixels),
        "an already attached next owner keeps its own pixel geometry"
    );
    state
        .pane_profile_in_window(&peer, 0, 0)
        .expect("peer resolves the transferred runtime");
}

#[tokio::test]
async fn runtime_owner_transfer_does_not_inherit_pixels_into_unattached_next_owner() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "pixel-unattached-owner").await;
    let peer = create_grouped_session(&handler, "pixel-unattached-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;

    let mut state = handler.state.lock().await;
    state.set_attached_terminal_pixels(&owner, Some(TerminalPixels::new(500, 600)));
    let current_runtime_owner = state.sessions.runtime_owner(&owner);
    let next_runtime_owner = state.sessions.runtime_owner_transfer_target(&owner);
    let _ = state
        .sessions
        .remove_session(&owner)
        .expect("runtime owner exists");
    state
        .remove_session_terminals(
            &owner,
            current_runtime_owner.as_ref(),
            next_runtime_owner.as_ref(),
        )
        .expect("runtime ownership transfers to peer");

    assert_eq!(state.attached_terminal_pixels_for_test(&owner), None);
    assert_eq!(
        state.attached_terminal_pixels_for_test(&peer),
        None,
        "an unattached next owner must not inherit destroyed owner pixel geometry"
    );
}

#[tokio::test]
async fn break_before_remaps_every_grouped_link_slot_by_window_identity() {
    let handler = RequestHandler::new();
    let (owner, peer, externals) =
        create_group_with_individually_linked_windows(&handler, "break-before-links", 2).await;
    let alpha = &externals[0];
    let beta = &externals[1];
    set_window_marker(
        &handler,
        WindowTarget::with_window(owner.clone(), 0),
        "alpha",
    )
    .await;
    set_window_marker(
        &handler,
        WindowTarget::with_window(owner.clone(), 1),
        "beta",
    )
    .await;
    select_window(&handler, &peer, 1).await;
    let (alpha_window_id, beta_window_id, alpha_pane_id, beta_pane_id) = {
        let state = handler.state.lock().await;
        assert_eq!(
            explicit_window_marker(&state, &owner, 0).as_deref(),
            Some("alpha")
        );
        assert_eq!(
            explicit_window_marker(&state, &owner, 1).as_deref(),
            Some("beta")
        );
        (
            window_id(&state, &owner, 0),
            window_id(&state, &owner, 1),
            pane_id(&state, &owner, 0, 0),
            pane_id(&state, &owner, 1, 0),
        )
    };

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 0),
            target: Some(WindowTarget::with_window(owner.clone(), 0)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected break-pane -b success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(owner.clone(), 0, 0)
    );

    let state = handler.state.lock().await;
    for session_name in [&owner, &peer] {
        assert_eq!(window_id(&state, session_name, 0), beta_window_id);
        assert_eq!(window_id(&state, session_name, 1), alpha_window_id);
        assert_eq!(pane_id(&state, session_name, 0, 0), beta_pane_id);
        assert_eq!(pane_id(&state, session_name, 1, 0), alpha_pane_id);
        assert_eq!(
            explicit_window_marker(&state, session_name, 0).as_deref(),
            Some("beta")
        );
        assert_eq!(
            explicit_window_marker(&state, session_name, 1).as_deref(),
            Some("alpha")
        );
        state
            .pane_profile_in_window(session_name, 0, 0)
            .expect("source linked runtime follows the moved winlink");
        state
            .pane_profile_in_window(session_name, 1, 0)
            .expect("target linked runtime follows its shifted winlink");
    }
    assert_eq!(window_id(&state, alpha, 0), alpha_window_id);
    assert_eq!(window_id(&state, beta, 0), beta_window_id);
    assert_eq!(
        explicit_window_marker(&state, alpha, 0).as_deref(),
        Some("alpha")
    );
    assert_eq!(
        explicit_window_marker(&state, beta, 0).as_deref(),
        Some("beta")
    );
    assert_eq!(
        state
            .sessions
            .session(&owner)
            .expect("owner survives")
            .active_window_index(),
        1
    );
    assert_eq!(
        state
            .sessions
            .session(&peer)
            .expect("peer survives")
            .active_window_index(),
        0
    );
}

#[tokio::test]
async fn break_after_remaps_shifted_sentinel_and_source_link_slots_by_window_identity() {
    let handler = RequestHandler::new();
    let (owner, peer, externals) =
        create_group_with_individually_linked_windows(&handler, "break-after-links", 3).await;
    for (window_index, marker) in ["alpha", "beta", "sentinel"].into_iter().enumerate() {
        set_window_marker(
            &handler,
            WindowTarget::with_window(owner.clone(), window_index as u32),
            marker,
        )
        .await;
    }
    select_window(&handler, &owner, 1).await;
    select_window(&handler, &peer, 1).await;
    let before = {
        let state = handler.state.lock().await;
        (0..3)
            .map(|window_index| {
                (
                    window_id(&state, &owner, window_index),
                    pane_id(&state, &owner, window_index, 0),
                )
            })
            .collect::<Vec<_>>()
    };

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 0),
            target: Some(WindowTarget::with_window(owner.clone(), 1)),
            name: None,
            detached: true,
            after: true,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected break-pane -a success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(owner.clone(), 2, 0)
    );

    let state = handler.state.lock().await;
    let expected = [(1, 1, "beta"), (2, 0, "alpha"), (3, 2, "sentinel")];
    for session_name in [&owner, &peer] {
        assert!(state
            .sessions
            .session(session_name)
            .expect("group member survives")
            .window_at(0)
            .is_none());
        for (window_index, before_index, marker) in expected {
            assert_eq!(
                window_id(&state, session_name, window_index),
                before[before_index].0
            );
            assert_eq!(
                pane_id(&state, session_name, window_index, 0),
                before[before_index].1
            );
            assert_eq!(
                explicit_window_marker(&state, session_name, window_index).as_deref(),
                Some(marker)
            );
            state
                .pane_profile_in_window(session_name, window_index, 0)
                .expect("remapped linked winlink retains its runtime");
        }
        assert_eq!(
            state
                .sessions
                .session(session_name)
                .expect("group member survives")
                .active_window_index(),
            1
        );
    }
    for (external_index, external) in externals.iter().enumerate() {
        assert_eq!(window_id(&state, external, 0), before[external_index].0);
        assert_eq!(
            explicit_window_marker(&state, external, 0).as_deref(),
            Some(["alpha", "beta", "sentinel"][external_index])
        );
        state
            .pane_profile_in_window(external, 0, 0)
            .expect("external linked session retains its runtime");
    }
}

#[tokio::test]
async fn grouped_sync_preserves_duplicate_winlink_active_index_during_unrelated_split() {
    let handler = RequestHandler::new();
    let (owner, peer, _, _) =
        create_group_with_duplicate_linked_winlinks(&handler, "duplicate-active-split").await;
    select_window(&handler, &owner, 0).await;
    select_window(&handler, &peer, 2).await;

    split_session(&handler, &owner).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&peer)
            .expect("peer survives")
            .active_window_index(),
        2,
        "an unrelated grouped sync must preserve the exact duplicate winlink index"
    );
}

#[tokio::test]
async fn break_before_remaps_the_selected_occurrence_of_duplicate_linked_winlinks() {
    let handler = RequestHandler::new();
    let (owner, peer, base_window_id, linked_window_id) =
        create_group_with_duplicate_linked_winlinks(&handler, "duplicate-break-before").await;
    set_window_marker(
        &handler,
        WindowTarget::with_window(owner.clone(), 0),
        "base",
    )
    .await;
    set_window_marker(
        &handler,
        WindowTarget::with_window(owner.clone(), 1),
        "linked",
    )
    .await;
    select_window(&handler, &owner, 1).await;
    select_window(&handler, &peer, 2).await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 2, 0),
            target: Some(WindowTarget::with_window(owner.clone(), 0)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected duplicate winlink break-pane -b success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(owner.clone(), 0, 0)
    );

    let state = handler.state.lock().await;
    for session_name in [&owner, &peer] {
        assert_eq!(window_id(&state, session_name, 0), linked_window_id);
        assert_eq!(window_id(&state, session_name, 1), base_window_id);
        assert_eq!(window_id(&state, session_name, 2), linked_window_id);
        assert_eq!(
            explicit_window_marker(&state, session_name, 0).as_deref(),
            Some("linked")
        );
        assert_eq!(
            explicit_window_marker(&state, session_name, 1).as_deref(),
            Some("base")
        );
        assert_eq!(
            explicit_window_marker(&state, session_name, 2).as_deref(),
            Some("linked")
        );
        for window_index in 0..=2 {
            state
                .pane_profile_in_window(session_name, window_index, 0)
                .expect("each duplicate occurrence resolves its linked runtime");
        }
    }
    assert_eq!(
        state
            .sessions
            .session(&owner)
            .expect("owner survives")
            .active_window_index(),
        2
    );
    assert_eq!(
        state
            .sessions
            .session(&peer)
            .expect("peer survives")
            .active_window_index(),
        0
    );
}

#[tokio::test]
async fn join_last_linked_pane_from_real_session_removes_all_old_aliases() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_single_pane_linked_window(&handler, "real-linked-join").await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &linked_owner, 0, 0)
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(linked_owner.clone(), 0, 0),
        "@real-linked-join",
        "tracked",
    )
    .await;

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(linked_owner.clone(), 0, 0),
            target: PaneTarget::with_window(owner.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::JoinPane(response) = response else {
        panic!("expected real linked source join success, got {response:?}");
    };
    let moved_target = response.target.clone();

    assert_linked_source_was_removed_and_moved_pane_is_live(
        &handler,
        &owner,
        &peer,
        &linked_owner,
        response.target,
        moved_pane_id,
    )
    .await;
    assert_eq!(
        pane_option(&handler, moved_target, "@real-linked-join").await,
        Some("tracked".to_owned())
    );
}

#[tokio::test]
async fn break_last_linked_pane_from_real_session_matches_tmux_success() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_single_pane_linked_window(&handler, "real-linked-break").await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &linked_owner, 0, 0)
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(linked_owner.clone(), 0, 0),
        "@real-linked-break",
        "tracked",
    )
    .await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(linked_owner.clone(), 0, 0),
            target: Some(WindowTarget::with_window(owner.clone(), 2)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected real linked source break success, got {response:?}");
    };
    let moved_target = response.target.clone();

    {
        let state = handler.state.lock().await;
        assert!(state.sessions.session(&linked_owner).is_none());
        assert!(!state.contains_session_terminals(&linked_owner));
        for group_member in [&owner, &peer] {
            assert_eq!(pane_ids(&state, group_member, 1), vec![moved_pane_id]);
            assert_eq!(pane_ids(&state, group_member, 2), vec![moved_pane_id]);
            let session = state
                .sessions
                .session(group_member)
                .expect("group member survives");
            assert_eq!(
                session.window_at(1).map(rmux_core::Window::id),
                session.window_at(2).map(rmux_core::Window::id),
                "old and new winlinks must address one shared window"
            );
            state
                .pane_profile_in_window(group_member, 1, 0)
                .expect("old winlink retains the moved runtime");
            state
                .pane_profile_in_window(group_member, 2, 0)
                .expect("new winlink resolves the moved runtime");
        }
    }
    for target in [
        moved_target,
        PaneTarget::with_window(owner.clone(), 1, 0),
        PaneTarget::with_window(peer.clone(), 1, 0),
        PaneTarget::with_window(peer, 2, 0),
    ] {
        assert_eq!(
            pane_option(&handler, target, "@real-linked-break").await,
            Some("tracked".to_owned())
        );
    }
}

#[tokio::test]
async fn break_last_linked_pane_within_group_owner_matches_tmux_slot_move() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_single_pane_linked_window(&handler, "same-session-linked-break").await;
    let (moved_pane_id, active_before) = {
        let state = handler.state.lock().await;
        (
            pane_id(&state, &owner, 1, 0),
            [
                state
                    .sessions
                    .session(&owner)
                    .expect("owner exists")
                    .active_window_index(),
                state
                    .sessions
                    .session(&peer)
                    .expect("peer exists")
                    .active_window_index(),
                state
                    .sessions
                    .session(&linked_owner)
                    .expect("linked owner exists")
                    .active_window_index(),
            ],
        )
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(owner.clone(), 1, 0),
        "@same-session-linked-break",
        "tracked",
    )
    .await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 0),
            target: Some(WindowTarget::with_window(owner.clone(), 2)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected same-session linked break success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(owner.clone(), 2, 0)
    );

    {
        let state = handler.state.lock().await;
        let current_active = [
            state
                .sessions
                .session(&owner)
                .expect("owner survives")
                .active_window_index(),
            state
                .sessions
                .session(&peer)
                .expect("peer survives")
                .active_window_index(),
            state
                .sessions
                .session(&linked_owner)
                .expect("linked owner survives")
                .active_window_index(),
        ];
        assert_eq!(current_active, active_before);
        for group_member in [&owner, &peer] {
            assert!(state
                .sessions
                .session(group_member)
                .and_then(|session| session.window_at(1))
                .is_none());
            assert_eq!(pane_ids(&state, group_member, 2), vec![moved_pane_id]);
            state
                .pane_profile_in_window(group_member, 2, 0)
                .expect("moved group slot retains linked runtime");
        }
        assert_eq!(pane_ids(&state, &linked_owner, 0), vec![moved_pane_id]);
        let owner_window_id = state
            .sessions
            .session(&owner)
            .and_then(|session| session.window_at(2))
            .map(rmux_core::Window::id);
        let linked_window_id = state
            .sessions
            .session(&linked_owner)
            .and_then(|session| session.window_at(0))
            .map(rmux_core::Window::id);
        assert_eq!(owner_window_id, linked_window_id);
    }
    for target in [
        response.target,
        PaneTarget::with_window(peer.clone(), 2, 0),
        PaneTarget::with_window(linked_owner, 0, 0),
    ] {
        assert_eq!(
            pane_option(&handler, target, "@same-session-linked-break").await,
            Some("tracked".to_owned())
        );
    }
}

#[tokio::test]
async fn break_last_linked_pane_preserves_other_source_session_windows_and_runtimes() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_single_pane_linked_window(&handler, "surviving-linked-break").await;
    let created = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: linked_owner.clone(),
            name: Some("survivor".to_owned()),
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: Some(1),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(created, Response::NewWindow(_)), "{created:?}");
    handler.wait_for_initial_panes_for_test().await;
    let (moved_pane_id, surviving_pane_id) = {
        let state = handler.state.lock().await;
        (
            pane_id(&state, &linked_owner, 0, 0),
            pane_id(&state, &linked_owner, 1, 0),
        )
    };

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(linked_owner.clone(), 0, 0),
            target: Some(WindowTarget::with_window(owner.clone(), 2)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    let linked_session = state
        .sessions
        .session(&linked_owner)
        .expect("source session with another window must survive");
    assert!(linked_session.window_at(0).is_none());
    assert_eq!(pane_ids(&state, &linked_owner, 1), vec![surviving_pane_id]);
    assert!(state.contains_session_terminals(&linked_owner));
    state
        .pane_profile_in_window(&linked_owner, 1, 0)
        .expect("surviving source window retains its terminal");
    for group_member in [&owner, &peer] {
        assert_eq!(pane_ids(&state, group_member, 1), vec![moved_pane_id]);
        assert_eq!(pane_ids(&state, group_member, 2), vec![moved_pane_id]);
        state
            .pane_profile_in_window(group_member, 2, 0)
            .expect("moved linked pane resolves through destination runtime");
    }
}

#[tokio::test]
async fn cross_session_swap_synchronizes_the_entire_linked_window_family() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "cross-linked-swap").await;
    let gamma = create_session(&handler, "cross-linked-swap-gamma").await;
    handler.wait_for_initial_panes_for_test().await;
    let source_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &owner, 1, 1)
    };
    let target_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &gamma, 0, 0)
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(owner.clone(), 1, 1),
        "@cross-linked-source",
        "source",
    )
    .await;
    set_pane_option(
        &handler,
        PaneTarget::with_window(gamma.clone(), 0, 0),
        "@cross-linked-target",
        "target",
    )
    .await;

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 1),
            target: PaneTarget::with_window(gamma.clone(), 0, 0),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert_linked_window_models_and_runtimes_match(&state, &owner, &peer, &linked_owner);
    assert_eq!(pane_id(&state, &owner, 1, 1), target_pane_id);
    assert_eq!(pane_id(&state, &linked_owner, 0, 1), target_pane_id);
    assert_eq!(pane_id(&state, &gamma, 0, 0), source_pane_id);
    state
        .pane_profile_in_window(&linked_owner, 0, 1)
        .expect("linked source alias remains live after cross-session swap");
    drop(state);

    for target in [
        PaneTarget::with_window(owner.clone(), 1, 1),
        PaneTarget::with_window(peer.clone(), 1, 1),
        PaneTarget::with_window(linked_owner.clone(), 0, 1),
    ] {
        assert_eq!(
            pane_option(&handler, target.clone(), "@cross-linked-target").await,
            Some("target".to_owned())
        );
        assert_eq!(
            pane_option(&handler, target, "@cross-linked-source").await,
            None
        );
    }
    let moved_source = PaneTarget::with_window(gamma, 0, 0);
    assert_eq!(
        pane_option(&handler, moved_source.clone(), "@cross-linked-source").await,
        Some("source".to_owned())
    );
    assert_eq!(
        pane_option(&handler, moved_source, "@cross-linked-target").await,
        None
    );
}

#[tokio::test]
async fn swap_between_two_aliases_of_the_same_linked_window_uses_shared_identity() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "same-linked-window-swap").await;
    let (first_pane_id, second_pane_id) = {
        let state = handler.state.lock().await;
        (
            pane_id(&state, &owner, 1, 0),
            pane_id(&state, &linked_owner, 0, 1),
        )
    };

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 0),
            target: PaneTarget::with_window(linked_owner.clone(), 0, 1),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    for (session_name, window_index) in [(&owner, 1), (&peer, 1), (&linked_owner, 0)] {
        assert_eq!(
            pane_ids(&state, session_name, window_index),
            vec![second_pane_id, first_pane_id]
        );
        state
            .pane_profile_in_window(session_name, window_index, 0)
            .expect("first swapped pane remains live through every alias");
        state
            .pane_profile_in_window(session_name, window_index, 1)
            .expect("second swapped pane remains live through every alias");
    }
}

#[tokio::test]
async fn join_between_two_aliases_of_the_same_linked_window_uses_shared_identity() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "same-linked-window-join").await;
    let panes_before = {
        let state = handler.state.lock().await;
        pane_ids(&state, &owner, 1)
    };

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 1),
            target: PaneTarget::with_window(linked_owner.clone(), 0, 0),
            direction: SplitDirection::Horizontal,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    for (session_name, window_index) in [(&owner, 1), (&peer, 1), (&linked_owner, 0)] {
        assert_eq!(pane_ids(&state, session_name, window_index), panes_before);
        for pane_index in 0..2 {
            state
                .pane_profile_in_window(session_name, window_index, pane_index)
                .expect("joined pane remains live through every alias");
        }
    }
}

#[tokio::test]
async fn move_between_two_aliases_of_the_same_linked_window_uses_shared_identity() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "same-linked-window-move").await;
    let panes_before = {
        let state = handler.state.lock().await;
        pane_ids(&state, &owner, 1)
    };

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 1),
            target: PaneTarget::with_window(linked_owner.clone(), 0, 0),
            direction: SplitDirection::Horizontal,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::MovePane(_)), "{response:?}");

    let state = handler.state.lock().await;
    for (session_name, window_index) in [(&owner, 1), (&peer, 1), (&linked_owner, 0)] {
        assert_eq!(pane_ids(&state, session_name, window_index), panes_before);
        for pane_index in 0..2 {
            state
                .pane_profile_in_window(session_name, window_index, pane_index)
                .expect("moved pane remains live through every alias");
        }
    }
}

#[tokio::test]
async fn cross_session_linked_swap_resize_failure_restores_the_full_transaction() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "cross-linked-rollback").await;
    let gamma = create_session(&handler, "cross-linked-rollback-gamma").await;
    handler.wait_for_initial_panes_for_test().await;
    let source_target = PaneTarget::with_window(owner.clone(), 1, 1);
    let target_target = PaneTarget::with_window(gamma.clone(), 0, 0);
    set_pane_option(
        &handler,
        source_target.clone(),
        "@cross-linked-rollback-source",
        "source",
    )
    .await;
    set_pane_option(
        &handler,
        target_target.clone(),
        "@cross-linked-rollback-target",
        "target",
    )
    .await;

    let (
        source_pane_id,
        target_pane_id,
        linked_panes_before,
        gamma_panes_before,
        source_lifecycle_before,
        target_lifecycle_before,
    ) = {
        let mut state = handler.state.lock().await;
        let source_pane_id = pane_id(&state, &owner, 1, 1);
        let target_pane_id = pane_id(&state, &gamma, 0, 0);
        state
            .mark_pane_dead_without_exit_details(&source_target)
            .expect("source pane can be marked dead for rollback probe");
        let source_lifecycle = state
            .pane_lifecycle(source_pane_id)
            .cloned()
            .expect("source pane lifecycle exists before rollback probe");
        let target_lifecycle = state
            .pane_lifecycle(target_pane_id)
            .cloned()
            .expect("target pane lifecycle exists before rollback probe");
        let linked_panes = pane_ids(&state, &owner, 1);
        let gamma_panes = pane_ids(&state, &gamma, 0);
        state.fail_next_resize_for_test();
        (
            source_pane_id,
            target_pane_id,
            linked_panes,
            gamma_panes,
            source_lifecycle,
            target_lifecycle,
        )
    };

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: source_target.clone(),
            target: target_target.clone(),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(
        matches!(&response, Response::Error(error) if error.error.to_string().contains("injected pane terminal resize failure")),
        "{response:?}"
    );

    let state = handler.state.lock().await;
    assert_eq!(pane_ids(&state, &owner, 1), linked_panes_before);
    assert_eq!(pane_ids(&state, &peer, 1), linked_panes_before);
    assert_eq!(pane_ids(&state, &linked_owner, 0), linked_panes_before);
    assert_eq!(pane_ids(&state, &gamma, 0), gamma_panes_before);
    assert_eq!(
        state.pane_lifecycle(source_pane_id),
        Some(&source_lifecycle_before)
    );
    assert_eq!(
        state.pane_lifecycle(target_pane_id),
        Some(&target_lifecycle_before)
    );
    assert_linked_window_models_and_runtimes_match(&state, &owner, &peer, &linked_owner);
    assert!(state.pane_is_dead(&owner, source_pane_id));
    state
        .pane_profile_in_window(&gamma, 0, 0)
        .expect("target runtime is restored after failed linked swap");
    drop(state);

    assert_eq!(
        pane_option(&handler, source_target, "@cross-linked-rollback-source").await,
        Some("source".to_owned())
    );
    assert_eq!(
        pane_option(&handler, target_target, "@cross-linked-rollback-target").await,
        Some("target".to_owned())
    );
}

#[tokio::test]
async fn cross_session_join_into_linked_target_synchronizes_every_alias() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "cross-linked-join").await;
    let gamma = create_session(&handler, "cross-linked-join-gamma").await;
    split_session(&handler, &gamma).await;
    handler.wait_for_initial_panes_for_test().await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &gamma, 0, 1)
    };

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(gamma.clone(), 0, 1),
            target: PaneTarget::with_window(owner.clone(), 1, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::JoinPane(response) = response else {
        panic!("expected join into linked target success, got {response:?}");
    };

    let state = handler.state.lock().await;
    assert_linked_window_models_and_runtimes_match(&state, &owner, &peer, &linked_owner);
    assert_eq!(
        pane_id(
            &state,
            response.target.session_name(),
            response.target.window_index(),
            response.target.pane_index(),
        ),
        moved_pane_id
    );
    assert!(pane_ids(&state, &linked_owner, 0).contains(&moved_pane_id));
}

#[tokio::test]
async fn cross_session_move_out_of_linked_source_synchronizes_every_alias() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "cross-linked-move").await;
    let gamma = create_session(&handler, "cross-linked-move-gamma").await;
    handler.wait_for_initial_panes_for_test().await;
    let moved_pane_id = {
        let mut state = handler.state.lock().await;
        let moved_pane_id = pane_id(&state, &owner, 1, 1);
        state
            .mark_pane_dead_without_exit_details(&PaneTarget::with_window(owner.clone(), 1, 1))
            .expect("linked source pane can be marked dead before move");
        moved_pane_id
    };

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 1),
            target: PaneTarget::with_window(gamma.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::MovePane(response) = response else {
        panic!("expected move out of linked source success, got {response:?}");
    };

    let state = handler.state.lock().await;
    assert_linked_window_models_and_runtimes_match(&state, &owner, &peer, &linked_owner);
    assert!(!pane_ids(&state, &linked_owner, 0).contains(&moved_pane_id));
    assert_eq!(
        pane_id(
            &state,
            response.target.session_name(),
            response.target.window_index(),
            response.target.pane_index(),
        ),
        moved_pane_id
    );
    assert!(state.pane_is_dead(response.target.session_name(), moved_pane_id));
}

#[tokio::test]
async fn cross_session_move_clears_options_from_every_duplicate_source_winlink() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) = create_group_with_duplicate_two_pane_linked_window(
        &handler,
        "cross-move-duplicate-options",
    )
    .await;
    let gamma = create_session(&handler, "cross-move-duplicate-options-gamma").await;
    handler.wait_for_initial_panes_for_test().await;
    let moved_source = PaneTarget::with_window(owner.clone(), 1, 0);
    let moved_pane_id = {
        let state = handler.state.lock().await;
        let moved_pane_id = pane_id(&state, &owner, 1, 0);
        assert_eq!(pane_id(&state, &owner, 2, 0), moved_pane_id);
        assert_eq!(pane_id(&state, &peer, 1, 0), moved_pane_id);
        assert_eq!(pane_id(&state, &peer, 2, 0), moved_pane_id);
        moved_pane_id
    };
    set_pane_option(
        &handler,
        moved_source.clone(),
        "@duplicate-source-pane",
        "moved",
    )
    .await;

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: moved_source,
            target: PaneTarget::with_window(gamma, 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::MovePane(response) = response else {
        panic!("expected duplicate-winlink move success, got {response:?}");
    };

    let state = handler.state.lock().await;
    for (session_name, window_index) in [
        (&owner, 1),
        (&owner, 2),
        (&peer, 1),
        (&peer, 2),
        (&linked_owner, 0),
    ] {
        assert!(!pane_ids(&state, session_name, window_index).contains(&moved_pane_id));
    }
    drop(state);
    for target in [
        PaneTarget::with_window(owner, 1, 0),
        PaneTarget::with_window(peer, 2, 0),
        PaneTarget::with_window(linked_owner, 0, 0),
    ] {
        assert_eq!(
            pane_option(&handler, target, "@duplicate-source-pane").await,
            None
        );
    }
    assert_eq!(
        pane_option(&handler, response.target, "@duplicate-source-pane").await,
        Some("moved".to_owned())
    );
}

#[tokio::test]
async fn kill_pane_rekeys_options_across_every_duplicate_linked_winlink() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_duplicate_two_pane_linked_window(&handler, "kill-duplicate-options")
            .await;
    let killed_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &owner, 1, 0)
    };
    let remaining_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &owner, 1, 1)
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(owner.clone(), 1, 0),
        "@duplicate-killed-pane",
        "killed",
    )
    .await;
    set_pane_option(
        &handler,
        PaneTarget::with_window(owner.clone(), 1, 1),
        "@duplicate-remaining-pane",
        "remaining",
    )
    .await;

    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(owner.clone(), 1, 0),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    for (session_name, window_index) in [
        (&owner, 1),
        (&owner, 2),
        (&peer, 1),
        (&peer, 2),
        (&linked_owner, 0),
    ] {
        assert_eq!(
            pane_ids(&state, session_name, window_index),
            vec![remaining_pane_id]
        );
        assert_ne!(
            pane_id(&state, session_name, window_index, 0),
            killed_pane_id
        );
    }
    drop(state);
    for (session_name, window_index) in [
        (&owner, 1),
        (&owner, 2),
        (&peer, 1),
        (&peer, 2),
        (&linked_owner, 0),
    ] {
        let target = PaneTarget::with_window(session_name.clone(), window_index, 0);
        assert_eq!(
            pane_option(&handler, target.clone(), "@duplicate-killed-pane").await,
            None
        );
        assert_eq!(
            pane_option(&handler, target, "@duplicate-remaining-pane").await,
            Some("remaining".to_owned())
        );
    }
}

#[tokio::test]
async fn grouped_move_rekeys_options_across_every_duplicate_linked_winlink() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_duplicate_two_pane_linked_window(&handler, "grouped-duplicate-options")
            .await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &owner, 1, 0)
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(owner.clone(), 1, 0),
        "@grouped-duplicate-moved",
        "moved",
    )
    .await;

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 0),
            target: PaneTarget::with_window(peer.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::MovePane(response) = response else {
        panic!("expected grouped duplicate move success, got {response:?}");
    };

    let state = handler.state.lock().await;
    for (session_name, window_index) in [
        (&owner, 1),
        (&owner, 2),
        (&peer, 1),
        (&peer, 2),
        (&linked_owner, 0),
    ] {
        assert!(!pane_ids(&state, session_name, window_index).contains(&moved_pane_id));
    }
    assert!(pane_ids(&state, &owner, 0).contains(&moved_pane_id));
    assert!(pane_ids(&state, &peer, 0).contains(&moved_pane_id));
    drop(state);
    assert_eq!(
        pane_option(&handler, response.target, "@grouped-duplicate-moved").await,
        Some("moved".to_owned())
    );
    for target in [
        PaneTarget::with_window(owner, 1, 0),
        PaneTarget::with_window(peer, 2, 0),
        PaneTarget::with_window(linked_owner, 0, 0),
    ] {
        assert_eq!(
            pane_option(&handler, target, "@grouped-duplicate-moved").await,
            None
        );
    }
}

#[tokio::test]
async fn grouped_move_last_pane_removes_every_duplicate_linked_occurrence() {
    let handler = RequestHandler::new();
    let (owner, peer, _base_window_id, _linked_window_id) =
        create_group_with_duplicate_linked_winlinks(&handler, "grouped-last-duplicate-move").await;
    let source = PaneTarget::with_window(owner.clone(), 1, 0);
    let moved_pane_id = {
        let state = handler.state.lock().await;
        let moved_pane_id = pane_id(&state, &owner, 1, 0);
        assert_eq!(pane_id(&state, &owner, 2, 0), moved_pane_id);
        moved_pane_id
    };
    set_pane_option(&handler, source.clone(), "@grouped-last-duplicate", "moved").await;

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source,
            target: PaneTarget::with_window(peer.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::MovePane(response) = response else {
        panic!("expected grouped last duplicate move success, got {response:?}");
    };

    let state = handler.state.lock().await;
    for group_member in [&owner, &peer] {
        assert!(state
            .sessions
            .session(group_member)
            .is_some_and(|session| session.window_at(1).is_none()));
        assert!(state
            .sessions
            .session(group_member)
            .is_some_and(|session| session.window_at(2).is_none()));
        assert!(pane_ids(&state, group_member, 0).contains(&moved_pane_id));
        let moved_index = state
            .sessions
            .session(group_member)
            .and_then(|session| session.window_at(0))
            .and_then(|window| {
                window
                    .panes()
                    .iter()
                    .find(|pane| pane.id() == moved_pane_id)
                    .map(rmux_core::Pane::index)
            })
            .expect("moved pane exists in grouped target");
        state
            .pane_profile_in_window(group_member, 0, moved_index)
            .expect("moved last duplicate pane retains runtime");
    }
    drop(state);
    assert_eq!(
        pane_option(&handler, response.target, "@grouped-last-duplicate").await,
        Some("moved".to_owned())
    );
}

#[tokio::test]
async fn split_before_linked_pane_keeps_option_on_stable_pane_identity() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_single_pane_linked_window(&handler, "linked-split-before-options").await;
    let old_target = PaneTarget::with_window(owner.clone(), 1, 0);
    let old_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &owner, 1, 0)
    };
    set_pane_option(
        &handler,
        old_target.clone(),
        "@linked-split-before",
        "old-pane",
    )
    .await;

    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(old_target),
            direction: SplitDirection::Vertical,
            before: true,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    for (session_name, window_index) in [(&owner, 1), (&peer, 1), (&linked_owner, 0)] {
        assert_eq!(pane_id(&state, session_name, window_index, 1), old_pane_id);
        state
            .pane_profile_in_window(session_name, window_index, 0)
            .expect("new split pane is live through linked alias");
        state
            .pane_profile_in_window(session_name, window_index, 1)
            .expect("old split pane is live through linked alias");
    }
    drop(state);
    for (session_name, window_index) in [(&owner, 1), (&peer, 1), (&linked_owner, 0)] {
        assert_eq!(
            pane_option(
                &handler,
                PaneTarget::with_window(session_name.clone(), window_index, 1),
                "@linked-split-before"
            )
            .await,
            Some("old-pane".to_owned())
        );
        assert_eq!(
            pane_option(
                &handler,
                PaneTarget::with_window(session_name.clone(), window_index, 0),
                "@linked-split-before"
            )
            .await,
            None
        );
    }
}

#[tokio::test]
async fn cross_session_break_out_of_linked_source_synchronizes_every_alias() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "cross-linked-break").await;
    let gamma = create_session(&handler, "cross-linked-break-gamma").await;
    handler.wait_for_initial_panes_for_test().await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &owner, 1, 1)
    };

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 1),
            target: Some(WindowTarget::with_window(gamma.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected break out of linked source success, got {response:?}");
    };

    let state = handler.state.lock().await;
    assert_linked_window_models_and_runtimes_match(&state, &owner, &peer, &linked_owner);
    assert!(!pane_ids(&state, &linked_owner, 0).contains(&moved_pane_id));
    assert_eq!(pane_id(&state, &gamma, 1, 0), moved_pane_id);
    assert_eq!(response.target, PaneTarget::with_window(gamma, 1, 0));
}

#[tokio::test]
async fn cross_session_break_before_remaps_duplicate_destination_winlinks() {
    let handler = RequestHandler::new();
    let (owner, peer, base_window_id, linked_window_id) =
        create_group_with_duplicate_linked_winlinks(&handler, "cross-break-before-duplicates")
            .await;
    let source = create_session(&handler, "cross-break-before-duplicates-source").await;
    split_session(&handler, &source).await;
    handler.wait_for_initial_panes_for_test().await;
    set_window_marker(
        &handler,
        WindowTarget::with_window(owner.clone(), 0),
        "base-slot",
    )
    .await;
    set_window_marker(
        &handler,
        WindowTarget::with_window(peer.clone(), 0),
        "base-slot",
    )
    .await;
    set_window_marker(
        &handler,
        WindowTarget::with_window(owner.clone(), 1),
        "linked-slot",
    )
    .await;
    set_window_marker(
        &handler,
        WindowTarget::with_window(peer.clone(), 1),
        "linked-slot",
    )
    .await;
    set_window_marker(
        &handler,
        WindowTarget::with_window(owner.clone(), 2),
        "linked-slot",
    )
    .await;
    set_window_marker(
        &handler,
        WindowTarget::with_window(peer.clone(), 2),
        "linked-slot",
    )
    .await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &source, 0, 1)
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(source.clone(), 0, 1),
        "@cross-break-before-moved",
        "moved",
    )
    .await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source, 0, 1),
            target: Some(WindowTarget::with_window(owner.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected cross-session break-before success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(owner.clone(), 1, 0)
    );

    let state = handler.state.lock().await;
    for group_member in [&owner, &peer] {
        assert_eq!(window_id(&state, group_member, 0), base_window_id);
        assert_eq!(window_id(&state, group_member, 2), linked_window_id);
        assert_eq!(window_id(&state, group_member, 3), linked_window_id);
        assert_eq!(pane_id(&state, group_member, 1, 0), moved_pane_id);
        assert_eq!(
            explicit_window_marker(&state, group_member, 0).as_deref(),
            Some("base-slot")
        );
        assert_eq!(
            explicit_window_marker(&state, group_member, 1),
            None,
            "inserted window must not inherit shifted winlink metadata"
        );
        assert_eq!(
            explicit_window_marker(&state, group_member, 2).as_deref(),
            Some("linked-slot")
        );
        assert_eq!(
            explicit_window_marker(&state, group_member, 3).as_deref(),
            Some("linked-slot")
        );
        state
            .pane_profile_in_window(group_member, 1, 0)
            .expect("broken pane uses the destination group runtime");
        state
            .pane_profile_in_window(group_member, 2, 0)
            .expect("first duplicate winlink retains its linked runtime");
        state
            .pane_profile_in_window(group_member, 3, 0)
            .expect("second duplicate winlink retains its linked runtime");
    }
    drop(state);
    assert_eq!(
        pane_option(&handler, response.target, "@cross-break-before-moved").await,
        Some("moved".to_owned())
    );
}

#[tokio::test]
async fn swap_between_group_aliases_moves_terminals_across_linked_runtime_owners() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "same-group-linked-swap").await;
    let (source_pane_id, target_pane_id) = {
        let state = handler.state.lock().await;
        (pane_id(&state, &owner, 0, 0), pane_id(&state, &peer, 1, 0))
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(owner.clone(), 0, 0),
        "@same-group-linked-swap",
        "tracked",
    )
    .await;

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 0),
            target: PaneTarget::with_window(peer.clone(), 1, 0),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert_eq!(pane_id(&state, &owner, 0, 0), target_pane_id);
    assert_eq!(pane_id(&state, &peer, 1, 0), source_pane_id);
    assert_eq!(pane_id(&state, &linked_owner, 0, 0), source_pane_id);
    state
        .pane_profile_in_window(&owner, 0, 0)
        .expect("target terminal moved to the group runtime");
    state
        .pane_profile_in_window(&peer, 1, 0)
        .expect("source terminal moved to the linked runtime owner");
    state
        .pane_profile_in_window(&linked_owner, 0, 0)
        .expect("linked slot resolves the moved source terminal");
    drop(state);

    for target in [
        PaneTarget::with_window(peer.clone(), 1, 0),
        PaneTarget::with_window(linked_owner.clone(), 0, 0),
    ] {
        assert_eq!(
            pane_option(&handler, target, "@same-group-linked-swap").await,
            Some("tracked".to_owned()),
        );
    }
    assert_eq!(
        pane_option(
            &handler,
            PaneTarget::with_window(owner, 0, 0),
            "@same-group-linked-swap",
        )
        .await,
        None,
    );
}

#[tokio::test]
async fn join_between_group_aliases_moves_terminal_into_linked_runtime_owner() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "same-group-linked-join").await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &owner, 0, 1)
    };

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 1),
            target: PaneTarget::with_window(peer.clone(), 1, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::JoinPane(response) = response else {
        panic!("expected linked-runtime join success, got {response:?}");
    };

    let state = handler.state.lock().await;
    assert!(pane_ids(&state, &owner, 0)
        .iter()
        .all(|pane_id| *pane_id != moved_pane_id));
    assert!(pane_ids(&state, &peer, 1).contains(&moved_pane_id));
    assert_eq!(
        pane_ids(&state, &peer, 1),
        pane_ids(&state, &linked_owner, 0)
    );
    state
        .pane_profile_in_window(
            &peer,
            response.target.window_index(),
            response.target.pane_index(),
        )
        .expect("joined pane resolves through the linked runtime owner");
}

#[tokio::test]
async fn break_last_linked_pane_between_group_aliases_matches_tmux_group_rejection() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_single_pane_linked_window(&handler, "last-group-linked-break").await;
    let (owner_before, peer_before, linked_before) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&owner)
                .expect("owner exists")
                .clone(),
            state.sessions.session(&peer).expect("peer exists").clone(),
            state
                .sessions
                .session(&linked_owner)
                .expect("linked owner exists")
                .clone(),
        )
    };

    for detached in [true, false] {
        let response = handler
            .handle(Request::BreakPane(Box::new(BreakPaneRequest {
                source: PaneTarget::with_window(peer.clone(), 1, 0),
                target: Some(WindowTarget::with_window(owner.clone(), 2)),
                name: None,
                detached,
                after: false,
                before: false,
                print_target: false,
                format: None,
            })))
            .await;
        assert!(
            matches!(&response, Response::Error(error) if error.error.to_string().contains("sessions are grouped")),
            "expected grouped-session rejection, got {response:?}"
        );
    }

    let state = handler.state.lock().await;
    assert_eq!(state.sessions.session(&owner), Some(&owner_before));
    assert_eq!(state.sessions.session(&peer), Some(&peer_before));
    assert_eq!(state.sessions.session(&linked_owner), Some(&linked_before));
    state
        .pane_profile_in_window(&peer, 1, 0)
        .expect("rejected pane retains its linked runtime alias");
    state
        .pane_profile_in_window(&linked_owner, 0, 0)
        .expect("linked runtime owner retains the rejected pane");
}

#[tokio::test]
async fn break_between_group_aliases_moves_terminal_out_of_linked_runtime_owner() {
    let handler = RequestHandler::new();
    let (owner, peer, linked_owner) =
        create_group_with_linked_runtime_window(&handler, "same-group-linked-break").await;
    let (moved_pane_id, peer_active_before, linked_active_before) = {
        let state = handler.state.lock().await;
        (
            pane_id(&state, &peer, 1, 1),
            state
                .sessions
                .session(&peer)
                .expect("peer exists")
                .active_window_index(),
            state
                .sessions
                .session(&linked_owner)
                .expect("linked owner exists")
                .active_window_index(),
        )
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(peer.clone(), 1, 1),
        "@same-group-linked-break",
        "tracked",
    )
    .await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(peer.clone(), 1, 1),
            target: Some(WindowTarget::with_window(owner.clone(), 2)),
            name: None,
            detached: false,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected linked-runtime break success, got {response:?}");
    };
    assert_eq!(
        response.target,
        PaneTarget::with_window(owner.clone(), 2, 0)
    );

    let state = handler.state.lock().await;
    for group_member in [&owner, &peer] {
        assert_eq!(pane_ids(&state, group_member, 2), vec![moved_pane_id]);
        assert_eq!(
            pane_ids(&state, group_member, 1),
            pane_ids(&state, &linked_owner, 0)
        );
        assert!(!pane_ids(&state, group_member, 1).contains(&moved_pane_id));
        state
            .pane_profile_in_window(group_member, 2, 0)
            .expect("broken pane moved back to the group runtime owner");
    }
    assert_eq!(
        state
            .sessions
            .session(&owner)
            .expect("owner survives")
            .active_window_index(),
        2,
    );
    assert_eq!(
        state
            .sessions
            .session(&peer)
            .expect("peer survives")
            .active_window_index(),
        peer_active_before,
    );
    assert_eq!(
        state
            .sessions
            .session(&linked_owner)
            .expect("linked owner survives")
            .active_window_index(),
        linked_active_before,
    );
    drop(state);

    for target in [response.target, PaneTarget::with_window(peer.clone(), 2, 0)] {
        assert_eq!(
            pane_option(&handler, target, "@same-group-linked-break").await,
            Some("tracked".to_owned())
        );
    }
    for target in [
        PaneTarget::with_window(owner, 1, 0),
        PaneTarget::with_window(peer, 1, 0),
        PaneTarget::with_window(linked_owner, 0, 0),
    ] {
        assert_eq!(
            pane_option(&handler, target, "@same-group-linked-break").await,
            None
        );
    }
}
