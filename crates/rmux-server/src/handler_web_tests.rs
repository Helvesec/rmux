#[path = "handler_web_destroy_rehome_tests.rs"]
mod destroy_rehome;
#[path = "handler_web_initial_render_tests.rs"]
mod initial_render;
#[path = "handler_web_scene_test_support.rs"]
mod scene_fixture;
#[path = "handler_web_slow_reader_tests.rs"]
mod slow_reader;
use super::*;
use crate::daemon::ShutdownHandle;
#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use rmux_core::events::SubscriptionLimits;
use rmux_proto::WebShareCreatedResponse;
use rmux_proto::{encode_attach_message, AttachMessage};
use rmux_proto::{
    CopyModeRequest, CreateWebShareRequest, HookLifecycle, HookName, KillPaneRequest,
    KillSessionRequest, LinkWindowRequest, ListWebSharesRequest, NewSessionRequest, OptionName,
    PaneTarget, RenameSessionRequest, Request, Response, ScopeSelector, SessionName,
    SetHookRequest, SetOptionMode, SetOptionRequest, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, StopWebShareRequest, TerminalSize, WebShareScope, WindowTarget,
};
use tokio::io::AsyncWriteExt;
use tokio::time::{sleep, timeout, Duration, Instant};

#[tokio::test]
async fn web_backspace_and_native_attach_deliver_the_same_erase_byte() {
    let handler = RequestHandler::new();
    let session = new_session(&handler, "web-backspace-erase").await;
    let target = PaneTarget::new(session.clone(), 0);
    let native_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(native_pid, session, control_tx)
        .await;
    for (erase, option) in [(0x7f, "^?"), (0x08, "C-h")] {
        {
            let mut state = handler.state.lock().await;
            state
                .options
                .set(
                    ScopeSelector::Global,
                    OptionName::Backspace,
                    option.to_owned(),
                    SetOptionMode::Replace,
                )
                .expect("configure erase byte");
            state.start_pane_input_capture_for_test(&target);
        }
        handler
            .web_send_text(&target.clone().into(), "abc".to_owned())
            .await
            .unwrap();
        handler
            .web_send_text(&target.clone().into(), "\x7f".to_owned())
            .await
            .unwrap();
        handler
            .handle_attached_live_input_for_test(native_pid, b"\x7f")
            .await
            .unwrap();
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&target),
            Some(vec![b'a', b'b', b'c', erase, erase])
        );
    }
}

#[tokio::test]
async fn web_scene_render_allows_independent_session_lookup() {
    use crate::web::{OutboundQueueResult, WebSocketOutbound, WEB_OUTBOUND_BYTES_MAX};
    let (slow, _slow_reader) = WebSocketOutbound::stalled_for_test();
    let (healthy, _healthy_reader) = WebSocketOutbound::stalled_for_test();
    assert_eq!(
        slow.queue_frame(vec![0; WEB_OUTBOUND_BYTES_MAX]),
        OutboundQueueResult::Queued
    );
    assert_eq!(slow.queue_frame(vec![0]), OutboundQueueResult::Backpressure);
    assert_eq!(
        healthy.queue_frame(b"independent".to_vec()),
        OutboundQueueResult::Queued
    );
    let handler = RequestHandler::new();
    let session = new_session(&handler, "web-scene-lock").await;
    let independent = new_session(&handler, "web-independent").await;
    let target = {
        let state = handler.state.lock().await;
        WebSessionTarget::new(
            session.clone(),
            state.sessions.session(&session).unwrap().id(),
        )
    };
    let state = handler.state.clone();
    let observed = std::rc::Rc::new(std::cell::Cell::new(false));
    let observed_render = observed.clone();
    WebSceneProjection::observe_next_render(move || {
        let state = state.try_lock().expect(
            "Web scene rendering must leave the global state available to independent operations",
        );
        assert!(state.sessions.session(&independent).is_some());
        observed_render.set(true);
    });
    handler
        .web_session_snapshot(&target)
        .await
        .expect("session snapshot");
    assert!(observed.get(), "the real Web scene renderer was exercised");
    assert_eq!(slow.queue_frame(vec![0]), OutboundQueueResult::Backpressure);
    assert_eq!(
        healthy.queue_snapshot(b"scene".to_vec()),
        OutboundQueueResult::Queued
    );
}

#[tokio::test]
async fn web_resnapshot_recovers_poisoned_transcript() {
    let handler = RequestHandler::new();
    // The live pane reader publishes whatever the child prints. A shell prompt
    // landing before the capture would move the boundary this test pins.
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultCommand,
            value: crate::test_shell::stdin_discard_command(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    let session = new_session(&handler, "web-poison-policy").await;
    let target = PaneTarget::new(session.clone(), 0);
    // The quiet child does not silence the ConPTY startup preamble; it must
    // land before the boundary read below, not between it and the capture.
    handler.wait_for_pane_startup_output_for_test(&target).await;
    let (output, transcript) = {
        let state = handler.state.lock().await;
        (
            state.pane_output_for_target(&session, 0, 0).unwrap(),
            state.transcript_handle(&target).unwrap(),
        )
    };
    let (published_from, _) = output.subscribe_live_from_now();
    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"WEB_CAPTURE".to_vec());
    let expected = transcript
        .lock()
        .capture_main(Default::default(), Default::default());
    transcript.poison_for_test();
    let (snapshot, mut receiver) = handler
        .web_resnapshot(&target.into())
        .await
        .expect("Web recovers at the output boundary");
    let mut terminal = rmux_core::TerminalScreen::new(
        TerminalSize {
            cols: snapshot.cols,
            rows: snapshot.rows,
        },
        64,
    );
    terminal.feed(
        snapshot
            .recovery_keyframe
            .as_deref()
            .expect("recovery keyframe"),
    );
    assert_eq!(
        terminal
            .screen()
            .capture_transcript(Default::default(), Default::default()),
        expected
    );
    assert_eq!(snapshot.output_sequence, published_from + 1);
    assert!(receiver.try_recv_observed().is_none());
    assert!(std::panic::catch_unwind(|| drop(transcript.lock())).is_err());
}

#[tokio::test]
async fn shutdown_rejects_web_pane_text_key_and_session_mutations() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-shutdown-rejected").await;
    let session_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .expect("session exists")
            .id()
    };
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(
            PaneTarget::new(session_name.clone(), 0).into(),
        )),
    )
    .await;
    let open_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let pane_target = PaneTargetRef::by_id(session_name.clone(), PaneId::new(1));
    let session_target = crate::web::WebSessionTarget::new(session_name, session_id);
    let requester_pid = std::process::id();
    handler.close_normal_request_admission();

    let mut results = Vec::new();
    results.push(handler.open_web_share(&open_token, None).await.map(drop));
    results.push(
        handler
            .web_send_text(&pane_target, "blocked-text".to_owned())
            .await,
    );
    results.push(handler.web_send_key(&pane_target, "Enter".to_owned()).await);
    results.push(
        handler
            .web_session_logout(&session_target, requester_pid)
            .await,
    );
    results.push(
        handler
            .web_session_select_pane(&session_target, requester_pid, PaneId::new(1))
            .await,
    );
    results.push(
        handler
            .web_session_resize_pane(
                &session_target,
                requester_pid,
                PaneId::new(1),
                rmux_proto::ResizePaneAdjustment::NoOp,
            )
            .await,
    );
    results.push(
        handler
            .web_session_split_pane(&session_target, requester_pid, SplitDirection::Horizontal)
            .await,
    );
    results.push(
        handler
            .web_session_new_window(&session_target, requester_pid)
            .await,
    );
    results.push(
        handler
            .web_session_kill_active_pane(&session_target, requester_pid)
            .await,
    );
    results.push(
        handler
            .web_session_select_window(&session_target, requester_pid, 0)
            .await,
    );
    results.push(
        handler
            .web_session_select_window_for_view(&session_target, requester_pid, 0)
            .await
            .map(drop),
    );
    results.push(
        handler
            .web_session_rename_window(&session_target, requester_pid, 0, "blocked-name".to_owned())
            .await,
    );
    results.push(
        handler
            .web_session_kill_window(&session_target, requester_pid, 0)
            .await,
    );

    assert_eq!(
        results.len(),
        13,
        "all Web mutation entry points are covered"
    );
    for result in results {
        let error = result.expect_err("Web mutation must be rejected after quiesce closes");
        assert!(
            error.to_string().contains("server is shutting down"),
            "unexpected rejection: {error}"
        );
    }

    assert!(handler.normal_drain_requests_quiesced());
}

