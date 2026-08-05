use std::sync::Arc;
use std::time::{Duration, Instant};

use rmux_core::events::SubscriptionLimits;
use rmux_proto::{
    NewSessionExtRequest, PaneRawRebase, PaneRawRebaseReason, PaneRecoveryCoverage,
    PaneSnapshotRequest, PaneStreamCursorRequest, PaneStreamEndReason, PaneStreamEvent,
    PaneStreamLifecycleEvent, PaneStreamMode, PaneSurfaceFrame, PaneTarget, PaneTargetRef,
    RenameSessionRequest, Request, Response, SessionName, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, SubscribePaneStreamRequest, SubscribePaneStreamResponse, TerminalSize,
    UnsubscribePaneStreamRequest, DEFAULT_MAX_DETACHED_FRAME_LENGTH,
};

use crate::pane_io::{PaneExitEvent, PaneInvalidationReason, PaneOutputSender};
use crate::pane_transcript::SharedPaneTranscript;

use super::{validate_raw_rebase_size, RequestHandler};

#[path = "handler_pane_stream_tests/subscription_kind.rs"]
mod subscription_kind;
#[path = "handler_pane_stream_tests/surface_budget.rs"]
mod surface_budget;
#[path = "handler_pane_stream_tests/surface_review_regressions.rs"]
mod surface_review_regressions;
#[path = "handler_pane_stream_tests/surface_test_support.rs"]
mod surface_test_support;

const CONNECTION_ID: u64 = 41;

#[cfg(unix)]
fn quiet_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
}

#[cfg(windows)]
fn quiet_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn test_pane(
    handler: &RequestHandler,
) -> (PaneTarget, PaneOutputSender, SharedPaneTranscript) {
    let session = SessionName::new("pane-stream").expect("valid session name");
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 12, rows: 4 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    let target = PaneTarget::with_window(session, 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    let (output, transcript) = {
        let state = handler.state.lock().await;
        (
            state
                .pane_output_for_target(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )
                .expect("pane output"),
            state.transcript_handle(&target).expect("pane transcript"),
        )
    };
    (target, output, transcript)
}

#[tokio::test]
async fn capture_retries_when_the_resolved_process_generation_changes() {
    let handler = RequestHandler::new();
    let (target, output, _) = test_pane(&handler).await;
    let source = {
        let state = handler.state.lock().await;
        super::stream_source_for_target(&state, target).expect("stream source")
    };
    let next_generation = source.generation.saturating_add(1);
    output.set_generation(next_generation);

    let (source, captured) = handler
        .capture_current_stream_source(source)
        .await
        .expect("stale source is re-resolved");

    assert_eq!(source.generation, next_generation);
    assert_eq!(captured.boundary.generation, next_generation);
}

#[tokio::test]
async fn stale_surface_refresh_cannot_cancel_a_recreated_driver() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let _subscription = subscribe(&handler, &target, PaneStreamMode::Surface).await;

    let mut subscriptions = handler
        .subscriptions
        .lock()
        .expect("subscription registry mutex");
    let key = subscriptions
        .surface_drivers
        .keys()
        .next()
        .expect("surface driver")
        .clone();
    let mut original = subscriptions
        .surface_drivers
        .remove(&key)
        .expect("original surface driver");
    let stale = original.begin_refresh().expect("original refresh");
    let mut replacement = super::SurfaceDriver::new(
        u64::MAX,
        original.receiver,
        Arc::clone(&original.latest),
        original.fingerprint,
    )
    .expect("replacement frame is already validated");
    let current = replacement.begin_refresh().expect("replacement refresh");
    let pending = super::PendingSurfaceRefresh {
        reset: true,
        frame_lifecycle_revision: 0,
    };

    replacement.cancel_refresh(stale, pending);
    assert!(
        replacement.refreshing,
        "an old driver's token must not cancel the replacement's refresh"
    );

    replacement.cancel_refresh(current, pending);
    assert!(
        !replacement.refreshing,
        "the replacement's own token must still cancel its refresh"
    );
}

async fn subscribe(
    handler: &RequestHandler,
    target: &PaneTarget,
    mode: PaneStreamMode,
) -> SubscribePaneStreamResponse {
    subscribe_with_snapshot(handler, target, mode, false).await
}

async fn subscribe_with_snapshot(
    handler: &RequestHandler,
    target: &PaneTarget,
    mode: PaneStreamMode,
    include_snapshot: bool,
) -> SubscribePaneStreamResponse {
    let response = handler
        .handle_subscribe_pane_stream(
            CONNECTION_ID,
            SubscribePaneStreamRequest {
                target: PaneTargetRef::slot(target.clone()),
                mode,
                include_snapshot,
            },
        )
        .await;
    let Response::SubscribePaneStream(response) = response else {
        panic!("unexpected subscribe response: {response:?}");
    };
    *response
}

async fn cursor(
    handler: &RequestHandler,
    subscription_id: rmux_proto::PaneOutputSubscriptionId,
) -> Vec<PaneStreamEvent> {
    cursor_with_limit(handler, subscription_id, 32).await.events
}

async fn cursor_with_limit(
    handler: &RequestHandler,
    subscription_id: rmux_proto::PaneOutputSubscriptionId,
    max_events: u16,
) -> rmux_proto::PaneStreamCursorResponse {
    let response = handler
        .handle_pane_stream_cursor(
            CONNECTION_ID,
            PaneStreamCursorRequest {
                subscription_id,
                max_events: Some(max_events),
            },
        )
        .await;
    let Response::PaneStreamCursor(response) = response else {
        panic!("unexpected cursor response: {response:?}");
    };
    *response
}

async fn cursor_until_unlimited(
    handler: &RequestHandler,
    subscription_id: rmux_proto::PaneOutputSubscriptionId,
    max_events: u16,
) -> Vec<PaneStreamEvent> {
    let mut events = Vec::new();
    loop {
        let response = cursor_with_limit(handler, subscription_id, max_events).await;
        events.extend(response.events);
        if !response.limited {
            return events;
        }
    }
}

fn raw_rebase(event: &PaneStreamEvent) -> Option<&PaneRawRebase> {
    match event {
        PaneStreamEvent::RawRebase(rebase) => Some(rebase),
        _ => None,
    }
}

fn surface_frame(event: &PaneStreamEvent) -> Option<&PaneSurfaceFrame> {
    match event {
        PaneStreamEvent::SurfaceReset(frame) | PaneStreamEvent::SurfacePatch(frame) => Some(frame),
        _ => None,
    }
}

#[tokio::test]
async fn raw_recovery_coverage_reports_sanitized_osc_metadata_incomplete() {
    let handler = RequestHandler::new();
    let (target, _, transcript) = test_pane(&handler).await;
    let payload = concat!(
        "\x1b]2;pile-\x7f-é\x1b\\",
        "\x1b[22;2t",
        "\x1b]2;courant-界\x1b\\",
        "\x1b]7;file:///rép\u{009d}ertoire/界\x1b\\"
    );
    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(payload.as_bytes());

    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let PaneStreamEvent::RawRebase(rebase) = subscribed.event else {
        panic!("raw subscription must start with a rebase");
    };

    assert!(!rebase.coverage.metadata_complete);
    assert!(!rebase.keyframe.contains(&0x7f));
    assert!(!rebase
        .keyframe
        .windows("\u{009d}".len())
        .any(|window| window == "\u{009d}".as_bytes()));
}

#[tokio::test]
async fn raw_stream_emits_bytes_and_rebases_in_band_after_resize() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let PaneStreamEvent::RawRebase(initial) = subscribed.event else {
        panic!("raw subscription must start with a rebase");
    };
    assert_eq!(initial.epoch, 1);
    assert_eq!(initial.reason, PaneRawRebaseReason::Initial);

    let marker = b"raw-stream-marker".to_vec();
    output.send(marker.clone());
    let events = cursor(&handler, subscribed.subscription_id).await;
    assert!(events.iter().any(|event| {
        matches!(
            event,
            PaneStreamEvent::RawBytes(bytes)
                if bytes.epoch == 1 && bytes.bytes == marker
        )
    }));

    output.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 13, rows: 4 });
        ((), true)
    });
    let events = cursor(&handler, subscribed.subscription_id).await;
    let rebase = events
        .iter()
        .find_map(raw_rebase)
        .expect("resize must automatically rebase the same stream");
    assert_eq!(rebase.epoch, 2);
    assert_eq!(rebase.reason, PaneRawRebaseReason::Resize);
    assert_eq!((rebase.cols, rebase.rows), (13, 4));
}

