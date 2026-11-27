#![cfg(unix)]

mod common;

use std::error::Error;
use std::future::{Future, IntoFuture};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_proto::{
    encode_frame, CancelSdkWaitResponse, CommandOutput, FrameDecoder, HandshakeResponse,
    HasSessionRequest, ListPanesResponse, PaneId, PaneOutputSubscriptionStart, PaneSnapshotCell,
    PaneSnapshotCursor, PaneSnapshotResponse, PaneTargetRef, Request, Response,
    SdkWaitForOutputRefRequest, SdkWaitForOutputResponse, SdkWaitOutcome,
    CAPABILITY_SDK_WAITS_ARMED,
};
use rmux_sdk::{
    EnsureSession, Pane, PaneRef, RmuxBuilder, RmuxError, SessionName, SplitDirection,
    TerminalLoadState,
};
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
        RmuxError::Protocol { source, .. } if source.server_message().is_some() => {
            let message = source.server_message().unwrap();
            assert!(message.contains("must not be empty"))
        }
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
        RmuxError::Protocol { source, .. } if source.server_message().is_some() => {
            let message = source.server_message().unwrap();
            assert!(message.contains("text must not be empty"))
        }
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
    let session = live_session(&harness)?.ensure(&rmux).await?;
    let pane = session.pane(0, 0);
    let marker = "rmux_sdk_wait_text_live";
    wait_for_live_shell(&pane).await?;

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

/// The live pane's shell, `HOME` and `ENV` come from [`live_session`], not from
/// whoever runs the tests.
///
/// The daemon exports the shell it resolved, so the pane can report what
/// spawned it. `HOME` is checked too, so this stays red when the launcher's own
/// `SHELL` happens to be `/bin/sh`.
#[tokio::test]
async fn live_wait_session_pins_the_pane_environment() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("env-pin").await?;
    let rmux = harness.rmux();
    let session = live_session(&harness)?.ensure(&rmux).await?;
    let pane = session.pane(0, 0);
    let home = harness.pane_home()?;
    wait_for_live_shell(&pane).await?;

    // The echo carries `%s`, so only the shell's own output can carry the values.
    pane.send_text("printf 'Pinned=%s|%s|%s.\\n' \"$SHELL\" \"$HOME\" \"$ENV\"")
        .await?;
    pane.send_key("Enter").await?;
    pane.wait_for_text(format!("Pinned={LIVE_PANE_SHELL}|{}|.", home.display()))
        .await?;

    harness.finish().await
}

/// `close` leaves no exit status, so the pane never settled: like `rmux
/// wait-pane --quiet`, the wait must fail instead of reporting the vacant
/// slot's empty snapshot as quiet while the session lives on.
#[tokio::test]
async fn quiet_load_state_fails_closed_when_observed_pane_is_closed() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("quiet-closed").await?;
    let rmux = harness.rmux();
    let session = live_session(&harness)?.ensure(&rmux).await?;
    let observed = session.pane(0, 0).split(SplitDirection::Right).await?;
    let observed_id = observed.id().await?.expect("the split pane is live");
    let closer = observed.clone();

    let started = Instant::now();
    let (outcome, closed) = tokio::join!(
        observed
            .wait_for_load_state(TerminalLoadState::Quiet)
            .stable_for(Duration::from_secs(2))
            .timeout(Duration::from_secs(10))
            .into_future(),
        async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            closer.close().await
        }
    );
    closed?;
    match outcome {
        Err(RmuxError::PaneNotFound {
            session_name,
            pane_id,
            ..
        }) => {
            assert_eq!(session_name, self::session_name());
            assert_eq!(pane_id, observed_id);
        }
        other => panic!("a closed pane must fail the quiet wait, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the closed pane must fail the wait before its quiet window elapses"
    );
    assert!(session.exists().await?, "the session must outlive the pane");

    harness.finish().await
}

