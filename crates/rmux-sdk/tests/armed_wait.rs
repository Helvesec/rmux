#![cfg(unix)]

mod common;

use std::error::Error;
use std::fmt::{Debug, Write as _};
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_proto::{
    encode_frame, CancelSdkWaitResponse, ErrorResponse, FrameDecoder, HandshakeResponse,
    HasSessionRequest, PaneOutputSubscriptionStart, Request, Response, SdkWaitForOutputRequest,
    SdkWaitForOutputResponse, SdkWaitOutcome, CAPABILITY_SDK_WAITS_ARMED,
};
use rmux_sdk::{ArmedWait, EnsureSession, Pane, PaneRef, RmuxBuilder, RmuxError, SessionName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio::time::Instant;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

static LIVE_DAEMON_LOCK: common::unix_smoke::LiveDaemonLock =
    common::unix_smoke::LiveDaemonLock::new();
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

fn assert_send<T: Send>() {}
fn assert_static<T: 'static>() {}
fn assert_debug<T: Debug>() {}

#[test]
fn armed_wait_public_type_is_send_static_and_debuggable() {
    assert_send::<ArmedWait>();
    assert_static::<ArmedWait>();
    assert_debug::<ArmedWait>();
}

#[tokio::test]
async fn wait_for_next_arms_before_returned_handle_completes() -> TestResult {
    let socket = TestSocket::new("armed-byte")?;
    let listener = UnixListener::bind(socket.path())?;
    let (seen_sender, seen) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let _handle = accept_peer(&listener).await?;
        let mut wait = accept_peer(&listener).await?;
        let mut cancel = accept_peer(&listener).await?;

        let request = expect_sdk_wait(&mut wait, b"needle").await?;
        write_armed(&mut wait, &request).await?;
        seen_sender
            .send(())
            .map_err(|_| "test dropped request observer")?;
        released.await.map_err(|_| "test dropped release sender")?;

        wait.write_response(Response::SdkWaitForOutput(SdkWaitForOutputResponse {
            wait_id: request.wait_id,
            outcome: SdkWaitOutcome::Matched,
        }))
        .await?;
        assert_no_cancel_request(&mut cancel).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let mut armed = Box::pin(pane.wait_for_next(b"needle").await?);
    seen.await?;
    assert_armed_wait_pending(&mut armed, "byte wait completed before daemon response").await?;

    release
        .send(())
        .map_err(|_| "server dropped release receiver")?;
    armed.await?;
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn wait_for_text_next_arms_daemon_byte_wait_for_text_bytes() -> TestResult {
    let socket = TestSocket::new("armed-text")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let _handle = accept_peer(&listener).await?;
        let mut wait = accept_peer(&listener).await?;
        let mut cancel = accept_peer(&listener).await?;

        let request = expect_sdk_wait(&mut wait, b"hello text").await?;
        write_armed(&mut wait, &request).await?;
        wait.write_response(Response::SdkWaitForOutput(SdkWaitForOutputResponse {
            wait_id: request.wait_id,
            outcome: SdkWaitOutcome::Matched,
        }))
        .await?;
        assert_no_cancel_request(&mut cancel).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    pane.wait_for_text_next("hello text").await?.await?;
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn independent_armed_waits_can_be_stored_and_moved_across_tasks() -> TestResult {
    let socket = TestSocket::new("armed-move")?;
    let listener = UnixListener::bind(socket.path())?;
    let (seen_sender, seen) = oneshot::channel();
    let server = tokio::spawn(async move {
        let _handle = accept_peer(&listener).await?;
        let mut first_wait = accept_peer(&listener).await?;
        let mut first_cancel = accept_peer(&listener).await?;
        let first = expect_sdk_wait(&mut first_wait, b"first").await?;
        write_armed(&mut first_wait, &first).await?;

        let mut second_wait = accept_peer(&listener).await?;
        let mut second_cancel = accept_peer(&listener).await?;
        let second = expect_sdk_wait(&mut second_wait, b"second").await?;
        write_armed(&mut second_wait, &second).await?;
        seen_sender
            .send(())
            .map_err(|_| "test dropped request observer")?;

        first_wait
            .write_response(Response::SdkWaitForOutput(SdkWaitForOutputResponse {
                wait_id: first.wait_id,
                outcome: SdkWaitOutcome::Matched,
            }))
            .await?;
        second_wait
            .write_response(Response::SdkWaitForOutput(SdkWaitForOutputResponse {
                wait_id: second.wait_id,
                outcome: SdkWaitOutcome::Matched,
            }))
            .await?;
        assert_no_cancel_request(&mut first_cancel).await?;
        assert_no_cancel_request(&mut second_cancel).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let waits = vec![
        pane.wait_for_next(b"first").await?,
        pane.wait_for_text_next("second").await?,
    ];
    seen.await?;

    let join = tokio::spawn(async move {
        let mut waits = waits.into_iter();
        let first = waits.next().expect("first armed wait stored");
        let second = waits.next().expect("second armed wait stored");
        let (first_result, second_result) = tokio::join!(first, second);
        first_result?;
        second_result
    });
    join.await??;
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn dropping_armed_wait_sends_best_effort_cancel() -> TestResult {
    let socket = TestSocket::new("armed-drop")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let _handle = accept_peer(&listener).await?;
        let mut wait = accept_peer(&listener).await?;
        let mut cancel = accept_peer(&listener).await?;

        let request = expect_sdk_wait(&mut wait, b"drop").await?;
        write_armed(&mut wait, &request).await?;
        expect_cancel(&mut cancel, &request).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let armed = pane.wait_for_next(b"drop").await?;
    drop(armed);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn armed_wait_timeout_sends_best_effort_cancel() -> TestResult {
    let socket = TestSocket::new("armed-timeout")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let _handle = accept_peer(&listener).await?;
        let mut wait = accept_peer(&listener).await?;
        let mut cancel = accept_peer(&listener).await?;

        let request = expect_sdk_wait(&mut wait, b"never").await?;
        write_armed(&mut wait, &request).await?;
        expect_cancel(&mut cancel, &request).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_millis(25)).await?;
    let error = pane
        .wait_for_next(b"never")
        .await?
        .await
        .expect_err("unmatched armed wait must time out");
    assert_timed_out(error, "wait for next pane output bytes");
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn armed_wait_preserves_cancelled_outcome() -> TestResult {
    let socket = TestSocket::new("armed-cancelled")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let _handle = accept_peer(&listener).await?;
        let mut wait = accept_peer(&listener).await?;
        let mut cancel = accept_peer(&listener).await?;

        let request = expect_sdk_wait(&mut wait, b"cancelled").await?;
        write_armed(&mut wait, &request).await?;
        wait.write_response(Response::SdkWaitForOutput(SdkWaitForOutputResponse {
            wait_id: request.wait_id,
            outcome: SdkWaitOutcome::Cancelled,
        }))
        .await?;
        assert_no_cancel_request(&mut cancel).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let error = pane
        .wait_for_next(b"cancelled")
        .await?
        .await
        .expect_err("daemon-cancelled wait must fail");
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::Server(message),
            ..
        } => assert!(message.contains("was cancelled")),
        other => panic!("expected typed cancelled protocol error, got {other:?}"),
    }
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn armed_wait_preserves_mismatched_id_failure_and_cancels_expected_wait() -> TestResult {
    let socket = TestSocket::new("armed-mismatch")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let _handle = accept_peer(&listener).await?;
        let mut wait = accept_peer(&listener).await?;
        let mut cancel = accept_peer(&listener).await?;

        let request = expect_sdk_wait(&mut wait, b"mismatch").await?;
        write_armed(&mut wait, &request).await?;
        wait.write_response(Response::SdkWaitForOutput(SdkWaitForOutputResponse {
            wait_id: rmux_proto::SdkWaitId::new(request.wait_id.as_u64() + 1),
            outcome: SdkWaitOutcome::Matched,
        }))
        .await?;
        expect_cancel(&mut cancel, &request).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let error = pane
        .wait_for_next(b"mismatch")
        .await?
        .await
        .expect_err("mismatched wait id must fail");
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::Server(message),
            ..
        } => assert!(message.contains("did not match request id")),
        other => panic!("expected protocol mismatch, got {other:?}"),
    }
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn armed_wait_surfaces_stale_pane_failure() -> TestResult {
    let socket = TestSocket::new("armed-stale")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let _handle = accept_peer(&listener).await?;
        let mut wait = accept_peer(&listener).await?;
        let mut cancel = accept_peer(&listener).await?;

        let _request = expect_sdk_wait(&mut wait, b"stale").await?;
        wait.write_response(Response::Error(ErrorResponse {
            error: rmux_proto::RmuxError::InvalidTarget {
                value: "wait:0.0".to_owned(),
                reason: "pane index does not exist in session".to_owned(),
            },
        }))
        .await?;
        assert_no_cancel_request(&mut cancel).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let error = pane
        .wait_for_next(b"stale")
        .await?
        .await
        .expect_err("stale pane wait must fail");
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::InvalidTarget { reason, .. },
            ..
        } => assert!(reason.contains("pane index does not exist")),
        other => panic!("expected stale-pane protocol error, got {other:?}"),
    }
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn wait_for_next_rejects_empty_needles_locally() -> TestResult {
    let socket = TestSocket::new("armed-empty")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut handle = accept_peer(&listener).await?;
        assert_peer_closed_without_request(&mut handle).await?;
        assert_no_extra_peer(&listener).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let error = pane
        .wait_for_next([])
        .await
        .expect_err("empty armed byte waits must be rejected locally");
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::Server(message),
            ..
        } => assert!(message.contains("must not be empty")),
        other => panic!("expected local protocol error, got {other:?}"),
    }
    let error = pane
        .wait_for_text_next("")
        .await
        .expect_err("empty armed text waits must be rejected locally");
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::Server(message),
            ..
        } => assert!(message.contains("must not be empty")),
        other => panic!("expected local protocol error, got {other:?}"),
    }
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn live_wait_for_next_ignores_history_and_matches_split_future_output() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("armed-live").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(unique_session_name("armedlive"))
        .create_only()
        .environment(["SHELL=/bin/sh"])
        .ensure(&rmux)
        .await?;
    let pane = session.pane(0, 0);
    let id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);

    let history_marker = format!("rmux_armed_history_{id}");
    pane.send_text(printf_line_command(&history_marker)).await?;
    pane.send_key("Enter").await?;
    pane.wait_for_text(&history_marker).await?;

    let mut history_wait = Box::pin(pane.wait_for_next(history_marker.as_bytes()).await?);
    assert_armed_wait_pending(
        &mut history_wait,
        "wait_for_next matched output that existed before arming",
    )
    .await?;
    pane.send_text(printf_line_command(&history_marker)).await?;
    pane.send_key("Enter").await?;
    history_wait.await?;

    let text_history_marker = format!("rmux_armed_text_history_{id}");
    pane.send_text(printf_line_command(&text_history_marker))
        .await?;
    pane.send_key("Enter").await?;
    pane.wait_for_text(&text_history_marker).await?;

    let mut text_history_wait = Box::pin(pane.wait_for_text_next(&text_history_marker).await?);
    assert_armed_wait_pending(
        &mut text_history_wait,
        "wait_for_text_next matched text output that existed before arming",
    )
    .await?;
    pane.send_text(printf_line_command(&text_history_marker))
        .await?;
    pane.send_key("Enter").await?;
    text_history_wait.await?;

    let byte_needle = format!("rmux_armed_bytes_{id}");
    let (byte_left, byte_right) = byte_needle.split_at(byte_needle.len() / 2);
    let byte_wait = pane.wait_for_next(byte_needle.as_bytes()).await?;
    pane.send_text(format!(
        "printf '{byte_left}'; sleep 0.05; printf '{byte_right}\\n'"
    ))
    .await?;
    pane.send_key("Enter").await?;
    byte_wait.await?;

    let text_needle = format!("rmux_armed_text_{id}");
    let (text_left, text_right) = text_needle.split_at(text_needle.len() / 2);
    let text_wait = pane.wait_for_text_next(&text_needle).await?;
    pane.send_text(format!(
        "printf '{text_left}'; sleep 0.05; printf '{text_right}\\n'"
    ))
    .await?;
    pane.send_key("Enter").await?;
    text_wait.await?;

    harness.finish().await
}

