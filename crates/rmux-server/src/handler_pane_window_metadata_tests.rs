use super::pane_group_transfer_tests::{create_grouped_session, create_session, split_session};
use super::{HandlerState, RequestHandler};
use rmux_proto::{
    BreakPaneRequest, JoinPaneRequest, LinkWindowRequest, MovePaneRequest, NewWindowRequest,
    OptionName, OptionScopeSelector, PaneTarget, Request, Response, ScopeSelector,
    SetOptionByNameRequest, SetOptionMode, SetOptionRequest, SplitDirection, WindowTarget,
};

const USER_OPTION: &str = "@pane-transfer-window";
const KNOWN_OPTION: &str = "monitor-silence";

async fn create_window(handler: &RequestHandler, session: &rmux_proto::SessionName, index: u32) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: Some(index),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;
}

async fn set_window_metadata(handler: &RequestHandler, target: &WindowTarget, marker: &str) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Window(target.clone()),
            name: USER_OPTION.to_owned(),
            value: Some(marker.to_owned()),
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
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(target.clone()),
            option: OptionName::MonitorSilence,
            value: "60".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

fn explicit_window_value(
    state: &HandlerState,
    target: &WindowTarget,
    name: &str,
) -> Option<String> {
    state
        .options
        .explicit_value_by_name(&OptionScopeSelector::Window(target.clone()), name)
        .expect("valid window option")
        .1
}

fn assert_window_metadata(
    state: &HandlerState,
    target: &WindowTarget,
    marker: Option<&str>,
    monitor_silence: Option<&str>,
) {
    assert_eq!(
        explicit_window_value(state, target, USER_OPTION).as_deref(),
        marker,
        "unexpected user option at {target}"
    );
    assert_eq!(
        explicit_window_value(state, target, KNOWN_OPTION).as_deref(),
        monitor_silence,
        "unexpected known option at {target}"
    );
}

async fn mark_auto_named(handler: &RequestHandler, target: &WindowTarget) {
    handler
        .state
        .lock()
        .await
        .mark_auto_named_window(target.session_name(), target.window_index());
}

async fn run_destroying_same_session_transfer(move_pane: bool) {
    let handler = RequestHandler::new();
    let session = create_session(
        &handler,
        if move_pane {
            "metadata-move"
        } else {
            "metadata-join"
        },
    )
    .await;
    create_window(&handler, &session, 1).await;
    let source_window = WindowTarget::with_window(session.clone(), 1);
    let target_window = WindowTarget::with_window(session.clone(), 0);
    set_window_metadata(&handler, &source_window, "discarded").await;
    mark_auto_named(&handler, &source_window).await;

    let source = PaneTarget::with_window(session.clone(), 1, 0);
    let target = PaneTarget::with_window(session.clone(), 0, 0);
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

    {
        let state = handler.state.lock().await;
        assert_window_metadata(&state, &source_window, None, None);
        assert_window_metadata(&state, &target_window, None, None);
        assert!(!state.tracks_auto_named_window(&session, 1));
    }
    create_window(&handler, &session, 1).await;
    let state = handler.state.lock().await;
    assert_window_metadata(&state, &source_window, None, None);
}

// Oracle probe 2026-07-12, pinned tmux 3.7b: a destroyed join/move source
// does not donate options to the target and a later window at that index is fresh.
#[tokio::test]
async fn join_and_move_drop_destroyed_source_window_metadata() {
    run_destroying_same_session_transfer(false).await;
    run_destroying_same_session_transfer(true).await;
}

#[tokio::test]
async fn cross_session_join_drops_destroyed_source_window_metadata() {
    let handler = RequestHandler::new();
    let source_session = create_session(&handler, "metadata-cross-join-source").await;
    create_window(&handler, &source_session, 1).await;
    let destination_session = create_session(&handler, "metadata-cross-join-destination").await;
    let source_window = WindowTarget::with_window(source_session.clone(), 1);
    let destination_window = WindowTarget::with_window(destination_session.clone(), 0);
    set_window_metadata(&handler, &source_window, "discarded").await;
    set_window_metadata(&handler, &destination_window, "destination").await;
    mark_auto_named(&handler, &source_window).await;

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(source_session.clone(), 1, 0),
            target: PaneTarget::with_window(destination_session, 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        assert_window_metadata(&state, &source_window, None, None);
        assert_window_metadata(&state, &destination_window, Some("destination"), Some("60"));
        assert!(!state.tracks_auto_named_window(&source_session, 1));
    }
    create_window(&handler, &source_session, 1).await;
    let state = handler.state.lock().await;
    assert_window_metadata(&state, &source_window, None, None);
}

