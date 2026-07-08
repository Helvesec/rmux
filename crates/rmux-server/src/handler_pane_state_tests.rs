use std::{
    sync::{Arc, Barrier},
    time::Duration,
};

use super::RequestHandler;
use crate::pane_state_journal::{PaneStateChange, PANE_STATE_JOURNAL_CAPACITY};
use rmux_core::{events::SubscriptionLimits, PaneId};
use rmux_proto::{
    ErrorResponse, KillSessionRequest, KillWindowRequest, LinkWindowRequest, MoveWindowRequest,
    MoveWindowTarget, NewSessionRequest, NewWindowRequest, OptionScopeSelector, PaneKillRequest,
    PaneOptionSetRequest, PaneStateClosedReason, PaneStateCursorRequest, PaneStateEventDto,
    PaneTarget, PaneTargetRef, Request, RespawnPaneRequest, RespawnWindowRequest, Response,
    RmuxError, SelectPaneRequest, SessionName, SetOptionByNameRequest, SetOptionMode,
    SourceFileRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget,
    SubscribePaneStateRequest, TerminalSize, UnlinkWindowRequest, WindowTarget,
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

async fn create_window_with_pane(
    handler: &RequestHandler,
    session: &SessionName,
    window_index: u32,
) -> (PaneTarget, PaneId) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
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

    let target = PaneTarget::with_window(session.clone(), window_index, 0);
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(session)
            .and_then(|session| session.window_at(window_index))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("created window pane exists")
    };
    (target, pane_id)
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
    read_cursor_with_max(handler, connection_id, subscription_id, after_revision, 16).await
}