#[tokio::test]
async fn shutdown_drains_a_web_session_mutation_admitted_before_close() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-shutdown-drain").await;
    let session_target = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        crate::web::WebSessionTarget::new(session_name.clone(), session.id())
    };

    // Hold the first state lock needed by the operation. Admission happens
    // before that lock, making the close-vs-mutation ordering deterministic.
    let state = handler.state.lock().await;
    let mutation_handler = handler.clone();
    let mutation = tokio::spawn(async move {
        mutation_handler
            .web_session_new_window(&session_target, std::process::id())
            .await
    });
    timeout(Duration::from_secs(1), async {
        while handler.normal_drain_requests_quiesced() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Web mutation acquires Drain admission before its first state lock");

    handler.close_normal_request_admission();
    assert!(
        !handler.normal_drain_requests_quiesced(),
        "an admitted Web mutation must retain the shutdown barrier"
    );
    drop(state);

    mutation
        .await
        .expect("Web mutation task joins")
        .expect("the already-admitted Web mutation completes");
    timeout(Duration::from_secs(1), async {
        while !handler.normal_drain_requests_quiesced() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed Web mutation releases the Drain barrier");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&session_name)
            .expect("session survives")
            .windows()
            .count(),
        2,
        "the admitted session mutation commits before shutdown can seal"
    );
}

#[tokio::test]
async fn web_new_window_uses_shared_initial_name_primitive_when_automatic_rename_is_off() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-initial-window-name").await;
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::AutomaticRename,
                value: "off".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    let session_target = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        crate::web::WebSessionTarget::new(session_name.clone(), session.id())
    };

    handler
        .web_session_new_window(&session_target, std::process::id())
        .await
        .expect("Web new-window succeeds");

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&session_name)
        .and_then(|session| session.window_at(1))
        .expect("Web created a second window");
    let runtime_name = state
        .pane_runtime_window_name_in_window(&session_name, 1, 0)
        .expect("Web pane has runtime state")
        .expect("Web pane has a useful runtime name");
    assert_eq!(window.name(), Some(runtime_name.as_str()));
}

#[tokio::test]
async fn web_share_create_starts_lazy_listener() {
    let handler = handler_with_automatic_web_port();
    let session_name = new_session(&handler, "lazy-start").await;

    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Create(
            share_request(WebShareScope::Session(session_name)),
        ))))
        .await;

    assert!(matches!(
        response,
        Response::WebShare(response)
            if matches!(response.as_ref(), rmux_proto::WebShareResponse::Created(_))
    ));
}

#[tokio::test]
async fn web_share_config_starts_lazy_listener() {
    let handler = handler_with_automatic_web_port();

    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Config(
            rmux_proto::WebShareConfigRequest,
        ))))
        .await;

    let Response::WebShare(response) = response else {
        panic!("expected web-share config response");
    };
    let rmux_proto::WebShareResponse::Config(config) = *response else {
        panic!("expected web-share config response");
    };
    assert_eq!(config.listener, handler.web_settings().listener());
}

#[tokio::test]
async fn implicit_web_share_port_falls_back_when_default_is_busy() {
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind blocker");
    let blocked_port = blocker.local_addr().expect("blocker addr").port();
    let handler = handler_with_web_settings(
        crate::web::WebShareSettings::from_options_with_port_explicit(blocked_port, None, false)
            .expect("web settings"),
    );

    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Config(
            rmux_proto::WebShareConfigRequest,
        ))))
        .await;

    let Response::WebShare(response) = response else {
        panic!("expected web-share config response");
    };
    let rmux_proto::WebShareResponse::Config(config) = *response else {
        panic!("expected web-share config response");
    };
    assert_eq!(config.listener.host, "127.0.0.1");
    assert_ne!(config.listener.port, blocked_port);
}

#[tokio::test]
async fn concurrent_web_share_create_waits_for_lazy_listener_start() {
    let handler = handler_with_automatic_web_port();
    let alpha = new_session(&handler, "lazy-alpha").await;
    let beta = new_session(&handler, "lazy-beta").await;

    let left_handler = handler.clone();
    let right_handler = handler.clone();
    let (left, right) = tokio::join!(
        left_handler.handle(Request::WebShare(Box::new(WebShareRequest::Create(
            share_request(WebShareScope::Session(alpha),)
        )))),
        right_handler.handle(Request::WebShare(Box::new(WebShareRequest::Create(
            share_request(WebShareScope::Session(beta),)
        )))),
    );

    assert!(matches!(
        left,
        Response::WebShare(response)
            if matches!(response.as_ref(), rmux_proto::WebShareResponse::Created(_))
    ));
    assert!(matches!(
        right,
        Response::WebShare(response)
            if matches!(response.as_ref(), rmux_proto::WebShareResponse::Created(_))
    ));
    assert_eq!(list_shares(&handler).await.len(), 2);
}

#[tokio::test]
async fn failed_lazy_listener_start_does_not_create_share() {
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind blocker");
    let port = blocker.local_addr().expect("blocker addr").port();
    let handler = handler_with_web_port(port);
    let session_name = new_session(&handler, "lazy-bind-failure").await;

    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Create(
            share_request(WebShareScope::Session(session_name)),
        ))))
        .await;

    let Response::Error(error) = response else {
        panic!("expected listener startup failure");
    };
    assert!(error.error.to_string().contains("listener unavailable"));
    assert!(list_shares(&handler).await.is_empty());

    drop(blocker);
}

#[tokio::test]
async fn web_share_create_resolves_slot_target_to_stable_pane_id() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "alpha").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(
            rmux_proto::PaneTarget::new(session_name.clone(), 0).into(),
        )),
    )
    .await;
    assert!(matches!(
        created.scope,
        WebShareScope::Pane(PaneTargetRef::Id {
            session_name: ref actual,
            ..
        }) if actual == &session_name
    ));
    assert!(created
        .spectator_url
        .as_deref()
        .expect("spectator URL")
        .contains("#e=wss://share.example/share&t="));
}

#[tokio::test]
async fn stopped_ttl_share_wakes_its_expiry_waiter() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "ttl-stop-wakeup").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            ttl_seconds: Some(7 * 24 * 60 * 60),
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let mut revoke_rx = handler
        .web_shares
        .expiry_revoke_receiver(&created.share_id)
        .expect("TTL share has an expiry cancellation receiver");

    let stopped = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Stop(
            StopWebShareRequest {
                share_id: created.share_id,
            },
        ))))
        .await;
    assert!(matches!(
        stopped,
        Response::WebShare(response)
            if matches!(response.as_ref(), rmux_proto::WebShareResponse::Stopped(stopped) if stopped.stopped)
    ));
    timeout(Duration::from_millis(100), revoke_rx.changed())
        .await
        .expect("stopping the share must wake the seven-day expiry waiter")
        .expect("revoke sender remains valid through notification");
    assert_eq!(
        *revoke_rx.borrow(),
        Some(crate::web::WebShareRevokeReason::StoppedByOwner)
    );
}

#[tokio::test]
async fn tunnel_completion_revalidation_rejects_a_removed_target() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-stale-target").await;
    let resolved = handler
        .resolve_create_web_share(share_request(WebShareScope::Pane(
            PaneTarget::new(session_name.clone(), 0).into(),
        )))
        .await
        .expect("initial target resolves");

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)));

    let state = handler.state.lock().await;
    assert!(validate_resolved_web_target(&state, resolved.target()).is_err());
    drop(state);
    assert!(list_shares(&handler).await.is_empty());
}

#[tokio::test]
async fn web_session_share_drains_initial_attach_output() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            spectator: true,
            controls: true,
            ..share_request(WebShareScope::Session(session_name))
        },
    )
    .await;
    let operator_url = created.operator_url.as_deref().expect("operator URL");
    let operator_token = token_from_url(operator_url);
    let stream = handler
        .open_web_share(&operator_token, None)
        .await
        .expect("session web share opens");
    let WebShareStream::Session(mut session_stream) = stream else {
        panic!("expected session web share stream");
    };
    let mut reader = session_stream.take_attach_reader();
    let event = timeout(Duration::from_secs(2), reader.read_event())
        .await
        .expect("attach stream should produce initial output")
        .expect("attach read succeeds")
        .expect("initial attach output is present");
    assert!(matches!(event, WebSessionAttachEvent::Data(_)));
    assert_eq!(session_stream.snapshot.size.cols, 80);
    assert_eq!(session_stream.snapshot.size.rows, 24);
}