#[tokio::test]
async fn raw_stream_rebases_after_rep_instead_of_replaying_parser_only_state() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"X\x1b[".to_vec());
    let prefix = cursor(&handler, subscribed.subscription_id).await;
    assert!(matches!(
        prefix.as_slice(),
        [PaneStreamEvent::RawBytes(bytes)] if bytes.bytes == b"X\x1b["
    ));

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"2b".to_vec());
    let events = cursor(&handler, subscribed.subscription_id).await;
    let rebase = events
        .iter()
        .find_map(raw_rebase)
        .expect("effective fragmented REP must rebase");
    assert_eq!(rebase.epoch, 2);
    assert_eq!(rebase.reason, PaneRawRebaseReason::TranscriptMutation);
    assert!(
        events.iter().all(
            |event| !matches!(event, PaneStreamEvent::RawBytes(bytes) if bytes.bytes == b"2b")
        ),
        "the final REP fragment must not be replayed after a keyframe"
    );

    let mut recovered = rmux_core::TerminalScreen::new(
        TerminalSize {
            cols: rebase.cols,
            rows: rebase.rows,
        },
        100,
    );
    recovered.feed(&rebase.keyframe);
    assert_eq!(
        recovered.screen().capture_transcript(
            rmux_core::ScreenCaptureRange::default(),
            rmux_core::GridRenderOptions::default()
        ),
        b"XXX\n\n\n\n"
    );

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"Z".to_vec());
    assert!(matches!(
        cursor(&handler, subscribed.subscription_id).await.as_slice(),
        [PaneStreamEvent::RawBytes(bytes)]
            if bytes.epoch == 2
                && bytes.sequence == rebase.next_sequence
                && bytes.bytes == b"Z"
    ));
}

#[tokio::test]
async fn raw_stream_keeps_same_event_rep_as_exact_raw_bytes() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let event = b"Q\x1b[2b".to_vec();

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, event.clone());

    assert!(matches!(
        cursor(&handler, subscribed.subscription_id).await.as_slice(),
        [PaneStreamEvent::RawBytes(bytes)] if bytes.bytes == event
    ));
}

#[tokio::test]
async fn raw_stream_reports_the_clamped_batch_boundary() {
    let handler = RequestHandler::new();
    let (target, output, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    output.send(b"one".to_vec());
    output.send(b"two".to_vec());

    let first = cursor_with_limit(&handler, subscribed.subscription_id, 1).await;
    assert_eq!(first.events.len(), 1);
    assert!(first.limited);
    let second = cursor_with_limit(&handler, subscribed.subscription_id, 1).await;
    assert_eq!(second.events.len(), 1);
    assert!(second.limited);
    assert!(cursor(&handler, subscribed.subscription_id)
        .await
        .is_empty());
}

#[tokio::test]
async fn oversized_raw_batch_ends_the_stream_before_transport_encoding() {
    let handler = RequestHandler::new();
    let (target, output, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    output.send(vec![b'x'; DEFAULT_MAX_DETACHED_FRAME_LENGTH]);

    assert_eq!(
        cursor(&handler, subscribed.subscription_id).await,
        vec![PaneStreamEvent::End(PaneStreamEndReason::SlowConsumer)]
    );
    assert!(
        handler
            .pane_output_subscription_key_for_test(subscribed.subscription_id)
            .is_none(),
        "an undeliverable batch must release its subscription immediately"
    );
}

#[tokio::test]
async fn raw_batch_defers_a_deliverable_event_that_would_overflow_the_response() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let event_len = DEFAULT_MAX_DETACHED_FRAME_LENGTH / 2;
    let output = crate::pane_io::pane_output_channel_with_limits(4, 2 * event_len + 1);
    {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let Some(super::PaneStreamSubscription::Raw(stream)) =
            subscriptions.streams.get_mut(&subscribed.subscription_id)
        else {
            panic!("raw stream state");
        };
        stream.receiver = output.subscribe();
    }

    output.send(vec![b'a'; event_len]);
    output.send(vec![b'b'; event_len]);

    let first = cursor_with_limit(&handler, subscribed.subscription_id, 2).await;
    assert!(first.limited);
    assert!(matches!(
        first.events.as_slice(),
        [PaneStreamEvent::RawBytes(bytes)]
            if bytes.bytes.len() == event_len && bytes.bytes.iter().all(|byte| *byte == b'a')
    ));
    assert!(
        handler
            .pane_output_subscription_key_for_test(subscribed.subscription_id)
            .is_some(),
        "a deliverable event must remain subscribed when only its batch is full"
    );

    let second = cursor_with_limit(&handler, subscribed.subscription_id, 2).await;
    assert!(!second.limited);
    assert!(matches!(
        second.events.as_slice(),
        [PaneStreamEvent::RawBytes(bytes)]
            if bytes.bytes.len() == event_len && bytes.bytes.iter().all(|byte| *byte == b'b')
    ));
}

#[tokio::test]
async fn raw_batch_defers_a_rebase_that_only_overflows_the_current_response() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let PaneStreamEvent::RawRebase(initial) = subscribed.event else {
        panic!("raw subscription must start with a rebase");
    };
    let key = handler
        .pane_output_subscription_key_for_test(subscribed.subscription_id)
        .expect("subscription key");
    let payload_len = 192 * 1024;
    let rebase_len = DEFAULT_MAX_DETACHED_FRAME_LENGTH - 128 * 1024;
    output.send(vec![b'x'; payload_len]);
    {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let Some(super::PaneStreamSubscription::Raw(stream)) =
            subscriptions.streams.get_mut(&subscribed.subscription_id)
        else {
            panic!("raw stream state");
        };
        let output = stream
            .receiver
            .try_recv_observed()
            .expect("queued raw output");
        stream.defer_observation(output);
    }
    output.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 13, rows: 4 });
        ((), true)
    });
    let (boundary, (), _) = output.capture_with_observer(|| ());
    let mut large_rebase = *initial;
    large_rebase.epoch = 2;
    large_rebase.reason = PaneRawRebaseReason::Resize;
    large_rebase.cols = 13;
    large_rebase.rows = 4;
    large_rebase.invalidation_revision = boundary.invalidation_revision;
    large_rebase.next_sequence = boundary.next_output_sequence;
    large_rebase.keyframe = vec![b' '; rebase_len];
    handler
        .subscriptions
        .lock()
        .expect("subscription lock")
        .raw_rebases
        .insert(
            key,
            std::sync::Arc::new(super::CachedRawRebase {
                boundary,
                rebase: large_rebase,
            }),
        );

    let first = cursor_with_limit(&handler, subscribed.subscription_id, 2).await;
    assert!(first.limited);
    assert!(matches!(
        first.events.as_slice(),
        [PaneStreamEvent::RawBytes(bytes)]
            if bytes.bytes.len() == payload_len
    ));
    assert!(
        handler
            .pane_output_subscription_key_for_test(subscribed.subscription_id)
            .is_some(),
        "a response-local size conflict must not end a healthy stream"
    );

    let second = cursor_with_limit(&handler, subscribed.subscription_id, 2).await;
    assert!(!second.limited);
    assert!(matches!(
        second.events.as_slice(),
        [PaneStreamEvent::RawRebase(rebase)]
            if rebase.reason == PaneRawRebaseReason::Resize
                && rebase.keyframe.len() == rebase_len
    ));
}

#[tokio::test]
async fn raw_batch_defers_lifecycle_that_would_overflow_the_response() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let event_len = DEFAULT_MAX_DETACHED_FRAME_LENGTH - 64;
    let output = crate::pane_io::pane_output_channel_with_limits(4, event_len + 1);
    {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let Some(super::PaneStreamSubscription::Raw(stream)) =
            subscriptions.streams.get_mut(&subscribed.subscription_id)
        else {
            panic!("raw stream state");
        };
        stream.receiver = output.subscribe();
    }

    output.send(vec![b'x'; event_len]);
    output.send(Vec::new());

    let first = cursor_with_limit(&handler, subscribed.subscription_id, 2).await;
    assert!(first.limited);
    assert!(matches!(
        first.events.as_slice(),
        [PaneStreamEvent::RawBytes(bytes)] if bytes.bytes.len() == event_len
    ));
    let second = cursor_with_limit(&handler, subscribed.subscription_id, 2).await;
    assert!(!second.limited);
    assert!(matches!(
        second.events.as_slice(),
        [PaneStreamEvent::Lifecycle(
            PaneStreamLifecycleEvent::ProcessExited {
                output_sequence: Some(_)
            }
        )]
    ));
}

#[tokio::test]
async fn raw_stream_rebases_when_new_output_expires_parser_state() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    crate::pane_io::publish_pane_bytes_for_test(
        &transcript,
        &output,
        b"\x1bPunterminated".to_vec(),
    );
    assert!(matches!(
        cursor(&handler, subscribed.subscription_id).await.as_slice(),
        [PaneStreamEvent::RawBytes(bytes)] if bytes.bytes == b"\x1bPunterminated"
    ));
    transcript
        .lock()
        .expect("transcript lock")
        .make_ground_timer_due_for_test();

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"AFTER".to_vec());
    let events = cursor(&handler, subscribed.subscription_id).await;
    let rebase = events
        .iter()
        .find_map(raw_rebase)
        .expect("parser expiry during append must rebase");
    assert_eq!(rebase.reason, PaneRawRebaseReason::TranscriptMutation);
    assert!(events.iter().all(
        |event| !matches!(event, PaneStreamEvent::RawBytes(bytes) if bytes.bytes == b"AFTER")
    ));
}

