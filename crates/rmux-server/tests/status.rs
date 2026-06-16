#![cfg(unix)]

mod common;

use std::error::Error;
use std::io;
use std::time::Duration;

use common::{session_name, start_server, ClientConnection, TestHarness, PTY_TEST_LOCK};
use rmux_proto::{
    AttachMessage, AttachSessionRequest, ListClientsRequest, NewSessionRequest, OptionName,
    Request, Response, ScopeSelector, SetOptionMode, SetOptionRequest, SuspendClientRequest,
    TerminalSize,
};
use tokio::io::AsyncReadExt;
use tokio::time::{timeout, Instant};

const STEP_TIMEOUT: Duration = Duration::from_secs(3);

#[tokio::test]
async fn attach_session_emits_status_row_for_single_pane_session() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("status-attach");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");

    let created = common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 20, rows: 4 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let (_response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest { target: alpha })
        .await?;

    let status_text = read_attach_data_until_contains(&mut attach_stream, "[alpha]").await?;
    assert!(status_text.contains("[alpha]"));
    assert!(status_text.contains("\u{1b}[4;1H"));
    assert!(!status_text.contains('┬'));

    drop(attach_stream);
    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn attach_session_status_context_populates_session_attached() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("status-session-attached");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");

    let created = common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 30, rows: 4 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    for (option, value) in [
        (OptionName::StatusLeft, "attached=#{session_attached}"),
        (OptionName::StatusRight, ""),
    ] {
        let response = common::send_request(
            &socket_path,
            &Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                option,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }),
        )
        .await?;
        assert!(matches!(response, Response::SetOption(_)));
    }

    let (_response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest { target: alpha })
        .await?;

    let status_text = read_attach_data_until_contains(&mut attach_stream, "attached=1").await?;
    assert!(status_text.contains("attached=1"));

    drop(attach_stream);
    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn status_interval_refreshes_time_formats_without_pane_output() -> Result<(), Box<dyn Error>>
{
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("status-interval");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");

    let created = common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 40, rows: 4 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    for (option, value) in [
        (OptionName::StatusInterval, "1"),
        (OptionName::StatusLeft, "[#{session_name}] "),
        (OptionName::StatusRight, "tick=%S"),
    ] {
        let response = common::send_request(
            &socket_path,
            &Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                option,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }),
        )
        .await?;
        assert!(matches!(response, Response::SetOption(_)));
    }

    let (_response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest { target: alpha })
        .await?;

    let first_status = read_attach_data_until_contains(&mut attach_stream, "tick=").await?;
    let first_tick = extract_tick_second(&first_status)
        .ok_or_else(|| io::Error::other(format!("missing first tick in {first_status:?}")))?;
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut output = String::new();

    while Instant::now() < deadline {
        let message = match timeout(
            deadline.saturating_duration_since(Instant::now()),
            read_attach_message(&mut attach_stream),
        )
        .await
        {
            Ok(message) => message?,
            Err(_) => break,
        };
        let Some(message) = message else {
            break;
        };
        if let AttachMessage::Data(bytes) | AttachMessage::Render(bytes) = message {
            output.push_str(&String::from_utf8_lossy(&bytes));
            if extract_tick_second(&output).is_some_and(|tick| tick != first_tick) {
                drop(attach_stream);
                handle.shutdown().await?;
                return Ok(());
            }
        }
    }

    drop(attach_stream);
    handle.shutdown().await?;
    Err(io::Error::other(format!(
        "status interval never refreshed tick from {first_tick}; output was {output:?}"
    ))
    .into())
}

#[tokio::test]
async fn status_interval_does_not_refresh_suspended_attach_client() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("status-interval-suspend");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");

    let created = common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 40, rows: 4 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    for (option, value) in [
        (OptionName::StatusInterval, "1"),
        (OptionName::StatusLeft, "[#{session_name}] "),
        (OptionName::StatusRight, "tick=%S"),
    ] {
        let response = common::send_request(
            &socket_path,
            &Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                option,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }),
        )
        .await?;
        assert!(matches!(response, Response::SetOption(_)));
    }

    let (_response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: alpha.clone(),
        })
        .await?;

    let _initial_status = read_attach_data_until_contains(&mut attach_stream, "tick=").await?;
    let attach_pid = attached_client_pid(&socket_path, &alpha).await?;
    let suspended = common::send_request(
        &socket_path,
        &Request::SuspendClient(SuspendClientRequest {
            target_client: Some(attach_pid),
        }),
    )
    .await?;
    assert!(matches!(suspended, Response::SuspendClient(_)));
    read_attach_until_suspend(&mut attach_stream).await?;

    let deadline = Instant::now() + Duration::from_millis(2200);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match timeout(remaining, read_attach_message(&mut attach_stream)).await {
            Ok(Ok(Some(AttachMessage::Data(bytes) | AttachMessage::Render(bytes)))) => {
                drop(attach_stream);
                handle.shutdown().await?;
                return Err(io::Error::other(format!(
                    "suspended attach client received status refresh bytes: {:?}",
                    String::from_utf8_lossy(&bytes)
                ))
                .into());
            }
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) | Err(_) => break,
            Ok(Err(error)) => return Err(error),
        }
    }

    drop(attach_stream);
    handle.shutdown().await?;
    Ok(())
}