#[tokio::test]
async fn web_pane_stream_resnapshots_instead_of_forwarding_cross_boundary_rep() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-pane-rep").await;
    let target = PaneTarget::new(session_name.clone(), 0);
    let (output, transcript) = {
        let state = handler.state.lock().await;
        (
            state
                .pane_output_for_target(&session_name, 0, 0)
                .expect("pane output"),
            state.transcript_handle(&target).expect("pane transcript"),
        )
    };
    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"X".to_vec());
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(target.clone().into())),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let stream = handler
        .open_web_share(&token, None)
        .await
        .expect("pane web share opens");
    let WebShareStream::Pane(mut pane) = stream else {
        panic!("expected pane web share stream");
    };

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"\x1b[2b".to_vec());

    assert!(matches!(
        pane.output.try_recv_observed(),
        Some(crate::pane_io::PaneObservationItem::Invalidated(invalidation))
            if invalidation.reason
                == crate::pane_io::PaneInvalidationReason::TranscriptMutation
    ));
    assert!(
        pane.output.try_recv_observed().is_none(),
        "the browser must not receive REP without parser INPUT_LAST"
    );
    let (snapshot, _) = handler
        .web_resnapshot(&pane.target)
        .await
        .expect("post-REP browser snapshot");
    let mut recovered = rmux_core::TerminalScreen::new(
        TerminalSize {
            cols: snapshot.cols,
            rows: snapshot.rows,
        },
        2_000,
    );
    recovered.feed(
        snapshot
            .recovery_keyframe
            .as_deref()
            .expect("pane resnapshot includes recovery keyframe"),
    );
    assert_eq!(
        recovered.screen().capture_transcript(
            rmux_core::ScreenCaptureRange::default(),
            rmux_core::GridRenderOptions::default(),
        ),
        transcript.lock().capture_main(
            rmux_core::ScreenCaptureRange::default(),
            rmux_core::GridRenderOptions::default(),
        )
    );
}

/// A pane-scoped WebShare advertises a read-only *view* of the pane and has no
/// scrollback protocol at all (`PaneScroll` closes with `scroll_requires_session`).
/// The recovery keyframe must therefore replay the visible viewport only: rows
/// that already scrolled out of the pane before the link was handed out must
/// never reach a viewer's browser scrollback.
#[tokio::test]
async fn web_pane_snapshot_never_replays_scrolled_off_history() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-pane-scrollback").await;
    let target = PaneTarget::new(session_name.clone(), 0);
    let (output, transcript) = {
        let state = handler.state.lock().await;
        (
            state
                .pane_output_for_target(&session_name, 0, 0)
                .expect("pane output"),
            state.transcript_handle(&target).expect("pane transcript"),
        )
    };
    // Secret scrolls out of the 24-row viewport, then `clear` blanks the screen.
    let mut bytes = b"AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI\r\n".to_vec();
    for line in 0..60_u32 {
        bytes.extend_from_slice(format!("filler {line}\r\n").as_bytes());
    }
    bytes.extend_from_slice(b"\x1b[H\x1b[2J");
    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, bytes);

    let pane_target = PaneTargetRef::from(target);
    let (snapshot, _) = handler
        .web_resnapshot(&pane_target)
        .await
        .expect("web pane resnapshot");
    let keyframe = snapshot
        .recovery_keyframe
        .as_deref()
        .expect("pane resnapshot includes recovery keyframe");

    assert!(
        snapshot.history_rows_total > 0,
        "the pane must actually hold scrollback for this probe to mean anything"
    );
    assert!(
        !contains_subslice(keyframe, b"AWS_SECRET_ACCESS_KEY"),
        "scrolled-off scrollback leaked into the WebShare pane snapshot"
    );
    assert!(
        !contains_subslice(keyframe, b"filler 0"),
        "scrolled-off scrollback leaked into the WebShare pane snapshot"
    );
    assert_eq!(
        snapshot.history_rows_included, 0,
        "a web pane snapshot must report zero replayed history rows"
    );
}

#[tokio::test]
async fn web_session_attach_renders_rep_from_authoritative_screen_state() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-session-rep").await;
    let target = PaneTarget::new(session_name.clone(), 0);
    let (output, transcript) = {
        let state = handler.state.lock().await;
        (
            state
                .pane_output_for_target(&session_name, 0, 0)
                .expect("pane output"),
            state.transcript_handle(&target).expect("pane transcript"),
        )
    };
    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"X".to_vec());
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name)),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let stream = handler
        .open_web_share(&token, None)
        .await
        .expect("session web share opens");
    let WebShareStream::Session(mut session) = stream else {
        panic!("expected session web share stream");
    };
    let mut reader = session.take_attach_reader();
    let _ = timeout(Duration::from_secs(2), reader.read_event())
        .await
        .expect("initial attach frame")
        .expect("attach read")
        .expect("initial data");

    crate::pane_io::publish_pane_bytes_for_test(&transcript, &output, b"\x1b[2b".to_vec());
    let event = timeout(Duration::from_secs(2), reader.read_event())
        .await
        .expect("post-REP attach render")
        .expect("attach read")
        .expect("post-REP data");
    let WebSessionAttachEvent::Data(frame) = event else {
        panic!("REP changes the rendered session surface");
    };
    assert!(
        !frame.windows(b"\x1b[2b".len()).any(|window| window == b"\x1b[2b"),
        "session Web clients start from a rendered keyframe, so REP must be rendered from the authoritative screen instead of forwarded raw"
    );
}

#[tokio::test]
async fn web_session_last_exit_drains_before_daemon_shutdown() {
    let handler = RequestHandler::new();
    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let session_name = new_session(&handler, "websession-exit-drain").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let stream = handler
        .open_web_share(
            &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
            None,
        )
        .await
        .expect("session web share opens");
    let WebShareStream::Session(mut session_stream) = stream else {
        panic!("expected session web share stream");
    };
    let attach_pid = session_stream.attach_pid();
    let control_tx = handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&attach_pid)
        .expect("web session attach is registered")
        .control_tx
        .clone();
    let mut reader = session_stream.take_attach_reader();
    let _ = timeout(Duration::from_secs(2), reader.read_event())
        .await
        .expect("attach stream should produce initial output")
        .expect("attach read succeeds");
    control_tx
        .send(AttachControl::Write(vec![b'x'; 128 * 1024]))
        .expect("fill the bounded in-process attach transport");
    tokio::task::yield_now().await;

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    assert!(
        !handler.request_shutdown_if_pending(),
        "web attach wire drain must defer exit-empty shutdown"
    );
    assert!(
        timeout(Duration::from_millis(25), &mut shutdown_rx)
            .await
            .is_err(),
        "daemon shutdown must stay pending until the browser receives its exit frame"
    );

    let exited = timeout(Duration::from_secs(2), async {
        loop {
            match reader.read_event().await.expect("attach read succeeds") {
                Some(WebSessionAttachEvent::Data(bytes))
                    if bytes
                        .windows(b"[exited]\r\n".len())
                        .any(|window| window == b"[exited]\r\n") =>
                {
                    break;
                }
                Some(_) => continue,
                None => panic!("web attach closed before its exit frame"),
            }
        }
    })
    .await;
    assert!(exited.is_ok(), "web attach exit frame should drain");

    timeout(Duration::from_millis(500), shutdown_rx)
        .await
        .expect("daemon should shut down after the web attach drains")
        .expect("shutdown receiver should complete cleanly");
}

#[tokio::test]
async fn web_session_attach_reader_emits_resize_events() {
    let (mut writer, reader) = tokio::io::duplex(128);
    let (reader, _) = tokio::io::split(reader);
    let mut reader = WebSessionAttachReader::new(reader);
    let frame = encode_attach_message(&AttachMessage::Resize(TerminalSize {
        cols: 100,
        rows: 30,
    }))
    .expect("resize attach message encodes");

    writer.write_all(&frame).await.expect("write attach frame");

    let event = timeout(Duration::from_secs(2), reader.read_event())
        .await
        .expect("attach reader should observe resize")
        .expect("attach read succeeds")
        .expect("resize event is present");
    assert!(matches!(event, WebSessionAttachEvent::Resize));
}

#[tokio::test]
async fn web_session_operator_registers_writable_attach() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-write").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));
    let stream = handler
        .open_web_share(&operator_token, None)
        .await
        .expect("session web share opens");
    let WebShareStream::Session(session_stream) = stream else {
        panic!("expected session web share stream");
    };
    assert!(session_stream.is_operator());
    assert!(session_stream.controls());

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .values()
        .find(|active| active.session_name == session_name)
        .expect("web session attach is registered");
    assert!(active.can_write);
    assert!(!active.flags.contains(ClientFlags::READONLY));
}

#[tokio::test]
async fn web_session_operator_without_browser_size_keeps_status_aware_geometry() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-sizeless-operator").await;
    seed_two_line_status_geometry(&handler, &session_name, 81_001).await;

    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));
    let stream = handler
        .open_web_share(&operator_token, None)
        .await
        .expect("session web share opens");
    let WebShareStream::Session(_session_stream) = stream else {
        panic!("expected session web share stream");
    };

    // A browser that never reported its size registers without one. Repeated
    // status changes must still take the status rows off the outer terminal
    // anchor exactly once, never off the already-subtracted content rows.
    for value in ["off", "2", "off", "2"] {
        set_session_status(&handler, &session_name, value).await;
    }
    assert_eq!(
        session_window_size(&handler, &session_name).await,
        TerminalSize { cols: 80, rows: 22 },
        "a sizeless WebShare operator must not shrink the session on every status change"
    );
}

