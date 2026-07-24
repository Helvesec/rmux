use std::sync::Arc;
use std::time::Duration;

use rmux_core::events::SubscriptionLimits;
use rmux_proto::{
    NewSessionExtRequest, PaneRawRebase, PaneRawRebaseReason, PaneRecoveryCoverage,
    PaneStreamCursorRequest, PaneStreamEndReason, PaneStreamEvent, PaneStreamLifecycleEvent,
    PaneStreamMode, PaneSurfaceFrame, PaneTarget, PaneTargetRef, Request, Response, SessionName,
    SubscribePaneStreamRequest, SubscribePaneStreamResponse, TerminalSize,
    UnsubscribePaneStreamRequest, DEFAULT_MAX_DETACHED_FRAME_LENGTH,
};

use crate::pane_io::{PaneInvalidationReason, PaneOutputSender};
use crate::pane_transcript::SharedPaneTranscript;

use super::{validate_raw_rebase_size, RequestHandler};

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

async fn subscribe(
    handler: &RequestHandler,
    target: &PaneTarget,
    mode: PaneStreamMode,
) -> SubscribePaneStreamResponse {
    let response = handler
        .handle_subscribe_pane_stream(
            CONNECTION_ID,
            SubscribePaneStreamRequest {
                target: PaneTargetRef::slot(target.clone()),
                mode,
                include_snapshot: false,
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
            super::PaneStreamSubscription::Reserved(PaneStreamMode::Surface),
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

#[tokio::test]
async fn pane_removal_finishes_stream_with_typed_end() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let subscribed = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let key = handler
        .pane_output_subscription_key_for_test(subscribed.subscription_id)
        .expect("subscription key");

    handler.cleanup_pane_output_subscriptions(&[key]).await;
    assert_eq!(
        cursor(&handler, subscribed.subscription_id).await,
        vec![PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved)]
    );
}

#[tokio::test]
async fn natural_pane_exit_finishes_raw_and_surface_streams() {
    let handler = RequestHandler::new();
    let (target, _, _) = test_pane(&handler).await;
    let raw = subscribe(&handler, &target, PaneStreamMode::Raw).await;
    let surface = subscribe(&handler, &target, PaneStreamMode::Surface).await;
    let key = handler
        .pane_output_subscription_key_for_test(raw.subscription_id)
        .expect("subscription key");

    handler.drain_exited_pane_output_subscriptions(key).await;

    for subscription_id in [raw.subscription_id, surface.subscription_id] {
        assert_eq!(
            cursor(&handler, subscription_id).await,
            vec![PaneStreamEvent::End(PaneStreamEndReason::PaneRemoved)]
        );
    }
    assert!(
        handler
            .subscriptions
            .lock()
            .expect("subscription lock")
            .is_empty(),
        "ended pane streams must not keep exit-empty shutdown busy"
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