#[tokio::test]
async fn process_exit_is_delivered_before_generation_rebase() {
    let handler = RequestHandler::new();
    let (target, output, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    output.send(Vec::new());
    output.set_generation(output.current_generation().saturating_add(1));
    let events = cursor(&handler, subscribed.subscription_id).await;
    assert!(matches!(
        events.first(),
        Some(PaneStreamEvent::Lifecycle(
            PaneStreamLifecycleEvent::ProcessExited {
                output_sequence: None
            }
        ))
    ));
    let rebase = events
        .iter()
        .find_map(raw_rebase)
        .expect("generation switch must rebase");
    assert_eq!(rebase.reason, PaneRawRebaseReason::GenerationChanged);
    assert_eq!(rebase.epoch, 2);
}

#[tokio::test]
async fn process_exit_after_resize_is_delivered_after_the_resize_rebase() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    output.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 13, rows: 4 });
        ((), true)
    });
    output.send(Vec::new());

    let first = cursor(&handler, subscribed.subscription_id).await;
    assert!(matches!(first.as_slice(), [PaneStreamEvent::RawRebase(_)]));
    let second = cursor(&handler, subscribed.subscription_id).await;
    assert_eq!(
        second,
        vec![PaneStreamEvent::Lifecycle(
            PaneStreamLifecycleEvent::ProcessExited {
                output_sequence: None,
            }
        )]
    );
}

#[tokio::test]
async fn cancelled_raw_rebase_remains_pending_on_the_same_subscription() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let token = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let Some(super::PaneStreamSubscription::Raw(stream)) =
            subscriptions.streams.get_mut(&subscribed.subscription_id)
        else {
            panic!("raw stream state");
        };
        stream.begin_rebase().expect("rebase token")
    };
    let guard = super::RawRebaseGuard::new(
        &handler.subscriptions,
        subscribed.subscription_id,
        token,
        PaneRawRebaseReason::Resize,
    );

    drop(guard);

    let subscriptions = handler.subscriptions.lock().expect("subscription lock");
    let Some(super::PaneStreamSubscription::Raw(stream)) =
        subscriptions.streams.get(&subscribed.subscription_id)
    else {
        panic!("raw stream state");
    };
    assert!(!stream.is_rebasing());
    assert_eq!(stream.pending_rebase(), Some(PaneRawRebaseReason::Resize));
}

#[tokio::test]
async fn surface_subscribers_share_one_driver_and_receive_the_same_frame() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let first = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let second = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    assert_eq!(
        handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .surface_drivers
            .len(),
        1
    );

    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(b"shared");
    output.send(b"shared".to_vec());
    let first_events = cursor(&handler, first.subscription_id).await;
    let second_events = cursor(&handler, second.subscription_id).await;
    let first_frame = first_events
        .iter()
        .find_map(surface_frame)
        .expect("first surface update");
    let second_frame = second_events
        .iter()
        .find_map(surface_frame)
        .expect("second surface update");
    assert_eq!(first_frame, second_frame);
    assert!(matches!(
        first_events.first(),
        Some(PaneStreamEvent::SurfacePatch(_))
    ));
}

#[tokio::test]
async fn surface_exposes_hyperlink_uris_and_dynamic_colors() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let payload = concat!(
        "\u{1b}]10;rgb:1111/2222/3333\u{1b}\\",
        "\u{1b}]11;#102030\u{1b}\\",
        "\u{1b}]12;rgb:aaaa/bbbb/cccc\u{1b}\\",
        "\u{1b}]8;id=docs;https://example.test/docs\u{1b}\\",
        "X",
        "\u{1b}]8;;\u{1b}\\"
    )
    .as_bytes();
    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(payload);
    output.send(payload.to_vec());

    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let frame = surface_frame(&subscribed.event).expect("initial surface reset");
    let linked = frame
        .snapshot
        .cells
        .iter()
        .find(|cell| cell.text == "X")
        .expect("linked visible cell");

    assert_ne!(linked.link, 0);
    assert_eq!(frame.snapshot.hyperlinks.len(), 1);
    assert_eq!(frame.snapshot.hyperlinks[0].id, linked.link);
    assert_eq!(
        frame.snapshot.hyperlinks[0].uri,
        "https://example.test/docs"
    );
    assert_eq!(
        frame.snapshot.dynamic_colors.foreground.as_deref(),
        Some("rgb:1111/2222/3333")
    );
    assert_eq!(
        frame.snapshot.dynamic_colors.background.as_deref(),
        Some("#102030")
    );
    assert_eq!(
        frame.snapshot.dynamic_colors.cursor.as_deref(),
        Some("rgb:aaaa/bbbb/cccc")
    );
    assert!(frame.snapshot.metadata_complete);
}

#[tokio::test]
async fn surface_dynamic_color_only_output_emits_a_patch() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let payload = b"\x1b]10;#abcdef\x1b\\";

    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(payload);
    output.send(payload.to_vec());

    let events = cursor(&handler, subscribed.subscription_id).await;
    let frame = events
        .iter()
        .find_map(surface_frame)
        .expect("dynamic colour change must update the surface");
    assert!(matches!(
        events.first(),
        Some(PaneStreamEvent::SurfacePatch(_))
    ));
    assert_eq!(
        frame.snapshot.dynamic_colors.foreground.as_deref(),
        Some("#abcdef")
    );

    let reset = b"\x1b]110\x1b\\";
    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(reset);
    output.send(reset.to_vec());
    let reset_events = cursor(&handler, subscribed.subscription_id).await;
    let reset_frame = reset_events
        .iter()
        .find_map(surface_frame)
        .expect("dynamic colour reset must update the surface");
    assert_eq!(reset_frame.snapshot.dynamic_colors.foreground, None);
}

#[tokio::test]
async fn surface_marks_omitted_hyperlink_metadata_incomplete() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let uri = format!("https://example.test/{}", "x".repeat(600));
    let payload = format!("\u{1b}]8;;{uri}\u{1b}\\X\u{1b}]8;;\u{1b}\\");
    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(payload.as_bytes());
    output.send(payload.into_bytes());

    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let frame = surface_frame(&subscribed.event).expect("initial surface reset");
    let linked = frame
        .snapshot
        .cells
        .iter()
        .find(|cell| cell.text == "X")
        .expect("linked visible cell");

    assert_ne!(linked.link, 0);
    assert!(frame.snapshot.hyperlinks.is_empty());
    assert!(!frame.snapshot.metadata_complete);
}

#[tokio::test]
async fn reserved_surface_subscriber_keeps_the_shared_driver_alive() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let first = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let key = handler
        .pane_output_subscription_key_for_test(first.subscription_id)
        .expect("subscription key");
    let reserved_id = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let id = subscriptions
            .registry
            .subscribe(CONNECTION_ID, key.clone(), std::time::Instant::now())
            .expect("second reservation")
            .id();
        subscriptions.streams.insert(
            id,
            super::PaneStreamSubscription::reserved(PaneStreamMode::Surface),
        );
        id
    };

    let response = handler
        .handle_unsubscribe_pane_stream(
            CONNECTION_ID,
            UnsubscribePaneStreamRequest {
                subscription_id: first.subscription_id,
            },
        )
        .await;
    assert!(matches!(response, Response::UnsubscribePaneStream(_)));
    assert!(
        handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .surface_drivers
            .contains_key(&key),
        "an in-flight surface reservation still needs the existing driver"
    );

    let _ = handler
        .handle_unsubscribe_pane_stream(
            CONNECTION_ID,
            UnsubscribePaneStreamRequest {
                subscription_id: reserved_id,
            },
        )
        .await;
}

#[tokio::test]
async fn surface_noop_mutation_is_silent_but_resize_resets_epoch() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;

    output.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |_transcript| {
        ((), false)
    });
    assert!(cursor(&handler, subscribed.subscription_id)
        .await
        .is_empty());

    output.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 14, rows: 4 });
        ((), true)
    });
    let events = cursor(&handler, subscribed.subscription_id).await;
    let reset = events
        .iter()
        .find_map(surface_frame)
        .expect("surface reset");
    assert_eq!(reset.epoch, 2);
    assert_eq!((reset.snapshot.cols, reset.snapshot.rows), (14, 4));
    assert!(matches!(
        events.first(),
        Some(PaneStreamEvent::SurfaceReset(_))
    ));
}

