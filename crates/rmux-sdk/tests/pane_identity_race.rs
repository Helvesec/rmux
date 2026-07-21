#![cfg(unix)]

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmux_proto::{
    encode_frame, CommandOutput, ErrorResponse, FrameDecoder, HandshakeResponse, ListPanesResponse,
    ListSessionsResponse, ListWindowsResponse, PaneOutputCursor, PaneOutputCursorResponse,
    PaneOutputEvent, PaneOutputSubscriptionId, PaneOutputSubscriptionStart, PaneSnapshotCell,
    PaneSnapshotCursor, PaneSnapshotResponse, PaneStateSnapshot, PaneStateSubscriptionId,
    PaneTarget, PaneTargetRef, Request, ResizePaneAdjustment, ResizePaneResponse, Response,
    SubscribePaneOutputResponse, SubscribePaneStateResponse, TerminalSize, WindowListEntry,
    WindowTarget, CAPABILITY_HANDSHAKE,
};
use rmux_sdk::{
    LayoutName, Pane, PaneId, PaneRef, PaneStateEvent, PaneStateEventsOptions, RmuxBuilder,
    RmuxError, SessionId, SessionName, TerminalSizeSpec, WindowId,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn stable_id_id_retries_reverse_after_inter_session_move_overlaps_scan() -> TestResult {
    let socket = TestSocket::new("id-move-race")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_initial_preferred_lookup(&mut peer).await?;
        expect_overlapping_move_resolution(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_by_id(socket.path()).await?;
    assert_eq!(pane.id().await?, Some(pane_id()));
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn stable_id_snapshot_never_defaults_when_inter_session_move_overlaps_scan() -> TestResult {
    let socket = TestSocket::new("snapshot-move-race")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_initial_preferred_lookup(&mut peer).await?;
        expect_overlapping_move_resolution(&mut peer).await?;
        expect_by_id_handshake(&mut peer).await?;
        expect_snapshot(&mut peer, 41, "moved").await?;
        TestResult::Ok(())
    });

    let pane = pane_by_id(socket.path()).await?;
    let snapshot = pane.snapshot().await?;
    assert_eq!(snapshot.revision, 41);
    assert_eq!(snapshot.visible_text(), "moved");
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn stable_id_render_stream_survives_inter_session_move_overlapping_open() -> TestResult {
    let socket = TestSocket::new("render-move-race")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_initial_preferred_lookup(&mut peer).await?;

        // Opening the raw output half of the render stream overlaps C -> B.
        expect_overlapping_move_resolution(&mut peer).await?;
        expect_direct_beta_resolution(&mut peer).await?;
        expect_by_id_handshake(&mut peer).await?;
        let subscription_id = expect_output_subscription(&mut peer).await?;

        // The baseline and output-driven snapshots both resolve the new B
        // location and must never inherit the stale A slot.
        expect_direct_beta_resolution(&mut peer).await?;
        expect_snapshot(&mut peer, 41, "base").await?;
        expect_output_event(&mut peer, subscription_id).await?;
        expect_direct_beta_resolution(&mut peer).await?;
        expect_snapshot(&mut peer, 42, "updated").await?;
        TestResult::Ok(())
    });

    let pane = pane_by_id(socket.path()).await?;
    let mut render = pane.render_stream().await?.with_debounce(Duration::ZERO);
    let update = render
        .next()
        .await?
        .expect("output produces a render update");
    assert_eq!(update.snapshot().revision, 42);
    assert_eq!(update.snapshot().visible_text(), "updated");
    drop(render);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn stable_id_output_stream_retries_a_move_after_resolution() -> TestResult {
    let socket = TestSocket::new("output-post-resolve")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_initial_preferred_lookup(&mut peer).await?;
        expect_direct_beta_resolution(&mut peer).await?;
        expect_by_id_handshake(&mut peer).await?;
        expect_output_subscription_stale(&mut peer, resolved_target()).await?;
        expect_direct_source_resolution(&mut peer).await?;
        expect_output_subscription_at(
            &mut peer,
            source_target(),
            source_slot(),
            PaneOutputSubscriptionId::new(23),
        )
        .await?;
        TestResult::Ok(())
    });

    let pane = pane_by_id(socket.path()).await?;
    let output = pane.output_stream().await?;
    drop(output);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn stable_id_render_stream_retries_an_output_move_after_resolution() -> TestResult {
    let socket = TestSocket::new("render-post-resolve")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_initial_preferred_lookup(&mut peer).await?;
        expect_direct_beta_resolution(&mut peer).await?;
        expect_direct_beta_resolution(&mut peer).await?;
        expect_by_id_handshake(&mut peer).await?;
        expect_output_subscription_stale(&mut peer, resolved_target()).await?;
        expect_direct_source_resolution(&mut peer).await?;
        expect_output_subscription_at(
            &mut peer,
            source_target(),
            source_slot(),
            PaneOutputSubscriptionId::new(29),
        )
        .await?;
        expect_direct_source_resolution(&mut peer).await?;
        expect_snapshot_at(&mut peer, source_target(), 43, "source").await?;
        TestResult::Ok(())
    });

    let pane = pane_by_id(socket.path()).await?;
    let render = pane.render_stream().await?;
    drop(render);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn slot_resize_keeps_the_preflighted_pane_after_slot_replacement() -> TestResult {
    let socket = TestSocket::new("resize-slot-reuse")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_slot_lookup(&mut peer, "0:0:%7\n").await?;
        expect_list_panes(&mut peer, &preferred_session(), Some("0:0:%8\n0:1:%7\n")).await?;
        expect_by_id_handshake(&mut peer).await?;
        expect_snapshot_at(&mut peer, preferred_target(), 1, "x").await?;

        expect_list_panes(&mut peer, &preferred_session(), Some("0:0:%8\n0:1:%7\n")).await?;
        expect_resize(
            &mut peer,
            ResizePaneAdjustment::AbsoluteWidth { columns: 2 },
        )
        .await?;

        expect_list_panes(&mut peer, &preferred_session(), Some("0:0:%8\n0:1:%7\n")).await?;
        expect_resize(&mut peer, ResizePaneAdjustment::AbsoluteHeight { rows: 3 }).await?;
        TestResult::Ok(())
    });

    let pane = pane_at_slot(socket.path()).await?;
    pane.resize(TerminalSizeSpec::new(2, 3)).await?;
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn slot_render_stream_keeps_output_and_snapshots_on_one_identity() -> TestResult {
    let socket = TestSocket::new("render-slot-reuse")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_slot_lookup(&mut peer, "0:0:%7\n").await?;
        expect_list_panes(&mut peer, &preferred_session(), Some("0:0:%8\n0:1:%7\n")).await?;
        expect_by_id_handshake(&mut peer).await?;
        let subscription_id = PaneOutputSubscriptionId::new(35);
        expect_output_subscription_at(
            &mut peer,
            preferred_target(),
            preferred_moved_slot(),
            subscription_id,
        )
        .await?;
        expect_list_panes(&mut peer, &preferred_session(), Some("0:0:%8\n0:1:%7\n")).await?;
        expect_snapshot_at(&mut peer, preferred_target(), 41, "base").await?;
        expect_output_event(&mut peer, subscription_id).await?;
        expect_list_panes(&mut peer, &preferred_session(), Some("0:0:%8\n0:1:%7\n")).await?;
        expect_snapshot_at(&mut peer, preferred_target(), 42, "updated").await?;
        TestResult::Ok(())
    });

    let pane = pane_at_slot(socket.path()).await?;
    let mut render = pane.render_stream().await?.with_debounce(Duration::ZERO);
    let update = render
        .next()
        .await?
        .expect("output produces a render update");
    assert_eq!(update.snapshot().revision, 42);
    assert_eq!(update.snapshot().visible_text(), "updated");
    drop(render);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn stable_info_retries_instead_of_returning_replacement_metadata() -> TestResult {
    assert_info_retries_after_slot_replacement(true).await
}