#[tokio::test]
async fn web_session_spectator_share_attach_ignores_browser_size() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-read-size").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let stream = handler
        .open_web_share(
            &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
            None,
        )
        .await
        .expect("session web share opens");
    let WebShareStream::Session(session_stream) = stream else {
        panic!("expected session web share stream");
    };
    assert!(!session_stream.is_operator());

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .values()
        .find(|active| active.session_name == session_name)
        .expect("web session attach is registered");
    assert!(!active.can_write);
    assert!(active.flags.contains(ClientFlags::READONLY));
    assert!(active.flags.contains(ClientFlags::IGNORESIZE));
}

#[tokio::test]
async fn web_session_snapshot_tracks_canonical_session_size() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-snapshot-size").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let stream = handler
        .open_web_share(
            &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
            None,
        )
        .await
        .expect("session web share opens");
    let WebShareStream::Session(session_stream) = stream else {
        panic!("expected session web share stream");
    };
    assert_eq!(
        session_stream.snapshot.size,
        TerminalSize { cols: 80, rows: 24 }
    );

    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .session_setup(&session_name)
            .expect("session exists")
            .resize_terminal(TerminalSize { cols: 60, rows: 10 });
    }

    let snapshot = handler
        .web_session_snapshot(session_stream.target())
        .await
        .expect("session snapshot refreshes");
    assert_eq!(snapshot.size, TerminalSize { cols: 60, rows: 10 });
}

#[tokio::test]
async fn web_session_snapshot_uses_content_geometry_without_reapplying_status_rows() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-content-geometry").await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            session_name.clone(),
            0,
            0,
        ))
        .await;
    {
        let mut state = handler.state.lock().await;
        state
            .mutate_session_and_resize_window_terminal(&session_name, 0, |session| {
                session.resize_active_window_geometry(
                    TerminalSize { cols: 80, rows: 24 },
                    TerminalSize { cols: 80, rows: 21 },
                );
                Ok(())
            })
            .expect("content geometry resize succeeds");
    }
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let bottom_target = PaneTarget::with_window(session_name.clone(), 0, 1);
    handler
        .wait_for_pane_startup_to_finish_for_test(&bottom_target)
        .await;
    let (session_id, bottom_pane_id, transcript) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        let pane = session
            .window()
            .pane(1)
            .expect("split creates a bottom pane");
        (
            session.id(),
            pane.id(),
            state
                .transcript_handle(&bottom_target)
                .expect("bottom pane transcript"),
        )
    };
    transcript.lock().append_bytes(
        b"00\r\n01\r\n02\r\n03\r\n04\r\n05\r\n06\r\n07\r\n08\r\n09\r\n\
              10\r\n11\r\n12\r\n13\r\n14\r\n15\r\n16\r\n17\r\n18\r\n19\r\n\
              20\r\n21\r\n22\r\n23\r\n24\r\n25\r\n26\r\n27\r\n28\r\n29\r\n",
    );
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(session_name.clone()),
                option: OptionName::Status,
                value: "3".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let stream = handler
        .open_web_share(
            &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
            None,
        )
        .await
        .expect("session web share opens");
    let WebShareStream::Session(session_stream) = stream else {
        panic!("expected session web share stream");
    };

    assert_eq!(
        session_stream.snapshot.size,
        TerminalSize { cols: 80, rows: 21 }
    );
    assert_eq!(
        session_stream.snapshot.view.size,
        session_stream.snapshot.size
    );
    assert_eq!(
        session_stream
            .snapshot
            .view
            .panes
            .iter()
            .map(|pane| pane.rows)
            .collect::<Vec<_>>(),
        vec![10, 10]
    );

    let bottom = session_stream
        .snapshot
        .view
        .panes
        .iter()
        .find(|pane| pane.id == bottom_pane_id.as_u32())
        .expect("bottom pane remains visible");
    let scroll_frame = handler
        .web_session_pane_scroll_frame(
            &crate::web::WebSessionTarget::new(session_name, session_id),
            bottom_pane_id,
            0,
            None,
        )
        .await
        .expect("Web pane scroll frame succeeds")
        .expect("scrollback produces a pane frame");
    assert_eq!(
        (scroll_frame.pane.x, scroll_frame.pane.y),
        (bottom.x, bottom.y)
    );
    assert_eq!(
        (scroll_frame.pane.cols, scroll_frame.pane.rows),
        (bottom.cols, bottom.rows)
    );
    assert!(
        session_stream
            .snapshot
            .accepts_rectangular_patch(&scroll_frame),
        "a stable scene without floats keeps the pane-frame fast path"
    );
}

#[tokio::test]
async fn web_session_scrollback_preserves_frontmost_signed_floating_scene() {
    let handler = RequestHandler::new();
    scene_fixture::pin_ready_producer(&handler).await;
    let session_name = new_session_with_size(
        &handler,
        "websession-floating-scene",
        TerminalSize { cols: 20, rows: 10 },
    )
    .await;
    set_session_status(&handler, &session_name, "off").await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            session_name.clone(),
            0,
            0,
        ))
        .await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            session_name.clone(),
            0,
            1,
        ))
        .await;

    // Native startup output must be consumed before this fixture owns the
    // scene contents. Input startup completion alone does not establish that.
    scene_fixture::settle_ready_producers(&handler, &session_name).await;

    let (session_id, tiled_id, floating_id) = {
        let mut state = handler.state.lock().await;
        let target = WindowTarget::with_window(session_name.clone(), 0);
        state
            .sessions
            .window_layout_context_mut(&target)
            .expect("window layout context")
            .float_pane(
                1,
                rmux_core::FloatingPaneRect::new(-3, 2, 8, 4).expect("floating rectangle"),
            )
            .expect("pane becomes floating");
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        let tiled_id = session.window().pane(0).expect("tiled pane").id();
        let floating_id = session.window().pane(1).expect("floating pane").id();
        let session_id = session.id();

        let mut tiled = b"\x1b[2J\x1b[H".to_vec();
        for _ in 0..20 {
            tiled.extend_from_slice(b"BBBBBBBBBBBBBBBBBBBB\r\n");
        }
        state
            .append_bytes_to_pane_transcript_for_test(&session_name, 0, 0, &tiled)
            .expect("tiled scrollback");
        state
            .append_bytes_to_pane_transcript_for_test(
                &session_name,
                0,
                1,
                b"\x1b[2J\x1b[H01234567\r\n01234567\r\n01234567\r\n01234567",
            )
            .expect("floating contents");
        (session_id, tiled_id, floating_id)
    };
    let target = crate::web::WebSessionTarget::new(session_name.clone(), session_id);
    let snapshot = handler
        .web_session_snapshot_with_scrolls(&target, None, &HashMap::from([(tiled_id, 0)]))
        .await
        .expect("floating Web snapshot");

    let floating = snapshot
        .view
        .panes
        .iter()
        .find(|pane| pane.id == floating_id.as_u32())
        .expect("floating metadata");
    assert_eq!(
        (floating.x, floating.y, floating.cols, floating.rows),
        (0, 2, 5, 4),
        "Web metadata must describe the scene-visible floating surface"
    );

    let mut rendered = rmux_core::TerminalScreen::new(snapshot.size, 0);
    rendered.feed(&snapshot.ansi_bytes());
    let mut cell = None;
    let mut column = 0_usize;
    assert!(rendered.screen().visit_visible_line_cells(2, 1, |value| {
        if column == 0 {
            cell = Some(value.text().to_owned());
        }
        column += 1;
    }));
    assert_eq!(
        cell.as_deref(),
        Some("3"),
        "scrolling the rear pane must not repaint the signed front pane"
    );

    let state = handler.state.lock().await;
    let mouse =
        crate::mouse::layout_for_session(&state, &session_name, 1).expect("mouse scene projection");
    let front = mouse
        .panes
        .iter()
        .max_by_key(|pane| pane.layer)
        .expect("frontmost mouse pane");
    assert_eq!(front.pane_id, floating_id);
    assert_eq!(front.coordinates().relative_position(0, 2), Some((3, 0)));
    drop(state);

    assert!(
        handler
            .web_session_pane_scroll_frame(&target, tiled_id, 0, None)
            .await
            .expect("scroll patch decision")
            .is_none(),
        "floating scenes must use the complete session keyframe"
    );

    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .window_layout_context_mut(&WindowTarget::with_window(session_name.clone(), 0))
            .expect("window layout context")
            .move_floating_pane(1, 4, 1)
            .expect("floating translation");
    }
    let moved = handler
        .web_session_snapshot_with_scrolls(&target, None, &HashMap::from([(tiled_id, 0)]))
        .await
        .expect("translated floating Web snapshot");
    assert!(
        snapshot.requires_keyframe_for(&moved),
        "translation must select a complete keyframe"
    );
    let moved_float = moved
        .view
        .panes
        .iter()
        .find(|pane| pane.id == floating_id.as_u32())
        .expect("translated floating metadata");
    assert_eq!(
        (
            moved_float.x,
            moved_float.y,
            moved_float.cols,
            moved_float.rows,
        ),
        (4, 1, 8, 4)
    );

    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .window_layout_context_mut(&WindowTarget::with_window(session_name.clone(), 0))
            .expect("window layout context")
            .resize_floating_pane(1, 6, 3)
            .expect("floating resize");
    }
    let resized = handler
        .web_session_snapshot_with_scrolls(&target, None, &HashMap::from([(tiled_id, 0)]))
        .await
        .expect("resized floating Web snapshot");
    assert!(
        moved.requires_keyframe_for(&resized),
        "resize must select a complete keyframe"
    );
    let resized_float = resized
        .view
        .panes
        .iter()
        .find(|pane| pane.id == floating_id.as_u32())
        .expect("resized floating metadata");
    assert_eq!(
        (
            resized_float.x,
            resized_float.y,
            resized_float.cols,
            resized_float.rows,
        ),
        (4, 1, 6, 3)
    );
}

