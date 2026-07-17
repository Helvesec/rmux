use super::RequestHandler;
use rmux_core::PaneId;
use rmux_proto::{
    ErrorResponse, KillPaneRequest, LinkWindowRequest, NewSessionRequest, NewWindowRequest,
    PaneTarget, Request, Response, RmuxError, SessionName, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, TerminalSize, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, value: &str) -> SessionName {
    let session = session_name(value);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    session
}

async fn split(handler: &RequestHandler, session: &SessionName) {
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
}

async fn create_window(handler: &RequestHandler, session: &SessionName, index: u32) {
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
}

async fn link_window(handler: &RequestHandler, owner: &SessionName, alias: &SessionName) {
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
}

fn pane_ids(state: &crate::pane_terminals::HandlerState, session: &SessionName) -> Vec<PaneId> {
    state
        .sessions
        .session(session)
        .expect("session exists")
        .window_at(0)
        .expect("window exists")
        .panes()
        .iter()
        .map(|pane| pane.id())
        .collect()
}

#[tokio::test]
async fn linked_pane_kill_from_alias_updates_owner_and_alias() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "linked-kill-owner").await;
    split(&handler, &owner).await;
    let alias = create_session(&handler, "linked-kill-alias").await;
    link_window(&handler, &owner, &alias).await;
    handler.wait_for_initial_panes_for_test().await;

    let removed_pane_id = {
        let state = handler.state.lock().await;
        pane_ids(&state, &owner)[1]
    };
    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(alias.clone(), 0, 1),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    let owner_ids = pane_ids(&state, &owner);
    assert_eq!(owner_ids, pane_ids(&state, &alias));
    assert_eq!(owner_ids.len(), 1);
    assert_ne!(owner_ids[0], removed_pane_id);
    state
        .ensure_window_panes_exist(&alias, 0, &owner_ids)
        .expect("linked alias resolves the surviving owner runtime");
}

#[tokio::test]
async fn linked_pane_kill_all_except_from_owner_updates_every_alias() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "linked-kill-all-owner").await;
    split(&handler, &owner).await;
    split(&handler, &owner).await;
    let alias = create_session(&handler, "linked-kill-all-alias").await;
    link_window(&handler, &owner, &alias).await;
    handler.wait_for_initial_panes_for_test().await;

    let kept_pane_id = {
        let state = handler.state.lock().await;
        pane_ids(&state, &owner)[1]
    };
    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(owner.clone(), 0, 1),
            kill_all_except: true,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert_eq!(pane_ids(&state, &owner), vec![kept_pane_id]);
    assert_eq!(pane_ids(&state, &alias), vec![kept_pane_id]);
    state
        .ensure_window_panes_exist(&alias, 0, &[kept_pane_id])
        .expect("the kept pane runtime remains reachable through the alias");
}

#[tokio::test]
async fn linked_alias_kill_resize_rollback_restores_shared_runtime() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "linked-rollback-owner").await;
    split(&handler, &owner).await;
    let alias = create_session(&handler, "linked-rollback-alias").await;
    link_window(&handler, &owner, &alias).await;
    handler.wait_for_initial_panes_for_test().await;

    let (pane_id, pane_pid) = {
        let mut state = handler.state.lock().await;
        let pane_id = pane_ids(&state, &owner)[1];
        let pane_pid = state
            .pane_pid_in_window(&owner, 0, 1)
            .expect("second pane has a runtime process");
        assert!(state
            .toggle_marked_pane(&PaneTarget::with_window(owner.clone(), 0, 1))
            .expect("pane can be marked"));
        state.fail_next_resize_for_test();
        (pane_id, pane_pid)
    };

    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(alias.clone(), 0, 1),
            kill_all_except: false,
        }))
        .await;
    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::Server("injected pane terminal resize failure".to_owned()),
        })
    );

    let state = handler.state.lock().await;
    for session in [&owner, &alias] {
        assert!(pane_ids(&state, session).contains(&pane_id));
    }
    state
        .ensure_window_panes_exist(&alias, 0, &[pane_id])
        .expect("rollback restores the owner runtime for the alias");
    state
        .pane_output_for_target(&alias, 0, 1)
        .expect("rollback restores pane output for the alias");
    assert!(state.pane_is_marked(&PaneTarget::with_window(owner.clone(), 0, 1)));
    assert_eq!(
        state
            .pane_pid_in_window(&alias, 0, 1)
            .expect("restored pane remains inspectable"),
        pane_pid
    );
}

#[tokio::test]
async fn linked_last_pane_kill_removes_shared_window_from_all_surviving_sessions() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "linked-last-owner").await;
    create_window(&handler, &owner, 1).await;
    let alias = create_session(&handler, "linked-last-alias").await;
    create_window(&handler, &alias, 1).await;
    link_window(&handler, &owner, &alias).await;
    handler.wait_for_initial_panes_for_test().await;

    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(alias.clone(), 0, 0),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    for session_name in [&owner, &alias] {
        let session = state
            .sessions
            .session(session_name)
            .expect("session survives");
        assert!(session.window_at(0).is_none());
        assert!(session.window_at(1).is_some());
    }
}

#[tokio::test]
async fn linked_last_pane_kill_destroys_only_alias_with_no_surviving_window() {
    let handler = RequestHandler::new();
    let owner = create_session(&handler, "linked-last-owner-survivor").await;
    create_window(&handler, &owner, 1).await;
    let alias = create_session(&handler, "linked-last-only-alias").await;
    link_window(&handler, &owner, &alias).await;
    handler.wait_for_initial_panes_for_test().await;

    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(alias.clone(), 0, 0),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alias).is_none());
    let owner_session = state.sessions.session(&owner).expect("owner survives");
    assert!(owner_session.window_at(0).is_none());
    assert!(owner_session.window_at(1).is_some());
}
