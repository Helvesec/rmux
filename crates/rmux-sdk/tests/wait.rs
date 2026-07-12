#![cfg(unix)]

mod common;

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_proto::{
    encode_frame, CancelSdkWaitResponse, CommandOutput, FrameDecoder, HandshakeResponse,
    HasSessionRequest, ListPanesResponse, PaneOutputSubscriptionStart, PaneSnapshotCell,
    PaneSnapshotCursor, PaneSnapshotResponse, Request, Response, SdkWaitForOutputRequest,
    SdkWaitForOutputResponse, SdkWaitOutcome, CAPABILITY_SDK_WAITS_ARMED,
};
use rmux_sdk::{EnsureSession, Pane, PaneRef, RmuxBuilder, RmuxError, SessionName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::time::Instant;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

static LIVE_DAEMON_LOCK: common::unix_smoke::LiveDaemonLock =
    common::unix_smoke::LiveDaemonLock::new();
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn wait_for_uses_daemon_sdk_byte_wait() -> TestResult {
    let socket = TestSocket::new("byte-wait")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let (mut main, mut peers, request) = accept_sdk_wait(&listener, b"needle").await?;

        main.write_response(Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(
            request.wait_id,
        )))
        .await?;
        main.write_response(Response::SdkWaitForOutput(SdkWaitForOutputResponse {
            wait_id: request.wait_id,
            outcome: SdkWaitOutcome::Matched,
        }))
        .await?;

        assert_no_sdk_cancel_request(&listener, &mut peers).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    pane.wait_for(b"needle").await?;
    server.await??;
    Ok(())
}

#[tokio::test]
async fn wait_for_rejects_empty_needles_before_daemon_wait() -> TestResult {
    let socket = TestSocket::new("empty-byte-wait")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut main = accept_peer(&listener).await?;
        assert_peer_closed_without_request(&mut main).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let error = pane
        .wait_for([])
        .await
        .expect_err("empty byte waits must be rejected locally");
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
async fn finite_timeout_wraps_wait_for_and_sends_best_effort_cancel() -> TestResult {
    let socket = TestSocket::new("byte-wait-timeout")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let (mut main, mut peers, request) = accept_sdk_wait(&listener, b"never").await?;
        main.write_response(Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(
            request.wait_id,
        )))
        .await?;

        let (cancel_peer, cancel_request) =
            expect_sdk_cancel_request(&listener, &mut peers).await?;
        let Request::CancelSdkWait(cancel_request) = cancel_request else {
            panic!("timeout must send best-effort SDK wait cancellation, got {cancel_request:?}");
        };
        assert_eq!(cancel_request.owner_id, request.owner_id);
        assert_eq!(cancel_request.wait_id, request.wait_id);

        peers
            .get_mut(cancel_peer)
            .expect("cancel request peer is retained")
            .write_response(Response::CancelSdkWait(CancelSdkWaitResponse {
                wait_id: request.wait_id,
                removed: true,
            }))
            .await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_millis(150)).await?;
    let error = pane
        .wait_for(b"never")
        .await
        .expect_err("unmatched byte wait must time out");
    assert_timed_out(error, "wait for pane output bytes");
    server.await??;
    Ok(())
}