/// A process exit is a final state: like `rmux wait-pane --quiet`, it ends the
/// wait at once, with or without a surviving session, and the snapshot keeps
/// the documented empty revision-0 shape of a pane that is no longer listed.
#[tokio::test]
async fn quiet_load_state_completes_when_observed_process_exits() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("quiet-exit").await?;
    let rmux = harness.rmux();
    let session = live_session(&harness)?.ensure(&rmux).await?;
    let first = session.pane(0, 0);
    let observed = first.split(SplitDirection::Right).await?;

    for (pane, session_survives) in [(observed, true), (first, false)] {
        // Arm the wait on the live pane first: a pane already gone when the
        // wait begins is a vacant slot, which keeps its full quiet window.
        let started = Instant::now();
        let (outcome, sent) = tokio::join!(
            pane.wait_for_load_state(TerminalLoadState::Quiet)
                .stable_for(Duration::from_secs(5))
                .timeout(Duration::from_secs(10))
                .into_future(),
            async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                pane.send_text("exit 7").await?;
                pane.send_key("Enter").await
            }
        );
        sent?;
        assert_eq!(outcome?, rmux_sdk::PaneSnapshot::default());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a process exit must end the wait before its quiet window elapses"
        );
        assert_eq!(session.exists().await?, session_survives);
    }

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
    // A loaded host can spend a short deadline before the first snapshot
    // arrives. That attempt times out without a last snapshot to report, so
    // the wait is retried with a longer deadline until one snapshot lands.
    for timeout in widening_deadlines(Duration::from_millis(35)) {
        let socket = TestSocket::new("visible-timeout")?;
        let listener = UnixListener::bind(socket.path())?;
        let server = tokio::spawn(serve_snapshots_until_client_leaves(listener));

        let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
        let error = pane
            .expect_visible_text()
            .to_contain("never")
            .timeout(timeout)
            .poll_interval(Duration::from_millis(10))
            .await
            .expect_err("missing visible text must time out");
        drop(pane);
        server.await??;
        let RmuxError::WaitTimeout { source, .. } = error else {
            assert_timed_out(error, "wait for pane snapshot text");
            continue;
        };
        assert_eq!(source.matcher(), "contain `never`");
        assert!(source.last_visible_text().contains("last visible screen"));
        return Ok(());
    }
    Err("every visible wait deadline expired before the first snapshot".into())
}

#[tokio::test]
async fn visible_text_timeout_bounds_a_stalled_snapshot_rpc() -> TestResult {
    assert_wait_bounds_a_stalled_snapshot(
        "visible-stalled-snapshot",
        "wait for pane snapshot text",
        |pane, timeout| async move {
            pane.expect_visible_text()
                .to_contain("never")
                .timeout(timeout)
                .await
                .map(drop)
        },
    )
    .await
}

#[tokio::test]
async fn load_state_timeout_bounds_a_stalled_snapshot_rpc() -> TestResult {
    assert_wait_bounds_a_stalled_snapshot(
        "load-state-stalled-snapshot",
        "wait for terminal load-state snapshot",
        |pane, timeout| async move {
            pane.wait_for_load_state(TerminalLoadState::Quiet)
                .timeout(timeout)
                .await
                .map(drop)
        },
    )
    .await
}

#[tokio::test]
async fn locator_timeout_bounds_a_stalled_snapshot_rpc() -> TestResult {
    assert_wait_bounds_a_stalled_snapshot(
        "locator-stalled-snapshot",
        "wait for locator snapshot",
        |pane, timeout| async move {
            pane.get_by_text("never")
                .wait_for()
                .timeout(timeout)
                .await
                .map(drop)
        },
    )
    .await
}

#[tokio::test]
async fn locator_assertion_timeout_bounds_a_stalled_snapshot_rpc() -> TestResult {
    assert_wait_bounds_a_stalled_snapshot(
        "locator-assert-stalled-snapshot",
        "wait for locator snapshot",
        |pane, timeout| async move {
            pane.get_by_text("never")
                .expect()
                .to_be_visible()
                .timeout(timeout)
                .await
                .map(drop)
        },
    )
    .await
}