async fn expect_sdk_wait(peer: &mut Peer, bytes: &[u8]) -> TestResult<SdkWaitForOutputRequest> {
    let mut request = peer.expect_request().await?;
    if let Request::Handshake(handshake) = &request {
        assert!(
            handshake
                .required_capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_SDK_WAITS_ARMED),
            "armed SDK wait handshake did not require {CAPABILITY_SDK_WAITS_ARMED}: {handshake:?}"
        );
        peer.write_response(Response::Handshake(HandshakeResponse::current()))
            .await?;
        request = peer.expect_request().await?;
    }
    let Request::SdkWaitForOutput(request) = request else {
        panic!("armed wait must use server-side SDK byte wait, got {request:?}");
    };
    assert_eq!(request.target, target().to_proto());
    assert_eq!(request.bytes, bytes);
    assert_eq!(request.start, PaneOutputSubscriptionStart::Now);
    Ok(request)
}

async fn write_armed(peer: &mut Peer, request: &SdkWaitForOutputRequest) -> TestResult {
    peer.write_response(Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(
        request.wait_id,
    )))
    .await
}

async fn expect_cancel(peer: &mut Peer, wait: &SdkWaitForOutputRequest) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::CancelSdkWait(cancel) = request else {
        panic!("armed wait drop/timeout must send SDK cancellation, got {request:?}");
    };
    assert_eq!(cancel.owner_id, wait.owner_id);
    assert_eq!(cancel.wait_id, wait.wait_id);
    peer.write_response(Response::CancelSdkWait(CancelSdkWaitResponse {
        wait_id: wait.wait_id,
        removed: true,
    }))
    .await
}

