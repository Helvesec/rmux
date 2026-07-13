#![cfg(unix)]

use std::error::Error;
use std::fmt::Debug;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmux_proto::{
    encode_frame, CommandOutput, ErrorResponse, FrameDecoder, HandshakeResponse, LayoutName,
    ListPanesResponse, ListSessionsRequest, ListSessionsResponse, ListWindowsResponse, PaneId,
    PaneOutputCursor, PaneOutputCursorResponse, PaneOutputEvent, PaneOutputLagNotice,
    PaneOutputLagResponse, PaneOutputSubscriptionId, PaneOutputSubscriptionStart, PaneRecentOutput,
    PaneSnapshotCell, PaneSnapshotCursor, PaneSnapshotResponse, PaneTargetRef, Request, Response,
    RmuxError as ProtoError, SubscribePaneOutputResponse, TerminalSize, WindowListEntry,
    WindowTarget,
};
use rmux_sdk::{
    CollectedPaneOutput, Pane, PaneCell, PaneCursor, PaneGlyph, PaneOutputStart, PaneRef,
    PaneSnapshot, PaneTextMatch, RmuxBuilder, SessionName,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn assert_static<T: 'static>() {}
fn assert_debug<T: Debug>() {}

#[test]
fn extract_public_types_keep_static_bounds() {
    assert_send::<PaneTextMatch>();
    assert_sync::<PaneTextMatch>();
    assert_static::<PaneTextMatch>();
    assert_debug::<PaneTextMatch>();

    assert_send::<CollectedPaneOutput>();
    assert_sync::<CollectedPaneOutput>();
    assert_static::<CollectedPaneOutput>();
    assert_debug::<CollectedPaneOutput>();
}

#[test]
fn snapshot_find_text_reports_visible_coordinates_for_wide_and_trimmed_rows() {
    let snapshot = PaneSnapshot::new(
        7,
        2,
        vec![
            cell("A"),
            wide("界", 2),
            PaneCell::padding(),
            cell("B"),
            cell(" "),
            cell(" "),
            cell(" "),
            cell("a"),
            cell("a"),
            cell("a"),
            cell("a"),
            cell(" "),
            cell(" "),
            cell(" "),
        ],
        PaneCursor::default(),
    )
    .expect("valid snapshot");

    let wide_match = snapshot.find_text("界B").expect("wide match found");
    assert_eq!(wide_match.start_row, 0);
    assert_eq!(wide_match.start_col, 1);
    assert_eq!(wide_match.end_col, 4);
    assert_eq!(wide_match.text, "界B");

    let trailing_space = snapshot.find_text("B ");
    assert!(
        trailing_space.is_none(),
        "search must use trimmed visible_lines text, not raw trailing cells"
    );

    let overlapping = snapshot.find_text_all("aa");
    assert_eq!(
        overlapping
            .iter()
            .map(|text_match| {
                (
                    text_match.start_row,
                    text_match.start_col,
                    text_match.end_col,
                )
            })
            .collect::<Vec<_>>(),
        vec![(1, 0, 2), (1, 1, 3), (1, 2, 4)]
    );
}

#[tokio::test]
async fn pane_find_text_searches_fresh_rendered_snapshot() -> TestResult {
    let socket = TestSocket::new("find-text")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_list_panes(&mut peer).await?;

        // Slot snapshots probe daemon capabilities and route through the
        // pane id resolved by the list above, so raw slot resolution can no
        // longer diverge from the visible indexes (issue #94).
        let request = peer.expect_request().await?;
        let Request::Handshake(_) = request else {
            panic!("slot snapshot must probe capabilities first, got {request:?}");
        };
        peer.write_response(Response::Handshake(rmux_proto::HandshakeResponse::current()))
            .await?;
        let request = peer.expect_request().await?;
        let Request::PaneSnapshotRef(request) = request else {
            panic!("find_text must capture the pane snapshot by id, got {request:?}");
        };
        assert_eq!(
            request.target,
            rmux_proto::PaneTargetRef::by_id(session_name(), rmux_proto::PaneId::new(1)),
        );
        peer.write_response(Response::PaneSnapshot(snapshot_response()))
            .await?;

        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let text_match = pane.find_text("界B").await?.expect("match found");
    assert_eq!(text_match.start_row, 0);
    assert_eq!(text_match.start_col, 1);
    assert_eq!(text_match.end_col, 4);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn wait_exit_returns_retained_exit_state() -> TestResult {
    let socket = TestSocket::new("wait-exit")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_info_probe(&mut peer, exited_details_line(7)).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let exit = pane
        .wait_exit()
        .await?
        .expect("dead pane carries exit details");
    assert_eq!(exit.code, Some(7));
    assert_eq!(exit.signal, None);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn collect_output_until_exit_caps_raw_bytes_and_records_lag() -> TestResult {
    let socket = TestSocket::new("collect-output")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;

        expect_output_subscription(&mut peer, PaneOutputSubscriptionStart::Now).await?;
        let subscription_id = PaneOutputSubscriptionId::new(11);
        peer.write_response(Response::SubscribePaneOutput(SubscribePaneOutputResponse {
            subscription_id,
            target: target().to_proto(),
            pane_id: rmux_proto::PaneId::new(1),
            cursor: PaneOutputCursor {
                next_sequence: 1,
                missed_events: 0,
            },
        }))
        .await?;

        expect_info_probe(&mut peer, running_details_line()).await?;

        expect_cursor(&mut peer, subscription_id).await?;
        peer.write_response(Response::PaneOutputCursor(PaneOutputCursorResponse {
            subscription_id,
            cursor: PaneOutputCursor {
                next_sequence: 3,
                missed_events: 0,
            },
            events: vec![
                PaneOutputEvent {
                    sequence: 1,
                    bytes: b"abc".to_vec(),
                },
                PaneOutputEvent {
                    sequence: 2,
                    bytes: b"\xffdef".to_vec(),
                },
            ],
            limited: false,
        }))
        .await?;

        expect_info_probe(&mut peer, running_details_line()).await?;

        expect_cursor(&mut peer, subscription_id).await?;
        peer.write_response(Response::PaneOutputLag(Box::new(PaneOutputLagResponse {
            subscription_id,
            cursor: PaneOutputCursor {
                next_sequence: 9,
                missed_events: 4,
            },
            lag: PaneOutputLagNotice {
                expected_sequence: 3,
                resume_sequence: 7,
                missed_events: 4,
                newest_sequence: 8,
                recent: PaneRecentOutput {
                    bytes: b"recent bytes are not spliced into collection".to_vec(),
                    oldest_sequence: Some(7),
                    newest_sequence: Some(8),
                },
            },
        })))
        .await?;

        expect_info_probe(&mut peer, running_details_line()).await?;

        expect_cursor(&mut peer, subscription_id).await?;
        peer.write_response(Response::Error(ErrorResponse {
            error: ProtoError::Server("subscription not found".to_owned()),
        }))
        .await?;

        expect_info_probe(&mut peer, exited_details_line(0)).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let collected = pane.collect_output_until_exit(5).await?;
    assert_eq!(collected.bytes, b"abc\xffd");
    assert!(collected.truncated);
    assert!(collected.lagged);
    assert_eq!(collected.missed_events, 4);
    assert_eq!(collected.exit_state.and_then(|exit| exit.code), Some(0));
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn collect_output_until_exit_observes_exit_with_empty_live_subscription() -> TestResult {
    let socket = TestSocket::new("collect-empty-live-exit")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;

        expect_output_subscription(&mut peer, PaneOutputSubscriptionStart::Now).await?;
        let subscription_id = PaneOutputSubscriptionId::new(13);
        peer.write_response(Response::SubscribePaneOutput(SubscribePaneOutputResponse {
            subscription_id,
            target: target().to_proto(),
            pane_id: rmux_proto::PaneId::new(1),
            cursor: PaneOutputCursor {
                next_sequence: 1,
                missed_events: 0,
            },
        }))
        .await?;

        expect_info_probe(&mut peer, running_details_line()).await?;

        expect_cursor(&mut peer, subscription_id).await?;
        peer.write_response(Response::PaneOutputCursor(PaneOutputCursorResponse {
            subscription_id,
            cursor: PaneOutputCursor {
                next_sequence: 2,
                missed_events: 0,
            },
            events: vec![PaneOutputEvent {
                sequence: 1,
                bytes: Vec::new(),
            }],
            limited: false,
        }))
        .await?;

        expect_info_probe(&mut peer, exited_details_line(4)).await?;

        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let collected = pane.collect_output_until_exit(16).await?;
    assert!(collected.bytes.is_empty());
    assert!(!collected.truncated);
    assert_eq!(collected.exit_state.and_then(|exit| exit.code), Some(4));
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn collect_output_until_exit_returns_after_post_subscribe_exit_observation() -> TestResult {
    let socket = TestSocket::new("collect-exited")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;

        expect_output_subscription(&mut peer, PaneOutputSubscriptionStart::Now).await?;
        let subscription_id = PaneOutputSubscriptionId::new(12);
        peer.write_response(Response::SubscribePaneOutput(SubscribePaneOutputResponse {
            subscription_id,
            target: target().to_proto(),
            pane_id: rmux_proto::PaneId::new(1),
            cursor: PaneOutputCursor {
                next_sequence: 1,
                missed_events: 0,
            },
        }))
        .await?;

        expect_info_probe(&mut peer, exited_details_line(9)).await?;
        expect_cursor(&mut peer, subscription_id).await?;
        peer.write_response(Response::PaneOutputCursor(PaneOutputCursorResponse {
            subscription_id,
            cursor: PaneOutputCursor {
                next_sequence: 2,
                missed_events: 0,
            },
            events: vec![PaneOutputEvent {
                sequence: 1,
                bytes: Vec::new(),
            }],
            limited: false,
        }))
        .await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let collected = pane.collect_output_until_exit(5).await?;
    assert!(collected.bytes.is_empty());
    assert_eq!(collected.exit_state.and_then(|exit| exit.code), Some(9));
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn collect_output_until_exit_drains_final_output_after_exit_observation() -> TestResult {
    let socket = TestSocket::new("collect-exit-then-tail")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;

        expect_output_subscription(&mut peer, PaneOutputSubscriptionStart::Now).await?;
        let subscription_id = PaneOutputSubscriptionId::new(14);
        peer.write_response(Response::SubscribePaneOutput(SubscribePaneOutputResponse {
            subscription_id,
            target: target().to_proto(),
            pane_id: rmux_proto::PaneId::new(1),
            cursor: PaneOutputCursor {
                next_sequence: 1,
                missed_events: 0,
            },
        }))
        .await?;

        expect_info_probe(&mut peer, running_details_line()).await?;
        expect_cursor(&mut peer, subscription_id).await?;
        peer.write_response(Response::PaneOutputCursor(PaneOutputCursorResponse {
            subscription_id,
            cursor: PaneOutputCursor {
                next_sequence: 1,
                missed_events: 0,
            },
            events: Vec::new(),
            limited: false,
        }))
        .await?;

        expect_info_probe(&mut peer, exited_details_line(7)).await?;
        expect_cursor(&mut peer, subscription_id).await?;
        peer.write_response(Response::PaneOutputCursor(PaneOutputCursorResponse {
            subscription_id,
            cursor: PaneOutputCursor {
                next_sequence: 3,
                missed_events: 0,
            },
            events: vec![
                PaneOutputEvent {
                    sequence: 1,
                    bytes: b"RMUX_BURST_START\nline-0300\nRMUX_BURST_END\n".to_vec(),
                },
                PaneOutputEvent {
                    sequence: 2,
                    bytes: Vec::new(),
                },
            ],
            limited: false,
        }))
        .await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let collected = pane.collect_output_until_exit(256).await?;
    let output = String::from_utf8_lossy(&collected.bytes);
    assert!(output.contains("RMUX_BURST_START"));
    assert!(output.contains("line-0300"));
    assert!(output.contains("RMUX_BURST_END"));
    assert_eq!(collected.exit_state.and_then(|exit| exit.code), Some(7));
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn collect_output_until_exit_starting_at_oldest_uses_requested_cursor() -> TestResult {
    let socket = TestSocket::new("collect-oldest")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_output_subscription(&mut peer, PaneOutputSubscriptionStart::Oldest).await?;
        peer.write_response(Response::Error(ErrorResponse {
            error: ProtoError::InvalidTarget {
                value: target().to_proto().to_string(),
                reason: "pane index does not exist in session".to_owned(),
            },
        }))
        .await?;
        expect_empty_session_probe(&mut peer).await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let collected = pane
        .collect_output_until_exit_starting_at(PaneOutputStart::Oldest, 16)
        .await?;
    assert!(collected.bytes.is_empty());
    assert_eq!(collected.exit_state, None);
    drop(pane);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn collect_output_until_exit_propagates_foreign_invalid_target() -> TestResult {
    let socket = TestSocket::new("collect-foreign-invalid")?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let mut peer = accept_peer(&listener).await?;
        expect_output_subscription(&mut peer, PaneOutputSubscriptionStart::Now).await?;
        peer.write_response(Response::Error(ErrorResponse {
            error: ProtoError::InvalidTarget {
                value: "other:0.0".to_owned(),
                reason: "pane index does not exist in session".to_owned(),
            },
        }))
        .await?;
        TestResult::Ok(())
    });

    let pane = pane_for(socket.path(), Duration::from_secs(1)).await?;
    let error = pane
        .collect_output_until_exit(16)
        .await
        .expect_err("foreign invalid target must propagate");
    assert!(
        error.to_string().contains("other:0.0"),
        "unexpected error: {error}"
    );
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
    SessionName::new("extract").expect("valid test session name")
}

fn cell(text: &str) -> PaneCell {
    PaneCell::new(PaneGlyph::new(text, 1))
}

fn wide(text: &str, width: u8) -> PaneCell {
    PaneCell::new(PaneGlyph::new(text, width))
}

async fn accept_peer(listener: &UnixListener) -> TestResult<Peer> {
    let (stream, _) = listener.accept().await?;
    Ok(Peer::new(stream))
}

async fn expect_cursor(peer: &mut Peer, expected: PaneOutputSubscriptionId) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::PaneOutputCursor(request) = request else {
        panic!("expected pane-output cursor poll, got {request:?}");
    };
    assert_eq!(request.subscription_id, expected);
    assert_eq!(request.max_events, Some(256));
    Ok(())
}

async fn expect_output_subscription(
    peer: &mut Peer,
    expected_start: PaneOutputSubscriptionStart,
) -> TestResult {
    expect_list_panes(peer).await?;

    let request = peer.expect_request().await?;
    let Request::Handshake(_) = request else {
        panic!("pane-output subscription must probe by-id capability, got {request:?}");
    };
    peer.write_response(Response::Handshake(HandshakeResponse::current()))
        .await?;

    let request = peer.expect_request().await?;
    let Request::SubscribePaneOutputRef(request) = request else {
        panic!("collection must subscribe to raw pane output by id, got {request:?}");
    };
    assert_eq!(
        request.target,
        PaneTargetRef::by_id(session_name(), PaneId::new(1))
    );
    assert_eq!(request.start, expected_start);
    Ok(())
}

async fn expect_list_panes(peer: &mut Peer) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListPanes(request) = request else {
        panic!("snapshot lookup must list panes before capture, got {request:?}");
    };
    assert_eq!(request.target, session_name());
    assert_eq!(request.target_window_index, Some(0));
    peer.write_response(Response::ListPanes(ListPanesResponse {
        output: CommandOutput::from_stdout(b"0:0:%1\n".to_vec()),
    }))
    .await
}

async fn expect_info_probe(peer: &mut Peer, details_line: String) -> TestResult {
    expect_list_sessions(peer).await?;
    expect_list_windows(peer).await?;
    expect_list_panes(peer).await?;

    let request = peer.expect_request().await?;
    let Request::ListPanes(request) = request else {
        panic!("exit probe must read pane details with list-panes, got {request:?}");
    };
    assert_eq!(request.target, session_name());
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
        output: CommandOutput::from_stdout(details_line),
    }))
    .await
}

async fn expect_empty_session_probe(peer: &mut Peer) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListSessions(request) = request else {
        panic!("stale exit probe must list sessions, got {request:?}");
    };
    assert_list_sessions_request(&request);
    peer.write_response(Response::ListSessions(ListSessionsResponse {
        output: CommandOutput::from_stdout(Vec::new()),
    }))
    .await
}