#[tokio::test]
async fn slot_info_pins_identity_before_assembling_metadata() -> TestResult {
    assert_info_retries_after_slot_replacement(false).await
}

#[tokio::test]
async fn stable_info_preserves_parent_when_pinned_pane_disappears() -> TestResult {
    assert_info_preserves_parent_when_pane_disappears(true).await
}

#[tokio::test]
async fn slot_info_preserves_parent_when_pinned_pane_disappears() -> TestResult {
    assert_info_preserves_parent_when_pane_disappears(false).await
}

#[tokio::test]
async fn slot_info_does_not_adopt_recreated_session_parent() -> TestResult {
    let socket = TestSocket::new("info-session-recreated")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_info_slot_location(&mut peer, Some("0\t0\t%7\t$1\t@1\n")).await?;
        for _ in 0..2 {
            expect_info_location(&mut peer, &preferred_session(), Some("0\t0\t%8\t$2\t@2\n"))
                .await?;
            expect_info_session_output(&mut peer, &format!("{}\t$2\n", preferred_session()))
                .await?;
        }
        expect_info_session_output(&mut peer, &format!("{}\t$2\n", preferred_session())).await?;
        TestResult::Ok(())
    });

    let pane = pane_at_slot(socket.path()).await?;
    assert_eq!(pane.info().await?, rmux_sdk::InfoSnapshot::default());
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn slot_info_does_not_adopt_recreated_window_parent() -> TestResult {
    let socket = TestSocket::new("info-window-recreated")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_info_slot_location(&mut peer, Some("0\t0\t%7\t$1\t@1\n")).await?;
        for _ in 0..2 {
            expect_info_location(&mut peer, &preferred_session(), Some("0\t0\t%8\t$1\t@2\n"))
                .await?;
            expect_info_session(&mut peer).await?;
        }
        expect_info_session(&mut peer).await?;
        expect_info_window_identity(&mut peer, &preferred_session(), "@2").await?;
        expect_info_session(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_at_slot(socket.path()).await?;
    let info = pane.info().await?;
    assert_eq!(info.sessions.len(), 1);
    assert_eq!(info.sessions[0].id, SessionId::new(1));
    assert!(info.windows.is_empty());
    assert!(info.panes.is_empty());
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn stable_info_preserves_renamed_parent_when_pane_disappears() -> TestResult {
    let socket = TestSocket::new("info-session-renamed")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_list_panes(&mut peer, &preferred_session(), Some("0:0:%7\n")).await?;
        expect_info_location(&mut peer, &preferred_session(), Some("0\t0\t%7\t$1\t@1\n")).await?;

        let renamed = renamed_session();
        let renamed_inventory = format!("{renamed}\t$1\n");
        expect_info_session_output(&mut peer, &renamed_inventory).await?;
        for _ in 0..2 {
            expect_info_location(&mut peer, &preferred_session(), None).await?;
            expect_info_session_output(&mut peer, &renamed_inventory).await?;
            expect_info_location(&mut peer, &renamed, None).await?;
        }
        expect_info_session_output(&mut peer, &renamed_inventory).await?;
        expect_info_window_identity(&mut peer, &renamed, "@1").await?;
        expect_info_session_output(&mut peer, &renamed_inventory).await?;
        TestResult::Ok(())
    });

    let pane = pane_by_id(socket.path()).await?;
    let info = pane.info().await?;
    assert_eq!(info.sessions.len(), 1);
    assert_eq!(info.sessions[0].id, SessionId::new(1));
    assert_eq!(info.sessions[0].name, renamed_session());
    assert_eq!(info.windows.len(), 1);
    assert_eq!(info.windows[0].id, WindowId::new(1));
    assert!(info.panes.is_empty());
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn slot_wait_for_text_never_matches_replacement_output() -> TestResult {
    let socket = TestSocket::new("wait-text-reuse")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_slot_lookup(&mut peer, "0:0:%7\n").await?;
        let mut snapshot_count = 0;
        while let Some(request) = peer.read_request().await? {
            match request {
                Request::ListPanes(request) => {
                    assert_eq!(request.target, preferred_session());
                    assert_eq!(request.target_window_index, None);
                    peer.write_response(Response::ListPanes(ListPanesResponse {
                        output: CommandOutput::from_stdout(b"0:0:%8\n0:1:%7\n".to_vec()),
                    }))
                    .await?;
                }
                Request::Handshake(_) => {
                    peer.write_response(Response::Handshake(HandshakeResponse::current()))
                        .await?;
                }
                Request::PaneSnapshotRef(request) => {
                    assert_eq!(request.target, preferred_target());
                    snapshot_count += 1;
                    peer.write_response(Response::PaneSnapshot(snapshot_response(
                        "original",
                        snapshot_count,
                    )))
                    .await?;
                }
                request => {
                    return Err(format!("unexpected wait-for-text request: {request:?}").into())
                }
            }
        }
        assert!(snapshot_count > 0, "wait must poll the original pane");
        TestResult::Ok(())
    });

    let pane = pane_at_slot_with_timeout(socket.path(), Duration::from_millis(80)).await?;
    let error = pane
        .wait_for_text("replacement")
        .await
        .expect_err("replacement output must not satisfy the wait");
    assert!(matches!(
        error,
        RmuxError::Transport { ref source, .. } if source.kind() == io::ErrorKind::TimedOut
    ));
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn slot_wait_exit_finishes_for_original_after_reindex() -> TestResult {
    let socket = TestSocket::new("wait-exit-reuse")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_slot_lookup(&mut peer, "0:0:%7\n").await?;

        expect_info_location(&mut peer, &preferred_session(), Some("0\t0\t%7\t$1\t@1\n")).await?;
        expect_info_session(&mut peer).await?;
        expect_info_window(&mut peer).await?;
        expect_slot_lookup(&mut peer, "0:0:%7\n").await?;
        expect_info_details(&mut peer).await?;
        expect_info_session(&mut peer).await?;
        expect_info_window(&mut peer).await?;
        expect_slot_lookup(&mut peer, "0:0:%7\n").await?;

        for _ in 0..2 {
            expect_info_location(&mut peer, &preferred_session(), Some("0\t0\t%8\t$1\t@1\n"))
                .await?;
            expect_single_session_inventory(&mut peer).await?;
        }
        TestResult::Ok(())
    });

    let pane = pane_at_slot_with_timeout(socket.path(), Duration::from_millis(500)).await?;
    let exit = tokio::time::timeout(Duration::from_millis(250), pane.wait_exit())
        .await
        .expect("wait_exit must finish for the original pane")?;
    assert_eq!(exit, None);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn stable_id_state_stream_retries_a_move_after_resolution() -> TestResult {
    let socket = TestSocket::new("state-post-resolve")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut main_peer = accept_peer(&listener).await?;
        expect_initial_preferred_lookup(&mut main_peer).await?;
        expect_direct_beta_resolution(&mut main_peer).await?;
        expect_by_id_handshake(&mut main_peer).await?;

        let mut cursor_peer = accept_peer(&listener).await?;
        expect_by_id_handshake(&mut cursor_peer).await?;
        expect_state_subscription_stale(&mut cursor_peer, resolved_target()).await?;
        expect_direct_source_resolution(&mut main_peer).await?;
        expect_state_subscription_at(&mut cursor_peer, source_target()).await?;
        TestResult::Ok(())
    });

    let pane = pane_by_id(socket.path()).await?;
    let mut stream = pane.state_events(PaneStateEventsOptions::default()).await?;
    let event = stream.next().await?.expect("initial state snapshot");
    assert!(matches!(
        event,
        PaneStateEvent::Snapshot {
            pane_id: id,
            title: Some(ref title),
            ..
        } if id == pane_id() && title == "source"
    ));
    drop(stream);
    drop(pane);
    server.await??;
    Ok(())
}

