#![cfg(unix)]

use std::error::Error;

mod common;

use common::{send_request, session_name, start_server, TestHarness};
use rmux_proto::{
    DeleteBufferRequest, ListBuffersRequest, NewSessionRequest, PaneTarget, PasteBufferRequest,
    Request, Response, SetBufferRequest, ShowBufferRequest, TerminalSize,
};

async fn create_session(harness: &TestHarness, name: &str) -> Result<(), Box<dyn Error>> {
    let response = send_request(
        harness.socket_path(),
        &Request::NewSession(NewSessionRequest {
            session_name: session_name(name),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(response, Response::NewSession(_)));
    Ok(())
}

#[tokio::test]
async fn set_and_show_buffer_round_trips_through_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-set-show");
    let handle = start_server(&harness).await?;

    let set_response = send_request(
        harness.socket_path(),
        &Request::SetBuffer(SetBufferRequest {
            name: None,
            content: b"hello world".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
        }),
    )
    .await?;

    match &set_response {
        Response::SetBuffer(r) => assert_eq!(r.buffer_name, "buffer0"),
        other => panic!("expected SetBuffer, got {other:?}"),
    }

    let show_response = send_request(
        harness.socket_path(),
        &Request::ShowBuffer(ShowBufferRequest { name: None }),
    )
    .await?;

    let output = show_response
        .command_output()
        .expect("show-buffer returns output");
    assert_eq!(output.stdout(), b"hello world");

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn list_buffers_returns_formatted_output_through_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-list");
    let handle = start_server(&harness).await?;

    send_request(
        harness.socket_path(),
        &Request::SetBuffer(SetBufferRequest {
            name: Some("alpha".to_owned()),
            content: b"first".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
        }),
    )
    .await?;

    send_request(
        harness.socket_path(),
        &Request::SetBuffer(SetBufferRequest {
            name: None,
            content: b"second".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
        }),
    )
    .await?;

    let list_response = send_request(
        harness.socket_path(),
        &Request::ListBuffers(ListBuffersRequest::default()),
    )
    .await?;

    let output = list_response
        .command_output()
        .expect("list-buffers returns output");
    let stdout = std::str::from_utf8(output.stdout()).expect("utf8");
    assert!(stdout.contains("alpha:"));
    assert!(stdout.contains("buffer0:"));

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn delete_buffer_removes_stack_head_through_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-delete");
    let handle = start_server(&harness).await?;

    send_request(
        harness.socket_path(),
        &Request::SetBuffer(SetBufferRequest {
            name: None,
            content: b"a".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
        }),
    )
    .await?;

    send_request(
        harness.socket_path(),
        &Request::SetBuffer(SetBufferRequest {
            name: None,
            content: b"b".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
        }),
    )
    .await?;

    let delete_response = send_request(
        harness.socket_path(),
        &Request::DeleteBuffer(DeleteBufferRequest { name: None }),
    )
    .await?;

    match &delete_response {
        Response::DeleteBuffer(r) => assert_eq!(r.buffer_name, "buffer1"),
        other => panic!("expected DeleteBuffer, got {other:?}"),
    }

    // Remaining buffer should be buffer0
    let show = send_request(
        harness.socket_path(),
        &Request::ShowBuffer(ShowBufferRequest { name: None }),
    )
    .await?;
    let output = show.command_output().expect("show-buffer returns output");
    assert_eq!(output.stdout(), b"a");

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn paste_buffer_to_session_pane_through_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-paste");
    let handle = start_server(&harness).await?;
    create_session(&harness, "alpha").await?;

    send_request(
        harness.socket_path(),
        &Request::SetBuffer(SetBufferRequest {
            name: None,
            content: b"paste-me".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
        }),
    )
    .await?;

    let paste_response = send_request(
        harness.socket_path(),
        &Request::PasteBuffer(Box::new(PasteBufferRequest {
            name: None,
            target: PaneTarget::new(session_name("alpha"), 0),
            delete_after: false,
            separator: None,
            linefeed: false,
            raw: false,
            bracketed: false,
        })),
    )
    .await?;

    match &paste_response {
        Response::PasteBuffer(r) => assert_eq!(r.buffer_name, "buffer0"),
        other => panic!("expected PasteBuffer, got {other:?}"),
    }

    // Buffer should still exist
    let show = send_request(
        harness.socket_path(),
        &Request::ShowBuffer(ShowBufferRequest { name: None }),
    )
    .await?;
    assert!(matches!(show, Response::ShowBuffer(_)));

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn paste_buffer_without_buffers_is_successful_noop_through_real_socket(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-paste-empty");
    let handle = start_server(&harness).await?;
    create_session(&harness, "alpha").await?;

    let paste_response = send_request(
        harness.socket_path(),
        &Request::PasteBuffer(Box::new(PasteBufferRequest {
            name: None,
            target: PaneTarget::new(session_name("alpha"), 0),
            delete_after: false,
            separator: None,
            linefeed: false,
            raw: false,
            bracketed: false,
        })),
    )
    .await?;

    match &paste_response {
        Response::PasteBuffer(r) => assert_eq!(r.buffer_name, ""),
        other => panic!("expected PasteBuffer empty success, got {other:?}"),
    }

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn paste_buffer_with_delete_removes_buffer_through_real_socket() -> Result<(), Box<dyn Error>>
{
    let harness = TestHarness::new("buf-paste-del");
    let handle = start_server(&harness).await?;
    create_session(&harness, "alpha").await?;

    send_request(
        harness.socket_path(),
        &Request::SetBuffer(SetBufferRequest {
            name: None,
            content: b"temp".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
        }),
    )
    .await?;

    let paste_response = send_request(
        harness.socket_path(),
        &Request::PasteBuffer(Box::new(PasteBufferRequest {
            name: None,
            target: PaneTarget::new(session_name("alpha"), 0),
            delete_after: true,
            separator: None,
            linefeed: false,
            raw: false,
            bracketed: false,
        })),
    )
    .await?;
    assert!(matches!(paste_response, Response::PasteBuffer(_)));

    // Buffer should be gone
    let show = send_request(
        harness.socket_path(),
        &Request::ShowBuffer(ShowBufferRequest { name: None }),
    )
    .await?;
    assert!(matches!(show, Response::Error(_)));

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn delete_nonexistent_buffer_returns_error_through_real_socket() -> Result<(), Box<dyn Error>>
{
    let harness = TestHarness::new("buf-del-missing");
    let handle = start_server(&harness).await?;

    let response = send_request(
        harness.socket_path(),
        &Request::DeleteBuffer(DeleteBufferRequest {
            name: Some("missing".to_owned()),
        }),
    )
    .await?;
    assert!(matches!(response, Response::Error(_)));

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn show_buffer_empty_store_returns_error_through_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-show-empty");
    let handle = start_server(&harness).await?;

    let response = send_request(
        harness.socket_path(),
        &Request::ShowBuffer(ShowBufferRequest { name: None }),
    )
    .await?;
    assert!(matches!(response, Response::Error(_)));

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn list_buffers_empty_returns_empty_output_through_real_socket() -> Result<(), Box<dyn Error>>
{
    let harness = TestHarness::new("buf-list-empty");
    let handle = start_server(&harness).await?;

    let response = send_request(
        harness.socket_path(),
        &Request::ListBuffers(ListBuffersRequest::default()),
    )
    .await?;

    let output = response
        .command_output()
        .expect("list-buffers returns output");
    assert!(output.stdout().is_empty());

    handle.shutdown().await?;
    Ok(())
}