async fn expect_list_sessions(peer: &mut Peer) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListSessions(request) = request else {
        panic!("exit probe must list sessions, got {request:?}");
    };
    assert_list_sessions_request(&request);
    peer.write_response(Response::ListSessions(ListSessionsResponse {
        output: CommandOutput::from_stdout(b"extract\t$1\n".to_vec()),
    }))
    .await
}

fn assert_list_sessions_request(request: &ListSessionsRequest) {
    assert_eq!(
        request.format.as_deref(),
        Some("#{session_name}\t#{session_id}")
    );
}

async fn expect_list_windows(peer: &mut Peer) -> TestResult {
    let request = peer.expect_request().await?;
    let Request::ListWindows(request) = request else {
        panic!("exit probe must list windows, got {request:?}");
    };
    assert_eq!(request.target, session_name());
    peer.write_response(Response::ListWindows(ListWindowsResponse {
        windows: vec![WindowListEntry {
            target: WindowTarget::with_window(session_name(), 0),
            window_id: "@1".to_owned(),
            name: Some("main".to_owned()),
            pane_count: 1,
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

fn exited_details_line(code: i32) -> String {
    format!("%1\t\t1\t{code}\t0\t80\t24\t0\t0\t1\t0\t0\t0\t\t1\t2\t3\t/tmp\n")
}

fn running_details_line() -> String {
    "%1\t1234\t0\t\t\t80\t24\t0\t0\t1\t0\t0\t0\t\t1\t2\t3\t/tmp\n".to_owned()
}

fn snapshot_response() -> PaneSnapshotResponse {
    PaneSnapshotResponse {
        cols: 4,
        rows: 1,
        cells: vec![
            snapshot_cell("A", 1, false),
            snapshot_cell("界", 2, false),
            snapshot_cell(" ", 0, true),
            snapshot_cell("B", 1, false),
        ],
        cursor: PaneSnapshotCursor {
            row: 0,
            col: 0,
            visible: true,
            style: 0,
        },
        revision: 1,
    }
}

fn snapshot_cell(text: &str, width: u8, padding: bool) -> PaneSnapshotCell {
    PaneSnapshotCell {
        text: text.to_owned(),
        width,
        padding,
        attributes: 0,
        fg: 0,
        bg: 0,
        us: 0,
        link: 0,
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
        // Keep fake Unix sockets short enough for macOS sockaddr_un.
        let root = PathBuf::from("/tmp").join(format!(
            "rmux-ex-{}-{}-{id}",
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
