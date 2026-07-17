use super::RequestHandler;
use rmux_proto::{
    BreakPaneRequest, JoinPaneRequest, MovePaneRequest, NewSessionExtRequest, NewSessionRequest,
    NewWindowRequest, OptionName, PaneOptionGetRequest, PaneOptionSetRequest, PaneTarget,
    PaneTargetRef, Request, Response, ScopeSelector, SessionName, SetOptionMode, SetOptionRequest,
    SplitDirection, SplitWindowRequest, SplitWindowTarget, SwapPaneRequest, TerminalSize,
    WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

pub(super) async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session_name = session_name(name);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session_name
}

pub(super) async fn create_grouped_session(
    handler: &RequestHandler,
    name: &str,
    group_target: &SessionName,
) -> SessionName {
    let session_name = session_name(name);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: Some(group_target.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session_name
}

async fn create_group_with_two_panes(
    handler: &RequestHandler,
    owner_name: &str,
    peer_name: &str,
) -> (SessionName, SessionName) {
    let owner = create_session(handler, owner_name).await;
    split_session(handler, &owner).await;
    let peer = create_grouped_session(handler, peer_name, &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    (owner, peer)
}

pub(super) async fn split_session(handler: &RequestHandler, session_name: &SessionName) {
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session_name.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
}

async fn create_window(handler: &RequestHandler, session_name: &SessionName, window_index: u32) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: Some(window_index),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;
}

async fn set_monitor_silence(handler: &RequestHandler, scope: ScopeSelector, seconds: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope,
            option: OptionName::MonitorSilence,
            value: seconds.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn assert_intra_window_transfer_preserves_unrelated_silence_timer(
    label: &str,
    move_pane: bool,
) {
    let handler = RequestHandler::new();
    let session = create_session(&handler, label).await;
    split_session(&handler, &session).await;
    create_window(&handler, &session, 1).await;

    let unrelated = WindowTarget::with_window(session.clone(), 1);
    set_monitor_silence(&handler, ScopeSelector::Window(unrelated.clone()), "60").await;
    let before = handler
        .silence_timer_snapshot_for_test(&unrelated)
        .expect("unrelated window timer is armed before the pane transfer");

    let source = PaneTarget::with_window(session.clone(), 0, 1);
    let target = PaneTarget::with_window(session, 0, 0);
    let response = if move_pane {
        handler
            .handle(Request::MovePane(MovePaneRequest {
                source,
                target,
                direction: SplitDirection::Vertical,
                detached: true,
                before: false,
                full_size: false,
                size: None,
            }))
            .await
    } else {
        handler
            .handle(Request::JoinPane(JoinPaneRequest {
                source,
                target,
                direction: SplitDirection::Vertical,
                detached: true,
                before: false,
                full_size: false,
                size: None,
            }))
            .await
    };
    assert!(
        matches!(response, Response::JoinPane(_) | Response::MovePane(_)),
        "{response:?}"
    );
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&unrelated),
        Some(before),
        "an intra-window pane transfer must not restart an unrelated window timer"
    );
}

#[tokio::test]
async fn intra_window_join_and_move_preserve_unrelated_silence_deadlines() {
    assert_intra_window_transfer_preserves_unrelated_silence_timer("join-unrelated-silence", false)
        .await;
    assert_intra_window_transfer_preserves_unrelated_silence_timer("move-unrelated-silence", true)
        .await;
}

#[tokio::test]
async fn grouped_break_preserves_peer_timer_and_arms_the_new_peer_window() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "break-silence-owner").await;
    split_session(&handler, &owner).await;
    let peer = create_grouped_session(&handler, "break-silence-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;

    set_monitor_silence(&handler, ScopeSelector::Session(peer.clone()), "60").await;
    let source_peer = WindowTarget::with_window(peer.clone(), 0);
    let before = handler
        .silence_timer_snapshot_for_test(&source_peer)
        .expect("group peer source timer is armed before break-pane");

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 1),
            target: Some(WindowTarget::with_window(peer.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&source_peer),
        Some(before),
        "break-pane must preserve the existing group peer deadline"
    );
    assert!(
        handler
            .silence_timer_snapshot_for_test(&WindowTarget::with_window(peer, 1))
            .is_some(),
        "break-pane arms the newly-created peer window from its session option"
    );
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&WindowTarget::with_window(owner, 1)),
        None,
        "mixed group options must not arm the owner peer"
    );
}