async fn pane_by_id(socket_path: &Path) -> TestResult<Pane> {
    let rmux = RmuxBuilder::new()
        .unix_socket(socket_path)
        .default_timeout(Duration::from_secs(2))
        .build();
    Ok(rmux.pane_by_id(preferred_session(), pane_id()).await?)
}

async fn pane_at_slot(socket_path: &Path) -> TestResult<Pane> {
    pane_at_slot_with_timeout(socket_path, Duration::from_secs(2)).await
}

async fn pane_at_slot_with_timeout(socket_path: &Path, timeout: Duration) -> TestResult<Pane> {
    let rmux = RmuxBuilder::new()
        .unix_socket(socket_path)
        .default_timeout(timeout)
        .build();
    Ok(rmux.pane(PaneRef::new(preferred_session(), 0, 0)).await?)
}

async fn assert_info_retries_after_slot_replacement(stable: bool) -> TestResult {
    let socket = TestSocket::new(if stable {
        "info-stable-reuse"
    } else {
        "info-slot-reuse"
    })?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        if stable {
            expect_list_panes(&mut peer, &preferred_session(), Some("0:0:%7\n")).await?;
        } else {
            expect_info_slot_location(&mut peer, Some("0\t0\t%7\t$1\t@1\n")).await?;
        }

        expect_info_location(&mut peer, &preferred_session(), Some("0\t0\t%7\t$1\t@1\n")).await?;
        expect_info_session(&mut peer).await?;
        expect_info_window(&mut peer).await?;
        if stable {
            expect_slot_lookup(&mut peer, "0:0:%7\n").await?;
            expect_info_details(&mut peer).await?;
            expect_info_session(&mut peer).await?;
            expect_info_window(&mut peer).await?;
            expect_slot_lookup(&mut peer, "0:0:%8\n").await?;
        } else {
            expect_slot_lookup(&mut peer, "0:0:%8\n").await?;
        }

        expect_info_location(
            &mut peer,
            &preferred_session(),
            Some("0\t0\t%8\t$1\t@1\n0\t1\t%7\t$1\t@1\n"),
        )
        .await?;
        expect_info_session(&mut peer).await?;
        expect_info_window(&mut peer).await?;
        expect_slot_lookup(&mut peer, "0:0:%8\n0:1:%7\n").await?;
        expect_info_details(&mut peer).await?;
        expect_info_session(&mut peer).await?;
        expect_info_window(&mut peer).await?;
        expect_slot_lookup(&mut peer, "0:0:%8\n0:1:%7\n").await?;
        TestResult::Ok(())
    });

    let pane = if stable {
        pane_by_id(socket.path()).await?
    } else {
        pane_at_slot(socket.path()).await?
    };
    let info = pane.info().await?;
    assert_eq!(info.panes.len(), 1);
    assert_eq!(info.panes[0].id, pane_id());
    assert_eq!(info.panes[0].index, 1);
    drop(pane);
    server.await??;
    Ok(())
}

