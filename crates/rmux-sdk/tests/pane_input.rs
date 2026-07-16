#![cfg(unix)]

mod common;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_proto::{
    encode_frame, BreakPaneRequest, ClientTerminalContext, ControlMode, ControlModeRequest,
    FrameDecoder, HasSessionRequest, JoinPaneRequest, KillPaneRequest, MovePaneRequest,
    NewWindowRequest, PaneTarget, Request, Response, SwapPaneRequest, WindowTarget,
};
use rmux_sdk::{
    EnsureSession, Input, PaneId, PaneRef, PaneSnapshot, RmuxBuilder, RmuxError, SessionName,
    SplitDirection, TerminalSizeSpec,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::Instant;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

static LIVE_DAEMON_LOCK: common::unix_smoke::LiveDaemonLock =
    common::unix_smoke::LiveDaemonLock::new();
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn send_text_is_literal_and_send_key_interprets_key_tokens() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-input-literal").await?;
    let rmux = harness.rmux();
    let alpha = session_name("sdkpaneinputlit");
    let session = EnsureSession::named(alpha)
        .create_only()
        .ensure(&rmux)
        .await?;
    let pane = session.pane(0, 0);
    let baseline = pane.snapshot().await?;

    pane.send_text("").await?;

    let no_newline_marker = "sdk_no_auto_42";
    pane.send_text("printf 'sdk_no_auto_%s' $((40+2))").await?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !pane
            .snapshot()
            .await?
            .visible_text()
            .contains(no_newline_marker),
        "send_text must not inject an implicit newline"
    );
    pane.send_key("Enter").await?;
    let after_enter =
        wait_for_revision_and_text(&pane, baseline.revision, no_newline_marker).await?;

    let literal_marker = "sdk_literal_Enter";
    pane.send_text("printf 'sdk_literal_%s' ").await?;
    pane.send_text("Enter").await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !pane
            .snapshot()
            .await?
            .visible_text()
            .contains(literal_marker),
        "send_text must not interpret `Enter` as a key token"
    );
    pane.send_key("Enter").await?;
    let after_literal =
        wait_for_revision_and_text(&pane, after_enter.revision, literal_marker).await?;

    let payload = " \t\u{1}éΩ\n";
    let expected_hex = hex_bytes(payload.as_bytes());
    let ready_marker = "SDKRDY";
    let byte_command = format!(
        "stty raw -echo;printf '\\123\\104\\113\\122\\104\\131';\
         dd bs=1 count={} 2>/dev/null|od -An -tx1 -v;stty sane",
        payload.len()
    );
    pane.send_text(&byte_command).await?;
    pane.send_key("Enter").await?;
    let after_ready =
        wait_for_revision_and_text(&pane, after_literal.revision, ready_marker).await?;
    assert!(
        !compact_hex(&pane.snapshot().await?).contains(&expected_hex),
        "raw byte marker appeared before payload was sent"
    );
    pane.send_text(payload).await?;
    wait_for_revision_and_compact_hex(&pane, after_ready.revision, &expected_hex).await?;

    harness.finish().await
}

#[tokio::test]
async fn resize_updates_geometry_and_emits_window_layout_change() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-input-resize").await?;
    let rmux = harness.rmux();
    let alpha = session_name("sdkpaneinputres");
    let session = EnsureSession::named(alpha.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    session.pane(0, 0).split(SplitDirection::Right).await?;
    let pane = session.pane(0, 0);
    let baseline = pane.snapshot().await?;
    let pane_info = pane.info().await?;
    let expected_window_id = pane_info
        .windows
        .first()
        .expect("resized pane has a listed window")
        .id
        .to_string();
    assert!(
        baseline.cols > 8,
        "split pane is too narrow for resize test"
    );

    let mut control = ControlClient::open(harness.socket_path(), &alpha).await?;
    control.drain_pending_lines().await?;
    let expected_cols = baseline.cols - 3;
    pane.resize(TerminalSizeSpec::new(expected_cols, baseline.rows))
        .await?;
    let after_resize = wait_for_pane_columns(&pane, expected_cols).await?;
    assert_ne!(
        (after_resize.cols, after_resize.rows),
        (baseline.cols, baseline.rows)
    );
    assert_ne!(after_resize.revision, baseline.revision);

    let layout_line = control
        .wait_for_line(|line| {
            line.starts_with("%layout-change ")
                && layout_change_window_id(line) == Some(expected_window_id.as_str())
        })
        .await?;
    assert_eq!(
        layout_change_window_id(&layout_line),
        Some(expected_window_id.as_str()),
        "layout notification did not target the resized window: {layout_line}"
    );
    assert!(
        layout_line.split_whitespace().count() >= 5,
        "unexpected layout notification: {layout_line}"
    );
    drop(control);

    harness.finish().await
}