async fn read_cursor_with_max(
    handler: &RequestHandler,
    connection_id: u64,
    subscription_id: rmux_proto::PaneStateSubscriptionId,
    after_revision: u64,
    max_events: u16,
) -> Response {
    handler
        .handle_pane_state_cursor(
            connection_id,
            PaneStateCursorRequest {
                subscription_id,
                after_revision,
                wait: false,
                max_events: Some(max_events),
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

fn expect_single_pane_state_event(response: Response) -> PaneStateEventDto {
    match response {
        Response::PaneStateCursor(mut response) => {
            assert_eq!(
                response.events.len(),
                1,
                "expected exactly one pane-state event, got {:?}",
                response.events
            );
            response.events.remove(0)
        }
        response => panic!("pane-state cursor failed: {response:?}"),
    }
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
    for (index, reason) in [
        ("exited", PaneStateClosedReason::Exited),
        ("died-kept", PaneStateClosedReason::DiedKept),
        ("killed", PaneStateClosedReason::Killed),
    ] {
        let (_session, target, pane_id) =
            create_session_with_pane(&handler, &format!("pane-state-closed-{index}")).await;
        let subscription_id = subscribe(&handler, 93, target, false, false).await;
        handler.record_pane_state_change(pane_id, Some(1), PaneStateChange::Closed { reason });

        match read_cursor(&handler, 93, subscription_id, 0).await {
            Response::PaneStateCursor(response) => {
                assert_eq!(response.events.len(), 1);
                assert!(matches!(
                    response.events.as_slice(),
                    [PaneStateEventDto::Closed {
                        reason: event_reason,
                        ..
                    }] if *event_reason == reason
                ));
            }
            response => panic!("pane-state cursor failed: {response:?}"),
        }

        match read_cursor(&handler, 93, subscription_id, 1).await {
            Response::Error(error) => assert!(matches!(
                error.error,
                RmuxError::Server(message) if message == "subscription not found"
            )),
            response => panic!("closed subscription should be forgotten, got {response:?}"),
        }
    }
}

#[tokio::test]
async fn duplicate_closed_for_same_pane_is_suppressed_until_reopened() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-duplicate-closed").await;
    let subscription_id = subscribe(&handler, 99, target.clone(), false, false).await;

    handler.record_pane_state_change(
        pane_id,
        Some(1),
        PaneStateChange::Closed {
            reason: PaneStateClosedReason::DiedKept,
        },
    );
    handler.record_pane_state_change(
        pane_id,
        None,
        PaneStateChange::Closed {
            reason: PaneStateClosedReason::Killed,
        },
    );

    match read_cursor(&handler, 99, subscription_id, 0).await {
        Response::PaneStateCursor(response) => {
            assert_eq!(response.events.len(), 1);
            assert!(matches!(
                response.events.as_slice(),
                [PaneStateEventDto::Closed {
                    reason: PaneStateClosedReason::DiedKept,
                    ..
                }]
            ));
        }
        response => panic!("pane-state cursor failed: {response:?}"),
    }

    handler.reopen_pane_state(pane_id);
    let after_reopen_revision = handler
        .pane_state_journal
        .lock()
        .expect("pane-state journal lock should not be poisoned")
        .current_revision();
    let reopened_subscription_id = subscribe(&handler, 100, target, false, false).await;
    handler.record_pane_state_change(
        pane_id,
        Some(2),
        PaneStateChange::Closed {
            reason: PaneStateClosedReason::Killed,
        },
    );

    match read_cursor(
        &handler,
        100,
        reopened_subscription_id,
        after_reopen_revision,
    )
    .await
    {
        Response::PaneStateCursor(response) => {
            assert_eq!(response.events.len(), 1);
            assert!(matches!(
                response.events.as_slice(),
                [PaneStateEventDto::Closed {
                    pane_id: event_pane_id,
                    reason: PaneStateClosedReason::Killed,
                    ..
                }] if *event_pane_id == pane_id
            ));
        }
        response => panic!("pane-state cursor failed: {response:?}"),
    }
}

#[tokio::test]
async fn pane_state_late_subscription_after_died_kept_receives_killed_close() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-late-died-kept-killed").await;

    handler.record_pane_state_change(
        pane_id,
        Some(1),
        PaneStateChange::Closed {
            reason: PaneStateClosedReason::DiedKept,
        },
    );
    let first_closed_revision = handler
        .pane_state_journal
        .lock()
        .expect("pane-state journal lock should not be poisoned")
        .current_revision();
    let subscription_id = subscribe(&handler, 106, target, false, false).await;
    handler.record_pane_state_change(
        pane_id,
        None,
        PaneStateChange::Closed {
            reason: PaneStateClosedReason::Killed,
        },
    );

    match read_cursor(&handler, 106, subscription_id, first_closed_revision).await {
        Response::PaneStateCursor(response) => {
            assert_eq!(response.events.len(), 1);
            assert!(matches!(
                response.events.as_slice(),
                [PaneStateEventDto::Closed {
                    pane_id: event_pane_id,
                    reason: PaneStateClosedReason::Killed,
                    ..
                }] if *event_pane_id == pane_id
            ));
        }
        response => panic!("late subscription should receive final killed close, got {response:?}"),
    }
}

#[tokio::test]
async fn respawn_pane_reopens_pane_state_before_future_close() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-respawn-pane-reopen").await;

    handler.record_pane_state_change(
        pane_id,
        Some(1),
        PaneStateChange::Closed {
            reason: PaneStateClosedReason::DiedKept,
        },
    );

    let response = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::RespawnPane(_)), "{response:?}");

    let after_respawn_revision = handler
        .pane_state_journal
        .lock()
        .expect("pane-state journal lock should not be poisoned")
        .current_revision();
    let subscription_id = subscribe(&handler, 104, target, false, false).await;
    handler.record_pane_state_change(
        pane_id,
        Some(2),
        PaneStateChange::Closed {
            reason: PaneStateClosedReason::Killed,
        },
    );

    match read_cursor(&handler, 104, subscription_id, after_respawn_revision).await {
        Response::PaneStateCursor(response) => {
            assert_eq!(response.events.len(), 1);
            assert!(matches!(
                response.events.as_slice(),
                [PaneStateEventDto::Closed {
                    pane_id: event_pane_id,
                    reason: PaneStateClosedReason::Killed,
                    ..
                }] if *event_pane_id == pane_id
            ));
        }
        response => panic!("pane-state cursor failed: {response:?}"),
    }
}

