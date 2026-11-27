use super::*;
use rmux_proto::{encode_internal_pane_exit_probe, ListPanesRequest, SessionId};

async fn exit_probe(
    handler: &RequestHandler,
    target: &SessionName,
    session_id: SessionId,
    pane_id: PaneId,
) -> String {
    let response = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.clone(),
            target_window_index: None,
            format: Some(encode_internal_pane_exit_probe(session_id, pane_id)),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    match response {
        Response::ListPanes(response) => {
            String::from_utf8_lossy(response.output.stdout()).into_owned()
        }
        other => panic!("exit probe must answer through list-panes: {other:?}"),
    }
}

async fn first_pane(handler: &RequestHandler, session: &SessionName) -> (SessionId, PaneId) {
    let state = handler.state.lock().await;
    let session = state.sessions.session(session).expect("session exists");
    let pane_id = session
        .window_at(0)
        .and_then(|window| window.pane(0))
        .map(|pane| pane.id())
        .expect("initial pane exists");
    (session.id(), pane_id)
}

/// A natural exit removes the pane under the state lock. The exit probe takes
/// that lock too, so between the removal and the rest of the publication it
/// must already find the retained exit; an empty answer is read as "stale" by
/// `--wait quiet`/`pane-exit` and fails a real exit.
async fn natural_exit_is_probeable_between_removal_and_publication(removes_session: bool) {
    let handler = Arc::new(RequestHandler::new());
    let session = create_session(&handler, "exit-probe-retention").await;
    // The probe request needs a live session to address; it is not the one
    // being looked up when the exiting session itself is removed.
    let keeper = create_session(&handler, "exit-probe-keeper").await;
    if !removes_session {
        create_window(&handler, &session, 1).await;
    }
    handler.wait_for_initial_panes_for_test().await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let (session_id, pane_id) = first_pane(&handler, &session).await;
    assert_eq!(
        exit_probe(&handler, &keeper, session_id, pane_id).await,
        format!("{pane_id}\t0\t\t\t0\n"),
        "the live pane answers the probe before its exit"
    );

    handler
        .state
        .lock()
        .await
        .mark_pane_dead_without_exit_details(&target)
        .expect("mark pane naturally exited");
    let pause = handler.install_pane_exit_commit_pause();
    let exit_handler = Arc::clone(&handler);
    let exit_session = session.clone();
    let exiting = tokio::spawn(async move {
        exit_handler
            .handle_pane_exit_event(PaneExitEvent::eof_published(exit_session, pane_id, None))
            .await;
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("natural exit reaches the post-commit pause");

    let (pane_removed, session_present) = {
        let state = handler.state.lock().await;
        let session_state = state.sessions.session(&session);
        (
            session_state.is_none_or(|session| session.window_index_for_pane_id(pane_id).is_none()),
            session_state.is_some(),
        )
    };
    assert!(pane_removed, "the pause must sit after the model removal");
    assert_eq!(session_present, !removes_session);
    let during = exit_probe(&handler, &keeper, session_id, pane_id).await;

    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), exiting)
        .await
        .expect("natural exit publication finishes")
        .expect("natural exit task joins");
    let after = exit_probe(&handler, &keeper, session_id, pane_id).await;

    assert_eq!(
        during,
        format!("{pane_id}\t1\t\t\t1\n"),
        "a removed pane must already be retained as exited (removes_session={removes_session})"
    );
    assert_eq!(
        during, after,
        "publication must not change the retained exit"
    );
}

#[tokio::test]
async fn natural_pane_exit_is_retained_in_the_removal_critical_section() {
    natural_exit_is_probeable_between_removal_and_publication(false).await;
}

#[tokio::test]
async fn natural_session_exit_is_retained_in_the_removal_critical_section() {
    natural_exit_is_probeable_between_removal_and_publication(true).await;
}

/// kill-pane is not an exit: it leaves no trace, so the probe stays empty and
/// the client keeps classifying the target as stale.
#[tokio::test]
async fn killed_pane_leaves_no_exit_trace() {
    let handler = Arc::new(RequestHandler::new());
    let session = create_session(&handler, "exit-probe-kill").await;
    create_window(&handler, &session, 1).await;
    handler.wait_for_initial_panes_for_test().await;
    let (session_id, pane_id) = first_pane(&handler, &session).await;

    let killed = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(session.clone(), pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)), "{killed:?}");

    assert_eq!(
        exit_probe(&handler, &session, session_id, pane_id).await,
        ""
    );
}