async fn attached_client_pid(
    socket_path: &std::path::Path,
    session_name: &rmux_proto::SessionName,
) -> Result<String, Box<dyn Error>> {
    match common::send_request(
        socket_path,
        &Request::ListClients(ListClientsRequest {
            format: Some("#{client_pid}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_session: Some(session_name.clone()),
        }),
    )
    .await?
    {
        Response::ListClients(response) => {
            assert_eq!(response.match_count, 1);
            Ok(String::from_utf8_lossy(response.output.stdout())
                .trim()
                .to_owned())
        }
        other => {
            Err(io::Error::other(format!("unexpected list-clients response: {other:?}")).into())
        }
    }
}

async fn read_attach_until_suspend(
    stream: &mut tokio::net::UnixStream,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + STEP_TIMEOUT;

    while Instant::now() < deadline {
        let message = match timeout(
            deadline.saturating_duration_since(Instant::now()),
            read_attach_message(stream),
        )
        .await
        {
            Ok(message) => message?,
            Err(_) => break,
        };
        match message {
            Some(AttachMessage::Suspend) => return Ok(()),
            Some(AttachMessage::Data(bytes)) if bytes == [5] => return Ok(()),
            Some(_) => {}
            None => break,
        }
    }

    Err(io::Error::other("attach stream never received suspend control").into())
}

async fn read_attach_data_until_contains(
    stream: &mut tokio::net::UnixStream,
    needle: &str,
) -> Result<String, Box<dyn Error>> {
    let deadline = Instant::now() + STEP_TIMEOUT;
    let mut output = String::new();

    while Instant::now() < deadline {
        let message = match timeout(
            deadline.saturating_duration_since(Instant::now()),
            read_attach_message(stream),
        )
        .await
        {
            Ok(message) => message?,
            Err(_) => break,
        };

        let Some(message) = message else {
            break;
        };

        if let AttachMessage::Data(bytes) | AttachMessage::Render(bytes) = message {
            output.push_str(&String::from_utf8_lossy(&bytes));
            if output.contains(needle) {
                return Ok(output);
            }
        }
    }

    Err(io::Error::other(format!(
        "attach stream never included expected status marker {needle:?}; output was {output:?}"
    ))
    .into())
}

async fn read_attach_message(
    stream: &mut tokio::net::UnixStream,
) -> Result<Option<AttachMessage>, Box<dyn Error>> {
    let mut tag = [0_u8; 1];
    let bytes_read = stream.read(&mut tag).await?;
    if bytes_read == 0 {
        return Ok(None);
    }

    match tag[0] {
        1 => {
            let mut length = [0_u8; 4];
            stream.read_exact(&mut length).await?;
            let payload_len = u32::from_le_bytes(length) as usize;
            let mut payload = vec![0_u8; payload_len];
            stream.read_exact(&mut payload).await?;
            Ok(Some(AttachMessage::Data(payload)))
        }
        2 => {
            let mut size = [0_u8; 4];
            stream.read_exact(&mut size).await?;
            Ok(Some(AttachMessage::Resize(rmux_proto::TerminalSize {
                cols: u16::from_le_bytes([size[0], size[1]]),
                rows: u16::from_le_bytes([size[2], size[3]]),
            })))
        }
        5 => Ok(Some(AttachMessage::Suspend)),
        13 => {
            let mut length = [0_u8; 4];
            stream.read_exact(&mut length).await?;
            let payload_len = u32::from_le_bytes(length) as usize;
            let mut payload = vec![0_u8; payload_len];
            stream.read_exact(&mut payload).await?;
            Ok(Some(AttachMessage::Render(payload)))
        }
        other => Err(rmux_proto::RmuxError::Decode(format!(
            "unknown attach-stream message tag {other}"
        ))
        .into()),
    }
}

fn extract_tick_second(output: &str) -> Option<String> {
    let start = output.rfind("tick=")? + "tick=".len();
    let tick = output.get(start..start + 2)?;
    tick.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| tick.to_owned())
}