async fn assert_armed_wait_pending(
    wait: &mut Pin<Box<ArmedWait>>,
    message: &'static str,
) -> TestResult {
    match tokio::time::timeout(Duration::from_millis(75), wait.as_mut()).await {
        Err(_) => Ok(()),
        Ok(result) => Err(format!("{message}: {result:?}").into()),
    }
}

async fn pane_for(socket_path: &Path, timeout: Duration) -> TestResult<Pane> {
    let rmux = RmuxBuilder::new()
        .unix_socket(socket_path)
        .default_timeout(timeout)
        .build();
    Ok(rmux.pane(target()).await?)
}

fn target() -> PaneRef {
    PaneRef::new(session_name(), 0, 0)
}

fn session_name() -> SessionName {
    SessionName::new("wait").expect("valid test session name")
}

fn unique_session_name(prefix: &str) -> SessionName {
    let id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    SessionName::new(format!("{prefix}{id}")).expect("valid generated session name")
}

async fn accept_peer(listener: &UnixListener) -> TestResult<Peer> {
    let (stream, _) = listener.accept().await?;
    Ok(Peer::new(stream))
}

async fn assert_no_cancel_request(peer: &mut Peer) -> TestResult {
    let mut buffer = [0_u8; 1];
    match tokio::time::timeout(Duration::from_millis(50), peer.stream.read(&mut buffer)).await {
        Err(_) | Ok(Ok(0)) => Ok(()),
        Ok(Ok(_)) => Err("completed armed wait unexpectedly wrote a cancel request".into()),
        Ok(Err(error)) => Err(error.into()),
    }
}

