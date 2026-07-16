#![cfg(unix)]

mod common;

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_proto::{encode_frame, FrameDecoder, HasSessionRequest, Request, Response};
use rmux_sdk::{
    EnsureSession, Input, PaneCloseOutcome, PaneRef, PaneSet, RmuxBuilder, RmuxError, SessionName,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::time::Instant;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

static LIVE_DAEMON_LOCK: common::unix_smoke::LiveDaemonLock =
    common::unix_smoke::LiveDaemonLock::new();
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn pane_set_broadcast_snapshot_and_visible_waits() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-set-main").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(session_name("sdkpanesetmain"))
        .create_only()
        .ensure(&rmux)
        .await?;
    let root = session.pane(0, 0);
    let right = root.split(rmux_sdk::SplitDirection::Right).await?;
    let panes = PaneSet::new(vec![root.clone(), right.clone()]);

    assert_eq!(panes.len(), 2);
    assert!(!panes.is_empty());
    assert_eq!(panes.panes()[0].target(), root.target());

    let broadcast = panes
        .broadcast(Input::text("printf 'sdk_paneset_all_%s\\n' $((40+2))"))
        .await?;
    assert_eq!(broadcast.len(), 2);
    let staged = panes
        .expect_all()
        .visible_text_contains("sdk_paneset_all_%s")
        .timeout(Duration::from_secs(15))
        .await;
    let staged = staged.all().expect("expect_all returns all outcome");
    assert!(
        staged.is_success(),
        "broadcast text should reach every pane before Enter: {staged:?}"
    );
    let enter = panes.broadcast(Input::key("Enter")).await?;
    assert_eq!(enter.len(), 2);

    let all = panes
        .expect_all()
        .visible_text_contains("sdk_paneset_all_42")
        .timeout(Duration::from_secs(15))
        .await;
    let all = all.all().expect("expect_all returns all outcome");
    assert!(all.is_success(), "all panes should match: {all:?}");
    assert_eq!(all.successes().len(), 2);

    let snapshots = panes.snapshot_all().await;
    assert!(snapshots.is_success(), "snapshot_all failed: {snapshots:?}");
    assert_eq!(snapshots.successes().len(), 2);
    assert!(snapshots.successes().iter().all(|success| success
        .value()
        .visible_text()
        .contains("sdk_paneset_all_42")));

    let any_marker = "sdk_paneset_any_only_left";
    root.send_text(format!("printf '{any_marker}\\n'\n"))
        .await?;
    let any = panes
        .expect_any()
        .visible_text_matches_any([any_marker])
        .timeout(Duration::from_secs(5))
        .await;
    let any = any.any().expect("expect_any returns any outcome");
    assert!(any.matched(), "one pane should satisfy wait_any: {any:?}");
    assert!(any
        .success()
        .expect("matched pane")
        .value()
        .visible_text()
        .contains(any_marker));

    let none = panes
        .expect_all()
        .visible_text_contains("sdk_paneset_never")
        .timeout(Duration::from_millis(50))
        .await;
    let none = none.all().expect("expect_all returns all outcome");
    assert!(!none.is_success());
    assert_eq!(none.failures().len(), 2);

    harness.finish().await
}

#[tokio::test]
async fn pane_set_close_all_reports_per_pane_outcomes() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-set-close").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(session_name("sdkpanesetclose"))
        .create_only()
        .ensure(&rmux)
        .await?;
    let root = session.pane(0, 0);
    let right = root.split(rmux_sdk::SplitDirection::Right).await?;
    let right_id_before_reindex = right.id().await?.expect("right pane has an id");
    let down = root.split(rmux_sdk::SplitDirection::Down).await?;
    let down_id = down.id().await?.expect("down pane has an id");
    assert_ne!(right_id_before_reindex, down_id);
    let panes = PaneSet::new(vec![right, down]);

    let closed = panes.close_all().await;
    assert!(closed.is_success(), "close_all failed: {closed:?}");
    assert_eq!(closed.successes().len(), 2);
    assert!(closed.successes().iter().all(|success| {
        matches!(
            success.value(),
            PaneCloseOutcome::Closed {
                window_destroyed: false,
                ..
            }
        )
    }));
    assert_eq!(
        closed
            .successes()
            .iter()
            .map(|success| success.pane_id())
            .collect::<Vec<_>>(),
        vec![Some(right_id_before_reindex), Some(down_id)],
        "close_all must preserve caller-provided stable pane order after recompression"
    );
    assert!(root.exists().await?, "root pane should remain alive");

    let neighbor = root.split(rmux_sdk::SplitDirection::Right).await?;
    let neighbor_id = neighbor.id().await?.expect("neighbor pane has an id");
    let neighbor_by_id = session.pane_by_id(neighbor_id).await?;
    let duplicate = PaneSet::new(vec![neighbor_by_id.clone(), neighbor_by_id])
        .close_all()
        .await;
    assert!(
        duplicate.is_success(),
        "duplicate stable close must stay idempotent: {duplicate:?}"
    );
    assert!(matches!(
        duplicate.successes()[0].value(),
        PaneCloseOutcome::Closed { .. }
    ));
    assert!(matches!(
        duplicate.successes()[1].value(),
        PaneCloseOutcome::AlreadyClosed { .. }
    ));
    assert!(
        root.exists().await?,
        "duplicate stable close must not retarget the surviving neighbor"
    );

    harness.finish().await
}

