use super::*;

async fn new_detached_session(handler: &RequestHandler, name: &str) -> PaneTarget {
    let session = session_name(name);
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await,
    );
    PaneTarget::with_window(session, 0, 0)
}

fn command_stdout(response: &Response) -> String {
    let output = response
        .command_output()
        .unwrap_or_else(|| panic!("command output expected: {response:?}"));
    String::from_utf8_lossy(output.stdout()).into_owned()
}

/// A panic under one pane's transcript lock poisons only that pane. The
/// per-request theme refresh and the client key-table sweep must not turn it
/// into a failure of list-sessions and of requests on unrelated panes.
#[tokio::test]
async fn poisoned_pane_transcript_leaves_other_requests_and_panes_served() {
    let handler = RequestHandler::new();
    let poisoned = new_detached_session(&handler, "poisoned").await;
    let healthy = new_detached_session(&handler, "healthy").await;
    handler
        .state
        .lock()
        .await
        .transcript_handle(&poisoned)
        .expect("poisoned pane transcript")
        .poison_for_test();

    let sessions = handler
        .handle(Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await;
    assert_eq!(command_stdout(&sessions), "healthy\npoisoned\n");

    let display = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(healthy.clone())),
            print: true,
            message: Some("#{session_name}:#{pane_index}".to_owned()),
            empty_target_context: false,
        }))
        .await;
    assert_eq!(command_stdout(&display), "healthy:0\n");

    let panes = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: healthy.session_name().clone(),
            target_window_index: None,
            format: Some("#{pane_index}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    assert_eq!(command_stdout(&panes), "0\n");

    new_detached_session(&handler, "after-poison").await;

    let transcript = handler
        .state
        .lock()
        .await
        .transcript_handle(&poisoned)
        .expect("poisoned pane transcript");
    assert!(
        std::panic::catch_unwind(|| drop(transcript.lock())).is_err(),
        "the poisoned pane itself keeps failing fast"
    );
}

impl RequestHandler {
    /// Poisons one pane's transcript the way a panic under its lock does.
    pub(crate) async fn poison_pane_transcript_for_test(&self, target: &PaneTarget) {
        self.state
            .lock()
            .await
            .transcript_handle(target)
            .expect("pane transcript to poison")
            .poison_for_test();
    }
}

fn capture_pane(target: PaneTarget) -> Request {
    Request::CapturePane(Box::new(rmux_proto::CapturePaneRequest {
        target,
        start: None,
        end: None,
        print: true,
        buffer_name: None,
        alternate: false,
        escape_ansi: false,
        escape_sequences: false,
        include_format: false,
        hyperlinks: false,
        line_numbers: false,
        join_wrapped: false,
        use_mode_screen: false,
        preserve_trailing_spaces: false,
        do_not_trim_spaces: false,
        pending_input: false,
        quiet: false,
        start_is_absolute: false,
        end_is_absolute: false,
    }))
}

fn display_message(target: &PaneTarget, format: &str) -> Request {
    Request::DisplayMessage(DisplayMessageRequest {
        target: Some(Target::Pane(target.clone())),
        print: true,
        message: Some(format.to_owned()),
        empty_target_context: false,
    })
}

/// A request that names the poisoned pane still aborts on its transcript, but
/// its client is answered with the typed error instead of a dropped
/// connection, and the same handler keeps serving the other pane.
#[tokio::test]
async fn request_naming_a_poisoned_pane_answers_pane_unavailable() {
    let handler = RequestHandler::new();
    let poisoned = new_detached_session(&handler, "poisoned").await;
    let healthy = new_detached_session(&handler, "healthy").await;
    let pane_id = command_stdout(
        &handler
            .handle(display_message(&poisoned, "#{pane_id}"))
            .await,
    );
    let pane_id = pane_id.trim_end();
    handler.poison_pane_transcript_for_test(&poisoned).await;
    let unavailable = Response::Error(rmux_proto::ErrorResponse {
        error: rmux_proto::RmuxError::Message(format!("pane {pane_id} unavailable")),
    });

    let requests = [
        ("capture-pane", capture_pane(poisoned.clone())),
        (
            "send-keys",
            Request::SendKeys(rmux_proto::SendKeysRequest {
                target: poisoned.clone(),
                keys: vec!["x".to_owned()],
            }),
        ),
        (
            "display-message",
            display_message(&poisoned, "#{pane_title}"),
        ),
    ];
    for (command, request) in requests {
        assert_eq!(handler.handle(request).await, unavailable, "{command}");
    }

    // list-panes names a session or a window, not a pane, and the CLI sends
    // this very request for each session of list-panes -a: the poisoned pane
    // is listed with its transcript values empty.
    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: poisoned.session_name().clone(),
            target_window_index: None,
            format: Some("#{pane_id}:#{pane_title}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    assert_eq!(
        command_stdout(&listed),
        format!("{pane_id}:\n"),
        "{listed:?}"
    );

    let healthy_display = handler
        .handle(display_message(&healthy, "#{session_name}:#{pane_title}"))
        .await;
    assert!(
        command_stdout(&healthy_display).starts_with("healthy:"),
        "{healthy_display:?}"
    );
    assert!(matches!(
        handler.handle(capture_pane(healthy)).await,
        Response::CapturePane(_)
    ));
}

/// The exit of a pane whose transcript is poisoned still tears it down: the
/// last pane of a session takes the session with it, as an ordinary pane does
/// and as a session whose last pane exits does under tmux 3.7b.
#[tokio::test]
async fn last_pane_exit_removes_its_session_despite_a_poisoned_transcript() {
    for (poisoned, pending_alert) in [(false, false), (true, false), (false, true), (true, true)] {
        let handler = RequestHandler::new();
        let exited = new_detached_session(&handler, "exited").await;
        new_detached_session(&handler, "healthy").await;
        let pane_id = {
            let mut state = handler.state.lock().await;
            state
                .mark_pane_dead_without_exit_details(&exited)
                .expect("the child exit is recorded");
            state
                .sessions
                .session(exited.session_name())
                .and_then(|session| session.active_pane())
                .map(rmux_core::Pane::id)
                .expect("the exited pane exists")
        };
        if poisoned {
            handler.poison_pane_transcript_for_test(&exited).await;
        }
        if pending_alert {
            // The exit flushes the alert itself, and with it the automatic
            // window name, before the coalescing delay can.
            handler
                .pane_alert_callback()
                .alert(bell_alert(&exited, pane_id));
        }

        handler
            .handle_pane_exit_event(crate::pane_io::PaneExitEvent::eof_published(
                exited.session_name().clone(),
                pane_id,
                None,
            ))
            .await;

        let sessions = handler
            .handle(Request::ListSessions(ListSessionsRequest {
                format: Some("#{session_name}".to_owned()),
                filter: None,
                sort_order: None,
                reversed: false,
            }))
            .await;
        assert_eq!(
            command_stdout(&sessions),
            "healthy\n",
            "poisoned={poisoned} pending_alert={pending_alert}"
        );
    }
}

fn bell_alert(target: &PaneTarget, pane_id: rmux_core::PaneId) -> crate::pane_io::PaneAlertEvent {
    crate::pane_io::PaneAlertEvent {
        session_name: target.session_name().clone(),
        pane_id,
        bell_count: 1,
        title_changed: false,
        title_change: None,
        path_changed: false,
        clipboard_set: false,
        clipboard_writes: Vec::new(),
        clipboard_queries: Vec::new(),
        mouse_mode_changed: false,
        alternate_mode_changed: false,
        queue_activity_alert: false,
        generation: None,
    }
}
