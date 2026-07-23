use super::buffer_support::{install_paste_buffer_identity_pause, OrderedPasteBufferResult};
use super::RequestHandler;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    DeleteBufferRequest, ErrorResponse, ListBuffersRequest, LoadBufferRequest, NewSessionRequest,
    OptionName, PaneTarget, PasteBufferRequest, Request, RespawnPaneRequest, Response, RmuxError,
    ScopeSelector, SetBufferRequest, SetOptionMode, SetOptionRequest, ShowBufferRequest,
    SwapPaneRequest, TerminalSize,
};
use std::fs;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Barrier;

fn session_name(value: &str) -> rmux_proto::SessionName {
    rmux_proto::SessionName::new(value).expect("valid session name")
}

fn set_buffer_request(name: Option<&str>, content: &[u8]) -> SetBufferRequest {
    SetBufferRequest {
        name: name.map(str::to_owned),
        content: content.to_vec(),
        append: false,
        new_name: None,
        set_clipboard: false,
        target_client: None,
    }
}

fn load_buffer_request(path: &str) -> LoadBufferRequest {
    LoadBufferRequest {
        path: path.to_owned(),
        cwd: None,
        name: None,
        set_clipboard: false,
        target_client: None,
    }
}

fn paste_buffer_request(
    name: Option<&str>,
    target: PaneTarget,
    delete_after: bool,
) -> PasteBufferRequest {
    PasteBufferRequest {
        name: name.map(str::to_owned),
        target,
        delete_after,
        separator: None,
        linefeed: false,
        raw: false,
        bracketed: false,
    }
}

async fn create_session(handler: &RequestHandler, name: &str) {
    let session_name = session_name(name);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;

    assert!(matches!(response, Response::NewSession(_)));
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session_name, 0))
        .await;
}

async fn wait_for_dead_pane(
    handler: &RequestHandler,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
    pane_index: u32,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let exited = {
            let mut state = handler.state.lock().await;
            state
                .clone_pane_master_if_alive(session_name, window_index, pane_index)
                .is_err()
        };
        if exited {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for pane {session_name}:{window_index}.{pane_index} to exit"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn take_write(control: AttachControl) -> Vec<u8> {
    match control {
        AttachControl::Write(bytes) => bytes,
        other => panic!("expected attach write, got {other:?}"),
    }
}

#[tokio::test]
async fn set_buffer_creates_unnamed_buffer() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None, b"hello",
        ))))
        .await;

    match response {
        Response::SetBuffer(r) => assert_eq!(r.buffer_name, "buffer0"),
        other => panic!("unexpected response: {other:?}"),
    }
}

#[tokio::test]
async fn set_buffer_creates_named_buffer() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("my-buf"),
            b"data",
        ))))
        .await;

    match response {
        Response::SetBuffer(r) => assert_eq!(r.buffer_name, "my-buf"),
        other => panic!("unexpected response: {other:?}"),
    }
}

#[tokio::test]
async fn concurrent_named_buffer_appends_preserve_both_writes() {
    let handler = Arc::new(RequestHandler::new());
    let response = handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("append-race"),
            b"base",
        ))))
        .await;
    assert!(matches!(response, Response::SetBuffer(_)));

    let state_guard = handler.state.lock().await;
    let start = Arc::new(Barrier::new(3));
    let spawn_append = |requester_pid: u32, connection_id: u64, suffix: &'static [u8]| {
        let handler = Arc::clone(&handler);
        let start = Arc::clone(&start);
        tokio::spawn(async move {
            let mut request = set_buffer_request(Some("append-race"), suffix);
            request.append = true;
            start.wait().await;
            handler
                .dispatch_for_connection(
                    requester_pid,
                    connection_id,
                    Request::SetBuffer(Box::new(request)),
                )
                .await
                .response
        })
    };
    let first = spawn_append(10_001, 20_001, b"-first");
    let second = spawn_append(10_002, 20_002, b"-second");
    start.wait().await;
    tokio::task::yield_now().await;
    drop(state_guard);

    let first_response = first.await.expect("first append task joins");
    let second_response = second.await.expect("second append task joins");
    assert!(matches!(first_response, Response::SetBuffer(_)));
    assert!(matches!(second_response, Response::SetBuffer(_)));

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("append-race".to_owned()),
        }))
        .await;
    let content = show
        .command_output()
        .expect("appended buffer remains readable")
        .stdout()
        .to_vec();
    assert!(
        content == b"base-first-second" || content == b"base-second-first",
        "both successful appends must be preserved, got {:?}",
        String::from_utf8_lossy(&content)
    );
}