pub(super) fn pane_id(
    state: &super::HandlerState,
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
) -> rmux_core::PaneId {
    state
        .sessions
        .session(session_name)
        .and_then(|session| session.pane_id_in_window(window_index, pane_index))
        .expect("pane exists")
}

fn session_contains_pane(session: &rmux_core::Session, pane_id: rmux_core::PaneId) -> bool {
    session
        .windows()
        .values()
        .any(|window| window.panes().iter().any(|pane| pane.id() == pane_id))
}

pub(super) fn pane_ids(
    state: &super::HandlerState,
    session_name: &SessionName,
    window_index: u32,
) -> Vec<rmux_core::PaneId> {
    state
        .sessions
        .session(session_name)
        .and_then(|session| session.window_at(window_index))
        .expect("window exists")
        .panes()
        .iter()
        .map(rmux_core::Pane::id)
        .collect()
}

pub(super) async fn set_pane_option(
    handler: &RequestHandler,
    target: PaneTarget,
    name: &str,
    value: &str,
) {
    let response = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::slot(target),
            name: name.to_owned(),
            value: Some(value.to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(
        matches!(response, Response::PaneOptionSet(_)),
        "{response:?}"
    );
}

pub(super) async fn pane_option(
    handler: &RequestHandler,
    target: PaneTarget,
    name: &str,
) -> Option<String> {
    match handler
        .handle(Request::PaneOptionGet(PaneOptionGetRequest {
            target: PaneTargetRef::slot(target),
            name: name.to_owned(),
        }))
        .await
    {
        Response::PaneOptionGet(response) => response.value,
        response => panic!("pane-option-get failed: {response:?}"),
    }
}

#[tokio::test]
async fn swap_pane_between_aliases_of_the_same_group_mutates_shared_state_once() {
    let handler = RequestHandler::new();
    let (owner, peer) =
        create_group_with_two_panes(&handler, "same-group-swap-owner", "same-group-swap-peer")
            .await;
    let before = {
        let state = handler.state.lock().await;
        pane_ids(&state, &owner, 0)
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(peer.clone(), 0, 0),
        "@same-group-swap",
        "tracked",
    )
    .await;

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 0),
            target: PaneTarget::with_window(peer.clone(), 0, 1),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    let expected = vec![before[1], before[0]];
    assert_eq!(pane_ids(&state, &owner, 0), expected);
    assert_eq!(pane_ids(&state, &peer, 0), expected);
    for pane_index in 0..2 {
        state
            .pane_profile_in_window(&peer, 0, pane_index)
            .expect("shared runtime terminal remains reachable through peer alias");
    }
    drop(state);

    for session_name in [&owner, &peer] {
        assert_eq!(
            pane_option(
                &handler,
                PaneTarget::with_window(session_name.clone(), 0, 1),
                "@same-group-swap",
            )
            .await,
            Some("tracked".to_owned()),
        );
        assert_eq!(
            pane_option(
                &handler,
                PaneTarget::with_window(session_name.clone(), 0, 0),
                "@same-group-swap",
            )
            .await,
            None,
        );
    }
}

