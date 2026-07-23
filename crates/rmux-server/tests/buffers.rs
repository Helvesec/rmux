#![cfg(unix)]

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod common;

use common::{send_request, session_name, start_server, TestHarness};
use rmux_proto::{
    DaemonStatusRequest, DeleteBufferRequest, KillServerRequest, ListBuffersRequest,
    LoadBufferRequest, NewSessionRequest, PaneTarget, PasteBufferRequest, Request, Response,
    SaveBufferRequest, SetBufferRequest, ShowBufferRequest, SourceFileRequest, TerminalSize,
};

const FIFO_REQUEST_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

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
async fn client_disconnect_cancels_load_buffer_blocked_on_fifo_without_shutdown(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-drop-load-fifo");
    let handle = start_server(&harness).await?;
    let fifo_path = fifo_path(&harness, "load.fifo");
    create_fifo(&fifo_path)?;

    let request = Request::LoadBuffer(Box::new(LoadBufferRequest {
        path: fifo_path.display().to_string(),
        cwd: None,
        name: Some("blocked".to_owned()),
        set_clipboard: false,
        target_client: None,
    }));
    assert_peer_disconnect_cleans_blocked_request(&harness, request).await?;

    request_kill_server(harness.socket_path()).await?;
    handle.wait().await?;
    Ok(())
}

#[tokio::test]
async fn client_disconnect_cancels_save_buffer_blocked_on_fifo_without_shutdown(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-drop-save-fifo");
    let handle = start_server(&harness).await?;
    let fifo_path = fifo_path(&harness, "save.fifo");
    create_fifo(&fifo_path)?;

    send_request(
        harness.socket_path(),
        &Request::SetBuffer(Box::new(SetBufferRequest {
            name: Some("blocked".to_owned()),
            content: b"blocked write".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })),
    )
    .await?;
    let request = Request::SaveBuffer(SaveBufferRequest {
        path: fifo_path.display().to_string(),
        cwd: None,
        name: Some("blocked".to_owned()),
        append: false,
    });
    assert_peer_disconnect_cleans_blocked_request(&harness, request).await?;

    request_kill_server(harness.socket_path()).await?;
    handle.wait().await?;
    Ok(())
}

#[test]
fn client_disconnect_cancels_sourced_load_buffer_blocked_on_fifo_without_shutdown(
) -> Result<(), Box<dyn Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()?;
    runtime.block_on(client_disconnect_cancels_sourced_load_buffer_blocked_on_fifo())
}

async fn client_disconnect_cancels_sourced_load_buffer_blocked_on_fifo(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-drop-source-fifo");
    let handle = start_server(&harness).await?;
    let fifo_path = fifo_path(&harness, "source-load.fifo");
    create_fifo(&fifo_path)?;

    let request = Request::SourceFile(Box::new(SourceFileRequest {
        paths: vec!["-".to_owned()],
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target: None,
        caller_cwd: None,
        stdin: Some(format!("load-buffer -b blocked {}\n", fifo_path.display())),
    }));
    assert_peer_disconnect_cleans_blocked_request(&harness, request).await?;

    request_kill_server(harness.socket_path()).await?;
    handle.wait().await?;
    Ok(())
}

async fn assert_peer_disconnect_cleans_blocked_request(
    harness: &TestHarness,
    request: Request,
) -> Result<(), Box<dyn Error>> {
    let socket_path = harness.socket_path().to_path_buf();
    let request_socket_path = socket_path.clone();
    let request_task = tokio::spawn(async move {
        let _ = send_request(&request_socket_path, &request).await;
    });

    wait_for_daemon_client_count(&socket_path, 2).await?;
    request_task.abort();
    assert!(
        request_task
            .await
            .expect_err("aborted FIFO client task must not complete")
            .is_cancelled(),
        "FIFO client task must be cancelled"
    );

    wait_for_daemon_client_count(&socket_path, 0).await?;
    assert!(matches!(
        send_request(
            &socket_path,
            &Request::ListBuffers(ListBuffersRequest::default()),
        )
        .await?,
        Response::ListBuffers(_)
    ));
    Ok(())
}