#[tokio::test]
async fn duration_max_wait_for_uses_no_sdk_timeout() -> TestResult {
    let socket = TestSocket::new("byte-no-timeout")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let (mut main, mut peers, request) = accept_sdk_wait(&listener, b"untimed").await?;

        main.write_response(Response::CancelSdkWait(CancelSdkWaitResponse::armed_ack(
            request.wait_id,
        )))
        .await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        main.write_response(Response::SdkWaitForOutput(SdkWaitForOutputResponse {
            wait_id: request.wait_id,
            outcome: SdkWaitOutcome::Matched,
        }))
        .await?;

        assert_no_sdk_cancel_request(&listener, &mut peers).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::MAX).await?;
    tokio::time::timeout(Duration::from_secs(1), pane.wait_for(b"untimed"))
        .await
        .expect("external test guard should not fire")?;
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn wait_for_text_succeeds_for_text_already_in_snapshot() -> TestResult {
    let socket = TestSocket::new("text-present")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "ready marker", 1).await?;
        assert_peer_closed_without_request(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    pane.wait_for_text("marker").await?;
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn pane_snapshot_returns_default_when_resolved_pane_closes_before_fetch() -> TestResult {
    let socket = TestSocket::new("snapshot-close-race")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_list_panes(&mut peer).await?;

        let mut request = peer.expect_request().await?;
        if matches!(request, Request::Handshake(_)) {
            peer.write_response(Response::Handshake(HandshakeResponse::current()))
                .await?;
            request = peer.expect_request().await?;
        }
        let Request::PaneSnapshotRef(request) = request else {
            panic!("snapshot must use the resolved pane id, got {request:?}");
        };
        let pane_id = rmux_proto::PaneId::new(1);
        assert_eq!(
            request.target,
            rmux_proto::PaneTargetRef::by_id(session_name(), pane_id),
        );
        peer.write_response(Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::pane_not_found(session_name(), pane_id),
        }))
        .await?;
        assert_peer_closed_without_request(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let snapshot = pane.snapshot().await?;
    assert_eq!(snapshot, rmux_sdk::PaneSnapshot::default());
    assert_eq!(snapshot.revision, 0);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn wait_for_text_rejects_empty_needles_before_snapshot() -> TestResult {
    let socket = TestSocket::new("empty-text-wait")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        assert_peer_closed_without_request(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let error = pane
        .wait_for_text("")
        .await
        .expect_err("empty text waits must be rejected locally");
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::Server(message),
            ..
        } => assert!(message.contains("text must not be empty")),
        other => panic!("expected local protocol error, got {other:?}"),
    }
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn wait_for_text_polls_fresh_snapshots_until_text_appears() -> TestResult {
    let socket = TestSocket::new("text-poll")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;

        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "not yet", 1).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "now ready marker", 2).await?;

        assert_peer_closed_without_request(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    pane.wait_for_text("marker").await?;
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn dropping_wait_for_text_stops_snapshot_polling_without_sdk_cancel() -> TestResult {
    let socket = TestSocket::new("text-drop")?;
    let listener = UnixListener::bind(socket.path())?;
    let (snapshot_seen, snapshot_seen_receiver) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;

        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "not yet", 1).await?;
        let _ = snapshot_seen.send(());

        assert_peer_closed_without_request(&mut peer).await?;
        assert_no_extra_peer(&listener).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let mut wait = Box::pin(pane.wait_for_text("missing"));
    tokio::select! {
        result = &mut wait => panic!("text wait completed unexpectedly: {result:?}"),
        seen = snapshot_seen_receiver => seen.expect("server should observe first snapshot poll"),
    }

    drop(wait);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn wait_for_text_times_out_cleanly_for_stale_panes() -> TestResult {
    let socket = TestSocket::new("text-timeout")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        while let Some(request) = peer.read_request().await? {
            match request {
                Request::ListPanes(_) => {
                    peer.write_response(Response::ListPanes(ListPanesResponse {
                        output: CommandOutput::from_stdout(Vec::new()),
                    }))
                    .await?;
                }
                other => {
                    panic!("stale pane snapshot polling should only list panes, got {other:?}")
                }
            }
        }
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_millis(40)).await?;
    let error = pane
        .wait_for_text("missing")
        .await
        .expect_err("stale pane text wait must time out");
    assert_timed_out(error, "wait for pane snapshot text");
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn duration_max_wait_for_text_uses_no_sdk_timeout() -> TestResult {
    let socket = TestSocket::new("text-no-timeout")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;

        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "not yet", 1).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "ready after untimed poll", 2).await?;

        assert_peer_closed_without_request(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::MAX).await?;
    tokio::time::timeout(Duration::from_secs(1), pane.wait_for_text("untimed"))
        .await
        .expect("external test guard should not fire")?;
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn wait_for_text_observes_rendered_daemon_output() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("text-live").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(session_name())
        .create_only()
        .ensure(&rmux)
        .await?;
    let pane = session.pane(0, 0);
    let marker = "rmux_sdk_wait_text_live";

    pane.send_text("printf 'rmux_sdk_wait_text_%s\\n' live")
        .await?;
    pane.send_key("Enter").await?;
    pane.wait_for_text(marker).await?;
    assert!(
        pane.snapshot().await?.visible_text().contains(marker),
        "wait_for_text returned before the daemon-rendered marker was visible"
    );

    harness.finish().await
}

#[tokio::test]
async fn expect_visible_text_to_contain_returns_matching_snapshot() -> TestResult {
    let socket = TestSocket::new("visible-contain")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "visible ready marker", 1).await?;
        assert_peer_closed_without_request(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let snapshot = pane.expect_visible_text().to_contain("ready").await?;
    assert!(snapshot.visible_text().contains("ready"));
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn expect_visible_text_supports_any_and_all_matchers() -> TestResult {
    let socket = TestSocket::new("visible-any-all")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "alpha beta gamma", 1).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "delta epsilon zeta", 2).await?;
        assert_peer_closed_without_request(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let any = pane
        .expect_visible_text()
        .to_match_any(["missing", "beta"])
        .await?;
    assert!(any.visible_text().contains("beta"));
    let all = pane
        .expect_visible_text()
        .to_match_all(["delta", "zeta"])
        .await?;
    assert!(all.visible_text().contains("delta"));
    assert!(all.visible_text().contains("zeta"));
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn expect_visible_text_not_to_contain_waits_until_text_disappears() -> TestResult {
    let socket = TestSocket::new("visible-not-contain")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "loading marker", 1).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "ready", 2).await?;
        assert_peer_closed_without_request(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let snapshot = pane
        .expect_visible_text()
        .not_to_contain("loading")
        .poll_interval(Duration::from_millis(10))
        .await?;
    assert_eq!(snapshot.visible_text(), "ready");
    drop(pane);
    server.await??;
    Ok(())
}