#[tokio::test]
async fn web_session_floating_scrollback_uses_canonical_scrollbar_projection() {
    let handler = RequestHandler::new();
    // The floating pane's scrollback is written below; a shell prompt landing
    // after it would scroll the first row this test pins out of view.
    handler.pin_quiet_default_command_for_test().await;
    let session_name = new_session_with_size(
        &handler,
        "websession-floating-scrollbar",
        TerminalSize { cols: 20, rows: 10 },
    )
    .await;
    set_session_status(&handler, &session_name, "off").await;
    handler
        .wait_for_pane_startup_output_for_test(&PaneTarget::with_window(session_name.clone(), 0, 0))
        .await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    handler
        .wait_for_pane_startup_output_for_test(&PaneTarget::with_window(session_name.clone(), 0, 1))
        .await;
    for (option, value) in [
        (OptionName::PaneScrollbars, "on"),
        (OptionName::PaneScrollbarsPosition, "left"),
        (OptionName::PaneScrollbarsStyle, "width=1,pad=1"),
    ] {
        assert!(matches!(
            handler
                .handle(Request::SetOption(SetOptionRequest {
                    scope: ScopeSelector::Window(WindowTarget::with_window(
                        session_name.clone(),
                        0,
                    )),
                    option,
                    value: value.to_owned(),
                    mode: SetOptionMode::Replace,
                }))
                .await,
            Response::SetOption(_)
        ));
    }

    let (session_id, floating_id) = {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .window_layout_context_mut(&WindowTarget::with_window(session_name.clone(), 0))
            .expect("window layout context")
            .float_pane(
                1,
                rmux_core::FloatingPaneRect::new(2, 2, 8, 4).expect("floating rectangle"),
            )
            .expect("pane becomes floating");
        let session = state.sessions.session(&session_name).expect("session");
        let floating_id = session.window().pane(1).expect("floating pane").id();
        let session_id = session.id();
        state
            .append_bytes_to_pane_transcript_for_test(
                &session_name,
                0,
                1,
                b"ABCDEF\r\nGHIJKL\r\nMNOPQR\r\nSTUVWX\r\nYZ0123\r\n456789\r\n",
            )
            .expect("floating scrollback");
        (session_id, floating_id)
    };
    let target = crate::web::WebSessionTarget::new(session_name.clone(), session_id);
    let snapshot = handler
        .web_session_snapshot_with_scrolls(&target, None, &HashMap::from([(floating_id, 0)]))
        .await
        .expect("floating scrollbar Web snapshot");

    let floating = snapshot
        .view
        .panes
        .iter()
        .find(|pane| pane.id == floating_id.as_u32())
        .expect("floating metadata");
    assert_eq!(
        (floating.x, floating.y, floating.cols, floating.rows),
        (4, 2, 6, 4),
        "Web metadata must use the renderer's left-scrollbar content projection"
    );

    let mut rendered = rmux_core::TerminalScreen::new(snapshot.size, 0);
    rendered.feed(&snapshot.ansi_bytes());
    let mut text = String::new();
    assert!(rendered.screen().visit_visible_line_cells(2, 20, |cell| {
        text.push_str(cell.text());
    }));
    assert_eq!(
        text.chars().nth(4),
        Some('A'),
        "scrollback content must begin after the canonical scrollbar and padding"
    );

    let state = handler.state.lock().await;
    let mouse =
        crate::mouse::layout_for_session(&state, &session_name, 1).expect("mouse scene projection");
    let floating_mouse = mouse
        .panes
        .iter()
        .max_by_key(|pane| pane.layer)
        .expect("floating mouse pane");
    assert_eq!(floating_mouse.pane_id, floating_id);
    assert_eq!(
        floating_mouse.coordinates().relative_position(4, 2),
        Some((0, 0))
    );
    assert_eq!(floating_mouse.coordinates().relative_position(2, 2), None);
}

#[tokio::test]
async fn web_session_floating_order_is_canonical_and_invalidates_presentation() {
    let handler = RequestHandler::new();
    let session_name = new_session_with_size(
        &handler,
        "websession-floating-order",
        TerminalSize { cols: 20, rows: 10 },
    )
    .await;
    set_session_status(&handler, &session_name, "off").await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            session_name.clone(),
            0,
            0,
        ))
        .await;
    for pane_index in 1..=2 {
        assert!(matches!(
            handler
                .handle(Request::SplitWindow(SplitWindowRequest {
                    target: SplitWindowTarget::Session(session_name.clone()),
                    direction: SplitDirection::Vertical,
                    before: false,
                    environment: None,
                }))
                .await,
            Response::SplitWindow(_)
        ));
        handler
            .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
                session_name.clone(),
                0,
                pane_index,
            ))
            .await;
    }

    let (session_id, rear_id, front_id) = {
        let mut state = handler.state.lock().await;
        let target = WindowTarget::with_window(session_name.clone(), 0);
        let mut layout = state
            .sessions
            .window_layout_context_mut(&target)
            .expect("window layout context");
        layout
            .float_pane(
                1,
                rmux_core::FloatingPaneRect::new(2, 1, 8, 4).expect("rear rectangle"),
            )
            .expect("rear floating pane");
        layout
            .float_pane(
                2,
                rmux_core::FloatingPaneRect::new(4, 2, 8, 4).expect("front rectangle"),
            )
            .expect("front floating pane");
        let session = state.sessions.session(&session_name).expect("session");
        (
            session.id(),
            session.window().pane(1).expect("rear pane").id(),
            session.window().pane(2).expect("front pane").id(),
        )
    };
    let target = crate::web::WebSessionTarget::new(session_name.clone(), session_id);
    let before = handler
        .web_session_snapshot_with_scrolls(&target, None, &HashMap::new())
        .await
        .expect("ordered floating scene");
    assert_eq!(
        before
            .view
            .panes
            .iter()
            .rev()
            .take(2)
            .map(|pane| pane.id)
            .collect::<Vec<_>>(),
        vec![front_id.as_u32(), rear_id.as_u32()]
    );

    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .window_layout_context_mut(&WindowTarget::with_window(session_name, 0))
            .expect("window layout context")
            .raise_floating_pane(1)
            .expect("raise rear float");
    }
    let after = handler
        .web_session_snapshot_with_scrolls(&target, None, &HashMap::new())
        .await
        .expect("raised floating scene");
    assert_eq!(
        after
            .view
            .panes
            .iter()
            .rev()
            .take(2)
            .map(|pane| pane.id)
            .collect::<Vec<_>>(),
        vec![rear_id.as_u32(), front_id.as_u32()]
    );
    assert!(
        before.requires_keyframe_for(&after),
        "a z-order and occlusion change must select a complete keyframe"
    );
}