#[tokio::test]
async fn locator_action_timeout_bounds_a_stalled_snapshot_rpc() -> TestResult {
    assert_wait_bounds_a_stalled_snapshot(
        "locator-action-stalled-snapshot",
        "wait for locator snapshot",
        |pane, timeout| async move {
            pane.get_by_text("never")
                .timeout(timeout)
                .click()
                .await
                .map(drop)
        },
    )
    .await
}

/// Proves that a public wait deadline bounds a snapshot RPC the daemon never
/// answers.
///
/// The deadline also covers the identity lookup and handshake that precede
/// the snapshot RPC. On a loaded host it can expire there, and the client
/// leaves before the stall exists. That attempt must still time out with the
/// typed error, but it proves nothing about the stall, so the scenario is
/// replayed with a longer deadline until the snapshot RPC is reached.
async fn assert_wait_bounds_a_stalled_snapshot<W, F>(
    label: &str,
    operation: &str,
    wait: W,
) -> TestResult
where
    W: Fn(Pane, Duration) -> F,
    F: Future<Output = Result<(), RmuxError>>,
{
    for timeout in widening_deadlines(Duration::from_millis(30)) {
        let socket = TestSocket::new(label)?;
        let listener = UnixListener::bind(socket.path())?;
        let server = tokio::spawn(hold_first_snapshot_response(listener, timeout * 5));

        let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
        let error = tokio::time::timeout(timeout * 16, wait(pane.clone(), timeout))
            .await
            .expect("the public wait deadline must beat the external watchdog")
            .expect_err("a stalled snapshot must time out");
        assert_timed_out(error, operation);
        drop(pane);
        if server.await?? == SnapshotRpc::Stalled {
            return Ok(());
        }
    }
    Err(format!("{label}: every wait deadline expired before the snapshot RPC").into())
}