async fn wait_for_daemon_client_count(
    socket_path: &Path,
    expected: usize,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + FIFO_REQUEST_CLEANUP_TIMEOUT;
    loop {
        let response =
            send_request(socket_path, &Request::DaemonStatus(DaemonStatusRequest)).await?;
        let Response::DaemonStatus(status) = response else {
            return Err(format!("expected daemon-status response, got {response:?}").into());
        };
        if status.client_count == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "daemon client count did not become {expected}; last observed {}",
                status.client_count
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn fifo_path(harness: &TestHarness, name: &str) -> PathBuf {
    harness
        .socket_path()
        .parent()
        .expect("test socket has parent")
        .join(name)
}

#[tokio::test]
async fn kill_server_cancels_load_buffer_blocked_opening_fifo() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-kill-load-fifo");
    let handle = start_server(&harness).await?;
    let fifo_path = harness
        .socket_path()
        .parent()
        .expect("test socket has parent")
        .join("load.fifo");
    create_fifo(&fifo_path)?;

    let request = Request::LoadBuffer(Box::new(LoadBufferRequest {
        path: fifo_path.display().to_string(),
        cwd: None,
        name: Some("blocked".to_owned()),
        set_clipboard: false,
        target_client: None,
    }));
    let mut blocked_request = Box::pin(send_request(harness.socket_path(), &request));
    assert_request_stays_blocked(blocked_request.as_mut()).await;

    request_kill_server(harness.socket_path()).await?;
    tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait())
        .await
        .expect("daemon shutdown must not wait for the blocked FIFO reader")?;
    assert_blocked_request_was_disconnected(blocked_request.as_mut()).await;
    Ok(())
}

#[tokio::test]
async fn kill_server_cancels_save_buffer_blocked_opening_fifo() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-kill-save-fifo");
    let handle = start_server(&harness).await?;
    let fifo_path = harness
        .socket_path()
        .parent()
        .expect("test socket has parent")
        .join("save.fifo");
    create_fifo(&fifo_path)?;

    send_request(
        harness.socket_path(),
        &Request::SetBuffer(Box::new(SetBufferRequest {
            name: Some("blocked".to_owned()),
            content: b"blocked write".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })),
    )
    .await?;

    let request = Request::SaveBuffer(SaveBufferRequest {
        path: fifo_path.display().to_string(),
        cwd: None,
        name: Some("blocked".to_owned()),
        append: false,
    });
    let mut blocked_request = Box::pin(send_request(harness.socket_path(), &request));
    assert_request_stays_blocked(blocked_request.as_mut()).await;

    request_kill_server(harness.socket_path()).await?;
    tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait())
        .await
        .expect("daemon shutdown must not wait for the blocked FIFO writer")?;
    assert_blocked_request_was_disconnected(blocked_request.as_mut()).await;
    Ok(())
}

async fn assert_request_stays_blocked<F>(mut request: std::pin::Pin<&mut F>)
where
    F: std::future::Future<Output = Result<Response, Box<dyn Error>>> + ?Sized,
{
    tokio::select! {
        response = request.as_mut() => panic!("FIFO request unexpectedly completed: {response:?}"),
        () = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
    }
}

async fn assert_blocked_request_was_disconnected<F>(request: std::pin::Pin<&mut F>)
where
    F: std::future::Future<Output = Result<Response, Box<dyn Error>>> + ?Sized,
{
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), request)
        .await
        .expect("blocked FIFO client should observe daemon shutdown");
    assert!(
        matches!(response, Err(_) | Ok(Response::Error(_))),
        "cancelled FIFO request must not report successful I/O"
    );
}

async fn request_kill_server(socket_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    match send_request(socket_path, &Request::KillServer(KillServerRequest)).await {
        Ok(Response::KillServer(_)) | Err(_) => Ok(()),
        Ok(other) => Err(format!("unexpected kill-server response: {other:?}").into()),
    }
}

fn create_fifo(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let output = std::process::Command::new("mkfifo").arg(path).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("mkfifo failed: {}", String::from_utf8_lossy(&output.stderr)).into())
    }
}

#[tokio::test]
async fn set_and_show_buffer_round_trips_through_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("buf-set-show");
    let handle = start_server(&harness).await?;

    let set_response = send_request(
        harness.socket_path(),
        &Request::SetBuffer(Box::new(SetBufferRequest {
            name: None,
            content: b"hello world".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })),
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
        &Request::SetBuffer(Box::new(SetBufferRequest {
            name: Some("alpha".to_owned()),
            content: b"first".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })),
    )
    .await?;

    send_request(
        harness.socket_path(),
        &Request::SetBuffer(Box::new(SetBufferRequest {
            name: None,
            content: b"second".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })),
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
        &Request::SetBuffer(Box::new(SetBufferRequest {
            name: None,
            content: b"a".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })),
    )
    .await?;

    send_request(
        harness.socket_path(),
        &Request::SetBuffer(Box::new(SetBufferRequest {
            name: None,
            content: b"b".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })),
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
        &Request::SetBuffer(Box::new(SetBufferRequest {
            name: None,
            content: b"paste-me".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })),
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
        &Request::SetBuffer(Box::new(SetBufferRequest {
            name: None,
            content: b"temp".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })),
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