#[cfg(feature = "regex")]
#[tokio::test]
async fn expect_visible_text_supports_regex_matchers() -> TestResult {
    let socket = TestSocket::new("visible-regex")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "worker 42 ready", 1).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "error E17", 2).await?;
        expect_list_panes(&mut peer).await?;
        expect_snapshot(&mut peer, "done", 3).await?;
        assert_peer_closed_without_request(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let matched = pane.expect_visible_text().to_match(r"worker \d+").await?;
    assert!(matched.visible_text().contains("worker 42"));

    let any = pane
        .expect_visible_text()
        .to_match_any_regex([r"missing \d+", r"E\d+"])
        .await?;
    assert!(any.visible_text().contains("E17"));

    let not_matched = pane
        .expect_visible_text()
        .not_to_match_regex(r"E\d+")
        .poll_interval(Duration::from_millis(10))
        .await?;
    assert_eq!(not_matched.visible_text(), "done");
    drop(pane);
    server.await??;
    Ok(())
}

#[cfg(feature = "regex")]
#[tokio::test]
async fn expect_visible_text_rejects_invalid_regex_before_polling() -> TestResult {
    let socket = TestSocket::new("visible-invalid-regex")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        assert_peer_closed_without_request(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let error = pane
        .expect_visible_text()
        .to_match_regex("[")
        .await
        .expect_err("invalid regex must be rejected before snapshot polling");
    match error {
        RmuxError::InvalidRegex { pattern, .. } => assert_eq!(pattern, "["),
        other => panic!("expected typed invalid regex error, got {other:?}"),
    }
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn expect_visible_text_timeout_includes_last_snapshot() -> TestResult {
    let socket = TestSocket::new("visible-timeout")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        while let Some(request) = peer.read_request().await? {
            match request {
                Request::ListPanes(_) => {
                    peer.write_response(Response::ListPanes(ListPanesResponse {
                        output: CommandOutput::from_stdout(b"0:0:%1\n".to_vec()),
                    }))
                    .await?;
                }
                Request::Handshake(_) => {
                    peer.write_response(Response::Handshake(
                        rmux_proto::HandshakeResponse::current(),
                    ))
                    .await?;
                }
                Request::PaneSnapshotRef(_) => {
                    peer.write_response(Response::PaneSnapshot(snapshot_response(
                        "last visible screen",
                        7,
                    )))
                    .await?;
                }
                other => panic!("visible wait should only poll snapshots, got {other:?}"),
            }
        }
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let error = pane
        .expect_visible_text()
        .to_contain("never")
        .timeout(Duration::from_millis(35))
        .poll_interval(Duration::from_millis(10))
        .await
        .expect_err("missing visible text must time out");
    let RmuxError::WaitTimeout { source, .. } = error else {
        return Err(format!("expected visible wait timeout, got {error:?}").into());
    };
    assert_eq!(source.matcher(), "contain `never`");
    assert!(source.last_visible_text().contains("last visible screen"));
    drop(pane);
    server.await??;
    Ok(())
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

async fn accept_peer(listener: &UnixListener) -> TestResult<Peer> {
    let (stream, _) = listener.accept().await?;
    Ok(Peer::new(stream))
}

async fn accept_sdk_wait(
    listener: &UnixListener,
    expected_bytes: &[u8],
) -> TestResult<(Peer, Vec<Peer>, SdkWaitForOutputRequest)> {
    let mut peers = Vec::new();
    let (mut main_index, mut request) =
        expect_request_from_existing_or_new(listener, &mut peers, Duration::from_secs(1)).await?;
    if let Request::Handshake(handshake) = &request {
        assert!(
            handshake
                .required_capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_SDK_WAITS_ARMED),
            "armed SDK wait handshake did not require {CAPABILITY_SDK_WAITS_ARMED}: {handshake:?}"
        );
        peers[main_index]
            .write_response(Response::Handshake(HandshakeResponse::current()))
            .await?;
        (main_index, request) =
            expect_request_from_existing_or_new(listener, &mut peers, Duration::from_secs(1))
                .await?;
    }
    let main = peers.swap_remove(main_index);
    let Request::SdkWaitForOutput(request) = request else {
        panic!("wait_for must use server-side SDK byte wait, got {request:?}");
    };
    assert_eq!(request.target, target().to_proto());
    assert_eq!(request.bytes, expected_bytes);
    assert_eq!(request.start, PaneOutputSubscriptionStart::Now);
    Ok((main, peers, request))
}

async fn expect_sdk_cancel_request(
    listener: &UnixListener,
    peers: &mut Vec<Peer>,
) -> TestResult<(usize, Request)> {
    expect_request_from_existing_or_new(listener, peers, Duration::from_secs(1)).await
}

async fn assert_no_sdk_cancel_request(
    listener: &UnixListener,
    peers: &mut Vec<Peer>,
) -> TestResult {
    let deadline = Instant::now() + Duration::from_millis(50);
    loop {
        if let Some((_, request)) = try_read_request_from_peers(peers)? {
            return Err(format!(
                "successful wait unexpectedly wrote a cancel request: {request:?}"
            )
            .into());
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }

        tokio::select! {
            accepted = accept_peer(listener) => peers.push(accepted?),
            _ = tokio::time::sleep(remaining.min(Duration::from_millis(10))) => {}
        }
    }
}

async fn expect_request_from_existing_or_new(
    listener: &UnixListener,
    peers: &mut Vec<Peer>,
    timeout: Duration,
) -> TestResult<(usize, Request)> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(found) = try_read_request_from_peers(peers)? {
            return Ok(found);
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("timed out waiting for SDK wait request".into());
        }

        tokio::select! {
            accepted = accept_peer(listener) => peers.push(accepted?),
            _ = tokio::time::sleep(remaining.min(Duration::from_millis(10))) => {}
        }
    }
}