#[tokio::test]
async fn pane_set_close_all_preflights_slot_handles_before_reindexing() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-set-slot-close").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(session_name("sdkpanesetslotclose"))
        .create_only()
        .ensure(&rmux)
        .await?;
    session
        .pane(0, 0)
        .split(rmux_sdk::SplitDirection::Right)
        .await?;

    let closed = PaneSet::new([session.pane(0, 0), session.pane(0, 1)])
        .close_all()
        .await;

    assert!(closed.is_success(), "slot close_all failed: {closed:?}");
    assert_eq!(closed.successes().len(), 2);
    assert!(closed
        .successes()
        .iter()
        .all(|success| matches!(success.value(), PaneCloseOutcome::Closed { .. })));
    assert!(
        matches!(
            closed.successes()[1].value(),
            PaneCloseOutcome::Closed {
                window_destroyed: true,
                ..
            }
        ),
        "the second preflighted identity must close the final pane/window"
    );

    harness.finish().await
}

#[tokio::test]
async fn pane_set_expect_any_timeout_bounds_stalled_identity_rpc() -> TestResult {
    let root = TestRoot::new("expect-any-stalled-id");
    std::fs::create_dir_all(root.path())?;
    let socket_path = root.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path)?;
    let pane = RmuxBuilder::new()
        .unix_socket(&socket_path)
        .default_timeout(Duration::from_secs(1))
        .build()
        .pane(PaneRef::new(session_name("sdkpanesetanyid"), 0, 0))
        .await?;
    let panes = PaneSet::new([pane]);

    let (guarded, server) = tokio::join!(
        tokio::time::timeout(
            Duration::from_millis(500),
            panes
                .expect_any()
                .visible_text_contains("never")
                .timeout(Duration::from_millis(25)),
        ),
        hold_first_list_panes_response(listener),
    );
    server?;
    let outcome = guarded.expect("the public PaneSet deadline must beat the external watchdog");
    let any = outcome.any().expect("expect_any returns any outcome");
    assert!(!any.matched());
    assert_eq!(any.failures().len(), 1);
    assert_timed_out(any.failures()[0].error(), "wait for pane snapshot text");
    Ok(())
}

#[tokio::test]
async fn pane_set_expect_all_timeout_bounds_stalled_identity_rpc() -> TestResult {
    let root = TestRoot::new("expect-all-stalled-id");
    std::fs::create_dir_all(root.path())?;
    let socket_path = root.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path)?;
    let pane = RmuxBuilder::new()
        .unix_socket(&socket_path)
        .default_timeout(Duration::from_secs(1))
        .build()
        .pane(PaneRef::new(session_name("sdkpanesetallid"), 0, 0))
        .await?;
    let panes = PaneSet::new([pane]);

    let (guarded, server) = tokio::join!(
        tokio::time::timeout(
            Duration::from_millis(500),
            panes
                .expect_all()
                .visible_text_contains("never")
                .timeout(Duration::from_millis(25)),
        ),
        hold_first_list_panes_response(listener),
    );
    server?;
    let outcome = guarded.expect("the public PaneSet deadline must beat the external watchdog");
    let all = outcome.all().expect("expect_all returns all outcome");
    assert!(!all.is_success());
    assert_eq!(all.failures().len(), 1);
    assert_timed_out(all.failures()[0].error(), "wait for pane snapshot text");
    Ok(())
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
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

async fn hold_first_list_panes_response(listener: UnixListener) -> TestResult {
    let (mut stream, _) = listener.accept().await?;
    let request = read_request(&mut stream).await?;
    assert!(
        matches!(request, Request::ListPanes(_)),
        "PaneSet identity setup must list panes, got {request:?}"
    );
    tokio::time::sleep(Duration::from_millis(750)).await;
    Ok(())
}

async fn read_request(stream: &mut UnixStream) -> TestResult<Request> {
    let mut decoder = FrameDecoder::new();
    let mut read_buffer = [0_u8; 8192];

    loop {
        if let Some(request) = decoder.next_frame::<Request>()? {
            return Ok(request);
        }

        let bytes_read = stream.read(&mut read_buffer).await?;
        if bytes_read == 0 {
            return Err("connection closed before request frame".into());
        }
        decoder.push_bytes(&read_buffer[..bytes_read]);
    }
}

fn assert_timed_out(error: &RmuxError, expected_operation: &str) {
    match error {
        RmuxError::Transport {
            operation, source, ..
        } => {
            assert_eq!(*operation, expected_operation);
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        other => panic!("expected typed timeout transport error, got {other:?}"),
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
        std::fs::create_dir_all(root.path())?;
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
        RmuxBuilder::new().unix_socket(&self.socket_path).build()
    }

    async fn finish(self) -> TestResult {
        let shutdown = self.rmux().shutdown().await;
        wait_for_child_exit(self, "server did not exit during cleanup").await?;
        if let Err(error) = shutdown {
            let peer_already_closed = matches!(
                &error,
                RmuxError::Transport { source, .. }
                    if matches!(
                        source.kind(),
                        io::ErrorKind::BrokenPipe
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::NotConnected
                            | io::ErrorKind::NotFound
                            | io::ErrorKind::UnexpectedEof
                    )
            );
            let rendered = error.to_string();
            assert!(
                peer_already_closed
                    || rendered.contains("connect to rmux daemon")
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
            "rmux-sdk-pane-set-{}-{}-{unique_id}",
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