async fn assert_info_preserves_parent_when_pane_disappears(stable: bool) -> TestResult {
    let socket = TestSocket::new(if stable {
        "info-stable-disappears"
    } else {
        "info-slot-disappears"
    })?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        if stable {
            expect_list_panes(&mut peer, &preferred_session(), Some("0:0:%7\n")).await?;
        } else {
            expect_info_slot_location(&mut peer, Some("0\t0\t%7\t$1\t@1\n")).await?;
        }

        expect_info_location(&mut peer, &preferred_session(), Some("0\t0\t%7\t$1\t@1\n")).await?;
        expect_info_session(&mut peer).await?;
        expect_info_window(&mut peer).await?;
        expect_slot_lookup(&mut peer, "").await?;

        for _ in 0..2 {
            expect_info_location(&mut peer, &preferred_session(), None).await?;
            expect_single_session_inventory(&mut peer).await?;
        }

        expect_info_session(&mut peer).await?;
        expect_info_window(&mut peer).await?;
        expect_info_session(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = if stable {
        pane_by_id(socket.path()).await?
    } else {
        pane_at_slot(socket.path()).await?
    };
    let info = pane.info().await?;
    assert_eq!(info.sessions.len(), 1);
    assert_eq!(info.sessions[0].id, SessionId::new(1));
    assert_eq!(info.windows.len(), 1);
    assert_eq!(info.windows[0].id, WindowId::new(1));
    assert!(info.panes.is_empty());
    drop(pane);
    server.await??;
    Ok(())
}

async fn expect_initial_preferred_lookup(peer: &mut Peer) -> TestResult {
    expect_list_panes(peer, &preferred_session(), Some("0:0:%7\n")).await
}

async fn expect_overlapping_move_resolution(peer: &mut Peer) -> TestResult {
    // First sweep: B is observed before the pane moves there; C is observed
    // after it left. A single forward sweep therefore sees neither location.
    expect_list_panes(peer, &preferred_session(), None).await?;
    expect_session_inventory(peer).await?;
    expect_list_panes(peer, &destination_session(), None).await?;
    expect_list_panes(peer, &source_session(), None).await?;

    // Second sweep: retain preferred-alias priority, refresh the inventory,
    // then reverse C/B so the moved pane is found in B.
    expect_list_panes(peer, &preferred_session(), None).await?;
    expect_session_inventory(peer).await?;
    expect_list_panes(peer, &source_session(), None).await?;
    expect_list_panes(peer, &destination_session(), Some("4:2:%7\n")).await
}

async fn expect_direct_beta_resolution(peer: &mut Peer) -> TestResult {
    expect_list_panes(peer, &preferred_session(), None).await?;
    expect_session_inventory(peer).await?;
    expect_list_panes(peer, &destination_session(), Some("4:2:%7\n")).await
}

async fn expect_direct_source_resolution(peer: &mut Peer) -> TestResult {
    expect_list_panes(peer, &preferred_session(), None).await?;
    expect_session_inventory(peer).await?;
    expect_list_panes(peer, &destination_session(), None).await?;
    expect_list_panes(peer, &source_session(), Some("5:3:%7\n")).await
}

async fn expect_list_panes(
    peer: &mut Peer,
    session_name: &SessionName,
    output: Option<&str>,
) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListPanes(request) = request else {
        return Err(format!("expected list-panes for {session_name}, got {request:?}").into());
    };
    assert_eq!(&request.target, session_name);
    assert_eq!(request.target_window_index, None);
    assert_eq!(
        request.format.as_deref(),
        Some("#{window_index}:#{pane_index}:#{pane_id}")
    );
    peer.write_response(Response::ListPanes(ListPanesResponse {
        output: CommandOutput::from_stdout(output.unwrap_or_default().as_bytes().to_vec()),
    }))
    .await
}