#[tokio::test]
async fn grouped_join_clears_destroyed_source_metadata_from_every_peer() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "metadata-group-owner").await;
    create_window(&handler, &owner, 1).await;
    let peer = create_grouped_session(&handler, "metadata-group-peer", &owner).await;
    handler.wait_for_initial_panes_for_test().await;
    let source_window = WindowTarget::with_window(owner.clone(), 1);
    set_window_metadata(&handler, &source_window, "discarded").await;
    mark_auto_named(&handler, &source_window).await;

    let response = handler
        .handle(Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 0),
            target: PaneTarget::with_window(owner.clone(), 0, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::JoinPane(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        for session in [&owner, &peer] {
            let target = WindowTarget::with_window(session.clone(), 1);
            assert_window_metadata(&state, &target, None, None);
            assert!(!state.tracks_auto_named_window(session, 1));
        }
    }
    create_window(&handler, &owner, 1).await;
    let state = handler.state.lock().await;
    for session in [&owner, &peer] {
        assert_window_metadata(
            &state,
            &WindowTarget::with_window(session.clone(), 1),
            None,
            None,
        );
    }
}

// Oracle probe 2026-07-12, pinned tmux 3.7b: breaking the only pane moves
// the existing window, including user and known window options, across sessions.
#[tokio::test]
async fn cross_session_single_pane_break_moves_window_metadata() {
    let handler = RequestHandler::new();
    let source_session = create_session(&handler, "metadata-break-source").await;
    create_window(&handler, &source_session, 1).await;
    let destination_session = create_session(&handler, "metadata-break-destination").await;
    let source_window = WindowTarget::with_window(source_session.clone(), 1);
    let destination_window = WindowTarget::with_window(destination_session.clone(), 1);
    set_window_metadata(&handler, &source_window, "moved").await;
    mark_auto_named(&handler, &source_window).await;
    let source_window_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&source_session)
            .and_then(|session| session.window_at(1))
            .map(rmux_core::Window::id)
            .expect("source window exists")
    };
    assert!(handler
        .silence_timer_snapshot_for_test(&source_window)
        .is_some());

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source_session.clone(), 1, 0),
            target: Some(destination_window.clone()),
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        assert_window_metadata(&state, &destination_window, Some("moved"), Some("60"));
        assert_window_metadata(&state, &source_window, None, None);
        assert_eq!(
            state
                .sessions
                .session(destination_window.session_name())
                .and_then(|session| session.window_at(1))
                .map(rmux_core::Window::id),
            Some(source_window_id)
        );
        assert!(state.tracks_auto_named_window(destination_window.session_name(), 1));
        assert!(!state.tracks_auto_named_window(&source_session, 1));
    }
    assert_eq!(
        handler.silence_timer_snapshot_for_test(&source_window),
        None
    );
    assert!(handler
        .silence_timer_snapshot_for_test(&destination_window)
        .is_some());
    create_window(&handler, &source_session, 1).await;
    let state = handler.state.lock().await;
    assert_window_metadata(&state, &source_window, None, None);
}

#[tokio::test]
async fn cross_session_single_pane_break_explicit_name_clears_automatic_tracking() {
    let handler = RequestHandler::new();
    let source_session = create_session(&handler, "named-break-source").await;
    let destination_session = create_session(&handler, "named-break-destination").await;
    let source_window = WindowTarget::with_window(source_session.clone(), 0);
    let destination_window = WindowTarget::with_window(destination_session.clone(), 1);
    mark_auto_named(&handler, &source_window).await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source_session, 0, 0),
            target: Some(destination_window.clone()),
            name: Some("pinned".to_owned()),
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&destination_session)
        .and_then(|session| session.window_at(1))
        .expect("explicitly named destination window exists");
    assert_eq!(window.name(), Some("pinned"));
    assert!(!window.automatic_rename());
    assert!(!state.tracks_auto_named_window(&destination_session, 1));
}

#[tokio::test]
async fn same_session_single_pane_break_explicit_name_clears_automatic_tracking() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "named-same-break").await;
    create_window(&handler, &session, 1).await;
    let source_window = WindowTarget::with_window(session.clone(), 1);
    let destination_window = WindowTarget::with_window(session.clone(), 3);
    mark_auto_named(&handler, &source_window).await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(session.clone(), 1, 0),
            target: Some(destination_window),
            name: Some("pinned".to_owned()),
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&session)
        .and_then(|session| session.window_at(3))
        .expect("explicitly named destination window exists");
    assert_eq!(window.name(), Some("pinned"));
    assert!(!window.automatic_rename());
    assert!(!state.tracks_auto_named_window(&session, 3));
}