#[tokio::test]
async fn input_and_resize_return_daemon_errors_for_stale_or_missing_panes() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-input-stale").await?;
    let rmux = harness.rmux();
    let alpha = session_name("sdkpaneinputerr");
    let session = EnsureSession::named(alpha.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    raw_new_window(harness.socket_path(), alpha.clone(), 99).await?;

    let stale = session.pane(0, 0);
    raw_kill_pane(
        harness.socket_path(),
        PaneTarget::with_window(alpha.clone(), 0, 0),
    )
    .await?;
    wait_for_pane_unlisted(&stale).await?;
    assert_error_contains(
        stale.send_text("after-close").await,
        "does not exist in session",
    );
    assert_error_contains(stale.send_key("Enter").await, "does not exist in session");
    assert_error_contains(
        stale.resize(TerminalSizeSpec::new(12, 6)).await,
        "does not exist in session",
    );
    assert_error_contains(
        stale.resize(TerminalSizeSpec::default()).await,
        "does not exist in session",
    );

    let missing_pane = session.pane(99, 9);
    assert_error_contains(
        missing_pane.send_text("missing-pane").await,
        "pane index does not exist",
    );
    assert_error_contains(
        missing_pane.resize(TerminalSizeSpec::default()).await,
        "pane index does not exist",
    );

    let missing_session = rmux
        .pane(PaneRef::new(session_name("sdkpaneinputgone"), 0, 0))
        .await?;
    assert_error_contains(
        missing_session.send_key("Enter").await,
        "does not exist in session",
    );

    harness.finish().await
}

#[tokio::test]
async fn pane_by_id_survives_index_recompression_for_critical_input_and_snapshot() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-input-by-id").await?;
    let rmux = harness.rmux();
    let alpha = session_name("sdkpaneinputid");
    let session = EnsureSession::named(alpha.clone())
        .create_only()
        .ensure(&rmux)
        .await?;

    session.pane(0, 0).split(SplitDirection::Right).await?;
    session.pane(0, 1).split(SplitDirection::Right).await?;

    let original_left = session.pane(0, 0).id().await?.expect("left pane has an id");
    let middle_id = session
        .pane(0, 1)
        .id()
        .await?
        .expect("middle pane has an id");
    let right_id = session
        .pane(0, 2)
        .id()
        .await?
        .expect("right pane has an id");
    let middle = session.pane_by_id(middle_id).await?;

    raw_kill_pane(
        harness.socket_path(),
        PaneTarget::with_window(alpha.clone(), 0, 0),
    )
    .await?;
    wait_for_slot_id(&session, 0, 0, Some(middle_id)).await?;
    wait_for_slot_id(&session, 0, 1, Some(right_id)).await?;
    assert_eq!(
        middle.id().await?,
        Some(middle_id),
        "stable pane handle must keep addressing the original pane id"
    );
    assert_eq!(
        session.pane(0, 0).id().await?,
        Some(middle_id),
        "middle pane should have recompressed into slot 0"
    );
    assert_ne!(
        session.pane(0, 0).id().await?,
        Some(original_left),
        "closed pane id must not be reused during the live session"
    );

    let marker = "sdk_by_id_recompressed_marker";
    middle.send_text(format!("printf '{marker}\\n'\n")).await?;
    wait_for_revision_and_text(&middle, 0, marker).await?;

    let raw_marker = "sdk_by_id_raw_wait_marker";
    let raw_wait = middle.wait_for_text_next(raw_marker).await?;
    let mut render_stream = middle.render_stream().await?;
    middle
        .send_text(format!("printf '{raw_marker}\\n'\n"))
        .await?;
    raw_wait.await?;
    let update = wait_for_render_text(&mut render_stream, raw_marker).await?;
    assert!(
        update.snapshot().visible_text().contains(raw_marker),
        "render_stream on a by-id handle must follow the recompressed pane"
    );
    drop(render_stream);

    let right_snapshot = session.pane(0, 1).snapshot().await?;
    assert!(
        !right_snapshot.visible_text().contains(marker),
        "slot-based stale targeting would have written into the right neighbor"
    );
    assert!(
        !right_snapshot.visible_text().contains(raw_marker),
        "slot-based stale stream/wait targeting would have followed the right neighbor"
    );

    harness.finish().await
}