/// Deadlines for a timing scenario: the nominal one first, then doublings
/// for a host too loaded to reach the scenario in time.
fn widening_deadlines(nominal: Duration) -> impl Iterator<Item = Duration> {
    (0..5).map(move |doublings| nominal * (1 << doublings))
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

/// Shell every live-daemon pane in this file runs.
const LIVE_PANE_SHELL: &str = "/bin/sh";

/// Session the live-daemon tests drive, with a pane that owes nothing to the
/// account running the tests.
///
/// The first pane of a new session starts from the environment of the client
/// that creates it, here this test process read by pid, and the session's `-e`
/// assignments override it. Its shell is the `default-shell` option, else that
/// environment's `SHELL`, else the password database. Left alone, the pane runs
/// the launcher's shell with the account's startup files, and the two-second
/// text wait times their startup instead of rmux. `SHELL` alone is not enough:
/// the pane is a login shell, so `/bin/sh` still reads `$HOME/.profile`, and an
/// interactive `sh` sources the file named by `ENV`. Both lead nowhere here.
///
/// An empty `HISTFILE` keeps the shell off the disk when it leaves: after
/// `exit` prints `logout`, an interactive shell still writes and truncates its
/// history file before it closes the PTY, so a slow disk would keep an exited
/// pane listed and time that write instead of the daemon's exit handling.
fn live_session(harness: &Harness) -> TestResult<EnsureSession> {
    Ok(EnsureSession::named(session_name())
        .create_only()
        .environment(live_pane_environment(&harness.pane_home()?)))
}

fn live_pane_environment(home: &Path) -> [String; 4] {
    [
        format!("SHELL={LIVE_PANE_SHELL}"),
        format!("HOME={}", home.display()),
        "ENV=".to_owned(),
        "HISTFILE=".to_owned(),
    ]
}

/// Bound for a live pane's shell to come up and run its first line.
///
/// Pinning `HOME` and `ENV` leaves the system startup: `exec` of `/bin/sh`,
/// `/etc/profile` (`path_helper` on macOS) and the disk they read. A stall
/// there is not rmux's, so it gets this bound, not the two-second one of
/// [`Harness::rmux`].
const LIVE_SHELL_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// Returns once the pane's shell has read and run a line.
///
/// Text typed afterwards is read at once, so a two-second wait that follows
/// times rmux's input, rendering and snapshots, not the shell's startup.
async fn wait_for_live_shell(pane: &Pane) -> TestResult {
    // The echo carries `%s`, so only the shell's own output reads `Ready.`.
    pane.send_text("printf 'Ready%s\\n' .").await?;
    pane.send_key("Enter").await?;
    pane.expect_visible_text()
        .to_contain("Ready.")
        .timeout(LIVE_SHELL_STARTUP_TIMEOUT)
        .await?;
    Ok(())
}

async fn accept_peer(listener: &UnixListener) -> TestResult<Peer> {
    let (stream, _) = listener.accept().await?;
    Ok(Peer::new(stream))
}

async fn accept_sdk_wait(
    listener: &UnixListener,
    expected_bytes: &[u8],
) -> TestResult<(Peer, Vec<Peer>, SdkWaitForOutputRefRequest)> {
    let mut peers = Vec::new();
    let mut saw_armed_handshake = false;
    loop {
        let (peer_index, request) =
            expect_request_from_existing_or_new(listener, &mut peers, Duration::from_secs(1))
                .await?;
        match request {
            Request::Handshake(handshake) => {
                saw_armed_handshake |= handshake
                    .required_capabilities
                    .iter()
                    .any(|capability| capability == CAPABILITY_SDK_WAITS_ARMED);
                peers[peer_index]
                    .write_response(Response::Handshake(HandshakeResponse::current()))
                    .await?;
            }
            Request::ListPanes(request) => {
                assert_eq!(request.target, session_name());
                assert_eq!(request.target_window_index, Some(0));
                peers[peer_index]
                    .write_response(Response::ListPanes(ListPanesResponse {
                        output: CommandOutput::from_stdout("0:0:%1\n"),
                    }))
                    .await?;
            }
            Request::SdkWaitForOutputRef(request) => {
                assert!(
                    saw_armed_handshake,
                    "SDK wait transport did not require {CAPABILITY_SDK_WAITS_ARMED}"
                );
                assert_eq!(
                    request.target,
                    PaneTargetRef::by_id(session_name(), PaneId::new(1))
                );
                assert_eq!(request.bytes, expected_bytes);
                assert_eq!(request.start, PaneOutputSubscriptionStart::Now);
                let main = peers.swap_remove(peer_index);
                return Ok((main, peers, request));
            }
            request => panic!("wait_for must use server-side SDK byte wait, got {request:?}"),
        }
    }
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
    assert!(
        matches!(request.target_window_index, Some(0) | None),
        "snapshot identity lookup must stay in the target session"
    );

    peer.write_response(Response::ListPanes(ListPanesResponse {
        output: CommandOutput::from_stdout(b"0:0:%1\n".to_vec()),
    }))
    .await
}

async fn expect_snapshot(peer: &mut Peer, text: &str, revision: u64) -> TestResult {
    // Slot snapshots probe capabilities once per transport and then route
    // through the pane id resolved by the preceding list (issue #94).
    let mut request = peer.expect_request().await?;
    if let Request::ListPanes(list) = &request {
        assert_eq!(list.target, session_name());
        assert_eq!(list.target_window_index, None);
        peer.write_response(Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout(b"0:0:%1\n".to_vec()),
        }))
        .await?;
        request = peer.expect_request().await?;
    }
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

async fn serve_snapshots_until_client_leaves(listener: UnixListener) -> TestResult {
    let mut peer = accept_peer(&listener).await?;
    while let Some(request) = peer.next_request_unless_departed().await? {
        let response = match request {
            Request::ListPanes(_) => Response::ListPanes(ListPanesResponse {
                output: CommandOutput::from_stdout(b"0:0:%1\n".to_vec()),
            }),
            Request::Handshake(_) => Response::Handshake(HandshakeResponse::current()),
            Request::PaneSnapshotRef(_) => {
                Response::PaneSnapshot(snapshot_response("last visible screen", 7))
            }
            other => panic!("visible wait should only poll snapshots, got {other:?}"),
        };
        // The client leaves at its deadline, possibly with a snapshot still
        // in flight; this fake daemon then has nobody left to answer.
        if !peer.write_response_unless_departed(response).await? {
            break;
        }
    }
    Ok(())
}