#[tokio::test]
async fn set_buffer_skips_existing_named_buffer_pattern_for_unnamed() {
    let handler = RequestHandler::new();

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("buffer0"),
            b"named",
        ))))
        .await;

    let response = handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None, b"unnamed",
        ))))
        .await;

    match response {
        Response::SetBuffer(r) => assert_eq!(r.buffer_name, "buffer1"),
        other => panic!("unexpected response: {other:?}"),
    }

    let show_named = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("buffer0".to_owned()),
        }))
        .await;
    let named_output = show_named
        .command_output()
        .expect("named buffer remains readable");
    assert_eq!(named_output.stdout(), b"named");

    let show_unnamed = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("buffer1".to_owned()),
        }))
        .await;
    let unnamed_output = show_unnamed
        .command_output()
        .expect("unnamed buffer was created");
    assert_eq!(unnamed_output.stdout(), b"unnamed");
}

#[tokio::test]
async fn set_buffer_rejects_empty_name() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some(""),
            b"data",
        ))))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn set_buffer_accepts_colon_in_name() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("a:b"),
            b"data",
        ))))
        .await;

    match response {
        Response::SetBuffer(response) => assert_eq!(response.buffer_name, "a:b"),
        other => panic!("unexpected response: {other:?}"),
    }
}

#[tokio::test]
async fn set_buffer_empty_content_does_not_create_buffer() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(None, b""))))
        .await;

    match response {
        Response::SetBuffer(response) => assert!(response.buffer_name.is_empty()),
        other => panic!("unexpected response: {other:?}"),
    }

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    assert!(matches!(show, Response::Error(_)));
}

#[tokio::test]
async fn show_buffer_returns_content() {
    let handler = RequestHandler::new();

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None,
            b"hello world",
        ))))
        .await;

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    let output = response
        .command_output()
        .expect("show-buffer returns output");
    assert_eq!(output.stdout(), b"hello world");
}

#[tokio::test]
async fn show_buffer_empty_store_returns_error() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn show_buffer_nonexistent_name_returns_error() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("missing".to_owned()),
        }))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn delete_buffer_removes_stack_head() {
    let handler = RequestHandler::new();

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(None, b"a"))))
        .await;
    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(None, b"b"))))
        .await;

    let response = handler
        .handle(Request::DeleteBuffer(DeleteBufferRequest { name: None }))
        .await;

    match response {
        Response::DeleteBuffer(r) => assert_eq!(r.buffer_name, "buffer1"),
        other => panic!("unexpected response: {other:?}"),
    }
}

#[tokio::test]
async fn delete_buffer_nonexistent_returns_error() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::DeleteBuffer(DeleteBufferRequest {
            name: Some("missing".to_owned()),
        }))
        .await;

    assert!(matches!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::Server(_)
        })
    ));
}

#[tokio::test]
async fn delete_buffer_empty_store_returns_error() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::DeleteBuffer(DeleteBufferRequest { name: None }))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn list_buffers_returns_formatted_output() {
    let handler = RequestHandler::new();

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None, b"first",
        ))))
        .await;
    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("named"),
            b"second",
        ))))
        .await;

    let response = handler
        .handle(Request::ListBuffers(ListBuffersRequest::default()))
        .await;
    let output = response
        .command_output()
        .expect("list-buffers returns output");
    let stdout = std::str::from_utf8(output.stdout()).expect("utf8");
    assert!(stdout.contains("named:"));
    assert!(stdout.contains("buffer0:"));
    // Most recent first
    assert!(stdout.find("named:").unwrap() < stdout.find("buffer0:").unwrap());
}

