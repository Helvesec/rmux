use std::error::Error;

use rmux_proto::{
    encode_attach_message, encode_frame, AttachMessage, AttachSessionRequest,
    AttachSessionResponse, DetachClientRequest, KillSessionRequest, NewSessionRequest, OptionName,
    PaneTarget, Request, Response, ScopeSelector, SelectPaneRequest, SendKeysRequest,
    SetOptionMode, SetOptionRequest, SplitWindowRequest, SplitWindowTarget, TerminalSize,
    WindowTarget,
};
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

use crate::common::{session_name, start_server, ClientConnection, TestHarness, PTY_TEST_LOCK};
use crate::support::{
    read_attach_until_contains, read_attach_until_eof, read_response_exact, retry_request_until,
    STEP_TIMEOUT,
};

async fn send_attach_command(
    stream: &mut tokio::net::UnixStream,
    command: &str,
) -> Result<(), Box<dyn Error>> {
    let mut bytes = command.as_bytes().to_vec();
    bytes.push(b'\r');
    let frame = encode_attach_message(&AttachMessage::Data(bytes))?;
    stream.write_all(&frame).await?;
    Ok(())
}

async fn send_attach_bytes(
    stream: &mut tokio::net::UnixStream,
    bytes: &[u8],
) -> Result<(), Box<dyn Error>> {
    let frame = encode_attach_message(&AttachMessage::Data(bytes.to_vec()))?;
    stream.write_all(&frame).await?;
    Ok(())
}