#[tokio::test]
async fn join_pane_between_aliases_of_the_same_group_uses_single_session_semantics() {
    let handler = RequestHandler::new();
    let (owner, peer) =
        create_group_with_two_panes(&handler, "same-group-join-owner", "same-group-join-peer")
            .await;
    set_pane_option(
        &handler,
        PaneTarget::with_window(owner.clone(), 0, 1),
        "@same-group-join",
        "tracked",
    )
    .await;

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 1),
            target: PaneTarget::with_window(peer.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::JoinPane(response) = response else {
        panic!("expected same-group join-pane success, got {response:?}");
    };
    assert_eq!(response.target.session_name(), &peer);
    let moved_index = response.target.pane_index();

    let state = handler.state.lock().await;
    assert_eq!(pane_ids(&state, &owner, 0), pane_ids(&state, &peer, 0));
    assert_eq!(pane_ids(&state, &peer, 0).len(), 2);
    state
        .pane_profile_in_window(
            &peer,
            response.target.window_index(),
            response.target.pane_index(),
        )
        .expect("joined pane remains backed by the shared runtime terminal");
    drop(state);

    for session_name in [&owner, &peer] {
        assert_eq!(
            pane_option(
                &handler,
                PaneTarget::with_window(session_name.clone(), 0, moved_index),
                "@same-group-join",
            )
            .await,
            Some("tracked".to_owned()),
        );
        assert_eq!(
            pane_option(
                &handler,
                PaneTarget::with_window(session_name.clone(), 0, u32::from(moved_index == 0)),
                "@same-group-join",
            )
            .await,
            None,
        );
    }
}

#[tokio::test]
async fn non_detached_join_between_group_aliases_selects_the_destination_alias() {
    let handler = RequestHandler::new();
    let (owner, peer) = create_group_with_two_panes(
        &handler,
        "same-group-join-select-owner",
        "same-group-join-select-peer",
    )
    .await;
    create_window(&handler, &owner, 1).await;

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 1),
            target: PaneTarget::with_window(peer.clone(), 1, 0),
            direction: SplitDirection::Vertical,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&owner)
            .map(rmux_core::Session::active_window_index),
        Some(0),
    );
    assert_eq!(
        state
            .sessions
            .session(&peer)
            .map(rmux_core::Session::active_window_index),
        Some(1),
    );
}

#[tokio::test]
async fn break_pane_between_aliases_of_the_same_group_uses_single_session_semantics() {
    let handler = RequestHandler::new();
    let (owner, peer) =
        create_group_with_two_panes(&handler, "same-group-break-owner", "same-group-break-peer")
            .await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &owner, 0, 1)
    };
    set_pane_option(
        &handler,
        PaneTarget::with_window(peer.clone(), 0, 1),
        "@same-group-break",
        "tracked",
    )
    .await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 1),
            target: Some(WindowTarget::with_window(peer.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected same-group break-pane success, got {response:?}");
    };
    assert_eq!(response.target.session_name(), &peer);

    let state = handler.state.lock().await;
    assert_eq!(pane_ids(&state, &owner, 0), pane_ids(&state, &peer, 0));
    assert_eq!(pane_ids(&state, &owner, 1), vec![moved_pane_id]);
    assert_eq!(pane_ids(&state, &peer, 1), vec![moved_pane_id]);
    state
        .pane_profile_in_window(&peer, 1, 0)
        .expect("broken pane remains backed by the shared runtime terminal");
    drop(state);

    for session_name in [&owner, &peer] {
        assert_eq!(
            pane_option(
                &handler,
                PaneTarget::with_window(session_name.clone(), 1, 0),
                "@same-group-break",
            )
            .await,
            Some("tracked".to_owned()),
        );
        assert_eq!(
            pane_option(
                &handler,
                PaneTarget::with_window(session_name.clone(), 0, 0),
                "@same-group-break",
            )
            .await,
            None,
        );
    }
}

#[tokio::test]
async fn non_detached_break_between_group_aliases_selects_the_destination_alias() {
    let handler = RequestHandler::new();
    let (owner, peer) = create_group_with_two_panes(
        &handler,
        "same-group-break-select-owner",
        "same-group-break-select-peer",
    )
    .await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 0, 1),
            target: Some(WindowTarget::with_window(peer.clone(), 1)),
            name: None,
            detached: false,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&owner)
            .map(rmux_core::Session::active_window_index),
        Some(0),
    );
    assert_eq!(
        state
            .sessions
            .session(&peer)
            .map(rmux_core::Session::active_window_index),
        Some(1),
    );
}