#[tokio::test]
async fn respawn_window_reopens_retained_pane_state_before_future_close() {
    let handler = RequestHandler::new();
    let (session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-respawn-window-reopen").await;

    handler.record_pane_state_change(
        pane_id,
        Some(1),
        PaneStateChange::Closed {
            reason: PaneStateClosedReason::DiedKept,
        },
    );

    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(session, 0),
            kill: true,
            environment: None,
            command: None,
            start_directory: None,
        })))
        .await;
    assert!(
        matches!(response, Response::RespawnWindow(_)),
        "{response:?}"
    );

    let after_respawn_revision = handler
        .pane_state_journal
        .lock()
        .expect("pane-state journal lock should not be poisoned")
        .current_revision();
    let subscription_id = subscribe(&handler, 105, target, false, false).await;
    handler.record_pane_state_change(
        pane_id,
        Some(2),
        PaneStateChange::Closed {
            reason: PaneStateClosedReason::Killed,
        },
    );

    match read_cursor(&handler, 105, subscription_id, after_respawn_revision).await {
        Response::PaneStateCursor(response) => {
            assert_eq!(response.events.len(), 1);
            assert!(matches!(
                response.events.as_slice(),
                [PaneStateEventDto::Closed {
                    pane_id: event_pane_id,
                    reason: PaneStateClosedReason::Killed,
                    ..
                }] if *event_pane_id == pane_id
            ));
        }
        response => panic!("pane-state cursor failed: {response:?}"),
    }
}

#[tokio::test]
async fn pane_state_wait_cursor_advances_past_filtered_events_on_timeout() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-filtered-wait").await;
    let subscription_id = subscribe(&handler, 94, target, false, false).await;
    let after_revision = handler
        .pane_state_journal
        .lock()
        .expect("pane-state journal lock should not be poisoned")
        .current_revision();

    handler.record_pane_state_change(
        pane_id,
        Some(1),
        PaneStateChange::OptionSet {
            name: "@agent.kind".to_owned(),
            old: None,
            new: "assistant".to_owned(),
        },
    );
    let expected_revision = handler
        .pane_state_journal
        .lock()
        .expect("pane-state journal lock should not be poisoned")
        .current_revision();
    assert_eq!(expected_revision, after_revision.saturating_add(1));

    match read_cursor_waiting(&handler, 94, subscription_id, after_revision).await {
        Response::PaneStateCursor(response) => {
            assert!(
                response.events.is_empty(),
                "filtered wait must stay empty, got {:?}",
                response.events
            );
            assert_eq!(
                response.next_revision, expected_revision,
                "filtered wait must advance exactly past the filtered event"
            );
        }
        response => panic!("pane-state wait cursor failed: {response:?}"),
    }
}

#[tokio::test]
async fn set_option_by_name_pane_origin_emits_pane_state_option_event() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-set-option-origin").await;
    let subscription_id = subscribe(&handler, 980, target.clone(), false, true).await;

    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Pane(target),
            name: "@d2.set-option".to_owned(),
            value: Some("direct".to_owned()),
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

    let event =
        expect_single_pane_state_event(read_cursor(&handler, 980, subscription_id, 0).await);
    assert!(matches!(
        event,
        PaneStateEventDto::OptionSet {
            pane_id: event_pane_id,
            name,
            old_value: None,
            new_value,
            ..
        } if event_pane_id == pane_id && name == "@d2.set-option" && new_value == "direct"
    ));
}