#[tokio::test]
async fn attach_stream_forwards_bytes_resize_and_client_eof() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("attach-forwarding");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));
    let (response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: session_name("alpha"),
        })
        .await?;
    assert_eq!(response.session_name, session_name("alpha"));

    send_attach_command(&mut attach_stream, "printf from-client").await?;
    let client_output =
        read_attach_until_contains(&mut attach_stream, "from-client", STEP_TIMEOUT).await?;
    assert!(client_output.contains("from-client"));

    let pane_output = crate::common::send_request(
        &socket_path,
        &Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(session_name("alpha"), 0),
            keys: vec!["printf from-pane".to_owned(), "Enter".to_owned()],
        }),
    )
    .await?;
    assert!(matches!(pane_output, Response::SendKeys(_)));
    let pane_output =
        read_attach_until_contains(&mut attach_stream, "from-pane", STEP_TIMEOUT).await?;
    assert!(pane_output.contains("from-pane"));

    let resize = TerminalSize { cols: 91, rows: 33 };
    let frame = encode_attach_message(&AttachMessage::Resize(resize))?;
    attach_stream.write_all(&frame).await?;
    send_attach_command(&mut attach_stream, "stty size").await?;
    let resized_output =
        read_attach_until_contains(&mut attach_stream, "32 91", STEP_TIMEOUT).await?;
    assert!(resized_output.contains("32 91"));

    drop(attach_stream);
    let removed = crate::common::send_request(
        &socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await?;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn attach_stream_emits_border_frames_for_multi_pane_sessions() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("attach-borders");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");

    let created = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let split_main = crate::common::send_request(
        &socket_path,
        &Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Horizontal,
            before: false,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(split_main, Response::SplitWindow(_)));

    let split_right = crate::common::send_request(
        &socket_path,
        &Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(PaneTarget::new(alpha.clone(), 1)),
            direction: rmux_proto::SplitDirection::Horizontal,
            before: false,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(split_right, Response::SplitWindow(_)));

    let selected = crate::common::send_request(
        &socket_path,
        &Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(alpha.clone(), 2),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })),
    )
    .await?;
    assert!(matches!(selected, Response::SelectPane(_)));

    for (scope, option, value) in [
        (
            ScopeSelector::Window(WindowTarget::new(alpha.clone())),
            OptionName::PaneBorderStyle,
            "blue",
        ),
        (
            ScopeSelector::Window(WindowTarget::new(alpha.clone())),
            OptionName::PaneActiveBorderStyle,
            "colour196",
        ),
        (
            ScopeSelector::Session(alpha.clone()),
            OptionName::Status,
            "off",
        ),
    ] {
        let response = crate::common::send_request(
            &socket_path,
            &Request::SetOption(SetOptionRequest {
                scope,
                option,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }),
        )
        .await?;
        assert!(matches!(response, Response::SetOption(_)));
    }

    let mut attach_stream = tokio::net::UnixStream::connect(&socket_path).await?;
    let request = encode_frame(&Request::AttachSession(AttachSessionRequest {
        target: alpha,
    }))?;
    attach_stream.write_all(&request).await?;
    assert_eq!(
        read_response_exact(&mut attach_stream).await?,
        Response::AttachSession(AttachSessionResponse {
            session_name: session_name("alpha"),
        })
    );

    let border_text =
        read_attach_until_contains(&mut attach_stream, "\u{1b}[34m", STEP_TIMEOUT).await?;

    assert!(border_text.contains("\u{1b}[34m"));
    assert!(border_text.contains("\u{1b}[38;5;196m"));
    assert!(border_text.contains('│'));
    assert!(border_text.matches('│').count() >= 2);

    drop(attach_stream);
    let removed = crate::common::send_request(
        &socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await?;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn attach_stream_preserves_bytes_sent_with_the_upgrade_request() -> Result<(), Box<dyn Error>>
{
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("attach-buffered-upgrade");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let created = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let mut stream = tokio::net::UnixStream::connect(&socket_path).await?;
    let mut bytes = encode_frame(&Request::AttachSession(AttachSessionRequest {
        target: session_name("alpha"),
    }))?;
    bytes.extend_from_slice(&encode_attach_message(&AttachMessage::Data(
        b"printf queued-upgrade\r".to_vec(),
    ))?);
    stream.write_all(&bytes).await?;

    assert_eq!(
        read_response_exact(&mut stream).await?,
        Response::AttachSession(AttachSessionResponse {
            session_name: session_name("alpha"),
        })
    );
    let output = read_attach_until_contains(&mut stream, "queued-upgrade", STEP_TIMEOUT).await?;
    assert!(output.contains("queued-upgrade"));

    drop(stream);
    let removed = crate::common::send_request(
        &socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await?;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn attach_stream_terminates_when_the_session_is_killed() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("attach-pane-eof");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let (_, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: session_name("alpha"),
        })
        .await?;
    read_attach_until_contains(&mut attach_stream, "[alpha]", STEP_TIMEOUT).await?;

    let removed = crate::common::send_request(
        &socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await?;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    read_attach_until_eof(&mut attach_stream, STEP_TIMEOUT).await?;

    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn detach_client_closes_the_attach_stream() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("detach-client");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let (_, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: session_name("alpha"),
        })
        .await?;
    read_attach_until_contains(&mut attach_stream, "[alpha]", STEP_TIMEOUT).await?;
    let detached =
        crate::common::send_request(&socket_path, &Request::DetachClient(DetachClientRequest))
            .await?;
    assert_eq!(
        detached,
        Response::DetachClient(rmux_proto::DetachClientResponse)
    );

    let detach_message = read_attach_until_contains(
        &mut attach_stream,
        "[detached (from session alpha)]",
        STEP_TIMEOUT,
    )
    .await?;
    assert!(detach_message.contains("[detached (from session alpha)]"));
    read_attach_until_eof(&mut attach_stream, STEP_TIMEOUT).await?;

    let removed = crate::common::send_request(
        &socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await?;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn attached_prefix_d_emits_detached_message_and_closes_stream() -> Result<(), Box<dyn Error>>
{
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("attached-prefix-detach");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let (_, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: session_name("alpha"),
        })
        .await?;
    read_attach_until_contains(&mut attach_stream, "[alpha]", STEP_TIMEOUT).await?;

    send_attach_bytes(&mut attach_stream, b"\x02d").await?;
    let detached = read_attach_until_contains(
        &mut attach_stream,
        "[detached (from session alpha)]",
        STEP_TIMEOUT,
    )
    .await?;
    assert!(detached.contains("[detached (from session alpha)]"));
    read_attach_until_eof(&mut attach_stream, STEP_TIMEOUT).await?;

    let removed = crate::common::send_request(
        &socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await?;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn pane_exit_emits_bracketed_exited_and_closes_attach_stream() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("attached-pane-exit");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let (_, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: session_name("alpha"),
        })
        .await?;
    read_attach_until_contains(&mut attach_stream, "[alpha]", STEP_TIMEOUT).await?;

    send_attach_bytes(&mut attach_stream, b"exit\r").await?;
    let exited = read_attach_until_contains(&mut attach_stream, "[exited]", STEP_TIMEOUT).await?;
    assert!(exited.contains("[exited]"));
    read_attach_until_eof(&mut attach_stream, STEP_TIMEOUT).await?;

    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn detach_client_clears_the_active_attach_state() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("detach-client-clears-state");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let (_, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: session_name("alpha"),
        })
        .await?;
    read_attach_until_contains(&mut attach_stream, "[alpha]", STEP_TIMEOUT).await?;

    let detached =
        crate::common::send_request(&socket_path, &Request::DetachClient(DetachClientRequest))
            .await?;
    assert_eq!(
        detached,
        Response::DetachClient(rmux_proto::DetachClientResponse)
    );

    read_attach_until_eof(&mut attach_stream, STEP_TIMEOUT).await?;

    let expected = Response::Error(rmux_proto::ErrorResponse {
        error: rmux_proto::RmuxError::Server(
            "detach-client requires an attached client".to_owned(),
        ),
    });
    let cleared_state = retry_request_until(
        &socket_path,
        &Request::DetachClient(DetachClientRequest),
        &expected,
    )
    .await?;
    assert_eq!(cleared_state, expected);

    let removed = crate::common::send_request(
        &socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await?;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}