#[tokio::test]
async fn linked_web_sessions_capture_the_same_canonical_window_scene() {
    let handler = RequestHandler::new();
    let owner = new_session_with_size(
        &handler,
        "web-scene-linked-owner",
        TerminalSize { cols: 20, rows: 10 },
    )
    .await;
    let peer = new_session_with_size(
        &handler,
        "web-scene-linked-peer",
        TerminalSize { cols: 20, rows: 10 },
    )
    .await;
    set_session_status(&handler, &owner, "off").await;
    set_session_status(&handler, &peer, "off").await;
    for session_name in [&owner, &peer] {
        handler
            .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
                session_name.clone(),
                0,
                0,
            ))
            .await;
    }

    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(owner.clone(), 0),
                target: WindowTarget::with_window(peer.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await,
        Response::LinkWindow(_)
    ));

    let (owner_id, peer_id, owner_window_id, peer_own_window_id, peer_window_id) = {
        let state = handler.state.lock().await;
        let owner_session = state.sessions.session(&owner).expect("owner session");
        let peer_session = state.sessions.session(&peer).expect("peer session");
        (
            owner_session.id(),
            peer_session.id(),
            owner_session.window_at(0).expect("owner window").id(),
            peer_session.window_at(0).expect("peer own window").id(),
            peer_session.window_at(1).expect("linked peer window").id(),
        )
    };
    assert_eq!(owner_window_id, peer_window_id);

    let owner_snapshot = handler
        .web_session_snapshot_with_scrolls(
            &crate::web::WebSessionTarget::new(owner.clone(), owner_id),
            Some(WebSessionWindowSelection::new(0, owner_window_id)),
            &HashMap::new(),
        )
        .await
        .expect("owner scene snapshot");
    let peer_own_snapshot = handler
        .web_session_snapshot_with_scrolls(
            &crate::web::WebSessionTarget::new(peer.clone(), peer_id),
            Some(WebSessionWindowSelection::new(0, peer_own_window_id)),
            &HashMap::new(),
        )
        .await
        .expect("peer own-window snapshot");
    assert!(
        owner_snapshot.requires_keyframe_for(&peer_own_snapshot),
        "equal geometry from another WindowId is another presentation"
    );
    let peer_snapshot = handler
        .web_session_snapshot_with_scrolls(
            &crate::web::WebSessionTarget::new(peer.clone(), peer_id),
            Some(WebSessionWindowSelection::new(1, peer_window_id)),
            &HashMap::new(),
        )
        .await
        .expect("linked scene snapshot");
    assert!(
        !owner_snapshot.requires_keyframe_for(&peer_snapshot),
        "equal contexts for one canonical WindowId must capture one scene"
    );

    set_session_status(&handler, &peer, "2").await;
    let peer_status_snapshot = handler
        .web_session_snapshot_with_scrolls(
            &crate::web::WebSessionTarget::new(peer, peer_id),
            Some(WebSessionWindowSelection::new(1, peer_window_id)),
            &HashMap::new(),
        )
        .await
        .expect("linked scene with another context");
    assert!(
        owner_snapshot.requires_keyframe_for(&peer_status_snapshot),
        "one WindowId resolved in another context requires a new keyframe"
    );
}

#[tokio::test]
async fn web_share_expiry_kills_session_after_unix_second_rounding_window() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-expire").await;
    create_share(
        &handler,
        CreateWebShareRequest {
            ttl_seconds: Some(1),
            kill_session_on_expire: true,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;

    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let removed = {
            let state = handler.state.lock().await;
            state.sessions.session(&session_name).is_none()
        };
        if removed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expired web-share did not kill its session"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn kill_session_prunes_web_session_share_before_name_reuse() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            spectator: true,
            controls: true,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session_name.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)));

    assert!(
        list_shares(&handler).await.is_empty(),
        "shares for a removed session should be pruned"
    );

    new_session(&handler, session_name.as_str()).await;

    let error = handler
        .open_web_share(&operator_token, None)
        .await
        .err()
        .expect("old share must not attach to a recreated session");
    assert!(error.to_string().contains("does not exist"));
}

#[tokio::test]
async fn kill_session_pane_prune_preserves_recreated_name_share() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-pane-session-aba").await;
    let (original_session_id, original_pane_id) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("original session exists");
        (
            session.id(),
            session.active_pane_id().expect("original pane exists"),
        )
    };
    let original = create_share(
        &handler,
        share_request(WebShareScope::Pane(PaneTargetRef::by_id(
            session_name.clone(),
            original_pane_id,
        ))),
    )
    .await;
    let original_token = token_from_url(original.spectator_url.as_deref().expect("spectator URL"));
    let pause = handler.install_kill_session_web_prune_pause(session_name.clone());
    let kill_handler = handler.clone();
    let kill_session_name = session_name.clone();
    let kill = tokio::spawn(async move {
        kill_handler
            .handle(Request::KillSession(KillSessionRequest {
                target: kill_session_name,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await
    });
    timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("kill-session reaches the Web prune pause");

    let recreate_handler = handler.clone();
    let recreate_session_name = session_name.clone();
    let recreate = tokio::spawn(async move {
        new_session(&recreate_handler, recreate_session_name.as_str()).await
    });
    let (replacement_session_id, replacement_pane_id) = timeout(Duration::from_secs(5), async {
        loop {
            let replacement = {
                let state = handler.state.lock().await;
                state
                    .sessions
                    .session(&session_name)
                    .filter(|session| session.id() != original_session_id)
                    .map(|session| {
                        (
                            session.id(),
                            session.active_pane_id().expect("replacement pane exists"),
                        )
                    })
            };
            if let Some(replacement) = replacement {
                break replacement;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement session becomes visible before lifecycle publication");
    assert_ne!(replacement_session_id, original_session_id);
    assert_ne!(replacement_pane_id, original_pane_id);
    let replacement = timeout(
        Duration::from_secs(5),
        create_share(
            &handler,
            share_request(WebShareScope::Pane(PaneTargetRef::by_id(
                session_name.clone(),
                replacement_pane_id,
            ))),
        ),
    )
    .await
    .expect("replacement share is created before stale cleanup resumes");
    let replacement_token = token_from_url(
        replacement
            .spectator_url
            .as_deref()
            .expect("replacement spectator URL"),
    );

    pause.release.notify_one();
    assert!(matches!(
        timeout(Duration::from_secs(5), kill)
            .await
            .expect("kill-session completes after Web prune resumes")
            .expect("kill-session task completes"),
        Response::KillSession(_)
    ));
    assert_eq!(
        timeout(Duration::from_secs(5), recreate)
            .await
            .expect("replacement lifecycle publication completes")
            .expect("replacement session task completes"),
        session_name
    );
    assert!(handler.open_web_share(&original_token, None).await.is_err());
    let replacement_stream = handler
        .open_web_share(&replacement_token, None)
        .await
        .expect("replacement pane share survives stale session cleanup");
    assert!(matches!(replacement_stream, WebShareStream::Pane(_)));
}

#[tokio::test]
async fn kill_session_revokes_origin_pane_share_when_real_winlink_survives() {
    let handler = RequestHandler::new();
    let owner = new_session(&handler, "web-pane-linked-owner").await;
    let survivor = new_session(&handler, "web-pane-linked-survivor").await;
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&owner)
            .expect("owner session exists")
            .active_pane_id()
            .expect("owner pane exists")
    };
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(PaneTargetRef::by_id(
            owner.clone(),
            pane_id,
        ))),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(survivor.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: owner,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&survivor)
                .and_then(|session| session.window_at(1))
                .and_then(|window| window.pane(0))
                .map(|pane| pane.id()),
            Some(pane_id),
            "the true winlink must keep the pane alive independently of the share"
        );
    }
    assert!(list_shares(&handler).await.is_empty());
    assert!(handler.open_web_share(&token, None).await.is_err());
}

#[tokio::test]
async fn killing_last_pane_prunes_web_session_share() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-kill-pane").await;
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let spectator_token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));

    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::new(session_name.clone(), 0),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)));

    assert!(
        list_shares(&handler).await.is_empty(),
        "session shares should be pruned when the last pane destroys the session"
    );

    let error = handler
        .open_web_share(&spectator_token, None)
        .await
        .err()
        .expect("old share must not attach after the session was destroyed");
    assert!(error.to_string().contains("does not exist"));
}

#[tokio::test]
async fn killing_one_shared_pane_revokes_only_its_stable_share() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-pane-kill").await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .expect("session exists")
            .window()
            .pane(1)
            .expect("second pane exists")
            .id()
    };
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(PaneTargetRef::by_id(
            session_name.clone(),
            pane_id,
        ))),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));

    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(session_name, 0, 1),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)));
    assert!(list_shares(&handler).await.is_empty());
    assert!(handler.open_web_share(&token, None).await.is_err());
}

#[tokio::test]
async fn web_kill_pane_drains_after_kill_pane_inline_hook() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-after-kill-hook").await;
    let hook_target = new_session(&handler, "web-after-kill-hook-target").await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Session(session_name.clone()),
                hook: HookName::AfterKillPane,
                command: format!("new-window -d -t {hook_target}"),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));
    let session_target = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        crate::web::WebSessionTarget::new(session_name.clone(), session.id())
    };

    handler
        .web_session_kill_active_pane(&session_target, std::process::id())
        .await
        .expect("web pane kill succeeds");

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&hook_target)
            .expect("hook target survives")
            .windows()
            .count(),
        2,
        "the after-kill hook must run outside the Web request identity guard"
    );
}

