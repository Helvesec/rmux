use super::RequestHandler;
use rmux_core::{AlertFlags, WINLINK_ACTIVITY, WINLINK_BELL};
use rmux_proto::{
    BreakPaneRequest, LinkWindowRequest, MoveWindowRequest, MoveWindowTarget, NewSessionExtRequest,
    NewWindowRequest, PaneTarget, Request, Response, SessionName, SplitDirection,
    SplitWindowExtRequest, SplitWindowTarget, TerminalSize, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(
    handler: &RequestHandler,
    name: &str,
    group_target: Option<SessionName>,
) -> SessionName {
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target,
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
    if handler
        .state
        .lock()
        .await
        .sessions
        .session(&session)
        .is_some_and(|created| created.group_name().is_none())
    {
        handler
            .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session.clone(), 0))
            .await;
    }
    session
}

async fn create_duplicate_group(
    handler: &RequestHandler,
    label: &str,
) -> (SessionName, SessionName) {
    let owner = create_session(handler, &format!("{label}-owner"), None).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(owner.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let peer = create_session(handler, &format!("{label}-peer"), Some(owner.clone())).await;
    (owner, peer)
}

async fn seed_peer_duplicate_flags(handler: &RequestHandler, peer: &SessionName) {
    let mut state = handler.state.lock().await;
    let peer_session = state
        .sessions
        .session_mut(peer)
        .expect("group peer exists before insertion");
    assert!(peer_session.add_winlink_alert_flags(0, WINLINK_BELL));
    assert!(peer_session.add_winlink_alert_flags(1, WINLINK_ACTIVITY));
}

async fn assert_peer_duplicate_flags_shifted(handler: &RequestHandler, peer: &SessionName) {
    let state = handler.state.lock().await;
    let peer_session = state
        .sessions
        .session(peer)
        .expect("group peer survives insertion");
    assert_eq!(peer_session.winlink_alert_flags(0), AlertFlags::empty());
    assert_eq!(peer_session.winlink_alert_flags(1), WINLINK_BELL);
    assert_eq!(peer_session.winlink_alert_flags(2), WINLINK_ACTIVITY);
}

#[tokio::test]
async fn grouped_new_window_insertion_preserves_peer_duplicate_alias_winlink_flags() {
    let handler = RequestHandler::new();
    let (owner, peer) = create_duplicate_group(&handler, "new-window-insert-alerts").await;
    seed_peer_duplicate_flags(&handler, &peer).await;

    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: owner,
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(0),
            insert_at_target: true,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");

    assert_peer_duplicate_flags_shifted(&handler, &peer).await;
}

#[tokio::test]
async fn grouped_link_window_insertion_preserves_peer_duplicate_alias_winlink_flags() {
    let handler = RequestHandler::new();
    let (owner, peer) = create_duplicate_group(&handler, "link-window-insert-alerts").await;
    let source = create_session(&handler, "link-window-insert-source", None).await;
    seed_peer_duplicate_flags(&handler, &peer).await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source, 0),
            target: WindowTarget::with_window(owner, 0),
            after: false,
            before: true,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    assert_peer_duplicate_flags_shifted(&handler, &peer).await;
}

#[tokio::test]
async fn grouped_break_pane_insertion_preserves_peer_duplicate_alias_winlink_flags() {
    let handler = RequestHandler::new();
    let (owner, peer) = create_duplicate_group(&handler, "group-break-insert-alerts").await;
    let split = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(owner.clone(), 0, 0)),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: true,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    seed_peer_duplicate_flags(&handler, &peer).await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(owner.clone(), 1, 1),
            target: Some(WindowTarget::with_window(owner, 0)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    assert_peer_duplicate_flags_shifted(&handler, &peer).await;
}

async fn assert_cross_session_break_preserves_flags(label: &str, linked_source: bool) {
    let handler = RequestHandler::new();
    let (owner, peer) = create_duplicate_group(&handler, label).await;
    let source = create_session(&handler, &format!("{label}-source"), None).await;
    if linked_source {
        let linked = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(source.clone(), 0),
                target: WindowTarget::with_window(source.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await;
        assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    }
    seed_peer_duplicate_flags(&handler, &peer).await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(source, 0, 0),
            target: Some(WindowTarget::with_window(owner, 0)),
            name: None,
            detached: true,
            after: false,
            before: true,
            print_target: false,
            format: None,
        })))
        .await;
    assert!(matches!(response, Response::BreakPane(_)), "{response:?}");

    assert_peer_duplicate_flags_shifted(&handler, &peer).await;
}

#[tokio::test]
async fn cross_session_break_pane_insertion_preserves_peer_duplicate_alias_winlink_flags() {
    assert_cross_session_break_preserves_flags("cross-break-insert-alerts", false).await;
}

#[tokio::test]
async fn linked_last_break_pane_insertion_preserves_peer_duplicate_alias_winlink_flags() {
    assert_cross_session_break_preserves_flags("linked-break-insert-alerts", true).await;
}

async fn assert_relative_move_preserves_flags(label: &str, linked_source: bool) {
    let handler = RequestHandler::new();
    let (owner, peer) = create_duplicate_group(&handler, label).await;
    let source = create_session(&handler, &format!("{label}-source"), None).await;
    if linked_source {
        let linked = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(source.clone(), 0),
                target: WindowTarget::with_window(source.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await;
        assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    }
    seed_peer_duplicate_flags(&handler, &peer).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(source, 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(owner, 0)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: true,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_peer_duplicate_flags_shifted(&handler, &peer).await;
}

#[tokio::test]
async fn cross_session_relative_move_preserves_peer_duplicate_alias_winlink_flags() {
    assert_relative_move_preserves_flags("cross-move-insert-alerts", false).await;
}

#[tokio::test]
async fn linked_relative_move_preserves_peer_duplicate_alias_winlink_flags() {
    assert_relative_move_preserves_flags("linked-move-insert-alerts", true).await;
}