fn try_read_request_from_peers(peers: &mut [Peer]) -> TestResult<Option<(usize, Request)>> {
    for (index, peer) in peers.iter_mut().enumerate() {
        if let Some(request) = peer.try_read_request()? {
            return Ok(Some((index, request)));
        }
    }
    Ok(None)
}

async fn expect_list_panes(peer: &mut Peer) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListPanes(request) = request else {
        panic!("snapshot wait must list panes before capture, got {request:?}");
    };
    assert_eq!(request.target, session_name());
    assert_eq!(request.target_window_index, Some(0));

    peer.write_response(Response::ListPanes(ListPanesResponse {
        output: CommandOutput::from_stdout(b"0:0:%1\n".to_vec()),
    }))
    .await
}

async fn expect_snapshot(peer: &mut Peer, text: &str, revision: u64) -> TestResult {
    // Slot snapshots probe capabilities once per transport and then route
    // through the pane id resolved by the preceding list (issue #94).
    let mut request = peer.expect_request().await?;
    if matches!(request, Request::Handshake(_)) {
        peer.write_response(Response::Handshake(rmux_proto::HandshakeResponse::current()))
            .await?;
        request = peer.expect_request().await?;
    }
    let Request::PaneSnapshotRef(request) = request else {
        panic!("snapshot wait must capture pane text by id, got {request:?}");
    };
    assert_eq!(
        request.target,
        rmux_proto::PaneTargetRef::by_id(session_name(), rmux_proto::PaneId::new(1)),
    );

    peer.write_response(Response::PaneSnapshot(snapshot_response(text, revision)))
        .await
}

fn snapshot_response(text: &str, revision: u64) -> PaneSnapshotResponse {
    PaneSnapshotResponse {
        cols: text.len() as u16,
        rows: 1,
        cells: text.bytes().map(snapshot_cell).collect(),
        cursor: PaneSnapshotCursor {
            row: 0,
            col: 0,
            visible: true,
            style: 0,
        },
        revision,
    }
}

fn snapshot_cell(byte: u8) -> PaneSnapshotCell {
    PaneSnapshotCell {
        text: char::from(byte).to_string(),
        width: 1,
        padding: false,
        attributes: 0,
        fg: 0,
        bg: 0,
        us: 0,
        link: 0,
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
        Ok(Ok(_)) => Err("text wait unexpectedly opened an SDK cancel connection".into()),
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

    fn try_read_request(&mut self) -> TestResult<Option<Request>> {
        if let Some(request) = self.decoder.next_frame::<Request>()? {
            return Ok(Some(request));
        }

        let mut buffer = [0_u8; 4096];
        match self.stream.try_read(&mut buffer) {
            Ok(0) => Ok(None),
            Ok(read) => {
                self.decoder.push_bytes(&buffer[..read]);
                Ok(self.decoder.next_frame::<Request>()?)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error.into()),
        }
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
        // macOS Unix socket paths must fit in sockaddr_un; avoid long TMPDIR.
        let root = PathBuf::from("/tmp").join(format!(
            "rmux-wt-{}-{}-{id}",
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
            "rmux-sdk-wait-live-{}-{}-{unique_id}",
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