#[tokio::test]
async fn linked_last_pane_break_explicit_name_clears_family_automatic_tracking() {
    let handler = RequestHandler::new();
    let source_session = create_session(&handler, "named-linked-break-source").await;
    create_window(&handler, &source_session, 1).await;
    let linked_session = create_session(&handler, "named-linked-break-peer").await;
    let destination_session = create_session(&handler, "named-linked-break-destination").await;
    let source_window = WindowTarget::with_window(source_session.clone(), 1);
    let linked_window = WindowTarget::with_window(linked_session.clone(), 1);
    let destination_window = WindowTarget::with_window(destination_session.clone(), 1);
    mark_auto_named(&handler, &source_window).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: source_window.clone(),
            target: linked_window.clone(),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source_session, 1, 0),
            target: Some(destination_window.clone()),
            name: Some("pinned".to_owned()),
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    for target in [&destination_window, &linked_window] {
        let window = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .unwrap_or_else(|| panic!("explicitly named linked window {target} exists"));
        assert_eq!(window.name(), Some("pinned"), "unexpected name at {target}");
        assert!(
            !window.automatic_rename(),
            "auto rename enabled at {target}"
        );
        assert!(!state.tracks_auto_named_window(target.session_name(), target.window_index()));
    }
}

#[tokio::test]
async fn linked_single_pane_break_moves_metadata_to_the_new_alias() {
    let handler = RequestHandler::new();
    let source_session = create_session(&handler, "metadata-linked-source").await;
    create_window(&handler, &source_session, 1).await;
    let linked_session = create_session(&handler, "metadata-linked-peer").await;
    let destination_session = create_session(&handler, "metadata-linked-destination").await;
    let source_window = WindowTarget::with_window(source_session.clone(), 1);
    let linked_window = WindowTarget::with_window(linked_session.clone(), 1);
    let destination_window = WindowTarget::with_window(destination_session, 1);
    set_window_metadata(&handler, &source_window, "linked").await;
    mark_auto_named(&handler, &source_window).await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: source_window.clone(),
            target: linked_window.clone(),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source_session.clone(), 1, 0),
            target: Some(destination_window.clone()),
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
    assert_window_metadata(&state, &destination_window, Some("linked"), Some("60"));
    assert_window_metadata(&state, &linked_window, Some("linked"), Some("60"));
    assert_window_metadata(&state, &source_window, None, None);
    assert!(state.tracks_auto_named_window(destination_window.session_name(), 1));
    assert!(state.tracks_auto_named_window(&linked_session, 1));
    assert!(!state.tracks_auto_named_window(&source_session, 1));
}

#[tokio::test]
async fn same_session_single_pane_break_keeps_moving_window_metadata() {
    let handler = RequestHandler::new();
    let session = create_session(&handler, "metadata-same-break").await;
    create_window(&handler, &session, 1).await;
    let source_window = WindowTarget::with_window(session.clone(), 1);
    let destination_window = WindowTarget::with_window(session.clone(), 3);
    set_window_metadata(&handler, &source_window, "same-session").await;
    mark_auto_named(&handler, &source_window).await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(session.clone(), 1, 0),
            target: Some(destination_window.clone()),
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
    assert_window_metadata(
        &state,
        &destination_window,
        Some("same-session"),
        Some("60"),
    );
    assert_window_metadata(&state, &source_window, None, None);
    assert!(state.tracks_auto_named_window(&session, 3));
    assert!(!state.tracks_auto_named_window(&session, 1));
}

// Oracle probe 2026-07-12, pinned tmux 3.7b: breaking one pane from a
// multi-pane window creates a fresh window; the source keeps its options.
#[tokio::test]
async fn cross_session_multi_pane_break_does_not_copy_window_metadata() {
    let handler = RequestHandler::new();
    let source_session = create_session(&handler, "metadata-multi-source").await;
    split_session(&handler, &source_session).await;
    let destination_session = create_session(&handler, "metadata-multi-destination").await;
    let source_window = WindowTarget::with_window(source_session.clone(), 0);
    let destination_window = WindowTarget::with_window(destination_session, 1);
    set_window_metadata(&handler, &source_window, "source-only").await;
    mark_auto_named(&handler, &source_window).await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source_session.clone(), 0, 1),
            target: Some(destination_window.clone()),
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
    assert_window_metadata(&state, &source_window, Some("source-only"), Some("60"));
    assert_window_metadata(&state, &destination_window, None, None);
    assert!(state.tracks_auto_named_window(&source_session, 0));
}