async fn expect_info_location(
    peer: &mut Peer,
    session_name: &SessionName,
    output: Option<&str>,
) -> TestResult {
    expect_info_location_at(peer, session_name, None, output).await
}

async fn expect_info_slot_location(peer: &mut Peer, output: Option<&str>) -> TestResult {
    expect_info_location_at(peer, &preferred_session(), Some(0), output).await
}

async fn expect_info_location_at(
    peer: &mut Peer,
    session_name: &SessionName,
    target_window_index: Option<u32>,
    output: Option<&str>,
) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListPanes(request) = request else {
        return Err(
            format!("expected pane info location for {session_name}, got {request:?}").into(),
        );
    };
    assert_eq!(&request.target, session_name);
    assert_eq!(request.target_window_index, target_window_index);
    assert_eq!(
        request.format.as_deref(),
        Some("#{window_index}\t#{pane_index}\t#{pane_id}\t#{session_id}\t#{window_id}")
    );
    peer.write_response(Response::ListPanes(ListPanesResponse {
        output: CommandOutput::from_stdout(output.unwrap_or_default().as_bytes().to_vec()),
    }))
    .await
}

async fn expect_slot_lookup(peer: &mut Peer, output: &str) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListPanes(request) = request else {
        return Err(format!("expected slot list-panes, got {request:?}").into());
    };
    assert_eq!(request.target, preferred_session());
    assert_eq!(request.target_window_index, Some(0));
    assert_eq!(
        request.format.as_deref(),
        Some("#{window_index}:#{pane_index}:#{pane_id}")
    );
    peer.write_response(Response::ListPanes(ListPanesResponse {
        output: CommandOutput::from_stdout(output.as_bytes().to_vec()),
    }))
    .await
}

