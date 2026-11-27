//! Shared harness for tests that drive a real client connection through the
//! production listener loop.
//!
//! Everything a listener-level regression needs is here: a connected pair of
//! streams on both platforms, a chosen peer identity, and
//! [`run_connection_with_cleanup`] doing the real read/dispatch/upgrade work.
//! Tests that call handler methods directly cannot observe the connection loop
//! at all, so anything the loop itself owes is proven from here.

use super::*;

use rmux_proto::{NewSessionExtRequest, PaneTarget, SessionName, TerminalSize};

#[path = "listener_slow_reader_tests.rs"]
pub(crate) mod slow_reader_tests;

#[cfg(unix)]
#[path = "listener_read_only_format_job_tests.rs"]
mod read_only_format_job_tests;

/// Async client end of the connection under test.
#[cfg(unix)]
pub(crate) type TestClientStream = LocalStream;
#[cfg(windows)]
pub(crate) type TestClientStream = rmux_ipc::WindowsPipeClient;

#[cfg(unix)]
pub(super) async fn connected_streams(_label: &str) -> io::Result<(LocalStream, TestClientStream)> {
    LocalStream::pair()
}

#[cfg(windows)]
pub(super) async fn connected_streams(label: &str) -> io::Result<(LocalStream, TestClientStream)> {
    let endpoint = rmux_ipc::endpoint_for_label(format!("{label}-{}", std::process::id()))?;
    let listener = LocalListener::bind(&endpoint)?;
    let client_endpoint = endpoint.clone();
    let client = tokio::spawn(async move {
        rmux_ipc::connect_windows_pipe(client_endpoint.as_pipe_name()).await
    });
    // The accepted peer identity is the test process itself; the connection is
    // served under the peer identity the caller chose instead.
    let (server, _accepted_peer) = listener.accept().await?;
    let client = client
        .await
        .map_err(|error| io::Error::other(format!("named-pipe client task failed: {error}")))??;
    Ok((server, client))
}

pub(super) fn spawn_connection(
    handler: &Arc<RequestHandler>,
    peer: PeerIdentity,
    server: LocalStream,
) -> (watch::Sender<()>, tokio::task::JoinHandle<io::Result<()>>) {
    let handler = Arc::clone(handler);
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let connection_id = handler.allocate_connection_id();
    let task = tokio::spawn(async move {
        run_connection_with_cleanup(
            server,
            peer,
            handler,
            connection_id,
            shutdown_rx,
            shutdown_handle,
        )
        .await
    });
    (shutdown_tx, task)
}

pub(crate) async fn finish_connection(
    task: tokio::task::JoinHandle<io::Result<()>>,
) -> io::Result<()> {
    match task.await.expect("connection task") {
        Ok(()) => Ok(()),
        // A dropped named-pipe client surfaces as a peer disconnect rather
        // than a clean end-of-stream.
        Err(error) if rmux_ipc::is_peer_disconnect(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) async fn write_test_request<S>(stream: &mut S, request: Request) -> io::Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let frame = encode_frame(&request).map_err(io::Error::other)?;
    stream.write_all(&frame).await
}

pub(super) async fn read_test_response<S>(stream: &mut S) -> io::Result<Response>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 512];

    loop {
        if let Some(response) = decoder.next_frame::<Response>().map_err(io::Error::other)? {
            return Ok(response);
        }

        let bytes_read = stream.read(&mut buffer).await?;
        if bytes_read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed before response frame",
            ));
        }
        decoder.push_bytes(&buffer[..bytes_read]);
    }
}

/// Reads one response frame without swallowing whatever follows it.
///
/// An attach response is immediately followed by the upgraded raw byte stream
/// on the same connection, and a buffering reader would consume the opening
/// frame of that stream into its own decoder. Feeding the decoder one byte at
/// a time leaves the transport positioned exactly after the response.
pub(super) async fn read_response_leaving_raw_bytes<S>(stream: &mut S) -> io::Result<Response>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut decoder = FrameDecoder::new();
    let mut byte = [0_u8; 1];

    loop {
        if let Some(response) = decoder.next_frame::<Response>().map_err(io::Error::other)? {
            return Ok(response);
        }
        if stream.read(&mut byte).await? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed before response frame",
            ));
        }
        decoder.push_bytes(&byte);
    }
}

#[cfg(unix)]
pub(super) fn quiet_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
}

#[cfg(windows)]
pub(super) fn quiet_command() -> Vec<String> {
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

pub(super) async fn start_quiet_pane(handler: &Arc<RequestHandler>, name: &str) -> PaneTarget {
    start_quiet_pane_sized(handler, name, TerminalSize { cols: 12, rows: 4 }).await
}

pub(super) async fn start_quiet_pane_sized(
    handler: &Arc<RequestHandler>,
    name: &str,
    size: TerminalSize,
) -> PaneTarget {
    let session = SessionName::new(name).expect("valid session name");
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(size),
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
    crate::session_fixture::assert_new_session(&response);
    let target = PaneTarget::with_window(session, 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    target
}
