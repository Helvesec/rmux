use super::{DaemonConfig, ServerDaemon};
use rmux_proto::{
    encode_frame, ErrorResponse, FrameDecoder, HasSessionRequest, KillServerRequest,
    KillSessionRequest, ListClientsRequest, ListPanesRequest, ListSessionsRequest,
    ListWindowsRequest, LockServerRequest, NewSessionRequest, Request, Response, RmuxError,
    SessionName, TerminalSize,
};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn windows_daemon_accepts_ipc_and_dispatches_runtime_requests() -> io::Result<()> {
    let endpoint = unique_endpoint()?;
    let socket_path = endpoint.clone().into_path();
    let handle = ServerDaemon::new(DaemonConfig::new(socket_path.clone()))
        .bind()
        .await?;

    let response = tokio::task::spawn_blocking(move || {
        let mut stream = rmux_ipc::connect_blocking(&endpoint, Duration::from_secs(5))?;
        let request = Request::LockServer(LockServerRequest);
        let frame = encode_frame(&request).map_err(io::Error::other)?;
        stream.write_all(&frame)?;
        read_response(&mut stream)
    })
    .await
    .map_err(io::Error::other)??;

    assert!(matches!(response, Response::LockServer(_)));
    assert_eq!(handle.socket_path(), socket_path.as_path());
    handle.shutdown().await
}

#[tokio::test]
async fn windows_daemon_kill_server_stops_runtime() -> io::Result<()> {
    let endpoint = unique_endpoint()?;
    let socket_path = endpoint.clone().into_path();
    let handle = ServerDaemon::new(DaemonConfig::new(socket_path))
        .bind()
        .await?;

    let response = tokio::task::spawn_blocking(move || {
        let mut stream = rmux_ipc::connect_blocking(&endpoint, Duration::from_secs(5))?;
        let frame =
            encode_frame(&Request::KillServer(KillServerRequest)).map_err(io::Error::other)?;
        stream.write_all(&frame)?;
        read_response(&mut stream)
    })
    .await
    .map_err(io::Error::other)??;

    assert!(matches!(response, Response::KillServer(_)));
    handle.wait().await
}

#[tokio::test]
async fn windows_daemon_empty_session_requests_match_unix_semantics() -> io::Result<()> {
    let endpoint = unique_endpoint()?;
    let socket_path = endpoint.clone().into_path();
    let handle = ServerDaemon::new(DaemonConfig::new(socket_path))
        .bind()
        .await?;
    let target = SessionName::new("missing").expect("valid session");

    let (has_session, kill_session) = tokio::task::spawn_blocking(move || {
        let has_session = roundtrip(
            &endpoint,
            Request::HasSession(HasSessionRequest {
                target: target.clone(),
            }),
        )?;
        let kill_session = roundtrip(
            &endpoint,
            Request::KillSession(KillSessionRequest {
                target,
                kill_all_except_target: false,
                clear_alerts: false,
            }),
        )?;

        Ok::<_, io::Error>((has_session, kill_session))
    })
    .await
    .map_err(io::Error::other)??;

    assert!(matches!(
        has_session,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: false })
    ));
    assert!(matches!(
        kill_session,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound(name)
        }) if name == "missing"
    ));

    handle.shutdown().await
}

#[tokio::test]
async fn windows_daemon_creates_detached_session() -> io::Result<()> {
    let endpoint = unique_endpoint()?;
    let socket_path = endpoint.clone().into_path();
    let handle = ServerDaemon::new(DaemonConfig::new(socket_path))
        .bind()
        .await?;

    let response = tokio::task::spawn_blocking(move || {
        roundtrip(
            &endpoint,
            Request::NewSession(NewSessionRequest {
                session_name: SessionName::new("alpha").expect("valid session"),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }),
        )
    })
    .await
    .map_err(io::Error::other)??;

    assert!(matches!(response, Response::NewSession(_)));
    handle.shutdown().await
}

#[tokio::test]
async fn windows_daemon_empty_listing_requests_succeed() -> io::Result<()> {
    let endpoint = unique_endpoint()?;
    let socket_path = endpoint.clone().into_path();
    let handle = ServerDaemon::new(DaemonConfig::new(socket_path))
        .bind()
        .await?;

    let responses = tokio::task::spawn_blocking(move || {
        let requests = vec![
            Request::ListSessions(ListSessionsRequest {
                format: None,
                filter: None,
                sort_order: None,
                reversed: false,
            }),
            Request::ListWindows(ListWindowsRequest {
                target: SessionName::new("alpha").expect("valid session"),
                format: None,
            }),
            Request::ListPanes(ListPanesRequest {
                target: SessionName::new("alpha").expect("valid session"),
                target_window_index: None,
                format: None,
            }),
            Request::ListClients(ListClientsRequest {
                format: None,
                filter: None,
                sort_order: None,
                reversed: false,
                target_session: None,
            }),
        ];

        requests
            .into_iter()
            .map(|request| roundtrip(&endpoint, request))
            .collect::<io::Result<Vec<_>>>()
    })
    .await
    .map_err(io::Error::other)??;

    for response in responses {
        match response {
            Response::ListSessions(response) => assert!(response.output.stdout().is_empty()),
            Response::ListClients(response) => {
                assert_eq!(response.match_count, 0);
                assert!(response.output.stdout().is_empty());
            }
            Response::Error(ErrorResponse {
                error: RmuxError::SessionNotFound(name),
            }) => assert_eq!(name, "alpha"),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    handle.shutdown().await
}

fn unique_endpoint() -> io::Result<rmux_ipc::LocalEndpoint> {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    rmux_ipc::endpoint_for_label(format!("server-windows-{}-{unique_id}", std::process::id()))
}

fn roundtrip(endpoint: &rmux_ipc::LocalEndpoint, request: Request) -> io::Result<Response> {
    let mut stream = rmux_ipc::connect_blocking(endpoint, Duration::from_secs(5))?;
    let frame = encode_frame(&request).map_err(io::Error::other)?;
    stream.write_all(&frame)?;
    read_response(&mut stream)
}

fn read_response(stream: &mut rmux_ipc::BlockingLocalStream) -> io::Result<Response> {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 8192];

    loop {
        if let Some(response) = decoder.next_frame::<Response>().map_err(io::Error::other)? {
            return Ok(response);
        }

        let bytes_read = stream.read(&mut buffer)?;
        if bytes_read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        decoder.push_bytes(&buffer[..bytes_read]);
    }
}
