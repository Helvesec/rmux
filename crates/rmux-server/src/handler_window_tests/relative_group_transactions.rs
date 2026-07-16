use super::*;
use rmux_proto::{OptionScopeSelector, SetOptionByNameRequest};

const MARKER: &str = "@relative-group-marker";
const PANE_MARKER: &str = "@relative-group-pane-marker";

pub(super) async fn set_marker(handler: &RequestHandler, target: WindowTarget, value: &str) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Window(target),
            name: MARKER.to_owned(),
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

pub(super) fn marker(
    state: &crate::pane_terminals::HandlerState,
    session_name: &SessionName,
    window_index: u32,
) -> Option<String> {
    state
        .options
        .explicit_value_by_name(
            &OptionScopeSelector::Window(WindowTarget::with_window(
                session_name.clone(),
                window_index,
            )),
            MARKER,
        )
        .expect("valid window user option")
        .1
}

async fn set_pane_marker(handler: &RequestHandler, target: PaneTarget, value: &str) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Pane(target),
            name: PANE_MARKER.to_owned(),
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

fn pane_marker(state: &crate::pane_terminals::HandlerState, target: PaneTarget) -> Option<String> {
    state
        .options
        .explicit_value_by_name(&OptionScopeSelector::Pane(target), PANE_MARKER)
        .expect("valid pane user option")
        .1
}