#[tokio::test]
async fn break_last_pane_between_group_aliases_matches_tmux_rejection() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "last-pane-group-break-owner").await;
    let peer = create_grouped_session(&handler, "last-pane-group-break-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    let (owner_before, peer_before) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&owner)
                .expect("owner exists")
                .clone(),
            state.sessions.session(&peer).expect("peer exists").clone(),
        )
    };

    for detached in [true, false] {
        let response = handler
            .handle(Request::BreakPane(Box::new(BreakPaneRequest {
                source: PaneTarget::with_window(owner.clone(), 0, 0),
                target: Some(WindowTarget::with_window(peer.clone(), 1)),
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
    state
        .pane_profile_in_window(&peer, 0, 0)
        .expect("rejected break must preserve grouped runtime terminal");
}

#[tokio::test]
async fn join_pane_from_group_peer_moves_the_runtime_owned_pane() {
    let handler = RequestHandler::new();
    let (owner, peer) =
        create_group_with_two_panes(&handler, "join-group-owner", "join-group-peer").await;
    let target = create_session(&handler, "join-group-target").await;
    handler.wait_for_initial_panes_for_test().await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &peer, 0, 1)
    };

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(peer.clone(), 0, 1),
            target: PaneTarget::with_window(target.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::JoinPane(response) = response else {
        panic!("expected grouped-peer join-pane success, got {response:?}");
    };

    let state = handler.state.lock().await;
    for group_member in [&owner, &peer] {
        assert!(
            state
                .sessions
                .session(group_member)
                .is_some_and(|session| !session_contains_pane(session, moved_pane_id)),
            "moved pane must leave grouped member {group_member}"
        );
    }
    assert_eq!(
        pane_id(
            &state,
            &target,
            response.target.window_index(),
            response.target.pane_index(),
        ),
        moved_pane_id
    );
    state
        .pane_profile_in_window(
            &target,
            response.target.window_index(),
            response.target.pane_index(),
        )
        .expect("moved pane terminal follows the model into target runtime");
}

#[tokio::test]
async fn break_pane_from_group_peer_moves_the_runtime_owned_pane() {
    let handler = RequestHandler::new();
    let (owner, peer) =
        create_group_with_two_panes(&handler, "break-group-owner", "break-group-peer").await;
    let target = create_session(&handler, "break-group-target").await;
    handler.wait_for_initial_panes_for_test().await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &peer, 0, 1)
    };

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(peer.clone(), 0, 1),
            target: Some(WindowTarget::with_window(target.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected grouped-peer break-pane success, got {response:?}");
    };

    let state = handler.state.lock().await;
    for group_member in [&owner, &peer] {
        assert!(
            state
                .sessions
                .session(group_member)
                .is_some_and(|session| !session_contains_pane(session, moved_pane_id)),
            "moved pane must leave grouped member {group_member}"
        );
    }
    assert_eq!(
        pane_id(
            &state,
            &target,
            response.target.window_index(),
            response.target.pane_index(),
        ),
        moved_pane_id
    );
    state
        .pane_profile_in_window(
            &target,
            response.target.window_index(),
            response.target.pane_index(),
        )
        .expect("broken pane terminal follows the model into target runtime");
}

#[tokio::test]
async fn swap_pane_from_group_peer_swaps_runtime_owned_panes() {
    let handler = RequestHandler::new();
    let (owner, peer) =
        create_group_with_two_panes(&handler, "swap-group-owner", "swap-group-peer").await;
    let target = create_session(&handler, "swap-group-target").await;
    handler.wait_for_initial_panes_for_test().await;
    let (source_pane_id, target_pane_id) = {
        let state = handler.state.lock().await;
        (pane_id(&state, &peer, 0, 0), pane_id(&state, &target, 0, 0))
    };

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: PaneTarget::with_window(peer.clone(), 0, 0),
            target: PaneTarget::with_window(target.clone(), 0, 0),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    for group_member in [&owner, &peer] {
        assert_eq!(pane_id(&state, group_member, 0, 0), target_pane_id);
        state
            .pane_profile_in_window(group_member, 0, 0)
            .expect("target terminal must move into the shared group runtime");
    }
    assert_eq!(pane_id(&state, &target, 0, 0), source_pane_id);
    state
        .pane_profile_in_window(&target, 0, 0)
        .expect("group-owned source terminal must move into target runtime");
}

#[tokio::test]
async fn join_pane_into_group_peer_moves_into_the_runtime_owner() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "join-destination-source").await;
    split_session(&handler, &source).await;
    let owner = create_session(&handler, "join-destination-owner").await;
    let peer = create_grouped_session(&handler, "join-destination-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &source, 0, 1)
    };

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(source.clone(), 0, 1),
            target: PaneTarget::with_window(peer.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    let Response::JoinPane(response) = response else {
        panic!("expected grouped-destination join-pane success, got {response:?}");
    };

    let state = handler.state.lock().await;
    for group_member in [&owner, &peer] {
        assert_eq!(
            pane_id(
                &state,
                group_member,
                response.target.window_index(),
                response.target.pane_index(),
            ),
            moved_pane_id
        );
        state
            .pane_profile_in_window(
                group_member,
                response.target.window_index(),
                response.target.pane_index(),
            )
            .expect("joined pane must be reachable through each grouped destination alias");
    }
}

#[tokio::test]
async fn break_pane_into_group_peer_moves_into_the_runtime_owner() {
    let handler = RequestHandler::new();
    let source = create_session(&handler, "break-destination-source").await;
    split_session(&handler, &source).await;
    let owner = create_session(&handler, "break-destination-owner").await;
    let peer = create_grouped_session(&handler, "break-destination-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    let moved_pane_id = {
        let state = handler.state.lock().await;
        pane_id(&state, &source, 0, 1)
    };

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source, 0, 1),
            target: Some(WindowTarget::with_window(peer.clone(), 1)),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected grouped-destination break-pane success, got {response:?}");
    };

    let state = handler.state.lock().await;
    for group_member in [&owner, &peer] {
        assert_eq!(
            pane_id(
                &state,
                group_member,
                response.target.window_index(),
                response.target.pane_index(),
            ),
            moved_pane_id
        );
        state
            .pane_profile_in_window(
                group_member,
                response.target.window_index(),
                response.target.pane_index(),
            )
            .expect("broken pane must be reachable through each grouped destination alias");
    }
}