#[tokio::test]
async fn list_buffers_empty_returns_empty_output() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::ListBuffers(ListBuffersRequest::default()))
        .await;
    let output = response
        .command_output()
        .expect("list-buffers returns output");
    assert!(output.stdout().is_empty());
}

#[tokio::test]
async fn paste_buffer_writes_to_pty() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None,
            b"paste-me",
        ))))
        .await;

    let response = handler
        .handle(Request::PasteBuffer(Box::new(paste_buffer_request(
            None,
            PaneTarget::new(session_name("alpha"), 0),
            false,
        ))))
        .await;

    match response {
        Response::PasteBuffer(r) => assert_eq!(r.buffer_name, "buffer0"),
        other => panic!("unexpected response: {other:?}"),
    }

    // Buffer should still exist
    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    assert!(matches!(show, Response::ShowBuffer(_)));
}

async fn assert_paste_buffer_rejects_same_slot_replacement(original_bracketed_mode: bool) {
    let handler = Arc::new(RequestHandler::new());
    let original_name = if original_bracketed_mode {
        "paste-bracketed-original"
    } else {
        "paste-plain-original"
    };
    let replacement_name = if original_bracketed_mode {
        "paste-plain-replacement"
    } else {
        "paste-bracketed-replacement"
    };
    let original = session_name(original_name);
    let replacement = session_name(replacement_name);
    create_session(handler.as_ref(), original_name).await;
    create_session(handler.as_ref(), replacement_name).await;

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("identity-race"),
            b"must-not-be-written",
        ))))
        .await;

    let original_target = PaneTarget::with_window(original.clone(), 0, 0);
    let replacement_target = PaneTarget::with_window(replacement.clone(), 0, 0);
    let (original_pane_id, replacement_pane_id) = {
        let mut state = handler.state.lock().await;
        if original_bracketed_mode {
            state
                .append_bytes_to_pane_transcript_for_test(&original, 0, 0, b"\x1b[?2004h")
                .expect("original pane enables bracketed paste mode");
        } else {
            state
                .append_bytes_to_pane_transcript_for_test(&replacement, 0, 0, b"\x1b[?2004h")
                .expect("replacement pane enables bracketed paste mode");
        }
        let original_pane_id = state
            .sessions
            .session(&original)
            .and_then(|session| session.pane_id_in_window(0, 0))
            .expect("original pane identity exists");
        let replacement_pane_id = state
            .sessions
            .session(&replacement)
            .and_then(|session| session.pane_id_in_window(0, 0))
            .expect("replacement pane identity exists");
        let mode_is_bracketed = |session_name, pane_id| {
            state
                .pane_screen_state(session_name, pane_id)
                .is_some_and(|screen| screen.mode & rmux_core::input::mode::MODE_BRACKETPASTE != 0)
        };
        assert_eq!(
            mode_is_bracketed(&original, original_pane_id),
            original_bracketed_mode
        );
        assert_eq!(
            mode_is_bracketed(&replacement, replacement_pane_id),
            !original_bracketed_mode
        );
        state.start_pane_input_capture_for_test(&original_target);
        state.start_pane_input_capture_for_test(&replacement_target);
        (original_pane_id, replacement_pane_id)
    };
    assert_ne!(original_pane_id, replacement_pane_id);

    let pause = install_paste_buffer_identity_pause(original.clone());
    let paste_handler = Arc::clone(&handler);
    let paste_target = original_target.clone();
    let paste = tokio::spawn(async move {
        let mut request = paste_buffer_request(Some("identity-race"), paste_target, false);
        request.bracketed = true;
        paste_handler
            .handle(Request::PasteBuffer(Box::new(request)))
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), pause.wait_until_reached())
        .await
        .expect("paste-buffer should pause after capturing pane identity");

    let swapped = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: original_target.clone(),
            target: replacement_target.clone(),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(swapped, Response::SwapPane(_)), "{swapped:?}");
    pause.release();

    let response = paste.await.expect("paste-buffer task should join");
    assert!(
        matches!(&response, Response::Error(ErrorResponse { error }) if error.to_string().contains("pane identity changed before paste-buffer write")),
        "same-slot replacement must fail closed, got {response:?}"
    );

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&original)
            .and_then(|session| session.pane_id_in_window(0, 0)),
        Some(replacement_pane_id),
        "the target slot must contain the replacement pane"
    );
    assert_eq!(
        state.pane_input_capture_for_test(&original_target),
        Some(Vec::new()),
        "paste-buffer must not write to the replacement pane"
    );
    assert_eq!(
        state.pane_input_capture_for_test(&replacement_target),
        Some(Vec::new()),
        "paste-buffer must not retarget the write to the moved original pane"
    );
}