async fn expect_resize(peer: &mut Peer, expected: ResizePaneAdjustment) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::PaneResize(request) = request else {
        return Err(format!("expected pane resize, got {request:?}").into());
    };
    assert_eq!(request.target, preferred_target());
    assert_eq!(request.adjustment, expected);
    peer.write_response(Response::ResizePane(ResizePaneResponse {
        target: preferred_moved_slot(),
        adjustment: request.adjustment,
    }))
    .await
}

async fn expect_info_session(peer: &mut Peer) -> TestResult {
    expect_info_session_output(peer, &format!("{}\t$1\n", preferred_session())).await
}

async fn expect_info_session_output(peer: &mut Peer, output: &str) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListSessions(request) = request else {
        return Err(format!("expected list-sessions for pane info, got {request:?}").into());
    };
    assert_eq!(
        request.format.as_deref(),
        Some("#{session_name}\t#{session_id}")
    );
    peer.write_response(Response::ListSessions(ListSessionsResponse {
        output: CommandOutput::from_stdout(output.as_bytes().to_vec()),
    }))
    .await
}

async fn expect_info_window(peer: &mut Peer) -> TestResult {
    expect_info_window_identity(peer, &preferred_session(), "@1").await
}

async fn expect_info_window_identity(
    peer: &mut Peer,
    session_name: &SessionName,
    window_id: &str,
) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListWindows(request) = request else {
        return Err(format!("expected list-windows for pane info, got {request:?}").into());
    };
    assert_eq!(&request.target, session_name);
    peer.write_response(Response::ListWindows(ListWindowsResponse {
        windows: vec![WindowListEntry {
            target: WindowTarget::with_window(session_name.clone(), 0),
            window_id: window_id.to_owned(),
            name: Some("main".to_owned()),
            pane_count: 2,
            size: TerminalSize { cols: 80, rows: 24 },
            layout: LayoutName::Tiled,
            active: true,
            last: false,
            rendered: String::new(),
        }],
        output: CommandOutput::from_stdout(Vec::new()),
    }))
    .await
}