fn assert_markers(
    state: &crate::pane_terminals::HandlerState,
    session_name: &SessionName,
    expected: &[Option<&str>],
) {
    let actual = (0..expected.len() as u32)
        .map(|window_index| marker(state, session_name, window_index))
        .collect::<Vec<_>>();
    let expected = expected
        .iter()
        .map(|value| value.map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

pub(super) fn window_ids(
    state: &crate::pane_terminals::HandlerState,
    session_name: &SessionName,
) -> Vec<(u32, u32)> {
    state
        .sessions
        .session(session_name)
        .expect("session exists")
        .windows()
        .iter()
        .map(|(index, window)| (*index, window.id().as_u32()))
        .collect()
}

pub(super) async fn create_three_window_group(
    handler: &RequestHandler,
    prefix: &str,
) -> (SessionName, SessionName) {
    let owner = session_name(&format!("{prefix}-owner"));
    let peer = session_name(&format!("{prefix}-peer"));
    create_session(handler, owner.as_str()).await;
    insert_window(handler, &owner, 1).await;
    insert_window(handler, &owner, 2).await;
    for (window_index, value) in [(0, "root"), (1, "linked"), (2, "auto")] {
        set_marker(
            handler,
            WindowTarget::with_window(owner.clone(), window_index),
            value,
        )
        .await;
    }
    create_grouped_session(handler, peer.as_str(), &owner).await;
    (owner, peer)
}

async fn link_owner_window_to_external(
    handler: &RequestHandler,
    owner: &SessionName,
    prefix: &str,
) -> SessionName {
    let external = session_name(&format!("{prefix}-external"));
    create_session(handler, external.as_str()).await;
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 1),
            target: WindowTarget::with_window(external.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    external
}

async fn mark_auto_named(handler: &RequestHandler, session_name: &SessionName, window_index: u32) {
    handler
        .state
        .lock()
        .await
        .mark_auto_named_window(session_name, window_index);
}

#[tokio::test]
async fn grouped_relative_move_via_peer_rekeys_canonical_window_metadata() {
    let handler = RequestHandler::new();
    let (owner, peer) = create_three_window_group(&handler, "peer-move").await;
    let external = link_owner_window_to_external(&handler, &owner, "peer-move").await;
    mark_auto_named(&handler, &owner, 2).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(peer.clone(), 2)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(peer.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: true,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    for session_name in [&owner, &peer] {
        assert_markers(
            &state,
            session_name,
            &[Some("auto"), Some("root"), Some("linked")],
        );
        assert_eq!(
            state.window_link_count(session_name, 2),
            2,
            "linked window count for {session_name}:2"
        );
    }
    assert_eq!(state.window_link_count(&external, 1), 2);
    assert!(state.tracks_auto_named_window(&owner, 0));
    assert!(state.tracks_auto_named_window(&peer, 0));
    assert!(!state.tracks_auto_named_window(&owner, 3));
}

#[tokio::test]
async fn grouped_new_window_via_peer_rekeys_canonical_window_metadata() {
    let handler = RequestHandler::new();
    let (owner, peer) = create_three_window_group(&handler, "peer-new").await;
    let external = link_owner_window_to_external(&handler, &owner, "peer-new").await;
    mark_auto_named(&handler, &owner, 2).await;

    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: peer.clone(),
            name: Some("inserted".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_window_test_command()),
            process_command: None,
            target_window_index: Some(0),
            insert_at_target: true,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    for session_name in [&owner, &peer] {
        assert_markers(
            &state,
            session_name,
            &[None, Some("root"), Some("linked"), Some("auto")],
        );
        assert_eq!(state.window_link_count(session_name, 2), 2);
    }
    assert_eq!(state.window_link_count(&external, 1), 2);
    assert!(state.tracks_auto_named_window(&owner, 3));
    assert!(state.tracks_auto_named_window(&peer, 3));
}

#[tokio::test]
async fn grouped_relative_link_via_peer_rekeys_canonical_window_metadata() {
    let handler = RequestHandler::new();
    let (owner, peer) = create_three_window_group(&handler, "peer-link").await;
    let external = link_owner_window_to_external(&handler, &owner, "peer-link").await;
    let source = session_name("peer-link-source");
    create_session(&handler, source.as_str()).await;
    set_marker(
        &handler,
        WindowTarget::with_window(source.clone(), 0),
        "incoming",
    )
    .await;
    mark_auto_named(&handler, &owner, 2).await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source.clone(), 0),
            target: WindowTarget::with_window(peer.clone(), 0),
            after: false,
            before: true,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    for session_name in [&owner, &peer] {
        assert_markers(
            &state,
            session_name,
            &[Some("incoming"), Some("root"), Some("linked"), Some("auto")],
        );
        assert_eq!(state.window_link_count(session_name, 0), 2);
        assert_eq!(state.window_link_count(session_name, 2), 2);
    }
    assert_eq!(state.window_link_count(&source, 0), 2);
    assert_eq!(state.window_link_count(&external, 1), 2);
    assert!(state.tracks_auto_named_window(&owner, 3));
}

#[tokio::test]
async fn grouped_relative_link_after_sparse_last_window_via_peer_succeeds() {
    let handler = RequestHandler::new();
    let owner = session_name("sparse-group-link-owner");
    let peer = session_name("sparse-group-link-peer");
    let source = session_name("sparse-group-link-source");
    create_session(&handler, owner.as_str()).await;
    insert_window(&handler, &owner, 10).await;
    create_grouped_session(&handler, peer.as_str(), &owner).await;
    create_session(&handler, source.as_str()).await;
    set_marker(
        &handler,
        WindowTarget::with_window(source.clone(), 0),
        "incoming",
    )
    .await;
    set_pane_marker(
        &handler,
        PaneTarget::with_window(source.clone(), 0, 0),
        "incoming-pane",
    )
    .await;
    mark_auto_named(&handler, &source, 0).await;
    {
        let state = handler.state.lock().await;
        assert_eq!(
            pane_marker(&state, PaneTarget::with_window(source.clone(), 0, 0),),
            Some("incoming-pane".to_owned())
        );
    }

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source.clone(), 0),
            target: WindowTarget::with_window(peer.clone(), 10),
            after: true,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    let source_window_id = state
        .sessions
        .session(&source)
        .and_then(|session| session.window_at(0))
        .map(rmux_core::Window::id)
        .expect("source window exists");
    for session_name in [&owner, &peer] {
        let session = state
            .sessions
            .session(session_name)
            .expect("group member exists");
        assert_eq!(
            session.windows().keys().copied().collect::<Vec<_>>(),
            vec![0, 10, 11]
        );
        assert_eq!(
            session.window_at(11).map(rmux_core::Window::id),
            Some(source_window_id)
        );
        assert_eq!(
            marker(&state, session_name, 11),
            Some("incoming".to_owned())
        );
        assert_eq!(
            pane_marker(&state, PaneTarget::with_window(session_name.clone(), 11, 0),),
            Some("incoming-pane".to_owned())
        );
        assert_eq!(state.window_link_count(session_name, 11), 2);
        assert!(state.tracks_auto_named_window(session_name, 11));
    }
    assert_eq!(marker(&state, &source, 0), Some("incoming".to_owned()));
    assert_eq!(
        pane_marker(&state, PaneTarget::with_window(source.clone(), 0, 0),),
        Some("incoming-pane".to_owned())
    );
    assert_eq!(state.window_link_count(&source, 0), 2);
    assert!(state.tracks_auto_named_window(&source, 0));
}

#[tokio::test]
async fn link_window_post_attach_failure_restores_group_metadata_and_links() {
    let handler = RequestHandler::new();
    let (owner, peer) = create_three_window_group(&handler, "link-rollback").await;
    let external = link_owner_window_to_external(&handler, &owner, "link-rollback").await;
    let source = session_name("link-rollback-source");
    create_session(&handler, source.as_str()).await;
    mark_auto_named(&handler, &owner, 2).await;

    let (owner_before, peer_before) = {
        let mut state = handler.state.lock().await;
        let owner_before = window_ids(&state, &owner);
        let peer_before = window_ids(&state, &peer);
        state.fail_next_link_window_after_attach_for_test();
        (owner_before, peer_before)
    };

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source.clone(), 0),
            target: WindowTarget::with_window(peer.clone(), 0),
            after: false,
            before: true,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(
        matches!(&response, Response::Error(error)
            if error.error.to_string().contains("post-attach failure")),
        "{response:?}"
    );

    let state = handler.state.lock().await;
    assert_eq!(window_ids(&state, &owner), owner_before);
    assert_eq!(window_ids(&state, &peer), peer_before);
    for session_name in [&owner, &peer] {
        assert_markers(
            &state,
            session_name,
            &[Some("root"), Some("linked"), Some("auto")],
        );
        assert_eq!(state.window_link_count(session_name, 1), 2);
        assert_eq!(state.window_link_count(session_name, 0), 1);
    }
    assert_eq!(state.window_link_count(&external, 1), 2);
    assert_eq!(state.window_link_count(&source, 0), 1);
    assert!(state.tracks_auto_named_window(&owner, 2));
}