#[tokio::test]
async fn identical_reset_does_not_regress_the_shared_pane_snapshot_revision() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let surface = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let initial = surface_frame(&surface.event)
        .expect("surface subscription starts with a frame")
        .snapshot
        .revision;

    // `clear_pane_history` reports a transcript mutation unconditionally, so a
    // pane whose scrollback is already empty is invalidated without any
    // observable state changing. tmux 3.7b behaves the same way: the grid,
    // cursor and history counters are identical across `clear-history` on an
    // empty-history pane (.rmux-audit/tmux-3.7b-identical-reset.txt).
    output.mutate_transcript(
        &transcript,
        PaneInvalidationReason::ClearHistory,
        |transcript| {
            transcript.clear_history(false);
            ((), true)
        },
    );

    let published = cursor(&handler, surface.subscription_id)
        .await
        .iter()
        .find_map(surface_frame)
        .expect("an identical reset still republishes the surface")
        .snapshot
        .revision;
    assert!(
        published > initial,
        "reset frame revision {published} must advance past {initial}"
    );

    // `PaneSurfaceSnapshot::revision` and `PaneSnapshotResponse::revision` are
    // documented as one shared monotonic counter, so no snapshot reader may
    // hand back a revision below one the surface stream already published.
    let response = handler
        .handle(Request::PaneSnapshot(PaneSnapshotRequest {
            target: target.clone(),
        }))
        .await;
    let Response::PaneSnapshot(snapshot) = response else {
        panic!("unexpected snapshot response: {response:?}");
    };
    assert!(
        snapshot.revision >= published,
        "pane snapshot revision regressed from {published} to {}",
        snapshot.revision
    );

    let raw = subscribe_with_snapshot(&handler, &target, PaneStreamMode::Raw, true).await;
    let rebased = raw_rebase(&raw.event)
        .expect("raw subscription starts with a rebase")
        .snapshot
        .as_ref()
        .expect("include_snapshot materializes a typed snapshot")
        .revision;
    assert!(
        rebased >= published,
        "raw rebase snapshot revision regressed from {published} to {rebased}"
    );
}

#[tokio::test]
async fn surface_refresh_retries_an_invalidation_captured_during_projection() {
    let handler = Arc::new(RequestHandler::new());
    let (target, output, transcript) = test_pane(handler.as_ref()).await;
    let subscribed = subscribe(handler.as_ref(), &target, PaneStreamMode::Surface).await;

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"visual".to_vec());
    let state = handler.state.lock().await;
    let poll_handler = Arc::clone(&handler);
    let subscription_id = subscribed.subscription_id;
    let poll =
        tokio::spawn(
            async move { cursor_with_limit(poll_handler.as_ref(), subscription_id, 32).await },
        );

    let mut refresh_started = false;
    for _ in 0..100 {
        refresh_started = handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .surface_drivers
            .values()
            .any(|driver| driver.refreshing);
        if refresh_started {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(refresh_started, "surface refresh starts before capture");

    output.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 14, rows: 4 });
        ((), true)
    });
    drop(state);

    let first = poll.await.expect("surface poll joins");
    let second = cursor_with_limit(handler.as_ref(), subscription_id, 32).await;
    let events = first
        .events
        .into_iter()
        .chain(second.events)
        .collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, PaneStreamEvent::SurfacePatch(_))),
        "an invalidated projection must not publish a patch in the old epoch"
    );
    let reset = events
        .iter()
        .find_map(|event| match event {
            PaneStreamEvent::SurfaceReset(frame) => Some(frame),
            _ => None,
        })
        .expect("the invalidation is retried as a reset");
    assert_eq!(reset.epoch, 2);
    assert_eq!((reset.snapshot.cols, reset.snapshot.rows), (14, 4));
}

#[tokio::test]
async fn surface_nonvisual_output_advances_receiver_without_emitting_a_patch() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;

    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(b"\x07");
    output.send(b"\x07".to_vec());

    assert!(
        cursor(&handler, subscribed.subscription_id)
            .await
            .is_empty(),
        "a bell changes no field in the structured surface"
    );
    assert!(
        cursor(&handler, subscribed.subscription_id)
            .await
            .is_empty(),
        "the shared driver must advance past nonvisual output"
    );
}

#[tokio::test]
async fn surface_delivers_metadata_completeness_changes_with_same_bounded_title() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let bounded_title = "x".repeat(crate::pane_recovery::MAX_RECOVERY_STRING_BYTES);

    for (title, expected_complete) in [
        (bounded_title.clone(), true),
        (format!("{bounded_title}y"), false),
        (bounded_title.clone(), true),
    ] {
        let bytes = format!("\x1b]2;{title}\x07").into_bytes();
        transcript
            .lock()
            .expect("transcript lock")
            .append_bytes(&bytes);
        output.send(bytes);

        let events = cursor(&handler, subscribed.subscription_id).await;
        let frame = events
            .iter()
            .find_map(surface_frame)
            .expect("metadata completeness transition");
        assert_eq!(frame.snapshot.title, bounded_title);
        assert_eq!(frame.snapshot.metadata_complete, expected_complete);
    }
}

#[tokio::test]
async fn surface_delivers_metadata_only_completeness_changes() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let uri = "x".repeat(crate::pane_recovery::MAX_RECOVERY_HYPERLINK_ENTRY_BYTES + 1);
    let bytes = format!("\x1b]8;;{uri}\x1b\\").into_bytes();

    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(&bytes);
    output.send(bytes);

    let events = cursor(&handler, subscribed.subscription_id).await;
    let frame = events
        .iter()
        .find_map(surface_frame)
        .expect("metadata-only surface update");
    assert!(!frame.snapshot.metadata_complete);
}

#[tokio::test]
async fn surface_refresh_does_not_hide_exit_published_during_its_rebase_window() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;

    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(b"visual");
    output.send(b"visual".to_vec());
    output.send(Vec::new());

    let first = cursor_with_limit(&handler, subscribed.subscription_id, 1).await;
    assert!(matches!(
        first.events.as_slice(),
        [PaneStreamEvent::SurfacePatch(_)]
    ));
    let second = cursor_with_limit(&handler, subscribed.subscription_id, 1).await;
    assert_eq!(
        second.events,
        vec![PaneStreamEvent::Lifecycle(
            PaneStreamLifecycleEvent::ProcessExited {
                output_sequence: None,
            }
        )]
    );
}

#[tokio::test]
async fn surface_patch_lifecycle_order_is_independent_of_cursor_batch_size() {
    async fn observed_order(max_events: u16, visual_before_exit: bool) -> Vec<&'static str> {
        let handler = RequestHandler::new();
        let (target, output, transcript) = test_pane(&handler).await;
        let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;

        let send_visual = || {
            transcript
                .lock()
                .expect("transcript lock")
                .append_bytes(b"visual");
            output.send(b"visual".to_vec());
        };
        if visual_before_exit {
            send_visual();
            output.send(Vec::new());
        } else {
            output.send(Vec::new());
            send_visual();
        }

        let mut order = Vec::new();
        loop {
            let response =
                cursor_with_limit(&handler, subscribed.subscription_id, max_events).await;
            order.extend(response.events.iter().map(|event| match event {
                PaneStreamEvent::SurfacePatch(_) => "patch",
                PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. }) => {
                    "lifecycle"
                }
                event => panic!("unexpected surface event: {event:?}"),
            }));
            if !response.limited {
                break;
            }
        }
        order
    }

    for (visual_before_exit, expected) in [
        (true, vec!["patch", "lifecycle"]),
        (false, vec!["lifecycle", "patch"]),
    ] {
        let single_event = observed_order(1, visual_before_exit).await;
        let batched = observed_order(32, visual_before_exit).await;

        assert_eq!(single_event, expected);
        assert_eq!(batched, single_event);
    }
}

#[tokio::test]
async fn surface_delivers_exit_before_a_following_generation_reset() {
    let handler = RequestHandler::new();
    let (target, output, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;

    output.send(Vec::new());
    output.set_generation(output.current_generation().saturating_add(1));

    let first = cursor_with_limit(&handler, subscribed.subscription_id, 1).await;
    assert_eq!(
        first.events,
        vec![PaneStreamEvent::Lifecycle(
            PaneStreamLifecycleEvent::ProcessExited {
                output_sequence: None,
            }
        )]
    );
    assert!(first.limited);

    let second = cursor_with_limit(&handler, subscribed.subscription_id, 1).await;
    assert!(matches!(
        second.events.as_slice(),
        [PaneStreamEvent::SurfaceReset(_)]
    ));
}

#[tokio::test]
async fn surface_delivers_exit_before_reset_when_visual_output_precedes_eof() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;

    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(b"visual");
    output.send(b"visual".to_vec());
    output.send(Vec::new());
    output.set_generation(output.current_generation().saturating_add(1));

    let events = cursor(&handler, subscribed.subscription_id).await;
    assert!(matches!(
        events.as_slice(),
        [
            PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. }),
            PaneStreamEvent::SurfaceReset(_)
        ]
    ));
}