async fn expect_info_details(peer: &mut Peer) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListPanes(request) = request else {
        return Err(format!("expected pane detail list, got {request:?}").into());
    };
    assert_eq!(request.target, preferred_session());
    assert_eq!(request.target_window_index, None);
    assert_eq!(
        request.format.as_deref(),
        Some(
            "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_dead_signal}\
             \t#{pane_width}\t#{pane_height}\t#{cursor_x}\t#{cursor_y}\t#{cursor_flag}\
             \t#{cursor_shape}\t#{history_bytes}\t#{history_size}\t#{pane_start_command}\
             \t#{pane_lifecycle_generation}\t#{pane_lifecycle_revision}\t#{pane_output_sequence}\
             \t#{pane_start_path}"
        )
    );
    peer.write_response(Response::ListPanes(ListPanesResponse {
        output: CommandOutput::from_stdout(
            b"%7\t1234\t0\t\t\t80\t24\t0\t0\t1\t0\t0\t0\t\t1\t2\t3\t/tmp\n".to_vec(),
        ),
    }))
    .await
}

async fn expect_session_inventory(peer: &mut Peer) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListSessions(request) = request else {
        return Err(format!("expected list-sessions, got {request:?}").into());
    };
    assert_eq!(
        request.format.as_deref(),
        Some("#{session_name}\t#{session_id}")
    );
    let output = format!(
        "{}\t$1\n{}\t$2\n{}\t$3\n",
        preferred_session(),
        destination_session(),
        source_session()
    );
    peer.write_response(Response::ListSessions(ListSessionsResponse {
        output: CommandOutput::from_stdout(output.into_bytes()),
    }))
    .await
}

async fn expect_single_session_inventory(peer: &mut Peer) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListSessions(request) = request else {
        return Err(format!("expected single-session inventory, got {request:?}").into());
    };
    assert_eq!(
        request.format.as_deref(),
        Some("#{session_name}\t#{session_id}")
    );
    peer.write_response(Response::ListSessions(ListSessionsResponse {
        output: CommandOutput::from_stdout(format!("{}\t$1\n", preferred_session())),
    }))
    .await
}

async fn expect_by_id_handshake(peer: &mut Peer) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::Handshake(request) = request else {
        return Err(format!("expected capability handshake, got {request:?}").into());
    };
    assert!(
        request
            .required_capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_HANDSHAKE),
        "capability negotiation must require {CAPABILITY_HANDSHAKE}"
    );
    peer.write_response(Response::Handshake(HandshakeResponse::current()))
        .await
}

async fn expect_output_subscription(peer: &mut Peer) -> TestResult<PaneOutputSubscriptionId> {
    let subscription_id = PaneOutputSubscriptionId::new(19);
    expect_output_subscription_at(peer, resolved_target(), resolved_slot(), subscription_id)
        .await?;
    Ok(subscription_id)
}

async fn expect_output_subscription_at(
    peer: &mut Peer,
    expected_target: PaneTargetRef,
    response_target: PaneTarget,
    subscription_id: PaneOutputSubscriptionId,
) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::SubscribePaneOutputRef(request) = request else {
        return Err(format!("expected by-id output subscription, got {request:?}").into());
    };
    assert_eq!(request.target, expected_target);
    assert_eq!(request.start, PaneOutputSubscriptionStart::Now);

    peer.write_response(Response::SubscribePaneOutput(SubscribePaneOutputResponse {
        subscription_id,
        target: response_target,
        pane_id: pane_id(),
        cursor: PaneOutputCursor {
            next_sequence: 1,
            missed_events: 0,
        },
    }))
    .await
}

async fn expect_output_subscription_stale(
    peer: &mut Peer,
    expected_target: PaneTargetRef,
) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::SubscribePaneOutputRef(request) = request else {
        return Err(format!("expected by-id output subscription, got {request:?}").into());
    };
    assert_eq!(request.target, expected_target);
    peer.write_response(Response::Error(ErrorResponse {
        error: rmux_proto::RmuxError::pane_not_found(destination_session(), pane_id()),
    }))
    .await
}