#[tokio::test]
async fn relative_link_preserves_sparse_source_slot_outside_shift_range() {
    let handler = RequestHandler::new();
    let alpha = session_name("sparse-relative-link");
    create_session(&handler, alpha.as_str()).await;
    insert_window(&handler, &alpha, 10).await;
    set_marker(
        &handler,
        WindowTarget::with_window(alpha.clone(), 0),
        "root",
    )
    .await;
    set_marker(
        &handler,
        WindowTarget::with_window(alpha.clone(), 10),
        "source",
    )
    .await;
    mark_auto_named(&handler, &alpha, 10).await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 10),
            target: WindowTarget::with_window(alpha.clone(), 0),
            after: false,
            before: true,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert_eq!(marker(&state, &alpha, 0), Some("source".to_owned()));
    assert_eq!(marker(&state, &alpha, 1), Some("root".to_owned()));
    assert_eq!(marker(&state, &alpha, 10), Some("source".to_owned()));
    assert_eq!(marker(&state, &alpha, 11), None);
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.window_at(0).map(rmux_core::Window::id),
        session.window_at(10).map(rmux_core::Window::id)
    );
    assert_eq!(state.window_link_count(&alpha, 0), 2);
    assert_eq!(state.window_link_count(&alpha, 10), 2);
    assert!(state.tracks_auto_named_window(&alpha, 0));
    assert!(state.tracks_auto_named_window(&alpha, 10));
}

#[tokio::test]
async fn relative_move_between_group_aliases_is_atomic_product_divergence() {
    // tmux 3.7b shifts only the target table before returning this error.
    // RMUX deliberately keeps rejected group mutations transactional.
    let handler = RequestHandler::new();
    let (owner, peer) = create_three_window_group(&handler, "tmux-move-group").await;
    let external = link_owner_window_to_external(&handler, &owner, "tmux-move-group").await;
    mark_auto_named(&handler, &owner, 2).await;
    let (owner_before, peer_before, options_before, hooks_before) = {
        let state = handler.state.lock().await;
        (
            window_ids(&state, &owner),
            window_ids(&state, &peer),
            state.options.clone(),
            state.hooks.clone(),
        )
    };

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(peer.clone(), 2)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(owner.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: true,
        }))
        .await;
    assert!(
        matches!(&response, Response::Error(error)
            if error.error.to_string().contains("sessions are grouped")),
        "{response:?}"
    );

    let state = handler.state.lock().await;
    assert_eq!(window_ids(&state, &owner), owner_before);
    assert_eq!(window_ids(&state, &peer), peer_before);
    assert_eq!(state.options, options_before);
    assert_eq!(state.hooks, hooks_before);
    for session_name in [&owner, &peer] {
        assert_markers(
            &state,
            session_name,
            &[Some("root"), Some("linked"), Some("auto")],
        );
        assert_eq!(state.window_link_count(session_name, 1), 2);
        assert!(state.tracks_auto_named_window(session_name, 2));
    }
    assert_eq!(state.window_link_count(&external, 1), 2);
}

#[tokio::test]
async fn relative_link_between_group_aliases_is_atomic_product_divergence() {
    // tmux 3.7b shifts only the target table before returning this error.
    // RMUX deliberately keeps rejected group mutations transactional.
    let handler = RequestHandler::new();
    let (owner, peer) = create_three_window_group(&handler, "tmux-link-group").await;
    let external = link_owner_window_to_external(&handler, &owner, "tmux-link-group").await;
    mark_auto_named(&handler, &owner, 2).await;
    let (owner_before, peer_before, options_before, hooks_before) = {
        let state = handler.state.lock().await;
        (
            window_ids(&state, &owner),
            window_ids(&state, &peer),
            state.options.clone(),
            state.hooks.clone(),
        )
    };

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(peer.clone(), 2),
            target: WindowTarget::with_window(owner.clone(), 0),
            after: false,
            before: true,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(
        matches!(&response, Response::Error(error)
            if error.error.to_string().contains("sessions are grouped")),
        "{response:?}"
    );

    let state = handler.state.lock().await;
    assert_eq!(window_ids(&state, &owner), owner_before);
    assert_eq!(window_ids(&state, &peer), peer_before);
    assert_eq!(state.options, options_before);
    assert_eq!(state.hooks, hooks_before);
    for session_name in [&owner, &peer] {
        assert_markers(
            &state,
            session_name,
            &[Some("root"), Some("linked"), Some("auto")],
        );
        assert_eq!(state.window_link_count(session_name, 1), 2);
        assert!(state.tracks_auto_named_window(session_name, 2));
    }
    assert_eq!(state.window_link_count(&external, 1), 2);
}