#[tokio::test]
async fn paste_buffer_bracketed_mode_snapshot_rejects_same_slot_replacement() {
    assert_paste_buffer_rejects_same_slot_replacement(true).await;
}

#[tokio::test]
async fn paste_buffer_plain_mode_snapshot_rejects_same_slot_replacement() {
    assert_paste_buffer_rejects_same_slot_replacement(false).await;
}

#[tokio::test]
async fn paste_buffer_with_delete_removes_buffer_after_write() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None,
            b"paste-then-delete",
        ))))
        .await;

    let response = handler
        .handle(Request::PasteBuffer(Box::new(paste_buffer_request(
            None,
            PaneTarget::new(session_name("alpha"), 0),
            true,
        ))))
        .await;

    assert!(matches!(response, Response::PasteBuffer(_)));

    // Buffer should be gone
    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    assert!(matches!(show, Response::Error(_)));
}

#[tokio::test]
async fn paste_buffer_with_delete_keeps_newer_named_replacements() {
    let handler = Arc::new(RequestHandler::new());
    create_session(handler.as_ref(), "alpha").await;

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("shared"),
            b"old",
        ))))
        .await;

    let pause = handler.install_paste_buffer_delete_pause();
    let paste_handler = Arc::clone(&handler);
    let paste = tokio::spawn(async move {
        paste_handler
            .handle(Request::PasteBuffer(Box::new(paste_buffer_request(
                Some("shared"),
                PaneTarget::new(session_name("alpha"), 0),
                true,
            ))))
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("paste-buffer should pause before deleting");

    let replace_handler = Arc::clone(&handler);
    let mut replace = tokio::spawn(async move {
        replace_handler
            .handle(Request::SetBuffer(Box::new(set_buffer_request(
                Some("shared"),
                b"new",
            ))))
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut replace)
            .await
            .is_err(),
        "a later buffer replacement must wait for the admitted paste-delete turn"
    );

    pause.release.notify_one();

    let response = paste.await.expect("paste-buffer task should join");
    assert!(matches!(response, Response::PasteBuffer(_)));
    let replace = replace.await.expect("replacement task should join");
    assert!(matches!(replace, Response::SetBuffer(_)));

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("shared".to_owned()),
        }))
        .await;
    assert_eq!(
        show.command_output()
            .expect("replacement buffer should survive")
            .stdout(),
        b"new"
    );
}