#[tokio::test]
async fn pane_option_set_sdk_origin_emits_pane_state_option_event() {
    let handler = RequestHandler::new();
    let (session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-sdk-option-origin").await;
    let subscription_id = subscribe(&handler, 981, target.clone(), false, true).await;

    let response = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::by_id(session, pane_id),
            name: "@d2.sdk".to_owned(),
            value: Some("sdk".to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(
        matches!(response, Response::PaneOptionSet(_)),
        "{response:?}"
    );

    let event =
        expect_single_pane_state_event(read_cursor(&handler, 981, subscription_id, 0).await);
    assert!(matches!(
        event,
        PaneStateEventDto::OptionSet {
            pane_id: event_pane_id,
            name,
            old_value: None,
            new_value,
            ..
        } if event_pane_id == pane_id && name == "@d2.sdk" && new_value == "sdk"
    ));
}

#[tokio::test]
async fn pane_option_set_sdk_origin_records_option_event_when_resize_fails() {
    let handler = RequestHandler::new();
    let (session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-sdk-option-resize-error").await;
    let subscription_id = subscribe(&handler, 984, target.clone(), false, true).await;
    {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
    }

    let response = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::by_id(session, pane_id),
            name: "pane-border-status".to_owned(),
            value: Some("top".to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(
        matches!(response, Response::Error(ErrorResponse { error: RmuxError::Server(ref message) }) if message == "injected pane terminal resize failure"),
        "{response:?}"
    );

    let event =
        expect_single_pane_state_event(read_cursor(&handler, 984, subscription_id, 0).await);
    assert!(matches!(
        event,
        PaneStateEventDto::OptionSet {
            pane_id: event_pane_id,
            name,
            old_value: None,
            new_value,
            ..
        } if event_pane_id == pane_id && name == "pane-border-status" && new_value == "top"
    ));
}

#[tokio::test]
async fn select_pane_title_origin_emits_pane_state_title_event() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-select-title-origin").await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    let subscription_id = subscribe(&handler, 982, target.clone(), true, false).await;

    let response = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target,
            title: Some("selected-title".to_owned()),
            input_disabled: None,
            preserve_zoom: false,
            style: None,
        })))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");

    let event =
        expect_single_pane_state_event(read_cursor(&handler, 982, subscription_id, 0).await);
    assert!(matches!(
        event,
        PaneStateEventDto::TitleChanged {
            pane_id: event_pane_id,
            new_title,
            ..
        } if event_pane_id == pane_id && new_title == "selected-title"
    ));
}

#[tokio::test]
async fn select_pane_style_origin_emits_pane_state_option_event() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-select-style-origin").await;
    let subscription_id = subscribe(&handler, 983, target.clone(), false, true).await;

    let response = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target,
            title: None,
            input_disabled: None,
            preserve_zoom: false,
            style: Some("fg=red".to_owned()),
        })))
        .await;
    assert!(matches!(response, Response::SelectPane(_)), "{response:?}");

    let event =
        expect_single_pane_state_event(read_cursor(&handler, 983, subscription_id, 0).await);
    assert!(matches!(
        event,
        PaneStateEventDto::OptionSet {
            pane_id: event_pane_id,
            name,
            old_value: None,
            new_value,
            ..
        } if event_pane_id == pane_id && name == "window-style" && new_value == "fg=red"
    ));
}

#[tokio::test]
async fn source_file_pane_option_origin_emits_pane_state_option_event() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-source-file-origin").await;
    let subscription_id = subscribe(&handler, 984, target.clone(), false, true).await;
    let source = format!("set-option -p -t {target} @d2.source stdin\n");

    let response = handler
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: Some(target),
            caller_cwd: None,
            stdin: Some(source),
        })))
        .await;
    assert!(matches!(response, Response::SourceFile(_)), "{response:?}");

    let event =
        expect_single_pane_state_event(read_cursor(&handler, 984, subscription_id, 0).await);
    assert!(matches!(
        event,
        PaneStateEventDto::OptionSet {
            pane_id: event_pane_id,
            name,
            old_value: None,
            new_value,
            ..
        } if event_pane_id == pane_id && name == "@d2.source" && new_value == "stdin"
    ));
}