#[tokio::test]
async fn surface_delivers_each_process_exit_without_collapsing_revisions() {
    let handler = RequestHandler::new();
    let (target, output, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;

    output.send(Vec::new());
    output.send(Vec::new());

    for _ in 0..2 {
        let response = cursor_with_limit(&handler, subscribed.subscription_id, 1).await;
        assert_eq!(
            response.events,
            vec![PaneStreamEvent::Lifecycle(
                PaneStreamLifecycleEvent::ProcessExited {
                    output_sequence: None,
                }
            )]
        );
    }
    assert!(cursor(&handler, subscribed.subscription_id)
        .await
        .is_empty());
}

#[tokio::test]
async fn cancelled_surface_refresh_is_retried_after_a_pane_rekey() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let previous = handler
        .pane_output_subscription_key_for_test(subscribed.subscription_id)
        .expect("subscription key");
    let pending = super::PendingSurfaceRefresh {
        reset: true,
        frame_lifecycle_revision: 0,
    };
    let token = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        subscriptions
            .surface_drivers
            .get_mut(&previous)
            .expect("surface driver")
            .begin_refresh()
            .expect("refresh token")
    };
    let guard =
        super::SurfaceRefreshGuard::new(&handler.subscriptions, previous.pane_id(), token, pending);
    let current = rmux_core::events::PaneOutputSubscriptionKey::new(
        SessionName::new("pane-stream-moved").expect("valid session"),
        previous.pane_id(),
    );
    handler
        .subscriptions
        .lock()
        .expect("subscription lock")
        .rekey_pane(&previous, current.clone());

    drop(guard);

    let subscriptions = handler.subscriptions.lock().expect("subscription lock");
    let driver = subscriptions
        .surface_drivers
        .get(&current)
        .expect("rekeyed surface driver");
    assert!(!driver.refreshing);
    assert_eq!(driver.pending_refresh(), Some(pending));
}

async fn assert_kill_drains_buffered_stream_events(
    handler: &RequestHandler,
    target: &PaneTarget,
    output: &PaneOutputSender,
    transcript: &SharedPaneTranscript,
    kill: Request,
) {
    let raw = subscribe(handler, target, PaneStreamMode::Raw).await;
    let surface = subscribe(handler, target, PaneStreamMode::Surface).await;

    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(b"killed-tail");
    let tail_sequence = output.send(b"killed-tail".to_vec());

    let response = handler.handle(kill).await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");

    let raw_events = cursor_until_unlimited(handler, raw.subscription_id, 1).await;
    assert_unique_pane_removed_end(&raw_events, "raw kill drain");
    assert!(
        raw_events[..raw_events.len() - 1].iter().all(|event| {
            matches!(
                event,
                PaneStreamEvent::RawBytes(_)
                    | PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            )
        }),
        "raw kill drain emitted an unexpected pre-terminal event: {raw_events:?}"
    );
    assert_eq!(
        raw_events
            .iter()
            .filter(|event| matches!(event, PaneStreamEvent::RawBytes(bytes) if bytes.bytes == b"killed-tail"))
            .count(),
        1,
        "raw kill drain must deliver the staged tail exactly once: {raw_events:?}"
    );
    assert!(
        raw_events
            .iter()
            .filter(|event| matches!(
                event,
                PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            ))
            .count()
            <= 1,
        "one killed process may publish at most one lifecycle event: {raw_events:?}"
    );

    let surface_events = cursor_until_unlimited(handler, surface.subscription_id, 1).await;
    assert_unique_pane_removed_end(&surface_events, "Surface kill drain");
    assert!(
        surface_events[..surface_events.len() - 1]
            .iter()
            .all(|event| matches!(
                event,
                PaneStreamEvent::SurfacePatch(_)
                    | PaneStreamEvent::SurfaceReset(_)
                    | PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            )),
        "Surface kill drain emitted an unexpected pre-terminal event: {surface_events:?}"
    );
    let surface_frames = surface_events
        .iter()
        .filter_map(surface_frame)
        .collect::<Vec<_>>();
    let final_surface_frame = surface_frames
        .last()
        .expect("a killed Surface stream must publish its final authoritative frame");
    // Raw above proves exact-byte delivery. Surface instead proves that its
    // final authoritative state includes the tail's output boundary, even if
    // terminal teardown has since moved those cells out of the visible grid.
    assert!(
        final_surface_frame.next_output_sequence > tail_sequence,
        "the final Surface frame must represent the staged output boundary before End: \
         tail sequence {tail_sequence}, events {surface_events:?}"
    );
    assert!(
        surface_events
            .iter()
            .filter(|event| matches!(
                event,
                PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            ))
            .count()
            <= 1,
        "one killed process may publish at most one Surface lifecycle event: {surface_events:?}"
    );
}

fn assert_unique_pane_removed_end(events: &[PaneStreamEvent], label: &str) {
    assert!(
        matches!(
            events.last(),
            Some(PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved))
        ),
        "{label} must end with PaneRemoved: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, PaneStreamEvent::End(_)))
            .count(),
        1,
        "{label} must publish exactly one terminal event: {events:?}"
    );
}

#[tokio::test]
async fn kill_pane_drains_buffered_stream_events_before_typed_end() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    assert_kill_drains_buffered_stream_events(
        &handler,
        &target,
        &output,
        &transcript,
        Request::KillPane(rmux_proto::KillPaneRequest {
            target: target.clone(),
            kill_all_except: false,
        }),
    )
    .await;
}

#[tokio::test]
async fn kill_pane_all_except_drains_the_removed_pane_stream() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(target.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");

    // Killing every pane except the split sibling leaves the window and the
    // session alive, so only the removed pane's staged source can project its
    // buffered tail.
    assert_kill_drains_buffered_stream_events(
        &handler,
        &target,
        &output,
        &transcript,
        Request::KillPane(rmux_proto::KillPaneRequest {
            target: PaneTarget::with_window(target.session_name().clone(), 0, 1),
            kill_all_except: true,
        }),
    )
    .await;
}

#[tokio::test]
async fn pane_kill_by_id_drains_buffered_stream_events_before_typed_end() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .and_then(|window| window.pane(target.pane_index()))
            .map(rmux_core::Pane::id)
            .expect("initial pane exists")
    };
    assert_kill_drains_buffered_stream_events(
        &handler,
        &target,
        &output,
        &transcript,
        Request::PaneKill(rmux_proto::PaneKillRequest {
            target: PaneTargetRef::by_id(target.session_name().clone(), pane_id),
            kill_all_except: false,
        }),
    )
    .await;
}

#[tokio::test]
async fn natural_pane_exit_drains_raw_and_surface_streams_before_end() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let raw = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let surface = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let initial_surface_revision = surface_frame(&surface.event)
        .expect("surface subscription starts with a frame")
        .snapshot
        .revision;
    let key = handler
        .pane_output_subscription_key_for_test(raw.subscription_id)
        .expect("subscription key");

    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(b"natural-tail");
    let tail_sequence = output.send(b"natural-tail".to_vec());
    output.send(Vec::new());
    handler
        .state
        .lock()
        .await
        .mark_pane_dead_without_exit_details(&target)
        .expect("mark pane naturally exited");
    handler
        .handle_pane_exit_event(PaneExitEvent::eof_published(
            target.session_name().clone(),
            key.pane_id(),
            None,
        ))
        .await;

    let raw_events = cursor_until_unlimited(&handler, raw.subscription_id, 1).await;
    assert_unique_pane_removed_end(&raw_events, "natural Raw exit drain");
    assert!(
        raw_events[..raw_events.len() - 1].iter().all(|event| {
            matches!(
                event,
                PaneStreamEvent::RawBytes(_)
                    | PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            )
        }),
        "natural Raw exit drain emitted an unexpected pre-terminal event: {raw_events:?}"
    );
    assert_eq!(
        raw_events
            .iter()
            .filter(|event| matches!(event, PaneStreamEvent::RawBytes(bytes) if bytes.bytes == b"natural-tail"))
            .count(),
        1,
        "natural Raw exit drain must deliver the staged tail exactly once: {raw_events:?}"
    );
    assert_eq!(
        raw_events
            .iter()
            .filter(|event| matches!(
                event,
                PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            ))
            .count(),
        1,
        "natural Raw exit must publish one lifecycle event: {raw_events:?}"
    );

    let surface_events = cursor_until_unlimited(&handler, surface.subscription_id, 1).await;
    assert_unique_pane_removed_end(&surface_events, "natural Surface exit drain");
    assert!(
        surface_events[..surface_events.len() - 1]
            .iter()
            .all(|event| matches!(
                event,
                PaneStreamEvent::SurfacePatch(_)
                    | PaneStreamEvent::SurfaceReset(_)
                    | PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            )),
        "natural Surface exit drain emitted an unexpected event: {surface_events:?}"
    );
    let lifecycle_positions = surface_events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(
                event,
                PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        lifecycle_positions.len(),
        1,
        "natural Surface exit must publish one lifecycle event: {surface_events:?}"
    );
    let final_surface_frame = surface_events
        .iter()
        .filter_map(surface_frame)
        .next_back()
        .expect("natural Surface exit must publish its final authoritative frame");
    assert!(
        final_surface_frame.snapshot.revision > initial_surface_revision
            && final_surface_frame.next_output_sequence > tail_sequence,
        "the final Surface frame must include the natural-exit tail: {surface_events:?}"
    );
    assert!(
        handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .is_empty(),
        "ended pane streams must not keep exit-empty shutdown busy"
    );
}