#[tokio::test]
async fn cancelling_paste_delete_caller_after_write_still_deletes_and_emits() {
    let handler = Arc::new(RequestHandler::new());
    create_session(handler.as_ref(), "paste-delete-durable").await;
    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("durable"),
            b"written-before-cancel",
        ))))
        .await;
    let mut lifecycle = handler.subscribe_lifecycle_events();
    let pause = handler.install_paste_buffer_delete_pause();
    let paste_handler = Arc::clone(&handler);
    let paste = tokio::spawn(async move {
        paste_handler
            .handle(Request::PasteBuffer(Box::new(paste_buffer_request(
                Some("durable"),
                PaneTarget::new(session_name("paste-delete-durable"), 0),
                true,
            ))))
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("durable paste reaches the post-write delete seam");
    paste.abort();
    let _ = paste.await;
    pause.release.notify_one();
    tokio::time::timeout(
        Duration::from_secs(2),
        handler.wait_for_post_commit_operations(),
    )
    .await
    .expect("accepted paste-delete completes after caller cancellation");

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("durable".to_owned()),
        }))
        .await;
    assert!(matches!(show, Response::Error(_)), "buffer must be deleted");
    let deleted = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = lifecycle
                .recv()
                .await
                .expect("lifecycle channel remains open");
            if matches!(
                event.event,
                LifecycleEvent::PasteBufferDeleted { ref buffer_name }
                    if buffer_name == "durable"
            ) {
                return;
            }
        }
    })
    .await;
    assert!(
        deleted.is_ok(),
        "durable delete must emit its lifecycle event"
    );
}

#[tokio::test]
async fn paste_buffer_nonexistent_session_returns_error() {
    let handler = RequestHandler::new();

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None, b"data",
        ))))
        .await;

    let response = handler
        .handle(Request::PasteBuffer(Box::new(paste_buffer_request(
            None,
            PaneTarget::new(session_name("missing"), 0),
            false,
        ))))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn paste_buffer_empty_store_is_successful_noop() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::PasteBuffer(Box::new(paste_buffer_request(
            None,
            PaneTarget::new(session_name("alpha"), 0),
            false,
        ))))
        .await;

    assert!(matches!(
        response,
        Response::PasteBuffer(rmux_proto::PasteBufferResponse { buffer_name })
            if buffer_name.is_empty()
    ));

    let ordered = handler
        .handle_paste_buffer_for_order(
            paste_buffer_request(None, PaneTarget::new(session_name("alpha"), 0), false),
            u64::MAX,
        )
        .await;
    assert!(
        matches!(
            ordered,
            OrderedPasteBufferResult::Completed(Response::PasteBuffer(
                rmux_proto::PasteBufferResponse { buffer_name }
            )) if buffer_name.is_empty()
        ),
        "the legitimate empty-name success must remain a completed response"
    );
}

#[tokio::test]
async fn ordered_paste_reports_stale_identity_without_wire_sentinel() {
    let handler = RequestHandler::new();
    create_session(&handler, "ordered-paste-stale").await;
    let set = handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("shared"),
            b"old",
        ))))
        .await;
    assert!(matches!(set, Response::SetBuffer(_)), "{set:?}");
    let captured_order = handler
        .state
        .lock()
        .await
        .buffers
        .show_with_order(Some("shared"))
        .expect("captured buffer exists")
        .2;

    let deleted = handler
        .handle(Request::DeleteBuffer(DeleteBufferRequest {
            name: Some("shared".to_owned()),
        }))
        .await;
    assert!(matches!(deleted, Response::DeleteBuffer(_)), "{deleted:?}");
    let replacement = handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("shared"),
            b"replacement",
        ))))
        .await;
    assert!(
        matches!(replacement, Response::SetBuffer(_)),
        "{replacement:?}"
    );

    let result = handler
        .handle_paste_buffer_for_order(
            paste_buffer_request(
                Some("shared"),
                PaneTarget::new(session_name("ordered-paste-stale"), 0),
                true,
            ),
            captured_order,
        )
        .await;
    assert!(matches!(result, OrderedPasteBufferResult::StaleIdentity));

    let replacement = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("shared".to_owned()),
        }))
        .await;
    assert_eq!(
        replacement
            .command_output()
            .expect("stale paste must preserve the replacement")
            .stdout(),
        b"replacement"
    );
}

#[tokio::test]
async fn implicit_show_buffer_ignores_newer_named_buffers() {
    let handler = RequestHandler::new();

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("alpha"),
            b"v1",
        ))))
        .await;
    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None, b"unnamed",
        ))))
        .await;
    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("alpha"),
            b"value-two",
        ))))
        .await;

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    let output = show.command_output().expect("show-buffer returns output");
    assert_eq!(output.stdout(), b"unnamed");
}