async fn expect_state_subscription_stale(
    peer: &mut Peer,
    expected_target: PaneTargetRef,
) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::SubscribePaneState(request) = request else {
        return Err(format!("expected pane-state subscription, got {request:?}").into());
    };
    assert_eq!(request.target, expected_target);
    peer.write_response(Response::Error(ErrorResponse {
        error: rmux_proto::RmuxError::pane_not_found(destination_session(), pane_id()),
    }))
    .await
}

async fn expect_state_subscription_at(
    peer: &mut Peer,
    expected_target: PaneTargetRef,
) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::SubscribePaneState(request) = request else {
        return Err(format!("expected pane-state subscription, got {request:?}").into());
    };
    assert_eq!(request.target, expected_target);
    peer.write_response(Response::SubscribePaneState(Box::new(
        SubscribePaneStateResponse {
            subscription_id: PaneStateSubscriptionId::new(31),
            pane_id: pane_id(),
            snapshot: PaneStateSnapshot {
                revision: 44,
                title: Some("source".to_owned()),
                options: Vec::new(),
                foreground: None,
            },
        },
    )))
    .await
}

async fn expect_output_event(
    peer: &mut Peer,
    subscription_id: PaneOutputSubscriptionId,
) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::PaneOutputCursor(request) = request else {
        return Err(format!("expected pane-output cursor, got {request:?}").into());
    };
    assert_eq!(request.subscription_id, subscription_id);
    peer.write_response(Response::PaneOutputCursor(PaneOutputCursorResponse {
        subscription_id,
        cursor: PaneOutputCursor {
            next_sequence: 2,
            missed_events: 0,
        },
        events: vec![PaneOutputEvent {
            sequence: 1,
            bytes: b"updated".to_vec(),
        }],
        limited: false,
    }))
    .await
}

async fn expect_snapshot(peer: &mut Peer, revision: u64, text: &str) -> TestResult {
    expect_snapshot_at(peer, resolved_target(), revision, text).await
}

async fn expect_snapshot_at(
    peer: &mut Peer,
    expected_target: PaneTargetRef,
    revision: u64,
    text: &str,
) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::PaneSnapshotRef(request) = request else {
        return Err(format!("expected pane snapshot by id, got {request:?}").into());
    };
    assert_eq!(request.target, expected_target);
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

fn preferred_session() -> SessionName {
    SessionName::new("racea").expect("valid preferred session")
}

fn destination_session() -> SessionName {
    SessionName::new("raceb").expect("valid destination session")
}

fn source_session() -> SessionName {
    SessionName::new("racec").expect("valid source session")
}

fn renamed_session() -> SessionName {
    SessionName::new("raced").expect("valid renamed session")
}

const fn pane_id() -> PaneId {
    PaneId::new(7)
}

fn resolved_target() -> PaneTargetRef {
    PaneTargetRef::by_id(destination_session(), pane_id())
}

fn preferred_target() -> PaneTargetRef {
    PaneTargetRef::by_id(preferred_session(), pane_id())
}

fn preferred_moved_slot() -> PaneTarget {
    PaneTarget::with_window(preferred_session(), 0, 1)
}

fn resolved_slot() -> PaneTarget {
    PaneTarget::with_window(destination_session(), 4, 2)
}

fn source_target() -> PaneTargetRef {
    PaneTargetRef::by_id(source_session(), pane_id())
}

fn source_slot() -> PaneTarget {
    PaneTarget::with_window(source_session(), 5, 3)
}

async fn accept_peer(listener: &UnixListener) -> TestResult<Peer> {
    let (stream, _) = listener.accept().await?;
    Ok(Peer::new(stream))
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

            let read = match self.stream.read(&mut buffer).await {
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {
                    // A timed-out client may abort the Unix socket instead of
                    // completing an orderly EOF handshake. Both mean the test
                    // peer is closed; expect_request still reports that close
                    // as an error when a request was required.
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            };
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
        let root = PathBuf::from("/tmp").join(format!(
            "rmux-pir-{}-{}-{id}",
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

fn compact_label(label: &str) -> String {
    let compact = label
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(12)
        .collect::<String>();
    if compact.is_empty() {
        "x".to_owned()
    } else {
        compact
    }
}