#[tokio::test]
async fn broadcast_sends_text_and_keys_to_multiple_panes() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-input-broadcast").await?;
    let rmux = harness.rmux();
    let alpha = session_name("sdkpaneinputbr");
    let session = EnsureSession::named(alpha)
        .create_only()
        .ensure(&rmux)
        .await?;
    let right = session.pane(0, 0).split(SplitDirection::Right).await?;
    let panes = vec![session.pane(0, 0), right];

    let command = "printf 'sdk_broadcast_%s\\n' $((40+2))";
    let text = rmux.broadcast(&panes, Input::Text(command)).await?;
    assert_eq!(text.len(), panes.len());
    let enter = rmux.broadcast(&panes, Input::Key("Enter")).await?;
    assert_eq!(enter.len(), panes.len());

    for pane in &panes {
        wait_for_revision_and_text(pane, 0, "sdk_broadcast_42").await?;
    }

    harness.finish().await
}

#[tokio::test]
async fn broadcast_reports_partial_failures_by_pane() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-input-broadcast-partial").await?;
    let rmux = harness.rmux();
    let alpha = session_name("sdkpaneinputbp");
    let session = EnsureSession::named(alpha.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    session.pane(0, 0).split(SplitDirection::Right).await?;
    let stale_id = session.pane(0, 0).id().await?.expect("left pane has an id");
    let live_id = session
        .pane(0, 1)
        .id()
        .await?
        .expect("right pane has an id");
    let stale = session.pane_by_id(stale_id).await?;
    let live = session.pane_by_id(live_id).await?;
    raw_kill_pane(
        harness.socket_path(),
        PaneTarget::with_window(alpha.clone(), 0, 0),
    )
    .await?;
    wait_for_pane_unlisted(&stale).await?;

    let stale_target = stale.target().clone();
    let live_target = live.target().clone();
    let panes = vec![stale, live];
    let error = rmux
        .broadcast(&panes, Input::Key("Enter"))
        .await
        .expect_err("stale pane must produce a partial broadcast failure");
    let RmuxError::PartialBroadcast { source, .. } = error else {
        return Err(format!("expected partial broadcast failure, got {error:?}").into());
    };
    assert_eq!(source.successes().len(), 1);
    assert_eq!(source.failures().len(), 1);
    assert_eq!(source.successes()[0].target(), &live_target);
    assert_eq!(source.successes()[0].pane_id(), Some(live_id));
    assert_eq!(source.failures()[0].target(), &stale_target);
    assert!(matches!(
        source.failures()[0].error(),
        RmuxError::PaneNotFound { .. }
    ));

    harness.finish().await
}

#[tokio::test]
async fn render_stream_emits_snapshot_after_output() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-input-render-stream").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(session_name("sdkpaneinputrs"))
        .create_only()
        .ensure(&rmux)
        .await?;
    let pane = session.pane(0, 0);
    let mut stream = pane.render_stream().await?;

    pane.send_text("printf 'sdk_render_42\\n'\n").await?;
    let update = wait_for_render_text(&mut stream, "sdk_render_42").await?;
    assert!(update.snapshot().visible_text().contains("sdk_render_42"));
    drop(stream);

    harness.finish().await
}

#[tokio::test]
async fn by_id_render_stream_follows_a_pane_moved_to_another_session() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-input-render-stream-move").await?;
    let rmux = harness.rmux();
    for transfer in CrossSessionTransfer::ALL {
        assert_by_id_render_stream_follows_transfer(&harness, &rmux, transfer).await?;
    }
    harness.finish().await
}