#[tokio::test]
async fn implicit_paste_buffer_ignores_newer_named_buffers() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None, b"unnamed",
        ))))
        .await;
    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("named"),
            b"value-two",
        ))))
        .await;

    let response = handler
        .handle(Request::PasteBuffer(Box::new(paste_buffer_request(
            None,
            PaneTarget::new(session_name("alpha"), 0),
            false,
        ))))
        .await;

    match response {
        Response::PasteBuffer(response) => assert_eq!(response.buffer_name, "buffer0"),
        other => panic!("unexpected response: {other:?}"),
    }
}

#[tokio::test]
async fn paste_buffer_nonexistent_pane_returns_error() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None, b"data",
        ))))
        .await;

    // Pane 5 doesn't exist in window 0
    let response = handler
        .handle(Request::PasteBuffer(Box::new(paste_buffer_request(
            None,
            PaneTarget::with_window(session_name("alpha"), 0, 5),
            false,
        ))))
        .await;

    assert!(matches!(response, Response::Error(_)));

    // Buffer should still exist (not deleted on write failure)
    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    assert!(matches!(show, Response::ShowBuffer(_)));
}

#[tokio::test]
async fn paste_buffer_dead_remain_on_exit_pane_returns_clean_error() {
    let handler = RequestHandler::new();
    let alpha_name = "alpha-dead-paste";
    let alpha = session_name(alpha_name);
    create_session(&handler, alpha_name).await;

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                option: OptionName::RemainOnExit,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                target: PaneTarget::with_window(alpha.clone(), 0, 0),
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(vec!["exit 0".to_owned()]),
                process_command: None,
            })))
            .await,
        Response::RespawnPane(_)
    ));
    wait_for_dead_pane(&handler, &alpha, 0, 0).await;

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None, b"data",
        ))))
        .await;

    let response = handler
        .handle(Request::PasteBuffer(Box::new(paste_buffer_request(
            None,
            PaneTarget::with_window(alpha, 0, 0),
            true,
        ))))
        .await;

    assert!(
        matches!(&response, Response::Error(error) if error.error.to_string().contains("target pane has exited")),
        "expected dead-pane error, got {response:?}"
    );
    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    assert!(matches!(show, Response::ShowBuffer(_)));
}

#[tokio::test]
async fn paste_buffer_with_delete_nonexistent_pane_preserves_buffer() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            None, b"data",
        ))))
        .await;

    // Pane 5 doesn't exist - paste fails, buffer should NOT be deleted
    let response = handler
        .handle(Request::PasteBuffer(Box::new(paste_buffer_request(
            None,
            PaneTarget::with_window(session_name("alpha"), 0, 5),
            true,
        ))))
        .await;

    assert!(matches!(response, Response::Error(_)));

    // Buffer must still be intact despite delete_after=true
    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    assert!(matches!(show, Response::ShowBuffer(_)));
}

#[tokio::test]
async fn delete_buffer_by_explicit_name_works() {
    let handler = RequestHandler::new();

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("target"),
            b"data",
        ))))
        .await;
    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("other"),
            b"keep",
        ))))
        .await;

    let response = handler
        .handle(Request::DeleteBuffer(DeleteBufferRequest {
            name: Some("target".to_owned()),
        }))
        .await;

    match response {
        Response::DeleteBuffer(r) => assert_eq!(r.buffer_name, "target"),
        other => panic!("unexpected response: {other:?}"),
    }

    // other buffer should still exist
    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("other".to_owned()),
        }))
        .await;
    assert!(matches!(show, Response::ShowBuffer(_)));
}