async fn assert_peer_closed_without_request(peer: &mut Peer) -> TestResult {
    match peer.read_request().await? {
        None => Ok(()),
        Some(request) => Err(format!("unexpected request before close: {request:?}").into()),
    }
}

async fn assert_no_extra_peer(listener: &UnixListener) -> TestResult {
    match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
        Err(_) => Ok(()),
        Ok(Ok(_)) => Err("local validation unexpectedly opened an armed wait connection".into()),
        Ok(Err(error)) => Err(error.into()),
    }
}

fn assert_timed_out(error: RmuxError, expected_operation: &str) {
    match error {
        RmuxError::Transport {
            operation, source, ..
        } => {
            assert_eq!(operation, expected_operation);
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        other => panic!("expected typed timeout transport error, got {other:?}"),
    }
}

struct Peer {
    stream: UnixStream,
    decoder: FrameDecoder,
}

impl Peer {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::new(),
        }
    }

    async fn expect_request(&mut self) -> TestResult<Request> {
        self.read_request()
            .await?
            .ok_or_else(|| "peer closed before request".into())
    }

    async fn read_request(&mut self) -> TestResult<Option<Request>> {
        let mut buffer = [0_u8; 4096];
        loop {
            if let Some(request) = self.decoder.next_frame::<Request>()? {
                return Ok(Some(request));
            }

            let read = self.stream.read(&mut buffer).await?;
            if read == 0 {
                return Ok(None);
            }
            self.decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn write_response(&mut self, response: Response) -> TestResult {
        let frame = encode_frame(&response)?;
        self.stream.write_all(&frame).await?;
        self.stream.flush().await?;
        Ok(())
    }
}

struct TestSocket {
    root: PathBuf,
    path: PathBuf,
}

impl TestSocket {
    fn new(label: &str) -> io::Result<Self> {
        let id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        // macOS keeps Unix socket paths under a tight sockaddr_un limit; do
        // not inherit the long /var/folders/... TMPDIR here.
        let root = PathBuf::from("/tmp").join(format!(
            "rmux-aw-{}-{}-{id}",
            compact_label(label),
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            path: root.join("s"),
            root,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Harness {
    _root: TestRoot,
    socket_path: PathBuf,
    child: Option<Child>,
}

impl Harness {
    async fn start(label: &str) -> TestResult<Self> {
        let root = TestRoot::new(label);
        let socket_path = root.path().join("daemon.sock");
        let mut child = Command::new(rmux_binary()?)
            .arg("--__internal-daemon")
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        wait_for_daemon_ready(&socket_path, &mut child).await?;

        Ok(Self {
            _root: root,
            socket_path,
            child: Some(child),
        })
    }

    fn rmux(&self) -> rmux_sdk::Rmux {
        RmuxBuilder::new()
            .unix_socket(&self.socket_path)
            .default_timeout(Duration::from_secs(2))
            .build()
    }

    async fn finish(self) -> TestResult {
        let shutdown = self.rmux().shutdown().await;
        wait_for_child_exit(self, "server did not exit during cleanup").await?;
        if let Err(error) = shutdown {
            let rendered = error.to_string();
            assert!(
                rendered.contains("connect to rmux daemon")
                    || rendered.contains("rmux daemon closed the transport")
                    || rendered.contains("rmux transport actor is closed")
                    || rendered.contains("Connection reset by peer"),
                "unexpected cleanup shutdown error: {rendered}"
            );
        }
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }
}

async fn wait_for_child_exit(mut harness: Harness, timeout_message: &'static str) -> TestResult {
    let mut child = harness.child.take().expect("harness owns daemon child");
    let deadline = Instant::now() + Duration::from_secs(60);

    loop {
        if let Some(status) = child.try_wait()? {
            assert!(status.success(), "daemon exited with status {status}");
            return Ok(());
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(timeout_message.into());
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_daemon_ready(socket_path: &Path, child: &mut Child) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(60);

    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!("daemon exited before accepting RPC: {status}").into());
        }

        if matches!(
            framed_request(
                socket_path,
                Request::HasSession(HasSessionRequest {
                    target: session_name()
                })
            )
            .await,
            Ok(Response::HasSession(_))
        ) {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "daemon at '{}' did not accept RPC before timeout",
                socket_path.display()
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn framed_request(socket_path: &Path, request: Request) -> TestResult<Response> {
    let mut stream = UnixStream::connect(socket_path).await?;
    let frame = encode_frame(&request)?;
    stream.write_all(&frame).await?;
    stream.flush().await?;
    read_response(&mut stream).await
}

async fn read_response(stream: &mut UnixStream) -> TestResult<Response> {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 4096];

    loop {
        if let Some(response) = decoder.next_frame::<Response>()? {
            return Ok(response);
        }

        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err("stream closed before response".into());
        }
        decoder.push_bytes(&buffer[..read]);
    }
}

fn rmux_binary() -> TestResult<&'static Path> {
    static RMUX_BINARY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    match RMUX_BINARY.get_or_init(|| resolve_rmux_binary().map_err(|error| error.to_string())) {
        Ok(path) => Ok(path.as_path()),
        Err(error) => Err(io::Error::other(error.clone()).into()),
    }
}

fn resolve_rmux_binary() -> TestResult<PathBuf> {
    if let Some(path) = option_env!("CARGO_BIN_EXE_rmux") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    let target_dir = target_dir()?;
    let candidate = target_dir.join("debug").join("rmux");
    let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .arg("build")
        .arg("--bin")
        .arg("rmux")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(workspace_root().join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()?;
    if !status.success() {
        return Err(format!("failed to build rmux binary for daemon tests: {status}").into());
    }
    if !candidate.is_file() {
        return Err(format!(
            "rmux daemon build succeeded but '{}' was not created",
            candidate.display()
        )
        .into());
    }

    Ok(candidate)
}

fn target_dir() -> TestResult<PathBuf> {
    if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(target_dir));
    }

    let current = std::env::current_exe()?;
    current
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "test executable is not under a target directory".into())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("rmux-sdk manifest lives under crates/rmux-sdk")
        .to_path_buf()
}

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(label: &str) -> Self {
        let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from("/tmp").join(format!(
            "rmux-sdk-armed-wait-live-{}-{}-{unique_id}",
            compact_label(label),
            std::process::id()
        ));
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn compact_label(label: &str) -> String {
    let compact = label
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>();
    if compact.is_empty() {
        "x".to_owned()
    } else {
        compact
    }
}

fn printf_line_command(text: &str) -> String {
    let mut escaped = String::new();
    for byte in text.bytes().chain([b'\n']) {
        write!(&mut escaped, "\\{byte:03o}").expect("writing to String cannot fail");
    }
    format!("printf '{escaped}'")
}