async fn assert_by_id_render_stream_follows_transfer(
    harness: &Harness,
    rmux: &rmux_sdk::Rmux,
    transfer: CrossSessionTransfer,
) -> TestResult {
    let alpha_name = session_name(&format!("sdkrsa{}", transfer.label()));
    let beta_name = session_name(&format!("sdkrsb{}", transfer.label()));
    let alpha = EnsureSession::named(alpha_name.clone())
        .create_only()
        .ensure(rmux)
        .await?;
    let beta = EnsureSession::named(beta_name.clone())
        .create_only()
        .ensure(rmux)
        .await?;
    let slot = alpha.pane(0, 0);
    let pane_id = slot.id().await?.expect("source pane has a stable id");
    let stable = alpha.pane_by_id(pane_id).await?;
    let mut render_stream = stable.render_stream().await?;

    raw_transfer_pane(
        harness.socket_path(),
        PaneTarget::with_window(alpha_name, 0, 0),
        PaneTarget::with_window(beta_name, 0, 0),
        transfer,
    )
    .await?;

    assert_eq!(
        stable.id().await?,
        Some(pane_id),
        "a by-id handle must resolve beyond its original session after {}",
        transfer.label(),
    );
    assert_ne!(
        slot.id().await?,
        Some(pane_id),
        "a slot handle must remain scoped to its original session and slot after {}",
        transfer.label(),
    );

    let moved = beta.pane_by_id(pane_id).await?;
    let marker = format!("sdk_render_after_cross_session_{}", transfer.label());
    moved.send_text(format!("printf '{marker}\\n'\n")).await?;
    let update = wait_for_render_text(&mut render_stream, &marker).await?;
    assert!(
        update.snapshot().visible_text().contains(&marker),
        "the pre-move output subscription and dynamic snapshot must converge on the same pane id"
    );
    assert!(
        stable.snapshot().await?.visible_text().contains(&marker),
        "direct snapshots from the original by-id handle must follow the moved pane"
    );
    drop(render_stream);

    let mut reopened = stable.render_stream().await?;
    let reopened_marker = format!("sdk_reopened_after_cross_session_{}", transfer.label());
    moved
        .send_text(format!("printf '{reopened_marker}\\n'\n"))
        .await?;
    let update = wait_for_render_text(&mut reopened, &reopened_marker).await?;
    assert!(
        update.snapshot().visible_text().contains(&reopened_marker),
        "a render stream opened after the transfer must use the pane's current session"
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum CrossSessionTransfer {
    Move,
    Join,
    Break,
    Swap,
}

impl CrossSessionTransfer {
    const ALL: [Self; 4] = [Self::Move, Self::Join, Self::Break, Self::Swap];

    const fn label(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Join => "join",
            Self::Break => "break",
            Self::Swap => "swap",
        }
    }
}

async fn wait_for_revision_and_text(
    pane: &rmux_sdk::Pane,
    previous_revision: u64,
    marker: &str,
) -> TestResult<PaneSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snapshot = pane.snapshot().await?;
        if snapshot.revision != previous_revision && snapshot.visible_text().contains(marker) {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pane revision did not change from {previous_revision} while waiting for `{marker}`"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_revision_and_compact_hex(
    pane: &rmux_sdk::Pane,
    previous_revision: u64,
    expected: &str,
) -> TestResult<PaneSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snapshot = pane.snapshot().await?;
        if snapshot.revision != previous_revision && compact_hex(&snapshot).contains(expected) {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pane revision did not change from {previous_revision} while waiting for hex `{expected}`"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_pane_columns(
    pane: &rmux_sdk::Pane,
    expected_cols: u16,
) -> TestResult<PaneSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = pane.snapshot().await?;
        if snapshot.cols == expected_cols {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pane columns did not reach {expected_cols} (got {}x{})",
                snapshot.cols, snapshot.rows,
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_render_text(
    stream: &mut rmux_sdk::PaneRenderStream,
    marker: &str,
) -> TestResult<rmux_sdk::RenderUpdate> {
    match tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match stream.next().await {
                Ok(Some(update)) => {
                    if update.snapshot().visible_text().contains(marker) {
                        return Ok(update);
                    }
                }
                Ok(None) => {
                    return Err::<_, Box<dyn Error>>("render stream closed before marker".into());
                }
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!("render stream did not emit {marker:?} within deadline").into()),
    }
}

async fn wait_for_pane_unlisted(pane: &rmux_sdk::Pane) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if pane.id().await?.is_none() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("pane remained listed after kill".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_slot_id(
    session: &rmux_sdk::Session,
    window_index: u32,
    pane_index: u32,
    expected: Option<PaneId>,
) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let observed = session.pane(window_index, pane_index).id().await?;
        if observed == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pane slot {window_index}.{pane_index} did not reach id {expected:?}; got {observed:?}"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_error_contains(result: rmux_sdk::Result<()>, expected: &str) {
    let error = result.expect_err("operation should fail through daemon");
    let rendered = error.to_string();
    assert!(
        rendered.contains(expected),
        "expected error containing `{expected}`, got `{rendered}`"
    );
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn compact_hex(snapshot: &PaneSnapshot) -> String {
    snapshot
        .visible_text()
        .chars()
        .filter(char::is_ascii_hexdigit)
        .collect()
}

fn layout_change_window_id(line: &str) -> Option<&str> {
    line.split_whitespace().nth(1)
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn raw_new_window(socket_path: &Path, target: SessionName, index: u32) -> TestResult {
    match framed_request(
        socket_path,
        Request::NewWindow(Box::new(NewWindowRequest {
            target: target.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            target_window_index: Some(index),
            insert_at_target: false,
        })),
    )
    .await?
    {
        Response::NewWindow(response) => {
            assert_eq!(
                response.target,
                rmux_proto::WindowTarget::with_window(target, index)
            );
            Ok(())
        }
        response => Err(format!("unexpected new-window response: {response:?}").into()),
    }
}

async fn raw_kill_pane(socket_path: &Path, target: PaneTarget) -> TestResult {
    match framed_request(
        socket_path,
        Request::KillPane(KillPaneRequest {
            target,
            kill_all_except: false,
        }),
    )
    .await?
    {
        Response::KillPane(_) => Ok(()),
        response => Err(format!("unexpected kill-pane response: {response:?}").into()),
    }
}

async fn raw_transfer_pane(
    socket_path: &Path,
    source: PaneTarget,
    target: PaneTarget,
    transfer: CrossSessionTransfer,
) -> TestResult {
    let request = match transfer {
        CrossSessionTransfer::Move => Request::MovePane(MovePaneRequest {
            source,
            target,
            direction: rmux_proto::SplitDirection::Vertical,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }),
        CrossSessionTransfer::Join => Request::JoinPane(JoinPaneRequest {
            source,
            target,
            direction: rmux_proto::SplitDirection::Vertical,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }),
        CrossSessionTransfer::Break => Request::BreakPane(Box::new(BreakPaneRequest {
            source,
            target: Some(WindowTarget::with_window(target.session_name().clone(), 1)),
            name: None,
            detached: false,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })),
        CrossSessionTransfer::Swap => Request::SwapPane(SwapPaneRequest {
            source,
            target,
            direction: None,
            detached: false,
            preserve_zoom: false,
        }),
    };
    let response = framed_request(socket_path, request).await?;
    let succeeded = matches!(
        (transfer, &response),
        (CrossSessionTransfer::Move, Response::MovePane(_))
            | (CrossSessionTransfer::Join, Response::JoinPane(_))
            | (CrossSessionTransfer::Break, Response::BreakPane(_))
            | (CrossSessionTransfer::Swap, Response::SwapPane(_))
    );
    if succeeded {
        Ok(())
    } else {
        Err(format!("unexpected {} response: {response:?}", transfer.label()).into())
    }
}

async fn framed_request(socket_path: &Path, request: Request) -> TestResult<Response> {
    let mut stream = UnixStream::connect(socket_path).await?;
    let frame = encode_frame(&request)?;
    stream.write_all(&frame).await?;
    read_response(&mut stream).await
}

async fn read_response(stream: &mut UnixStream) -> TestResult<Response> {
    let mut decoder = FrameDecoder::new();
    let mut read_buffer = [0_u8; 8192];

    loop {
        if let Some(response) = decoder.next_frame::<Response>()? {
            return Ok(response);
        }

        let bytes_read = stream.read(&mut read_buffer).await?;
        if bytes_read == 0 {
            return Err("connection closed before response frame".into());
        }
        decoder.push_bytes(&read_buffer[..bytes_read]);
    }
}

struct ControlClient {
    stream: UnixStream,
    buffer: Vec<u8>,
}

impl ControlClient {
    async fn open(socket_path: &Path, target: &SessionName) -> TestResult<Self> {
        let mut stream = UnixStream::connect(socket_path).await?;
        let frame = encode_frame(&Request::ControlMode(ControlModeRequest {
            mode: ControlMode::Plain,
            client_terminal: ClientTerminalContext {
                terminal_features: Vec::new(),
                utf8: true,
            },
            initial_command_count: 0,
        }))?;
        stream.write_all(&frame).await?;
        match read_response(&mut stream).await? {
            Response::ControlMode(_) => {}
            response => {
                return Err(format!("unexpected control-mode response: {response:?}").into())
            }
        }

        let mut client = Self {
            stream,
            buffer: Vec::new(),
        };
        client
            .write_line(&format!("attach-session -t {target}"))
            .await?;
        client.wait_for_command_end().await?;
        Ok(client)
    }

    async fn write_line(&mut self, line: &str) -> TestResult {
        self.stream.write_all(line.as_bytes()).await?;
        self.stream.write_all(b"\n").await?;
        Ok(())
    }

    async fn wait_for_command_end(&mut self) -> TestResult {
        let line = self
            .wait_for_line(|line| line.starts_with("%end ") || line.starts_with("%error "))
            .await?;
        if line.starts_with("%error ") {
            return Err(format!("control command failed: {line}").into());
        }
        Ok(())
    }

    async fn drain_pending_lines(&mut self) -> TestResult {
        loop {
            while self.pop_line().is_some() {}

            let mut read_buffer = [0_u8; 1024];
            match tokio::time::timeout(
                Duration::from_millis(25),
                self.stream.read(&mut read_buffer),
            )
            .await
            {
                Ok(Ok(0)) => return Err("control connection closed while draining lines".into()),
                Ok(Ok(bytes_read)) => self.buffer.extend_from_slice(&read_buffer[..bytes_read]),
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => return Ok(()),
            }
        }
    }

    async fn wait_for_line(
        &mut self,
        mut matches_line: impl FnMut(&str) -> bool,
    ) -> TestResult<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            while let Some(line) = self.pop_line() {
                if matches_line(&line) {
                    return Ok(line);
                }
            }
            if Instant::now() >= deadline {
                return Err("control line did not arrive within deadline".into());
            }

            let mut read_buffer = [0_u8; 1024];
            match tokio::time::timeout(
                Duration::from_millis(50),
                self.stream.read(&mut read_buffer),
            )
            .await
            {
                Ok(Ok(0)) => return Err("control connection closed before line arrived".into()),
                Ok(Ok(bytes_read)) => self.buffer.extend_from_slice(&read_buffer[..bytes_read]),
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => {}
            }
        }
    }

    fn pop_line(&mut self) -> Option<String> {
        let line_end = self.buffer.iter().position(|byte| *byte == b'\n')?;
        let line = self.buffer.drain(..=line_end).collect::<Vec<_>>();
        Some(
            String::from_utf8_lossy(&line)
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
        )
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

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn rmux(&self) -> rmux_sdk::Rmux {
        RmuxBuilder::new().unix_socket(&self.socket_path).build()
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
    let probe = session_name("sdkprobe");

    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!("daemon exited before accepting RPC: {status}").into());
        }

        if matches!(
            framed_request(
                socket_path,
                Request::HasSession(HasSessionRequest {
                    target: probe.clone()
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

fn rmux_binary() -> TestResult<&'static Path> {
    static RMUX_BINARY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    match RMUX_BINARY.get_or_init(|| resolve_rmux_binary().map_err(|error| error.to_string())) {
        Ok(path) => Ok(path.as_path()),
        Err(error) => Err(std::io::Error::other(error.clone()).into()),
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
    let status =
        std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
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
            "rmux-sdk-pane-input-{}-{}-{unique_id}",
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