#[tokio::test]
async fn set_buffer_empty_content_is_not_listed() {
    let handler = RequestHandler::new();

    let set = handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            Some("empty"),
            b"",
        ))))
        .await;
    match set {
        Response::SetBuffer(response) => assert!(response.buffer_name.is_empty()),
        other => panic!("unexpected response: {other:?}"),
    }

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("empty".to_owned()),
        }))
        .await;
    assert!(matches!(show, Response::Error(_)));

    // List should remain empty because zero-size content does not create a buffer.
    let list = handler
        .handle(Request::ListBuffers(ListBuffersRequest::default()))
        .await;
    let list_output = list.command_output().expect("list-buffers returns output");
    assert!(list_output.stdout().is_empty());
}

#[tokio::test]
async fn set_buffer_clipboard_write_uses_attached_terminal_features() {
    let handler = RequestHandler::new();
    let session = session_name("alpha");
    create_session(&handler, "alpha").await;

    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            41,
            session,
            control_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        )
        .await;

    let mut request = set_buffer_request(None, b"hello clipboard");
    request.set_clipboard = true;
    let response = handler
        .dispatch(41, Request::SetBuffer(Box::new(request)))
        .await
        .response;
    assert!(matches!(response, Response::SetBuffer(_)));

    let bytes = take_write(control_rx.try_recv().expect("clipboard write"));
    assert_eq!(bytes, b"\x1b]52;;aGVsbG8gY2xpcGJvYXJk\x07");
}

#[tokio::test]
async fn set_buffer_clipboard_write_flag_overrides_set_clipboard_off() {
    let handler = RequestHandler::new();
    let session = session_name("alpha");
    create_session(&handler, "alpha").await;

    let set_option = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::SetClipboard,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_option, Response::SetOption(_)));

    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            41,
            session,
            control_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        )
        .await;

    let mut request = set_buffer_request(None, b"forced clipboard");
    request.set_clipboard = true;
    let response = handler
        .dispatch(41, Request::SetBuffer(Box::new(request)))
        .await
        .response;
    assert!(matches!(response, Response::SetBuffer(_)));

    let bytes = take_write(control_rx.try_recv().expect("forced clipboard write"));
    assert_eq!(bytes, b"\x1b]52;;Zm9yY2VkIGNsaXBib2FyZA==\x07");
}

#[tokio::test]
async fn clipboard_writes_require_explicit_buffer_command_flag() {
    let handler = RequestHandler::new();
    let session = session_name("alpha");
    create_session(&handler, "alpha").await;

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::SetClipboard,
                value: "external".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            41,
            session,
            control_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        )
        .await;

    let response = handler
        .dispatch(
            41,
            Request::SetBuffer(Box::new(set_buffer_request(None, b"private"))),
        )
        .await
        .response;
    assert!(matches!(response, Response::SetBuffer(_)));
    assert!(
        control_rx.try_recv().is_err(),
        "set-buffer must not write OSC52 unless -w/set_clipboard is requested"
    );

    let temp_path = std::env::temp_dir().join(format!(
        "rmux-load-buffer-no-clipboard-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos()
    ));
    fs::write(&temp_path, b"loaded private").expect("write test buffer");
    let response = handler
        .dispatch(
            41,
            Request::LoadBuffer(Box::new(load_buffer_request(
                temp_path.to_str().expect("utf8 temp path"),
            ))),
        )
        .await
        .response;
    let _ = fs::remove_file(&temp_path);
    assert!(matches!(response, Response::LoadBuffer(_)));
    assert!(
        control_rx.try_recv().is_err(),
        "load-buffer must not write OSC52 unless -w/set_clipboard is requested"
    );
}

#[tokio::test]
async fn set_buffer_clipboard_write_is_suppressed_without_unique_attached_client() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let (first_tx, mut first_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            101,
            alpha.clone(),
            first_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        )
        .await;
    let (second_tx, mut second_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            202,
            alpha,
            second_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        )
        .await;

    let mut request = set_buffer_request(None, b"ambiguous");
    request.set_clipboard = true;
    let response = handler
        .dispatch(303, Request::SetBuffer(Box::new(request)))
        .await
        .response;
    assert!(matches!(response, Response::SetBuffer(_)));
    assert!(first_rx.try_recv().is_err());
    assert!(second_rx.try_recv().is_err());
}