#[tokio::test]
async fn grouped_peer_cross_session_swap_rollback_restores_model_and_runtimes() {
    let handler = RequestHandler::new();
    let (owner, peer) =
        create_group_with_two_panes(&handler, "swap-rollback-owner", "swap-rollback-peer").await;
    let target = create_session(&handler, "swap-rollback-target").await;
    handler.wait_for_initial_panes_for_test().await;
    let (owner_before, peer_before, target_before) = {
        let mut state = handler.state.lock().await;
        let snapshots = (
            state
                .sessions
                .session(&owner)
                .expect("owner exists")
                .clone(),
            state.sessions.session(&peer).expect("peer exists").clone(),
            state
                .sessions
                .session(&target)
                .expect("target exists")
                .clone(),
        );
        state.fail_next_resize_for_test();
        snapshots
    };

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: PaneTarget::with_window(peer.clone(), 0, 0),
            target: PaneTarget::with_window(target.clone(), 0, 0),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(
        matches!(&response, Response::Error(error) if error.error.to_string().contains("injected pane terminal resize failure")),
        "expected injected rollback path, got {response:?}"
    );

    let state = handler.state.lock().await;
    assert_eq!(state.sessions.session(&owner), Some(&owner_before));
    assert_eq!(state.sessions.session(&peer), Some(&peer_before));
    assert_eq!(state.sessions.session(&target), Some(&target_before));
    state
        .pane_profile_in_window(&peer, 0, 0)
        .expect("group runtime terminal must be restored");
    state
        .pane_profile_in_window(&target, 0, 0)
        .expect("standalone target terminal must be restored");
}