#[tokio::test]
async fn pane_state_revisions_are_strictly_increasing_under_concurrent_recording() {
    let handler = RequestHandler::new();
    let (_session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-concurrent-revisions").await;
    let subscription_id = subscribe(&handler, 985, target, false, true).await;
    let barrier = Arc::new(Barrier::new(101));
    let mut tasks = Vec::new();

    for index in 0_u64..100 {
        let task_handler = handler.clone();
        let task_barrier = barrier.clone();
        tasks.push(std::thread::spawn(move || {
            task_barrier.wait();
            task_handler.record_pane_state_change(
                pane_id,
                Some(1),
                PaneStateChange::OptionSet {
                    name: format!("@d2.concurrent.{index}"),
                    old: None,
                    new: index.to_string(),
                },
            );
        }));
    }

    barrier.wait();
    for task in tasks {
        task.join()
            .expect("concurrent pane-state task should finish");
    }

    let response = read_cursor_with_max(&handler, 985, subscription_id, 0, 128).await;
    let events = match response {
        Response::PaneStateCursor(response) => response.events,
        response => panic!("pane-state cursor failed: {response:?}"),
    };
    assert_eq!(events.len(), 100, "expected all concurrent events");

    let revisions = events
        .iter()
        .map(|event| match event {
            PaneStateEventDto::OptionSet { revision, .. } => *revision,
            event => panic!("expected option event, got {event:?}"),
        })
        .collect::<Vec<_>>();
    assert!(
        revisions.windows(2).all(|pair| pair[0] < pair[1]),
        "revisions must be delivered in strict order: {revisions:?}"
    );
    let mut sorted = revisions;
    sorted.sort_unstable();
    assert_eq!(sorted, (1_u64..=100).collect::<Vec<_>>());
}

#[tokio::test]
async fn pane_state_subscriptions_obey_connection_limits() {
    let handler = RequestHandler::with_owner_uid_and_subscription_limits(
        0,
        SubscriptionLimits::new(1, 16, 16, Duration::from_secs(60)),
    );
    let (_session, target, _pane_id) =
        create_session_with_pane(&handler, "pane-state-subscription-limit").await;
    let _subscription_id = subscribe(&handler, 98, target.clone(), false, false).await;

    match handler
        .handle_subscribe_pane_state(
            98,
            SubscribePaneStateRequest {
                target: PaneTargetRef::slot(target),
                include_title: true,
                include_options: true,
                include_foreground: false,
            },
        )
        .await
    {
        Response::Error(error) => assert!(
            error
                .error
                .to_string()
                .contains("pane state subscription limit exceeded for connection"),
            "{error:?}"
        ),
        response => panic!("second pane-state subscription should hit the limit: {response:?}"),
    }
}

#[tokio::test]
async fn kill_session_emits_closed_for_pane_state_subscribers() {
    let handler = RequestHandler::new();
    let (session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-kill-session").await;
    let subscription_id = subscribe(&handler, 95, target, false, false).await;

    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");

    assert_closed_event(&handler, 95, subscription_id, pane_id).await;
}

#[tokio::test]
async fn kill_window_emits_closed_for_pane_state_subscribers() {
    let handler = RequestHandler::new();
    let (session, _initial_target, _initial_pane_id) =
        create_session_with_pane(&handler, "pane-state-kill-window").await;
    let (target, pane_id) = create_window_with_pane(&handler, &session, 1).await;
    let subscription_id = subscribe(&handler, 96, target, false, false).await;

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(session, 1),
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");

    assert_closed_event(&handler, 96, subscription_id, pane_id).await;
}

#[tokio::test]
async fn respawn_window_kill_emits_closed_for_destroyed_pane_state_subscribers() {
    let handler = RequestHandler::new();
    let (session, _initial_target, _initial_pane_id) =
        create_session_with_pane(&handler, "pane-state-respawn-window-destroyed").await;

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    let split_target = match split {
        Response::SplitWindow(response) => response.pane,
        response => panic!("expected split-window success, got {response:?}"),
    };
    let split_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(split_target.pane_index()))
            .map(|pane| pane.id())
            .expect("split pane exists")
    };
    let subscription_id = subscribe(&handler, 106, split_target, false, false).await;

    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(session, 0),
            kill: true,
            environment: None,
            command: None,
            start_directory: None,
        })))
        .await;
    assert!(
        matches!(response, Response::RespawnWindow(_)),
        "{response:?}"
    );

    assert_closed_event(&handler, 106, subscription_id, split_pane_id).await;
}

#[tokio::test]
async fn link_window_replacement_emits_closed_for_replaced_pane_state_subscribers() {
    let handler = RequestHandler::new();
    let (alpha, _alpha_target, _alpha_pane_id) =
        create_session_with_pane(&handler, "pane-state-link-source").await;
    let (beta, beta_target, beta_pane_id) =
        create_session_with_pane(&handler, "pane-state-link-dest").await;
    let subscription_id = subscribe(&handler, 101, beta_target, false, false).await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha, 0),
            target: WindowTarget::with_window(beta, 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    assert_closed_event(&handler, 101, subscription_id, beta_pane_id).await;
}

