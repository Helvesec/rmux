use super::RequestHandler;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;
use rmux_proto::{
    DeleteBufferRequest, ErrorResponse, ListBuffersRequest, LoadBufferRequest, NewSessionRequest,
    OptionName, PaneTarget, PasteBufferRequest, Request, Response, RmuxError, ScopeSelector,
    SetBufferRequest, SetOptionMode, SetOptionRequest, ShowBufferRequest, TerminalSize,
};
use std::fs;
use std::sync::Arc;
use std::time::Duration;

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
    }
}

fn load_buffer_request(path: &str) -> LoadBufferRequest {
    LoadBufferRequest {
        path: path.to_owned(),
        cwd: None,
        name: None,
        set_clipboard: false,
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
        .handle(Request::SetBuffer(set_buffer_request(None, b"hello")))
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
        .handle(Request::SetBuffer(set_buffer_request(
            Some("my-buf"),
            b"data",
        )))
        .await;

    match response {
        Response::SetBuffer(r) => assert_eq!(r.buffer_name, "my-buf"),
        other => panic!("unexpected response: {other:?}"),
    }
}

#[tokio::test]
async fn set_buffer_skips_existing_named_buffer_pattern_for_unnamed() {
    let handler = RequestHandler::new();

    handler
        .handle(Request::SetBuffer(set_buffer_request(
            Some("buffer0"),
            b"named",
        )))
        .await;

    let response = handler
        .handle(Request::SetBuffer(set_buffer_request(None, b"unnamed")))
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
        .handle(Request::SetBuffer(set_buffer_request(Some(""), b"data")))
        .await;

    assert!(matches!(response, Response::Error(_)));
}

#[tokio::test]
async fn set_buffer_accepts_colon_in_name() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SetBuffer(set_buffer_request(Some("a:b"), b"data")))
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
        .handle(Request::SetBuffer(set_buffer_request(None, b"")))
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
        .handle(Request::SetBuffer(set_buffer_request(None, b"hello world")))
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
        .handle(Request::SetBuffer(set_buffer_request(None, b"a")))
        .await;
    handler
        .handle(Request::SetBuffer(set_buffer_request(None, b"b")))
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
        .handle(Request::SetBuffer(set_buffer_request(None, b"first")))
        .await;
    handler
        .handle(Request::SetBuffer(set_buffer_request(
            Some("named"),
            b"second",
        )))
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
        .handle(Request::SetBuffer(set_buffer_request(None, b"paste-me")))
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

#[tokio::test]
async fn paste_buffer_with_delete_removes_buffer_after_write() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    handler
        .handle(Request::SetBuffer(set_buffer_request(
            None,
            b"paste-then-delete",
        )))
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
        .handle(Request::SetBuffer(set_buffer_request(
            Some("shared"),
            b"old",
        )))
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

    let replace = handler
        .handle(Request::SetBuffer(set_buffer_request(
            Some("shared"),
            b"new",
        )))
        .await;
    assert!(matches!(replace, Response::SetBuffer(_)));

    pause.release.notify_one();

    let response = paste.await.expect("paste-buffer task should join");
    assert!(matches!(response, Response::PasteBuffer(_)));

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
async fn paste_buffer_nonexistent_session_returns_error() {
    let handler = RequestHandler::new();

    handler
        .handle(Request::SetBuffer(set_buffer_request(None, b"data")))
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

    assert!(matches!(response, Response::PasteBuffer(_)));
}

#[tokio::test]
async fn implicit_show_buffer_ignores_newer_named_buffers() {
    let handler = RequestHandler::new();

    handler
        .handle(Request::SetBuffer(set_buffer_request(Some("alpha"), b"v1")))
        .await;
    handler
        .handle(Request::SetBuffer(set_buffer_request(None, b"unnamed")))
        .await;
    handler
        .handle(Request::SetBuffer(set_buffer_request(
            Some("alpha"),
            b"value-two",
        )))
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
        .handle(Request::SetBuffer(set_buffer_request(None, b"unnamed")))
        .await;
    handler
        .handle(Request::SetBuffer(set_buffer_request(
            Some("named"),
            b"value-two",
        )))
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
        .handle(Request::SetBuffer(set_buffer_request(None, b"data")))
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
async fn paste_buffer_with_delete_nonexistent_pane_preserves_buffer() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    handler
        .handle(Request::SetBuffer(set_buffer_request(None, b"data")))
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
        .handle(Request::SetBuffer(set_buffer_request(
            Some("target"),
            b"data",
        )))
        .await;
    handler
        .handle(Request::SetBuffer(set_buffer_request(
            Some("other"),
            b"keep",
        )))
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
        .handle(Request::SetBuffer(set_buffer_request(Some("empty"), b"")))
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
        .dispatch(41, Request::SetBuffer(request))
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
        .dispatch(41, Request::SetBuffer(request))
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
        .dispatch(41, Request::SetBuffer(set_buffer_request(None, b"private")))
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
            Request::LoadBuffer(load_buffer_request(
                temp_path.to_str().expect("utf8 temp path"),
            )),
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
        .dispatch(303, Request::SetBuffer(request))
        .await
        .response;
    assert!(matches!(response, Response::SetBuffer(_)));
    assert!(first_rx.try_recv().is_err());
    assert!(second_rx.try_recv().is_err());
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
        .dispatch(77, Request::LoadBuffer(request))
        .await
        .response;
    let _ = fs::remove_file(&temp_path);
    assert!(matches!(response, Response::LoadBuffer(_)));
    let bytes = take_write(control_rx.try_recv().expect("forced clipboard write"));
    assert_eq!(bytes, b"\x1b]52;;bG9hZGVkIGNsaXBib2FyZA==\x07");
}