#[tokio::test]
async fn web_session_identity_guard_preserves_recreated_name() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-session-identity-aba").await;
    let stale_session_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .expect("original session exists")
            .id()
    };
    assert!(matches!(
        handler
            .handle(Request::KillSession(KillSessionRequest {
                target: session_name.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await,
        Response::KillSession(_)
    ));
    let _ = new_session(&handler, session_name.as_str()).await;

    let response = super::super::web_request_identity::with_expected_session_identity(
        session_name.clone(),
        stale_session_id,
        handler.handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
        }))),
    )
    .await;

    assert!(matches!(response, Response::Error(_)));
    let state = handler.state.lock().await;
    let replacement = state
        .sessions
        .session(&session_name)
        .expect("replacement session survives stale web request");
    assert_ne!(replacement.id(), stale_session_id);
    assert_eq!(replacement.windows().count(), 1);
}

#[tokio::test]
async fn web_window_identity_guard_preserves_recreated_slot() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-window-identity-aba").await;
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session_name.clone(),
                name: None,
                detached: true,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                target_window_index: Some(1),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));
    let (session_id, stale_window_id) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        (
            session.id(),
            session.window_at(1).expect("original window exists").id(),
        )
    };
    assert!(matches!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(session_name.clone(), 1),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session_name.clone(),
                name: None,
                detached: true,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                target_window_index: Some(1),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));

    let response = super::super::web_request_identity::with_expected_window_identity(
        session_name.clone(),
        session_id,
        1,
        stale_window_id,
        handler.handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(session_name.clone(), 1),
            kill_all_others: false,
        })),
    )
    .await;

    assert!(matches!(response, Response::Error(_)));
    let state = handler.state.lock().await;
    let replacement = state
        .sessions
        .session(&session_name)
        .and_then(|session| session.window_at(1))
        .expect("replacement window survives stale web request");
    assert_ne!(replacement.id(), stale_window_id);
}

#[tokio::test]
async fn web_session_view_selection_rejects_missing_and_recycled_window_identity() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-view-window-identity-aba").await;
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session_name.clone(),
                name: Some("selected".to_owned()),
                detached: true,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                target_window_index: Some(1),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));
    let (session_id, selected_window_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&session_name).expect("session");
        (
            session.id(),
            session.window_at(1).expect("selected window").id(),
        )
    };
    let session_target = crate::web::WebSessionTarget::new(session_name.clone(), session_id);
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session_name.clone())),
    )
    .await;
    let stream = handler
        .open_web_share(
            &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
            None,
        )
        .await
        .expect("session share opens");
    let WebShareStream::Session(mut stream) = stream else {
        panic!("expected session stream");
    };
    let selection = handler
        .web_session_select_window_for_view(&session_target, stream.attach_pid(), 1)
        .await
        .expect("spectator selection succeeds")
        .expect("selected window exists");
    assert_eq!(selection.window_id(), selected_window_id);
    stream.select_window_for_view(selection);

    assert!(matches!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(session_name.clone(), 1),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(_)
    ));
    let missing = handler
        .web_session_snapshot_with_scrolls(
            &session_target,
            stream.selected_window(),
            &HashMap::new(),
        )
        .await;
    assert!(
        missing.is_err(),
        "a persisted Web view must refuse a vanished selected WindowId"
    );

    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session_name,
                name: Some("replacement".to_owned()),
                detached: true,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                target_window_index: Some(1),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));
    let recycled = handler
        .web_session_snapshot_with_scrolls(
            &session_target,
            stream.selected_window(),
            &HashMap::new(),
        )
        .await;
    assert!(
        recycled.is_err(),
        "a persisted Web view must refuse a replacement in the selected index"
    );
}

#[tokio::test]
async fn renaming_session_rekeys_stable_pane_share() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-pane-old").await;
    let renamed = SessionName::new("web-pane-new").expect("valid session name");
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(
            PaneTarget::new(session_name.clone(), 0).into(),
        )),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name,
            new_name: renamed.clone(),
        }))
        .await;
    assert!(matches!(response, Response::RenameSession(_)));
    let shares = list_shares(&handler).await;
    assert!(matches!(
        shares.as_slice(),
        [share]
            if matches!(&share.scope, WebShareScope::Pane(target) if target.session_name() == &renamed)
    ));
    assert!(handler.open_web_share(&token, None).await.is_ok());
}

#[tokio::test]
async fn open_pane_stream_follows_session_rename_by_stable_identity() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-pane-stream-old").await;
    let renamed = SessionName::new("web-pane-stream-new").expect("valid session name");
    let created = create_share(
        &handler,
        share_request(WebShareScope::Pane(
            PaneTarget::new(session_name.clone(), 0).into(),
        )),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let stream = handler
        .open_web_share(&token, None)
        .await
        .expect("pane stream opens before rename");
    let WebShareStream::Pane(mut pane) = stream else {
        panic!("expected pane stream");
    };

    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name,
            new_name: renamed.clone(),
        }))
        .await;
    assert!(matches!(response, Response::RenameSession(_)));

    let current = handler
        .current_web_pane_target(pane.session_id(), pane.target())
        .await
        .expect("open stream resolves renamed session by id");
    pane.set_target(current);
    assert_eq!(pane.target().session_name(), &renamed);
    assert!(handler.web_target_alive(pane.target()).await);
    assert!(handler.web_resnapshot(pane.target()).await.is_ok());
}

#[tokio::test]
async fn kill_session_on_expire_follows_renamed_session_id() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-expiry").await;
    let renamed_session = SessionName::new("websession-expiry-renamed").expect("valid session");
    create_share(
        &handler,
        CreateWebShareRequest {
            ttl_seconds: Some(6),
            operator: true,
            kill_session_on_expire: true,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;

    let renamed = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: session_name.clone(),
            new_name: renamed_session.clone(),
        }))
        .await;
    assert!(matches!(renamed, Response::RenameSession(_)));

    timeout(Duration::from_secs(10), async {
        loop {
            let session_gone = {
                let state = handler.state.lock().await;
                state.sessions.session(&renamed_session).is_none()
            };
            if session_gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("expiry task should kill the renamed session by id");

    let state = handler.state.lock().await;
    assert!(state.sessions.session(&session_name).is_none());
    assert!(state.sessions.session(&renamed_session).is_none());
}

#[tokio::test]
async fn web_session_select_pane_uses_explicit_pane_id() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-select-pane").await;
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let right_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .expect("session exists")
            .window()
            .pane(1)
            .expect("right pane exists")
            .id()
    };
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            spectator: false,
            max_spectators: None,
            max_operators: None,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));
    let stream = handler
        .open_web_share(&operator_token, None)
        .await
        .expect("session web share opens");
    let WebShareStream::Session(session_stream) = stream else {
        panic!("expected session web share stream");
    };

    handler
        .web_session_select_pane(
            session_stream.target(),
            session_stream.attach_pid(),
            right_pane_id,
        )
        .await
        .expect("pane selection succeeds");

    let state = handler.state.lock().await;
    let active = state
        .sessions
        .session(&session_name)
        .expect("session exists")
        .window()
        .active_pane()
        .expect("active pane exists")
        .id();
    assert_eq!(active, right_pane_id);
}