async fn assert_exit_commit_keeps_stream_source_available(
    mode: PaneStreamMode,
    keep_session: bool,
) {
    let handler = Arc::new(RequestHandler::new());
    let (target, output, transcript) = test_pane(handler.as_ref()).await;
    if keep_session {
        let response = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(target.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
    }
    let subscribed =
        subscribe_with_snapshot(handler.as_ref(), &target, mode, mode == PaneStreamMode::Raw).await;
    let key = handler
        .pane_output_subscription_key_for_test(subscribed.subscription_id)
        .expect("subscription key");

    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(b"commit-tail");
    let tail_sequence = output.send(b"commit-tail".to_vec());
    output.mutate_transcript(&transcript, PaneInvalidationReason::Resize, |transcript| {
        transcript.resize(TerminalSize { cols: 13, rows: 4 });
        ((), true)
    });
    output.send(Vec::new());
    handler
        .state
        .lock()
        .await
        .mark_pane_dead_without_exit_details(&target)
        .expect("mark pane naturally exited");

    let pause = handler.install_pane_exit_commit_pause();
    let exit_handler = Arc::clone(&handler);
    let exit_session = target.session_name().clone();
    let exit_task = tokio::spawn(async move {
        exit_handler
            .handle_pane_exit_event(PaneExitEvent::eof_published(
                exit_session,
                key.pane_id(),
                None,
            ))
            .await;
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("pane exit reaches the post-commit pause");

    let mut events = cursor(handler.as_ref(), subscribed.subscription_id).await;
    events.extend(cursor(handler.as_ref(), subscribed.subscription_id).await);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PaneStreamEvent::End(_))),
        "stream must not end between state removal and drain publication: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
        )),
        "process-exit lifecycle must remain available: {events:?}"
    );
    match mode {
        PaneStreamMode::Raw => assert!(events.iter().any(|event| match event {
            PaneStreamEvent::RawBytes(bytes) => bytes.bytes == b"commit-tail",
            PaneStreamEvent::RawRebase(rebase) =>
                rebase.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot
                        .cells
                        .iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .contains("commit-tail")
                }),
            _ => false,
        })),
        PaneStreamMode::Surface => {
            let final_surface_frame = events
                .iter()
                .filter_map(surface_frame)
                .next_back()
                .expect("committed Surface source must remain available after state removal");
            assert!(
                final_surface_frame.next_output_sequence > tail_sequence,
                "committed Surface source must advance past the staged output boundary: \
                 tail sequence {tail_sequence}, events {events:?}"
            );
        }
        _ => panic!("test requested an unsupported pane stream projection"),
    }
    pause.release.notify_one();
    exit_task.await.expect("pane exit task joins");
    let final_events = cursor(handler.as_ref(), subscribed.subscription_id).await;
    assert_unique_pane_removed_end(&final_events, "post-commit pane exit");
    let final_projection_events = &final_events[..final_events.len() - 1];
    assert!(
        final_projection_events.iter().all(|event| match mode {
            PaneStreamMode::Raw => matches!(
                event,
                PaneStreamEvent::RawRebase(_)
                    | PaneStreamEvent::RawBytes(_)
                    | PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            ),
            PaneStreamMode::Surface => matches!(
                event,
                PaneStreamEvent::SurfaceReset(_)
                    | PaneStreamEvent::SurfacePatch(_)
                    | PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            ),
            _ => false,
        }),
        "only valid projection events may precede the terminal event: {final_events:?}"
    );
    assert!(
        final_projection_events
            .iter()
            .filter(|event| matches!(
                event,
                PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
            ))
            .count()
            <= 1,
        "the fixture child may publish at most one additional EOF: {final_events:?}"
    );
    // This fixture injects an EOF while its quiet OS child is still running.
    // Windows may synchronously emit terminal-mode teardown bytes and a second,
    // real EOF after the commit pause. Every raw output sequence must still be
    // new and strictly ordered before the logical-pane end.
    if mode == PaneStreamMode::Raw {
        let raw_sequences = events
            .iter()
            .chain(final_projection_events)
            .filter_map(|event| match event {
                PaneStreamEvent::RawBytes(bytes) => Some(bytes.sequence),
                PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited {
                    output_sequence: Some(output_sequence),
                }) => Some(*output_sequence),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            raw_sequences.windows(2).all(|pair| pair[0] < pair[1]),
            "raw output and EOF sequences must remain strictly monotonic: \
             {raw_sequences:?}; events={events:?}; final={final_events:?}"
        );
    }
}

#[tokio::test]
async fn natural_exit_publishes_drain_source_atomically_with_pane_removal() {
    for mode in [PaneStreamMode::Raw, PaneStreamMode::Surface] {
        assert_exit_commit_keeps_stream_source_available(mode, true).await;
    }
}

#[tokio::test]
async fn natural_exit_publishes_drain_source_atomically_with_session_removal() {
    for mode in [PaneStreamMode::Raw, PaneStreamMode::Surface] {
        assert_exit_commit_keeps_stream_source_available(mode, false).await;
    }
}

#[tokio::test]
async fn idle_before_exit_does_not_consume_the_pane_stream_drain_window() {
    let handler = RequestHandler::new();
    let (target, output, transcript) = test_pane(&handler).await;
    let raw = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let surface = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let key = handler
        .pane_output_subscription_key_for_test(raw.subscription_id)
        .expect("subscription key");

    tokio::time::sleep(Duration::from_millis(2_100)).await;
    transcript
        .lock()
        .expect("transcript lock")
        .append_bytes(b"idle-tail");
    output.send(b"idle-tail".to_vec());
    output.send(Vec::new());
    handler
        .state
        .lock()
        .await
        .mark_pane_dead_without_exit_details(&target)
        .expect("mark pane naturally exited");
    handler
        .handle_pane_exit_event(PaneExitEvent::eof_published(
            target.session_name().clone(),
            key.pane_id(),
            None,
        ))
        .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let raw_events = cursor_until_unlimited(&handler, raw.subscription_id, 1).await;
    assert!(raw_events.iter().any(
        |event| matches!(event, PaneStreamEvent::RawBytes(bytes) if bytes.bytes == b"idle-tail")
    ));
    assert!(raw_events.iter().any(|event| matches!(
        event,
        PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
    )));
    assert!(matches!(
        raw_events.last(),
        Some(PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved))
    ));
    let surface_events = cursor_until_unlimited(&handler, surface.subscription_id, 1).await;
    assert!(surface_events
        .iter()
        .any(|event| surface_frame(event).is_some()));
    assert!(surface_events.iter().any(|event| matches!(
        event,
        PaneStreamEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited { .. })
    )));
    assert!(matches!(
        surface_events.last(),
        Some(PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved))
    ));
}

#[tokio::test]
async fn reserved_surface_subscription_keeps_exit_reason_when_initialization_finishes() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let active = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let source = {
        let state = handler.state.lock().await;
        super::stream_source_for_target(&state, target).expect("stream source")
    };
    let key = handler
        .pane_output_subscription_key_for_test(active.subscription_id)
        .expect("subscription key");
    let reserved_id = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let id = subscriptions
            .registry
            .subscribe(CONNECTION_ID, key.clone(), std::time::Instant::now())
            .expect("second reservation")
            .id();
        subscriptions.streams.insert(
            id,
            super::PaneStreamSubscription::reserved(PaneStreamMode::Surface),
        );
        subscriptions.mark_pane_streams_ending(&key, PaneStreamEndReason::PaneRemoved);
        id
    };

    let response = handler.finish_existing_surface_subscription(CONNECTION_ID, reserved_id, source);
    assert!(
        matches!(response, Response::SubscribePaneStream(_)),
        "{response:?}"
    );
    assert_eq!(
        cursor(&handler, reserved_id).await,
        vec![PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved)]
    );
    handler.cleanup_connection_subscriptions_sync(CONNECTION_ID);
}

#[tokio::test]
async fn reserved_raw_subscription_keeps_exit_reason_when_initialization_finishes() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let source = {
        let state = handler.state.lock().await;
        super::stream_source_for_target(&state, target).expect("stream source")
    };
    let (source, captured) = handler
        .capture_current_stream_source(source)
        .await
        .expect("capture stream source");
    let rebase = super::materialize_raw_rebase(
        &handler,
        source.key.pane_id(),
        1,
        PaneRawRebaseReason::Initial,
        false,
        &captured,
    )
    .expect("materialize raw rebase");
    let reserved_id = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let id = subscriptions
            .registry
            .subscribe(CONNECTION_ID, source.key.clone(), std::time::Instant::now())
            .expect("raw reservation")
            .id();
        subscriptions.streams.insert(
            id,
            super::PaneStreamSubscription::reserved(PaneStreamMode::Raw),
        );
        subscriptions.mark_pane_streams_ending(&source.key, PaneStreamEndReason::PaneRemoved);
        id
    };

    let response = handler.finish_raw_subscription(
        CONNECTION_ID,
        reserved_id,
        source,
        super::RawSubscriptionStart::captured(captured, rebase, false),
    );
    assert!(
        matches!(response, Response::SubscribePaneStream(_)),
        "{response:?}"
    );
    assert_eq!(
        cursor(&handler, reserved_id).await,
        vec![PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved)]
    );
}

