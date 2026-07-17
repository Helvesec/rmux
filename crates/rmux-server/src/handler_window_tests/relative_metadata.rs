use super::*;
use rmux_proto::{OptionScopeSelector, SetOptionByNameRequest};

const MARKER: &str = "@relative-marker";

async fn set_marker(handler: &RequestHandler, target: WindowTarget, value: &str) {
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

fn marker(
    state: &super::super::HandlerState,
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

fn assert_markers(
    state: &super::super::HandlerState,
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

#[tokio::test]
async fn move_window_before_preserves_duplicate_linked_winlink_metadata() {
    let handler = RequestHandler::new();
    let alpha = session_name("move-linked-metadata");
    create_session(&handler, alpha.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 1),
            target: WindowTarget::with_window(alpha.clone(), 2),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    insert_window(&handler, &alpha, 3).await;

    set_marker(
        &handler,
        WindowTarget::with_window(alpha.clone(), 0),
        "root",
    )
    .await;
    set_marker(
        &handler,
        WindowTarget::with_window(alpha.clone(), 1),
        "linked",
    )
    .await;
    set_marker(
        &handler,
        WindowTarget::with_window(alpha.clone(), 3),
        "mover",
    )
    .await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 3)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: true,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert_markers(
        &state,
        &alpha,
        &[Some("mover"), Some("root"), Some("linked"), Some("linked")],
    );
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.window_at(2).map(rmux_core::Window::id),
        session.window_at(3).map(rmux_core::Window::id)
    );
}

#[tokio::test]
async fn grouped_move_window_keeps_window_options_with_shared_windows() {
    let handler = RequestHandler::new();
    let owner = session_name("group-move-owner");
    let peer = session_name("group-move-peer");
    create_session(&handler, owner.as_str()).await;
    insert_window(&handler, &owner, 1).await;
    insert_window(&handler, &owner, 2).await;
    for (window_index, value) in [(0, "root"), (1, "anchor"), (2, "mover")] {
        set_marker(
            &handler,
            WindowTarget::with_window(owner.clone(), window_index),
            value,
        )
        .await;
    }

    create_grouped_session(&handler, peer.as_str(), &owner).await;
    {
        let state = handler.state.lock().await;
        assert_markers(
            &state,
            &peer,
            &[Some("root"), Some("anchor"), Some("mover")],
        );
    }
    set_marker(
        &handler,
        WindowTarget::with_window(peer.clone(), 1),
        "anchor-updated",
    )
    .await;
    {
        let state = handler.state.lock().await;
        assert_eq!(marker(&state, &owner, 1), Some("anchor-updated".to_owned()));
    }

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(owner.clone(), 2)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(owner.clone(), 0)),
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
            &[Some("mover"), Some("root"), Some("anchor-updated")],
        );
    }
}

#[tokio::test]
async fn new_window_before_rekeys_existing_window_metadata() {
    let handler = RequestHandler::new();
    let alpha = session_name("new-before-metadata");
    create_session(&handler, alpha.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;
    for (window_index, value) in [(0, "root"), (1, "one"), (2, "two")] {
        set_marker(
            &handler,
            WindowTarget::with_window(alpha.clone(), window_index),
            value,
        )
        .await;
    }

    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
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
    assert_markers(
        &state,
        &alpha,
        &[None, Some("root"), Some("one"), Some("two")],
    );
}

#[tokio::test]
async fn link_window_before_rekeys_existing_window_metadata() {
    let handler = RequestHandler::new();
    let alpha = session_name("link-before-target");
    let beta = session_name("link-before-source");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    insert_window(&handler, &alpha, 1).await;
    insert_window(&handler, &alpha, 2).await;
    for (window_index, value) in [(0, "root"), (1, "one"), (2, "two")] {
        set_marker(
            &handler,
            WindowTarget::with_window(alpha.clone(), window_index),
            value,
        )
        .await;
    }
    set_marker(
        &handler,
        WindowTarget::with_window(beta.clone(), 0),
        "linked",
    )
    .await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(beta, 0),
            target: WindowTarget::with_window(alpha.clone(), 0),
            after: false,
            before: true,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert_markers(
        &state,
        &alpha,
        &[Some("linked"), Some("root"), Some("one"), Some("two")],
    );
}