#[tokio::test]
async fn web_session_operator_resize_reaches_attached_session() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-browser-resize").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            spectator: false,
            max_spectators: None,
            max_operators: None,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));
    let stream = handler
        .open_web_share(&operator_token, None)
        .await
        .expect("session web share opens");
    let WebShareStream::Session(mut session_stream) = stream else {
        panic!("expected session web share stream");
    };

    session_stream
        .send_attach_resize(TerminalSize {
            cols: 100,
            rows: 40,
        })
        .await
        .expect("resize is written to attach stream");

    timeout(Duration::from_secs(2), async {
        loop {
            let geometry = {
                let state = handler.state.lock().await;
                let session = state
                    .sessions
                    .session(&session_name)
                    .expect("session exists");
                (session.terminal_size(), session.window().size())
            };
            if geometry
                == (
                    TerminalSize {
                        cols: 100,
                        rows: 40,
                    },
                    TerminalSize {
                        cols: 100,
                        rows: 39,
                    },
                )
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("browser resize reaches the attached session as external and content geometry");
}

#[tokio::test]
async fn passive_web_session_view_does_not_contribute_native_pane_focus() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "web-focus-provenance").await;
    let created = create_share(
        &handler,
        CreateWebShareRequest {
            operator: true,
            spectator: false,
            max_spectators: None,
            max_operators: None,
            ..share_request(WebShareScope::Session(session_name.clone()))
        },
    )
    .await;
    let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));
    let web_stream = handler
        .open_web_share(&operator_token, None)
        .await
        .expect("session web share opens");
    assert!(matches!(web_stream, WebShareStream::Session(_)));

    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&session_name, 0, 0, b"\x1b[?1004h")
            .expect("pane enables focus reporting");
    }
    let native_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(native_pid, session_name.clone(), control_tx)
        .await;
    let target = PaneTarget::new(session_name.clone(), 0);
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&target);
    }
    handler
        .handle_attached_live_input_for_test(native_pid, b"\x1b[O")
        .await
        .expect("native focus-out succeeds");

    let state = handler.state.lock().await;
    assert_eq!(
        state.pane_input_capture_for_test(&target),
        Some(b"\x1b[O".to_vec()),
        "the virtual attach must not keep the native pane aggregate focused"
    );
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[tokio::test]
async fn web_session_pane_scroll_frame_degrades_when_recovery_metadata_is_bounded_out() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-scroll-hyperlinks").await;
    let session_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .expect("session exists")
            .id()
    };
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    let (pane_id, transcript) = {
        let state = handler.state.lock().await;
        (
            state
                .sessions
                .session(&session_name)
                .expect("session exists")
                .window()
                .active_pane()
                .expect("active pane exists")
                .id(),
            state.transcript_handle(&target).expect("pane transcript"),
        )
    };

    // Scrollback plus one hyperlink entry beyond the bounded Web recovery
    // contract: an `ls --hyperlink=auto` row or a presigned object URL.
    let mut payload = Vec::new();
    for row in 0..60 {
        payload.extend_from_slice(format!("line {row}\r\n").as_bytes());
    }
    let uri = format!(
        "https://example.test/{}",
        "x".repeat(crate::pane_recovery::MAX_RECOVERY_HYPERLINK_ENTRY_BYTES)
    );
    payload.extend_from_slice(format!("\x1b]8;;{uri}\x1b\\X\x1b]8;;\x1b\\").as_bytes());
    transcript.lock().append_bytes(&payload);

    let session_target = crate::web::WebSessionTarget::new(session_name, session_id);
    let frame = handler
        .web_session_pane_scroll_frame(&session_target, pane_id, 0, None)
        .await;

    // The full-snapshot twin marks the view incomplete and keeps serving; the
    // scroll patch must fall back to it rather than fail the viewer socket.
    let frame = frame.expect("bounded-out recovery metadata must not fail the scroll frame");
    assert!(
        frame.is_none(),
        "an incomplete-metadata scroll patch must defer to a full snapshot"
    );

    // ... and that fallback must actually render the requested scroll.
    let snapshot = handler
        .web_session_snapshot_with_scrolls(
            &session_target,
            None,
            &HashMap::from([(pane_id, 0_usize)]),
        )
        .await
        .expect("the full snapshot fallback serves the same scroll");
    assert!(!snapshot.view.metadata_complete);
    let scrolled = snapshot
        .view
        .panes
        .iter()
        .find(|pane| pane.id == pane_id.as_u32())
        .expect("scrolled pane is in the view");
    assert!(scrolled.scroll_offset > 0);
}

#[tokio::test]
async fn web_session_snapshot_degrades_for_a_copy_mode_pane_with_bounded_out_metadata() {
    let handler = RequestHandler::new();
    let session_name = new_session(&handler, "websession-copy-hyperlinks").await;
    let session_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session_name)
            .expect("session exists")
            .id()
    };
    let target = PaneTarget::with_window(session_name.clone(), 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    let transcript = {
        let state = handler.state.lock().await;
        state.transcript_handle(&target).expect("pane transcript")
    };

    // Copy mode clones the pane screen, so the over-budget hyperlink table has
    // to exist before the mode is entered.
    let uri = format!(
        "https://example.test/{}",
        "x".repeat(crate::pane_recovery::MAX_RECOVERY_HYPERLINK_ENTRY_BYTES)
    );
    transcript
        .lock()
        .append_bytes(format!("\x1b]8;;{uri}\x1b\\X\x1b]8;;\x1b\\").as_bytes());
    let response = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: false,
        }))
        .await;
    assert!(matches!(response, Response::CopyMode(_)), "{response:?}");

    let session_target = crate::web::WebSessionTarget::new(session_name, session_id);
    let snapshot = handler
        .web_session_snapshot(&session_target)
        .await
        .expect("a copy-mode pane with bounded-out metadata still snapshots");

    // The non-copy-mode branch of the same render drops the over-budget
    // metadata instead of failing; copy mode must degrade the same way and
    // report the gap rather than fail the viewer's frame.
    assert!(!snapshot.view.metadata_complete);

    // Resetting the live pane's hyperlink storage does not reach copy mode's
    // frozen backing screen, which is what the viewer renders, so the reported
    // coverage must still follow the degraded render.
    let response = handler
        .handle(Request::ClearHistory(rmux_proto::ClearHistoryRequest {
            target: PaneTarget::with_window(session_target.name().clone(), 0, 0),
            reset_hyperlinks: true,
        }))
        .await;
    assert!(
        matches!(response, Response::ClearHistory(_)),
        "{response:?}"
    );
    let snapshot = handler
        .web_session_snapshot(&session_target)
        .await
        .expect("a copy-mode pane with bounded-out metadata still snapshots");

    assert!(!snapshot.view.metadata_complete);
}

fn token_from_url(url: &str) -> String {
    url.split_once('#')
        .and_then(|(_, fragment)| {
            fragment.split('&').find_map(|param| {
                let (key, value) = param.split_once('=')?;
                (key == "t").then_some(value.to_owned())
            })
        })
        .expect("URL contains access token")
}

async fn new_session(handler: &RequestHandler, name: &str) -> SessionName {
    new_session_with_size(handler, name, TerminalSize { cols: 80, rows: 24 }).await
}

async fn new_session_with_size(
    handler: &RequestHandler,
    name: &str,
    size: TerminalSize,
) -> SessionName {
    let session_name = SessionName::new(name).expect("valid session");
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(size),
                environment: None,
            }))
            .await,
    );
    session_name
}

async fn set_session_status(handler: &RequestHandler, session_name: &SessionName, value: &str) {
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(session_name.clone()),
                option: OptionName::Status,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
}

async fn session_window_size(handler: &RequestHandler, session_name: &SessionName) -> TerminalSize {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session_name)
        .expect("session exists")
        .window()
        .size()
}

/// Leaves `session_name` with a two-line status, an 80x24 outer terminal and an
/// 80x22 content window, with no client still attached.
async fn seed_two_line_status_geometry(
    handler: &RequestHandler,
    session_name: &SessionName,
    attach_pid: u32,
) {
    set_session_status(handler, session_name, "2").await;
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let attach_id = handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;
    handler
        .handle_attached_resize(attach_pid, TerminalSize { cols: 80, rows: 24 })
        .await
        .expect("declared terminal geometry seeds status-aware content size");
    handler.finish_attach(attach_pid, attach_id).await;
    assert_eq!(
        session_window_size(handler, session_name).await,
        TerminalSize { cols: 80, rows: 22 }
    );
}

async fn create_share(
    handler: &RequestHandler,
    request: CreateWebShareRequest,
) -> WebShareCreatedResponse {
    handler.mark_web_listener_available();
    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::Create(
            request,
        ))))
        .await;
    let Response::WebShare(response) = response else {
        panic!("expected created web-share response");
    };
    let rmux_proto::WebShareResponse::Created(created) = *response else {
        panic!("expected created web-share response");
    };
    created
}

fn share_request(scope: WebShareScope) -> CreateWebShareRequest {
    CreateWebShareRequest {
        scope,
        public_base_url: Some("https://share.example".to_owned()),
        tunnel_provider: None,
        frontend_url: None,
        ttl_seconds: None,
        expires_at_unix: None,
        max_spectators: Some(1),
        max_operators: None,
        url_options: Default::default(),
        require_pin: false,
        operator_pin: None,
        spectator_pin: None,
        terminal_palette: None,
        operator: false,
        spectator: true,
        controls: false,
        kill_session_on_expire: false,
    }
}

fn handler_with_web_port(port: u16) -> RequestHandler {
    handler_with_web_settings(
        crate::web::WebShareSettings::from_options(port, None).expect("web settings"),
    )
}

fn handler_with_automatic_web_port() -> RequestHandler {
    handler_with_web_settings(
        crate::web::WebShareSettings::from_options_with_port_explicit(
            unused_web_port(),
            None,
            false,
        )
        .expect("web settings"),
    )
}

fn handler_with_web_settings(settings: crate::web::WebShareSettings) -> RequestHandler {
    RequestHandler::with_owner_uid_subscription_limits_and_web_settings(
        current_owner_uid(),
        SubscriptionLimits::default(),
        settings,
    )
}

fn unused_web_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind free port probe");
    listener.local_addr().expect("free port addr").port()
}

async fn list_shares(handler: &RequestHandler) -> Vec<rmux_proto::WebShareSummary> {
    let response = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::List(
            ListWebSharesRequest,
        ))))
        .await;
    let Response::WebShare(response) = response else {
        panic!("expected listed web-share response");
    };
    let rmux_proto::WebShareResponse::List(listed) = *response else {
        panic!("expected listed web-share response");
    };
    listed.shares
}