#[tokio::test]
async fn move_window_replacement_emits_closed_for_replaced_pane_state_subscribers() {
    let handler = RequestHandler::new();
    let (alpha, _alpha_target, _alpha_pane_id) =
        create_session_with_pane(&handler, "pane-state-move-source").await;
    let (_source_target, _source_pane_id) = create_window_with_pane(&handler, &alpha, 1).await;
    let (beta, beta_target, beta_pane_id) =
        create_session_with_pane(&handler, "pane-state-move-dest").await;
    let subscription_id = subscribe(&handler, 102, beta_target, false, false).await;

    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha, 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(beta, 0)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    assert_closed_event(&handler, 102, subscription_id, beta_pane_id).await;
}

#[tokio::test]
async fn move_window_replacement_keeps_linked_destination_pane_state_open() {
    let handler = RequestHandler::new();
    let (alpha, _alpha_target, _alpha_pane_id) =
        create_session_with_pane(&handler, "pane-state-move-linked-source").await;
    let (_source_target, _source_pane_id) = create_window_with_pane(&handler, &alpha, 1).await;
    let (beta, beta_target, beta_pane_id) =
        create_session_with_pane(&handler, "pane-state-move-linked-dest").await;
    let (gamma, _gamma_target, _gamma_pane_id) =
        create_session_with_pane(&handler, "pane-state-move-linked-peer").await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(beta.clone(), 0),
            target: WindowTarget::with_window(gamma, 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let subscription_id = subscribe(&handler, 107, beta_target, false, false).await;
    let response = handler
        .handle(Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha, 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(beta, 0)),
            renumber: false,
            kill_destination: true,
            detached: true,
            after: false,
            before: false,
        }))
        .await;
    assert!(matches!(response, Response::MoveWindow(_)), "{response:?}");

    match read_cursor(&handler, 107, subscription_id, 0).await {
        Response::PaneStateCursor(response) => {
            assert!(
                response.events.is_empty(),
                "linked destination pane {beta_pane_id:?} must not be closed by move-window -k: {:?}",
                response.events
            );
        }
        response => panic!("pane-state cursor failed: {response:?}"),
    }
}

#[tokio::test]
async fn unlink_window_kill_if_last_emits_closed_for_removed_pane_state_subscribers() {
    let handler = RequestHandler::new();
    let (session, _initial_target, _initial_pane_id) =
        create_session_with_pane(&handler, "pane-state-unlink-last").await;
    let (target, pane_id) = create_window_with_pane(&handler, &session, 1).await;
    let subscription_id = subscribe(&handler, 103, target, false, false).await;

    let response = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(session, 1),
            kill_if_last: true,
        }))
        .await;
    assert!(
        matches!(response, Response::UnlinkWindow(_)),
        "{response:?}"
    );

    assert_closed_event(&handler, 103, subscription_id, pane_id).await;
}

#[tokio::test]
async fn pane_kill_ref_emits_closed_for_pane_state_subscribers() {
    let handler = RequestHandler::new();
    let (session, target, pane_id) =
        create_session_with_pane(&handler, "pane-state-pane-kill").await;
    let subscription_id = subscribe(&handler, 97, target, false, false).await;

    let response = handler
        .handle(Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(session, pane_id),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    assert_closed_event(&handler, 97, subscription_id, pane_id).await;
}

async fn assert_closed_event(
    handler: &RequestHandler,
    connection_id: u64,
    subscription_id: rmux_proto::PaneStateSubscriptionId,
    pane_id: PaneId,
) {
    match read_cursor(handler, connection_id, subscription_id, 0).await {
        Response::PaneStateCursor(response) => {
            assert_eq!(response.events.len(), 1);
            assert!(matches!(
                &response.events[0],
                PaneStateEventDto::Closed {
                    pane_id: event_pane_id,
                    reason: PaneStateClosedReason::Killed,
                    ..
                } if *event_pane_id == pane_id
            ));
        }
        response => panic!("pane-state cursor failed: {response:?}"),
    }
}