#[tokio::test]
async fn raw_subscription_response_uses_rekeyed_session_after_concurrent_rename() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let renamed_session =
        SessionName::new("pane-stream-renamed").expect("valid renamed session name");
    let source = {
        let state = handler.state.lock().await;
        super::stream_source_for_target(&state, target).expect("stream source")
    };
    let (source, captured) = handler
        .capture_current_stream_source(source)
        .await
        .expect("capture stream source");
    let rebase = super::materialize_raw_rebase(
        &handler,
        source.key.pane_id(),
        1,
        PaneRawRebaseReason::Initial,
        false,
        &captured,
    )
    .expect("materialize raw rebase");
    let reserved_id = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let id = subscriptions
            .registry
            .subscribe(CONNECTION_ID, source.key.clone(), std::time::Instant::now())
            .expect("raw reservation")
            .id();
        subscriptions.streams.insert(
            id,
            super::PaneStreamSubscription::reserved(PaneStreamMode::Raw),
        );
        id
    };

    let rename = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: source.target.session_name().clone(),
            new_name: renamed_session.clone(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameSession(_)), "{rename:?}");

    let response = handler.finish_raw_subscription(
        CONNECTION_ID,
        reserved_id,
        source,
        super::RawSubscriptionStart::captured(captured, rebase, false),
    );
    let Response::SubscribePaneStream(response) = response else {
        panic!("raw subscription should succeed: {response:?}");
    };
    assert_eq!(response.target.session_name(), &renamed_session);
    assert_eq!(
        handler
            .pane_output_subscription_key_for_test(reserved_id)
            .expect("subscription survives rename")
            .runtime_session_name(),
        &renamed_session
    );
}

#[tokio::test]
async fn existing_surface_response_uses_rekeyed_session_after_concurrent_rename() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let _active = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let renamed_session =
        SessionName::new("surface-stream-renamed").expect("valid renamed session name");
    let source = {
        let state = handler.state.lock().await;
        super::stream_source_for_target(&state, target).expect("stream source")
    };
    let reserved_id = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let id = subscriptions
            .registry
            .subscribe(CONNECTION_ID, source.key.clone(), Instant::now())
            .expect("surface reservation")
            .id();
        subscriptions.streams.insert(
            id,
            super::PaneStreamSubscription::reserved(PaneStreamMode::Surface),
        );
        id
    };

    let rename = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: source.target.session_name().clone(),
            new_name: renamed_session.clone(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameSession(_)), "{rename:?}");

    let response = handler.finish_existing_surface_subscription(CONNECTION_ID, reserved_id, source);
    let Response::SubscribePaneStream(response) = response else {
        panic!("surface subscription should succeed: {response:?}");
    };
    assert_eq!(response.target.session_name(), &renamed_session);
    assert_eq!(
        handler
            .pane_output_subscription_key_for_test(reserved_id)
            .expect("subscription survives rename")
            .runtime_session_name(),
        &renamed_session
    );
}

#[tokio::test]
async fn waiting_surface_response_uses_rekeyed_session_after_concurrent_rename() {
    let handler = Arc::new(RequestHandler::new());
    let (target, _, _) = test_pane(handler.as_ref()).await;
    let renamed_session =
        SessionName::new("surface-wait-renamed").expect("valid renamed session name");
    let key = {
        let state = handler.state.lock().await;
        state
            .pane_output_subscription_key_for_target(&target)
            .expect("pane output key")
    };
    let initialization_token = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let super::SurfaceDriverRoute::Initialize { token } =
            subscriptions.surface_driver_route(&key)
        else {
            panic!("test must reserve the surface initializer");
        };
        token
    };

    let task_handler = Arc::clone(&handler);
    let request_target = target.clone();
    let task = tokio::spawn(async move {
        task_handler
            .handle_subscribe_pane_stream(
                CONNECTION_ID,
                SubscribePaneStreamRequest {
                    target: PaneTargetRef::slot(request_target),
                    mode: PaneStreamMode::Surface,
                    include_snapshot: false,
                },
            )
            .await
    });
    for _ in 0..100 {
        if !handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        !handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .is_empty(),
        "surface waiter must reserve its stream before the rename"
    );

    let rename = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: target.session_name().clone(),
            new_name: renamed_session.clone(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameSession(_)), "{rename:?}");
    handler
        .subscriptions
        .lock()
        .expect("subscription lock")
        .finish_surface_initialization(initialization_token);

    let response = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("surface waiter must finish")
        .expect("surface waiter task must not panic");
    let Response::SubscribePaneStream(response) = response else {
        panic!("surface waiter should subscribe: {response:?}");
    };
    assert_eq!(response.target.session_name(), &renamed_session);
}

#[tokio::test]
async fn draining_surface_source_uses_rekeyed_session_after_concurrent_rename() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let renamed_session =
        SessionName::new("surface-drain-renamed").expect("valid renamed session name");
    let source = {
        let state = handler.state.lock().await;
        super::stream_source_for_target(&state, target).expect("stream source")
    };
    let reserved_id = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        subscriptions
            .registry
            .subscribe(CONNECTION_ID, source.key.clone(), Instant::now())
            .expect("surface reservation")
            .id()
    };
    handler.stage_exited_pane_stream_source(source.key.clone(), source.clone());

    let rename = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: source.target.session_name().clone(),
            new_name: renamed_session.clone(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameSession(_)), "{rename:?}");
    let current_key = handler
        .pane_output_subscription_key_for_test(reserved_id)
        .expect("subscription survives rename");
    let draining_source = handler
        .subscriptions
        .lock()
        .expect("subscription lock")
        .draining_stream_source(&current_key)
        .expect("draining source survives rename");
    assert_eq!(draining_source.target.session_name(), &renamed_session);
    assert_eq!(draining_source.key, current_key);
}

#[tokio::test]
async fn late_stream_subscriptions_are_rejected_while_an_exited_pane_drains() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let active = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let source = {
        let state = handler.state.lock().await;
        super::stream_source_for_target(&state, target.clone()).expect("stream source")
    };
    let key = handler
        .pane_output_subscription_key_for_test(active.subscription_id)
        .expect("subscription key");
    let removed_session = handler
        .state
        .lock()
        .await
        .sessions
        .remove_session(target.session_name())
        .expect("remove session before stream drain");
    handler
        .drain_exited_pane_output_subscriptions(key.clone(), Some(source))
        .await;

    for mode in [PaneStreamMode::Raw, PaneStreamMode::Surface] {
        let response = handler
            .handle_subscribe_pane_stream(
                CONNECTION_ID + 1,
                SubscribePaneStreamRequest {
                    target: PaneTargetRef::slot(target.clone()),
                    mode,
                    include_snapshot: false,
                },
            )
            .await;
        assert!(
            matches!(response, Response::Error(_)),
            "{mode:?} late subscription unexpectedly succeeded: {response:?}"
        );
    }

    handler
        .subscriptions
        .lock()
        .expect("subscription lock")
        .expire_pane_drain(&key, std::time::Instant::now());
    handler
        .state
        .lock()
        .await
        .sessions
        .insert_existing_session(removed_session)
        .expect("restore test session");
}

#[tokio::test]
async fn pane_exit_drain_timeout_force_ends_an_undrained_stream() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let source = {
        let state = handler.state.lock().await;
        super::stream_source_for_target(&state, target).expect("stream source")
    };
    let key = handler
        .pane_output_subscription_key_for_test(subscribed.subscription_id)
        .expect("subscription key");
    handler
        .drain_exited_pane_output_subscriptions(key.clone(), Some(source))
        .await;

    handler
        .subscriptions
        .lock()
        .expect("subscription lock")
        .expire_pane_drain(&key, std::time::Instant::now());

    assert_eq!(
        cursor(&handler, subscribed.subscription_id).await,
        vec![PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved)]
    );
}