#[tokio::test]
async fn set_buffer_target_client_writes_only_to_the_selected_client() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let terminal = OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]);
    let (first_tx, mut first_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(101, alpha.clone(), first_tx, terminal.clone())
        .await;
    let (second_tx, mut second_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(202, alpha, second_tx, terminal)
        .await;

    let mut request = set_buffer_request(Some("targeted"), b"selected clipboard");
    request.set_clipboard = true;
    request.target_client = Some("202".to_owned());
    let response = handler
        .dispatch(303, Request::SetBuffer(Box::new(request)))
        .await
        .response;

    assert!(matches!(response, Response::SetBuffer(_)));
    assert!(first_rx.try_recv().is_err());
    assert_eq!(
        take_write(second_rx.try_recv().expect("selected clipboard write")),
        b"\x1b]52;;c2VsZWN0ZWQgY2xpcGJvYXJk\x07"
    );
}

#[tokio::test]
async fn load_buffer_target_client_writes_only_to_the_selected_client() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let terminal = OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]);
    let (first_tx, mut first_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(101, alpha.clone(), first_tx, terminal.clone())
        .await;
    let (second_tx, mut second_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(202, alpha, second_tx, terminal)
        .await;

    let temp_path = std::env::temp_dir().join(format!(
        "rmux-load-buffer-target-client-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos()
    ));
    fs::write(&temp_path, b"loaded selected").expect("write test buffer");
    let mut request = load_buffer_request(temp_path.to_str().expect("utf8 temp path"));
    request.set_clipboard = true;
    request.target_client = Some("101".to_owned());
    let response = handler
        .dispatch(303, Request::LoadBuffer(Box::new(request)))
        .await
        .response;
    let _ = fs::remove_file(&temp_path);

    assert!(matches!(response, Response::LoadBuffer(_)));
    assert_eq!(
        take_write(first_rx.try_recv().expect("selected clipboard write")),
        b"\x1b]52;;bG9hZGVkIHNlbGVjdGVk\x07"
    );
    assert!(second_rx.try_recv().is_err());
}

#[tokio::test]
async fn missing_buffer_target_client_is_a_successful_clipboard_noop() {
    let handler = RequestHandler::new();
    let mut request = set_buffer_request(Some("stored"), b"still stored");
    request.set_clipboard = true;
    request.target_client = Some("missing-client".to_owned());

    let response = handler
        .dispatch(303, Request::SetBuffer(Box::new(request)))
        .await
        .response;
    assert!(matches!(response, Response::SetBuffer(_)));
    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("stored".to_owned()),
            }))
            .await
            .command_output()
            .expect("stored buffer")
            .stdout(),
        b"still stored"
    );
}

#[tokio::test]
async fn load_buffer_clipboard_write_flag_overrides_set_clipboard_off() {
    let handler = RequestHandler::new();
    let session = session_name("alpha");
    create_session(&handler, "alpha").await;

    let set_option = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::SetClipboard,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_option, Response::SetOption(_)));

    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach_with_terminal_context(
            77,
            session,
            control_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        )
        .await;

    let temp_path = std::env::temp_dir().join(format!(
        "rmux-load-buffer-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos()
    ));
    fs::write(&temp_path, b"loaded clipboard").expect("write test buffer");

    let mut request = load_buffer_request(temp_path.to_str().expect("utf8 temp path"));
    request.set_clipboard = true;
    let response = handler
        .dispatch(77, Request::LoadBuffer(Box::new(request)))
        .await
        .response;
    let _ = fs::remove_file(&temp_path);
    assert!(matches!(response, Response::LoadBuffer(_)));
    let bytes = take_write(control_rx.try_recv().expect("forced clipboard write"));
    assert_eq!(bytes, b"\x1b]52;;bG9hZGVkIGNsaXBib2FyZA==\x07");
}
