use super::attach_support::ActiveAttachIdentity;
use super::{with_expected_attach_and_session_identity, RequestHandler};
use rmux_proto::{
    KillPaneRequest, KillSessionRequest, LinkWindowRequest, MoveWindowRequest, MoveWindowTarget,
    NewSessionRequest, NewWindowRequest, PaneTarget, Request, RespawnPaneRequest,
    RespawnWindowRequest, Response, SessionId, SessionName, SwapWindowRequest, TerminalSize,
    UnlinkWindowRequest, WindowTarget,
};

async fn create_session(handler: &RequestHandler, value: &str) -> SessionName {
    let session_name = SessionName::new(value).expect("valid session name");
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

async fn create_window(handler: &RequestHandler, session_name: &SessionName, index: u32) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
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
}

async fn session_id(handler: &RequestHandler, session_name: &SessionName) -> SessionId {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session_name)
        .expect("session exists")
        .id()
}

async fn replace_session(handler: &RequestHandler, session_name: &SessionName) -> SessionId {
    let stale_id = session_id(handler, session_name).await;
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    let recreated = create_session(handler, session_name.as_str()).await;
    assert_eq!(&recreated, session_name);
    assert_ne!(session_id(handler, session_name).await, stale_id);
    stale_id
}

async fn run_as_stale_attached_session(
    handler: &RequestHandler,
    session_name: SessionName,
    stale_id: SessionId,
    request: Request,
) -> Response {
    with_expected_attach_and_session_identity(
        ActiveAttachIdentity::new(990_001, 1, stale_id),
        session_name,
        stale_id,
        handler.handle(request),
    )
    .await
}

#[tokio::test]
async fn stale_attached_session_cannot_respawn_replacement_window() {
    let handler = RequestHandler::new();
    let session_name = create_session(&handler, "identity-respawn-window").await;
    let stale_id = replace_session(&handler, &session_name).await;

    let response = run_as_stale_attached_session(
        &handler,
        session_name.clone(),
        stale_id,
        Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(session_name.clone(), 0),
            kill: true,
            environment: None,
            command: None,
            start_directory: None,
        })),
    )
    .await;

    assert!(matches!(response, Response::Error(_)), "{response:?}");
    assert_ne!(session_id(&handler, &session_name).await, stale_id);
}

#[tokio::test]
async fn stale_attached_session_cannot_respawn_or_kill_replacement_pane() {
    let handler = RequestHandler::new();
    let session_name = create_session(&handler, "identity-pane-mutations").await;
    let stale_id = replace_session(&handler, &session_name).await;
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);

    let respawn = run_as_stale_attached_session(
        &handler,
        session_name.clone(),
        stale_id,
        Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })),
    )
    .await;
    assert!(matches!(respawn, Response::Error(_)), "{respawn:?}");

    let killed = run_as_stale_attached_session(
        &handler,
        session_name.clone(),
        stale_id,
        Request::KillPane(KillPaneRequest {
            target,
            kill_all_except: false,
        }),
    )
    .await;
    assert!(matches!(killed, Response::Error(_)), "{killed:?}");
    assert_ne!(session_id(&handler, &session_name).await, stale_id);
}

#[tokio::test]
async fn stale_attached_session_cannot_unlink_recreated_window_slot() {
    let handler = RequestHandler::new();
    let session_name = create_session(&handler, "identity-unlink-window").await;
    create_window(&handler, &session_name, 1).await;
    let stale_id = replace_session(&handler, &session_name).await;
    create_window(&handler, &session_name, 1).await;

    let response = run_as_stale_attached_session(
        &handler,
        session_name.clone(),
        stale_id,
        Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(session_name.clone(), 1),
            kill_if_last: true,
        }),
    )
    .await;

    assert!(matches!(response, Response::Error(_)), "{response:?}");
    let state = handler.state.lock().await;
    assert!(
        state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(1))
            .is_some(),
        "replacement window must survive the stale unlink"
    );
}

#[tokio::test]
async fn stale_attached_session_cannot_drive_cross_session_window_mutations() {
    let handler = RequestHandler::new();
    let attached = create_session(&handler, "identity-window-source").await;
    let other = create_session(&handler, "identity-window-other").await;
    let stale_id = replace_session(&handler, &attached).await;

    let move_response = run_as_stale_attached_session(
        &handler,
        attached.clone(),
        stale_id,
        Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(attached.clone(), 0)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(other.clone(), 1)),
            renumber: false,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        }),
    )
    .await;
    assert!(
        matches!(move_response, Response::Error(_)),
        "{move_response:?}"
    );

    let swap_response = run_as_stale_attached_session(
        &handler,
        attached.clone(),
        stale_id,
        Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(attached.clone(), 0),
            target: WindowTarget::with_window(other.clone(), 0),
            detached: true,
        }),
    )
    .await;
    assert!(
        matches!(swap_response, Response::Error(_)),
        "{swap_response:?}"
    );

    let link_response = run_as_stale_attached_session(
        &handler,
        attached.clone(),
        stale_id,
        Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(other, 0),
            target: WindowTarget::with_window(attached.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }),
    )
    .await;
    assert!(
        matches!(link_response, Response::Error(_)),
        "{link_response:?}"
    );
    assert_ne!(session_id(&handler, &attached).await, stale_id);
}

#[tokio::test]
async fn attached_queue_can_still_mutate_explicit_other_sessions() {
    let handler = RequestHandler::new();
    let attached = create_session(&handler, "identity-explicit-attached").await;
    let source = create_session(&handler, "identity-explicit-source").await;
    let destination = create_session(&handler, "identity-explicit-destination").await;
    let attached_id = session_id(&handler, &attached).await;

    let response = with_expected_attach_and_session_identity(
        ActiveAttachIdentity::new(990_002, 1, attached_id),
        attached,
        attached_id,
        handler.handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source, 0),
            target: WindowTarget::with_window(destination.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        })),
    )
    .await;

    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&destination)
        .and_then(|session| session.window_at(1))
        .is_some());
}