/// How far a client got before leaving [`hold_first_snapshot_response`].
#[derive(Debug, PartialEq, Eq)]
enum SnapshotRpc {
    /// The snapshot RPC arrived and stayed unanswered for the whole hold.
    Stalled,
    /// The client left before sending the snapshot RPC.
    NotReached,
}

async fn hold_first_snapshot_response(
    listener: UnixListener,
    hold: Duration,
) -> TestResult<SnapshotRpc> {
    let mut peer = accept_peer(&listener).await?;
    if !reach_first_snapshot_request(&mut peer).await? {
        return Ok(SnapshotRpc::NotReached);
    }
    tokio::time::sleep(hold).await;
    Ok(SnapshotRpc::Stalled)
}

/// Answers the requests that precede a wait's first pane snapshot RPC.
///
/// Returns `false` when the client leaves before that RPC.
async fn reach_first_snapshot_request(peer: &mut Peer) -> TestResult<bool> {
    let Some(request) = peer.next_request_unless_departed().await? else {
        return Ok(false);
    };
    let Request::ListPanes(list) = &request else {
        panic!("snapshot wait must list panes before capture, got {request:?}");
    };
    assert_eq!(list.target, session_name());
    assert!(
        matches!(list.target_window_index, Some(0) | None),
        "snapshot identity lookup must stay in the target session"
    );
    let Some(mut request) = peer.answer_list_panes_then_read().await? else {
        return Ok(false);
    };
    if let Request::ListPanes(list) = &request {
        assert_eq!(list.target, session_name());
        assert_eq!(list.target_window_index, None);
        let Some(next) = peer.answer_list_panes_then_read().await? else {
            return Ok(false);
        };
        request = next;
    }
    if matches!(request, Request::Handshake(_)) {
        let handshake = Response::Handshake(HandshakeResponse::current());
        if !peer.write_response_unless_departed(handshake).await? {
            return Ok(false);
        }
        let Some(next) = peer.next_request_unless_departed().await? else {
            return Ok(false);
        };
        request = next;
    }
    assert!(
        matches!(request, Request::PaneSnapshotRef(_)),
        "snapshot wait must issue a pane snapshot RPC, got {request:?}"
    );
    Ok(true)
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

    /// Reads the next request, or `None` once the client has left.
    ///
    /// A client that leaves with responses still unread resets the
    /// connection instead of closing it, so a reset also means departure.
    async fn next_request_unless_departed(&mut self) -> TestResult<Option<Request>> {
        match self.read_request().await {
            Err(error) if is_client_departure(error.as_ref()) => Ok(None),
            result => result,
        }
    }

    /// Writes a response, or returns `false` when the client has left.
    async fn write_response_unless_departed(&mut self, response: Response) -> TestResult<bool> {
        match self.write_response(response).await {
            Ok(()) => Ok(true),
            Err(error) if is_client_departure(error.as_ref()) => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn answer_list_panes_then_read(&mut self) -> TestResult<Option<Request>> {
        let list = Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout(b"0:0:%1\n".to_vec()),
        });
        if !self.write_response_unless_departed(list).await? {
            return Ok(None);
        }
        self.next_request_unless_departed().await
    }
}

fn is_client_departure(error: &(dyn Error + Send + Sync + 'static)) -> bool {
    error.downcast_ref::<io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
        )
    })
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
    root: TestRoot,
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
            root,
            socket_path,
            child: Some(child),
        })
    }

    /// Empty `HOME` for live panes, so no account startup file is reachable.
    fn pane_home(&self) -> TestResult<PathBuf> {
        let home = self.root.path().join("home");
        std::fs::create_dir_all(&home)?;
        Ok(home)
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