#[tokio::test]
async fn stale_stream_finishes_with_subscription_expired() {
    let handler = RequestHandler::with_owner_uid_and_subscription_limits(
        crate::server_access::current_owner_uid(),
        SubscriptionLimits::new(8, 8, 32, Duration::from_millis(1)),
    );
    let (target, _, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    tokio::time::sleep(Duration::from_millis(5)).await;

    assert_eq!(
        cursor(&handler, subscribed.subscription_id).await,
        vec![PaneStreamEvent::End(
            PaneStreamEndReason::SubscriptionExpired
        )]
    );
}

#[tokio::test]
async fn configured_connection_limit_preserves_every_grouped_stream_end() {
    const CONFIGURED_LIMIT: usize = 65;
    let handler = RequestHandler::with_owner_uid_and_subscription_limits(
        crate::server_access::current_owner_uid(),
        SubscriptionLimits::new(
            CONFIGURED_LIMIT,
            CONFIGURED_LIMIT,
            32,
            Duration::from_secs(60),
        ),
    );
    let (target, _, _) = test_pane(&handler).await;
    let mut subscription_ids = Vec::with_capacity(CONFIGURED_LIMIT);
    for _ in 0..CONFIGURED_LIMIT {
        subscription_ids.push(
            subscribe(&handler, &target, PaneStreamMode::Raw)
                .await
                .subscription_id,
        );
    }
    let key = handler
        .pane_output_subscription_key_for_test(subscription_ids[0])
        .expect("first stream key");
    handler
        .subscriptions
        .lock()
        .expect("subscription lock")
        .end_pane_streams(
            &key,
            PaneStreamMode::Raw,
            PaneStreamEndReason::PaneRemoved,
            Instant::now(),
        );

    assert_eq!(
        cursor(&handler, subscription_ids[0]).await,
        vec![PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved)]
    );
}

#[tokio::test]
async fn quota_is_reserved_before_a_second_snapshot_capture() {
    let handler = RequestHandler::with_owner_uid_and_subscription_limits(
        crate::server_access::current_owner_uid(),
        SubscriptionLimits::new(1, 1, 32, Duration::from_secs(60)),
    );
    let (target, output, _) = test_pane(&handler).await;
    let _first = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let receiver_count = output.receiver_count_for_test();

    let response = handler
        .handle_subscribe_pane_stream(
            CONNECTION_ID,
            SubscribePaneStreamRequest {
                target: PaneTargetRef::slot(target),
                mode: PaneStreamMode::Raw,
                include_snapshot: false,
            },
        )
        .await;
    assert!(matches!(response, Response::Error(_)), "{response:?}");
    assert_eq!(
        output.receiver_count_for_test(),
        receiver_count,
        "rejected subscribers must not pay the capture/receiver cost"
    );
}

#[tokio::test]
async fn cancelled_surface_waiter_releases_its_reserved_quota() {
    let handler = Arc::new(RequestHandler::new());
    let (target, _, _) = test_pane(handler.as_ref()).await;
    let key = {
        let state = handler.state.lock().await;
        state
            .pane_output_subscription_key_for_target(&target)
            .expect("pane output key")
    };
    let initialization_token = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let super::SurfaceDriverRoute::Initialize { token } =
            subscriptions.surface_driver_route(&key)
        else {
            panic!("test must reserve the surface initializer");
        };
        token
    };

    let task_handler = Arc::clone(&handler);
    let task = tokio::spawn(async move {
        task_handler
            .handle_subscribe_pane_stream(
                CONNECTION_ID,
                SubscribePaneStreamRequest {
                    target: PaneTargetRef::slot(target),
                    mode: PaneStreamMode::Surface,
                    include_snapshot: false,
                },
            )
            .await
    });
    for _ in 0..100 {
        if !handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        !handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .is_empty(),
        "surface waiter must reserve quota before waiting"
    );

    task.abort();
    let _ = task.await;
    assert!(
        handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .is_empty(),
        "cancelling a waiting subscribe must release its quota immediately"
    );
    handler
        .subscriptions
        .lock()
        .expect("subscription lock")
        .finish_surface_initialization(initialization_token);
}

#[tokio::test]
async fn raw_waiter_re_elects_after_initializer_wakeup() {
    let handler = Arc::new(RequestHandler::new());
    let (target, _, _) = test_pane(handler.as_ref()).await;
    let key = {
        let state = handler.state.lock().await;
        state
            .pane_output_subscription_key_for_target(&target)
            .expect("pane output key")
    };
    let initialization_token = {
        let mut subscriptions = handler.subscriptions.lock().expect("subscription lock");
        let super::super::subscription_support::RawInitializationRoute::Initialize { token } =
            subscriptions.raw_initialization_route(&key, false)
        else {
            panic!("test must reserve the raw initializer");
        };
        token
    };

    let task_handler = Arc::clone(&handler);
    let task = tokio::spawn(async move {
        task_handler
            .handle_subscribe_pane_stream(
                CONNECTION_ID,
                SubscribePaneStreamRequest {
                    target: PaneTargetRef::slot(target),
                    mode: PaneStreamMode::Raw,
                    include_snapshot: false,
                },
            )
            .await
    });
    for _ in 0..100 {
        if !handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    handler
        .subscriptions
        .lock()
        .expect("subscription lock")
        .finish_raw_initialization(initialization_token);

    let response = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("raw waiter completes")
        .expect("raw waiter task joins");
    assert!(
        matches!(
            response,
            Response::SubscribePaneStream(ref subscribed)
                if matches!(subscribed.event, PaneStreamEvent::RawRebase(_))
        ),
        "{response:?}"
    );
}

#[tokio::test]
async fn revoked_access_finishes_an_owned_stream_with_a_typed_end() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;

    assert_eq!(
        handler.handle_revoked_pane_stream_cursor(
            CONNECTION_ID,
            PaneStreamCursorRequest {
                subscription_id: subscribed.subscription_id,
                max_events: None,
            },
        ),
        Response::PaneStreamCursor(Box::new(rmux_proto::PaneStreamCursorResponse {
            subscription_id: subscribed.subscription_id,
            events: vec![PaneStreamEvent::End(PaneStreamEndReason::AccessRevoked)],
            limited: false,
        }))
    );
    assert!(
        handler
            .pane_output_subscription_key_for_test(subscribed.subscription_id)
            .is_none(),
        "revocation must release quota immediately"
    );
}

#[tokio::test]
async fn revoked_access_replaces_an_inflight_subscribe_and_releases_quota() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let subscription_id = subscribed.subscription_id;

    let response = handler.revoke_inflight_pane_stream_response(
        CONNECTION_ID,
        Response::SubscribePaneStream(Box::new(subscribed)),
    );
    let Response::SubscribePaneStream(response) = response else {
        panic!("unexpected revoked subscribe response: {response:?}");
    };
    assert_eq!(
        response.event,
        PaneStreamEvent::End(PaneStreamEndReason::AccessRevoked)
    );
    assert!(
        handler
            .pane_output_subscription_key_for_test(subscription_id)
            .is_none(),
        "inflight revocation must release its reserved stream quota"
    );
}

/// Pins the exact opening response a revoked Raw subscribe puts on the wire.
///
/// The SDK checks the answered identity before admitting the opening event, so
/// substituting the terminal reason must keep the response addressed to the
/// pane the caller named. The rmux-sdk recovery test
/// `recovery_admits_a_revoked_access_end_without_exposing_or_releasing_anything`
/// consumes exactly this shape on the client side of the same wire type.
#[tokio::test]
async fn revoked_raw_subscribe_keeps_its_identity_and_only_replaces_the_keyframe() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let subscription_id = subscribed.subscription_id;
    let answered_target = subscribed.target.clone();
    let answered_pane_id = subscribed.pane_id;
    assert!(
        matches!(subscribed.event, PaneStreamEvent::RawRebase(_)),
        "a Raw subscribe opens on a keyframe before revocation: {:?}",
        subscribed.event
    );

    let response = handler.revoke_inflight_pane_stream_response(
        CONNECTION_ID,
        Response::SubscribePaneStream(Box::new(subscribed)),
    );

    let Response::SubscribePaneStream(response) = response else {
        panic!("unexpected revoked subscribe response: {response:?}");
    };
    assert_eq!(response.subscription_id, subscription_id);
    assert_eq!(response.target, answered_target);
    assert_eq!(response.pane_id, answered_pane_id);
    assert_eq!(
        response.event,
        PaneStreamEvent::End(PaneStreamEndReason::AccessRevoked)
    );
    assert!(
        handler
            .pane_output_subscription_key_for_test(subscription_id)
            .is_none(),
        "the revoked stream is released before the client sees its typed end"
    );
}

#[test]
fn raw_rebases_reserve_enough_space_for_the_detached_response_envelope() {
    let rebase = PaneRawRebase {
        epoch: 1,
        generation: 1,
        invalidation_revision: 0,
        next_sequence: 0,
        cols: 80,
        rows: 24,
        keyframe: vec![0; rmux_proto::DEFAULT_MAX_DETACHED_FRAME_LENGTH],
        alternate: false,
        coverage: PaneRecoveryCoverage {
            history_rows_total: 0,
            history_rows_included: 0,
            metadata_complete: true,
        },
        snapshot: None,
        reason: PaneRawRebaseReason::Initial,
    };
    assert!(matches!(
        validate_raw_rebase_size(&rebase),
        Err(rmux_proto::RmuxError::FrameTooLarge { .. })
    ));
}
