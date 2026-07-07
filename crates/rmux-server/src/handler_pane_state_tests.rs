use super::RequestHandler;
use crate::pane_state_journal::{PaneStateChange, PANE_STATE_JOURNAL_CAPACITY};
use rmux_core::PaneId;
use rmux_proto::{
    NewSessionRequest, PaneStateClosedReason, PaneStateCursorRequest, PaneStateEventDto,
    PaneTarget, PaneTargetRef, Request, Response, RmuxError, SessionName,
    SubscribePaneStateRequest, TerminalSize,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session_with_pane(
    handler: &RequestHandler,
    name: &str,
) -> (SessionName, PaneTarget, PaneId) {
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");

    let target = PaneTarget::new(session.clone(), 0);
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("initial pane exists")
    };
    (session, target, pane_id)
}

async fn subscribe(
    handler: &RequestHandler,
    connection_id: u64,
    target: PaneTarget,
    include_title: bool,
    include_options: bool,
) -> rmux_proto::PaneStateSubscriptionId {
    match handler
        .handle_subscribe_pane_state(
            connection_id,
            SubscribePaneStateRequest {
                target: PaneTargetRef::slot(target),
                include_title,
                include_options,
                include_foreground: false,
            },
        )
        .await
    {
        Response::SubscribePaneState(response) => response.subscription_id,
        response => panic!("subscribe-pane-state failed: {response:?}"),
    }
}

async fn read_cursor(
    handler: &RequestHandler,
    connection_id: u64,
    subscription_id: rmux_proto::PaneStateSubscriptionId,
    after_revision: u64,
) -> Response {
    handler
        .handle_pane_state_cursor(
            connection_id,
            PaneStateCursorRequest {
                subscription_id,
                after_revision,
                wait: false,
                max_events: Some(16),
            },
        )
        .await
}

async fn read_cursor_waiting(
    handler: &RequestHandler,
    connection_id: u64,
    subscription_id: rmux_proto::PaneStateSubscriptionId,
    after_revision: u64,
) -> Response {
    handler
        .handle_pane_state_cursor(
            connection_id,
            PaneStateCursorRequest {
                subscription_id,
                after_revision,
                wait: true,
                max_events: Some(16),
            },
        )
        .await
}

#[tokio::test]
async fn pane_state_cursor_delivers_matching_pane_events_with_global_revisions() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-revisions").await;
    let subscription_id = subscribe(&handler, 91, target, true, true).await;

    handler.record_pane_state_change(
        pane_id,
        Some(1),
        PaneStateChange::TitleChanged {
            old: "old".to_owned(),
            new: "new".to_owned(),
        },
    );
    handler.record_pane_state_change(
        PaneId::new(999),
        Some(1),
        PaneStateChange::TitleChanged {
            old: "other-old".to_owned(),
            new: "other-new".to_owned(),
        },
    );
    handler.record_pane_state_change(
        pane_id,
        Some(1),
        PaneStateChange::OptionSet {
            name: "@agent.kind".to_owned(),
            old: None,
            new: "assistant".to_owned(),
        },
    );

    match read_cursor(&handler, 91, subscription_id, 0).await {
        Response::PaneStateCursor(response) => {
            assert_eq!(response.next_revision, 3);
            assert_eq!(response.events.len(), 2);
            assert!(matches!(
                &response.events[0],
                PaneStateEventDto::TitleChanged {
                    revision: 1,
                    pane_id: event_pane_id,
                    new_title,
                    ..
                } if *event_pane_id == pane_id && new_title == "new"
            ));
            assert!(matches!(
                &response.events[1],
                PaneStateEventDto::OptionSet {
                    revision: 3,
                    pane_id: event_pane_id,
                    name,
                    new_value,
                    ..
                } if *event_pane_id == pane_id
                    && name == "@agent.kind"
                    && new_value == "assistant"
            ));
        }
        response => panic!("pane-state cursor failed: {response:?}"),
    }
}

#[tokio::test]
async fn pane_state_cursor_lag_returns_rebased_snapshot() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) = create_session_with_pane(&handler, "pane-state-lag").await;
    let subscription_id = subscribe(&handler, 92, target, true, false).await;

    for index in 0..=PANE_STATE_JOURNAL_CAPACITY {
        handler.record_pane_state_change(
            pane_id,
            Some(1),
            PaneStateChange::TitleChanged {
                old: index.to_string(),
                new: (index + 1).to_string(),
            },
        );
    }

    match read_cursor(&handler, 92, subscription_id, 0).await {
        Response::PaneStateLag(response) => {
            assert_eq!(response.subscription_id, subscription_id);
            assert_eq!(response.missed_from_revision, 0);
            assert!(response.resume_revision > 0);
            assert_eq!(
                response.snapshot.revision,
                (PANE_STATE_JOURNAL_CAPACITY + 1) as u64
            );
            assert!(response.snapshot.title.is_some());
        }
        response => panic!("expected pane-state lag response, got {response:?}"),
    }
}

#[tokio::test]
async fn pane_state_closed_reason_variants_are_delivered_before_end_of_stream() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) = create_session_with_pane(&handler, "pane-state-closed").await;
    let subscription_id = subscribe(&handler, 93, target, false, false).await;

    for reason in [
        PaneStateClosedReason::Exited,
        PaneStateClosedReason::DiedKept,
        PaneStateClosedReason::Killed,
    ] {
        handler.record_pane_state_change(pane_id, Some(1), PaneStateChange::Closed { reason });
    }

    match read_cursor(&handler, 93, subscription_id, 0).await {
        Response::PaneStateCursor(response) => {
            let reasons = response
                .events
                .iter()
                .map(|event| match event {
                    PaneStateEventDto::Closed { reason, .. } => *reason,
                    event => panic!("expected closed event, got {event:?}"),
                })
                .collect::<Vec<_>>();
            assert_eq!(
                reasons,
                vec![
                    PaneStateClosedReason::Exited,
                    PaneStateClosedReason::DiedKept,
                    PaneStateClosedReason::Killed,
                ]
            );
        }
        response => panic!("pane-state cursor failed: {response:?}"),
    }

    match read_cursor(&handler, 93, subscription_id, 3).await {
        Response::Error(error) => assert!(
            matches!(error.error, RmuxError::Server(message) if message == "subscription not found")
        ),
        response => panic!("closed subscription should be forgotten, got {response:?}"),
    }
}

#[tokio::test]
async fn pane_state_wait_cursor_advances_past_filtered_events_on_timeout() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-filtered-wait").await;
    let subscription_id = subscribe(&handler, 94, target, true, false).await;

    handler.record_pane_state_change(
        pane_id,
        Some(1),
        PaneStateChange::OptionSet {
            name: "@agent.kind".to_owned(),
            old: None,
            new: "assistant".to_owned(),
        },
    );

    match read_cursor_waiting(&handler, 94, subscription_id, 0).await {
        Response::PaneStateCursor(response) => {
            assert!(response.events.is_empty());
            assert_eq!(response.next_revision, 1);
        }
        response => panic!("pane-state wait cursor failed: {response:?}"),
    }
}
