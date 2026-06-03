#![cfg(unix)]

mod common;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_proto::{
    encode_frame, FrameDecoder, HasSessionRequest, LinkWindowRequest, ListPanesRequest,
    ListWindowsRequest, NewWindowRequest, Request, Response, WindowListEntry, WindowTarget,
};
use rmux_sdk::{
    EnsureSession, RmuxBuilder, SessionName, SplitDirection, WindowCloseOutcome, WindowRef,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::Instant;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

static LIVE_DAEMON_LOCK: common::unix_smoke::LiveDaemonLock =
    common::unix_smoke::LiveDaemonLock::new();
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn window_split_info_pane_listing_ids_and_idempotent_close_use_daemon_paths() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("window-basic").await?;
    let rmux = harness.rmux();
    let alpha = session_name("sdkwinalpha");
    let session = EnsureSession::named(alpha.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    raw_new_window(harness.socket_path(), alpha.clone(), 1).await?;

    let window = session.window(1);
    let window_id = window.id().await?.expect("window 1 is listed");
    let initial_panes = window.panes().await?;
    assert_eq!(initial_panes.len(), 1);
    assert_eq!(
        initial_panes[0].target,
        rmux_sdk::PaneRef::new(alpha.clone(), 1, 0)
    );

    let split_pane = session.pane(1, 0).split(SplitDirection::Down).await?;
    let split_pane_ref = split_pane.target().clone();
    assert_eq!(split_pane_ref.session_name, alpha);
    assert_eq!(split_pane_ref.window_index, 1);

    let panes = window.panes().await?;
    assert_eq!(panes.len(), 2);
    assert_eq!(
        panes.iter().filter(|pane| pane.active).count(),
        1,
        "list-panes format should identify the active pane"
    );
    assert!(panes.iter().any(|pane| pane.target == split_pane_ref));

    let info = window.info().await?;
    assert_eq!(info.sessions.len(), 1);
    assert_eq!(info.windows.len(), 1);
    assert_eq!(info.windows[0].id, window_id);
    assert_eq!(info.windows[0].index, 1);
    assert_eq!(info.panes.len(), 2);
    assert!(info
        .panes
        .iter()
        .all(|pane| pane.window_id == window_id && pane.session_id == info.sessions[0].id));

    let raw_windows = raw_list_windows(harness.socket_path(), session.name().clone()).await?;
    assert!(raw_windows.iter().any(|entry| {
        entry.target == WindowTarget::with_window(session.name().clone(), 1)
            && entry.window_id == window_id.to_string()
            && entry.pane_count == 2
    }));
    let killed_pane_ids =
        raw_list_pane_ids(harness.socket_path(), session.name().clone(), Some(1)).await?;
    assert_eq!(killed_pane_ids.len(), 2);
    assert!(window.exists().await?);

    let same_path_observer = window.clone();
    assert_eq!(
        window.close().await?,
        WindowCloseOutcome::Closed {
            active: WindowRef::first(session.name().clone())
        }
    );
    assert!(
        !raw_list_window_indices(harness.socket_path(), session.name().clone())
            .await?
            .contains(&1)
    );
    let remaining_pane_ids =
        raw_list_pane_ids(harness.socket_path(), session.name().clone(), None).await?;
    assert!(killed_pane_ids
        .iter()
        .all(|pane_id| !remaining_pane_ids.contains(pane_id)));

    assert!(!same_path_observer.exists().await?);
    assert_eq!(same_path_observer.id().await?, None);
    assert_eq!(same_path_observer.panes().await?, Vec::new());
    let stale_info = same_path_observer.info().await?;
    assert_eq!(stale_info.sessions.len(), 1);
    assert_eq!(stale_info.windows, Vec::new());
    assert_eq!(stale_info.panes, Vec::new());
    assert_eq!(
        same_path_observer.close().await?,
        WindowCloseOutcome::AlreadyClosed {
            target: WindowRef::new(session.name().clone(), 1)
        }
    );

    harness.finish().await
}

#[tokio::test]
async fn window_close_synchronizes_grouped_session_listings() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("window-grouped-close").await?;
    let rmux = harness.rmux();

    let grouped_a = session_name("sdkwingroupa");
    let grouped_b = session_name("sdkwingroupb");
    let grouped_session = EnsureSession::named(grouped_a.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    raw_new_window(harness.socket_path(), grouped_a.clone(), 1).await?;
    EnsureSession::named(grouped_b.clone())
        .create_only()
        .group_target(grouped_a.clone())
        .ensure(&rmux)
        .await?;

    assert_eq!(
        raw_list_window_indices(harness.socket_path(), grouped_a.clone()).await?,
        vec![0, 1]
    );
    assert_eq!(
        raw_list_window_indices(harness.socket_path(), grouped_b.clone()).await?,
        vec![0, 1]
    );
    assert!(matches!(
        grouped_session.window(1).close().await?,
        WindowCloseOutcome::Closed { .. }
    ));
    assert_eq!(
        raw_list_window_indices(harness.socket_path(), grouped_a.clone()).await?,
        vec![0]
    );
    assert_eq!(
        raw_list_window_indices(harness.socket_path(), grouped_b.clone()).await?,
        vec![0]
    );
    assert_eq!(
        rmux.window(WindowRef::new(grouped_b.clone(), 1))
            .await?
            .close()
            .await?,
        WindowCloseOutcome::AlreadyClosed {
            target: WindowRef::new(grouped_b, 1)
        }
    );

    harness.finish().await
}

#[tokio::test]
async fn window_close_removes_linked_window_from_every_affected_listing() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("window-linked-close").await?;
    let rmux = harness.rmux();

    let linked_a = session_name("sdkwinlinka");
    let linked_b = session_name("sdkwinlinkb");
    let linked_b_peer = session_name("sdkwinlinkc");
    let linked_session = EnsureSession::named(linked_a.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    EnsureSession::named(linked_b.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    EnsureSession::named(linked_b_peer.clone())
        .create_only()
        .group_target(linked_b.clone())
        .ensure(&rmux)
        .await?;
    raw_new_window(harness.socket_path(), linked_a.clone(), 2).await?;
    raw_link_window(
        harness.socket_path(),
        linked_a.clone(),
        0,
        linked_b.clone(),
        1,
    )
    .await?;

    let linked_a_window = raw_window(harness.socket_path(), linked_a.clone(), 0).await?;
    let linked_b_window = raw_window(harness.socket_path(), linked_b.clone(), 1).await?;
    let linked_b_peer_window = raw_window(harness.socket_path(), linked_b_peer.clone(), 1).await?;
    assert_eq!(
        linked_a_window.window_id, linked_b_window.window_id,
        "link-window should expose one stable window id through both sessions"
    );
    assert_eq!(
        linked_b_window.window_id, linked_b_peer_window.window_id,
        "grouped sessions should expose the linked window through every member"
    );
    let linked_panes = raw_list_pane_ids(harness.socket_path(), linked_a.clone(), Some(0)).await?;

    assert_eq!(
        linked_session.window(0).close().await?,
        WindowCloseOutcome::Closed {
            active: WindowRef::new(linked_a.clone(), 2)
        }
    );
    assert_eq!(
        raw_list_window_indices(harness.socket_path(), linked_a.clone()).await?,
        vec![2]
    );
    assert_eq!(
        raw_list_window_indices(harness.socket_path(), linked_b.clone()).await?,
        vec![0]
    );
    assert_eq!(
        raw_list_window_indices(harness.socket_path(), linked_b_peer.clone()).await?,
        vec![0]
    );
    let remaining_linked_panes =
        raw_list_pane_ids(harness.socket_path(), linked_a.clone(), None).await?;
    assert!(linked_panes
        .iter()
        .all(|pane_id| !remaining_linked_panes.contains(pane_id)));
    let remaining_linked_b_panes =
        raw_list_pane_ids(harness.socket_path(), linked_b.clone(), None).await?;
    assert!(linked_panes
        .iter()
        .all(|pane_id| !remaining_linked_b_panes.contains(pane_id)));
    let remaining_linked_b_peer_panes =
        raw_list_pane_ids(harness.socket_path(), linked_b_peer.clone(), None).await?;
    assert!(linked_panes
        .iter()
        .all(|pane_id| !remaining_linked_b_peer_panes.contains(pane_id)));

    let stale_linked_target = rmux.window(WindowRef::new(linked_b.clone(), 1)).await?;
    assert_eq!(stale_linked_target.panes().await?, Vec::new());
    assert_eq!(
        stale_linked_target.close().await?,
        WindowCloseOutcome::AlreadyClosed {
            target: WindowRef::new(linked_b, 1)
        }
    );

    harness.finish().await
}

#[tokio::test]
async fn window_split_through_linked_target_updates_every_linked_view() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("window-linked-split").await?;
    let rmux = harness.rmux();

    let linked_a = session_name("sdkwinsplita");
    let linked_b = session_name("sdkwinsplitb");
    let linked_b_peer = session_name("sdkwinsplitc");
    EnsureSession::named(linked_a.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    EnsureSession::named(linked_b.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    EnsureSession::named(linked_b_peer.clone())
        .create_only()
        .group_target(linked_b.clone())
        .ensure(&rmux)
        .await?;
    raw_link_window(
        harness.socket_path(),
        linked_a.clone(),
        0,
        linked_b.clone(),
        1,
    )
    .await?;

    let linked_target = rmux.window(WindowRef::new(linked_b.clone(), 1)).await?;
    let window_id = linked_target.id().await?.expect("linked target is listed");
    assert_eq!(linked_target.panes().await?.len(), 1);

    let split_pane = rmux
        .pane(rmux_sdk::PaneRef::new(linked_b.clone(), 1, 0))
        .await?
        .split(SplitDirection::Right)
        .await?;
    let split_ref = split_pane.target().clone();
    assert_eq!(split_ref.session_name, linked_b);
    assert_eq!(split_ref.window_index, 1);

    let linked_target_panes = linked_target.panes().await?;
    assert_eq!(linked_target_panes.len(), 2);
    assert!(linked_target_panes
        .iter()
        .any(|pane| pane.target == split_ref));

    let linked_a_window = raw_window(harness.socket_path(), linked_a.clone(), 0).await?;
    let linked_b_window = raw_window(harness.socket_path(), linked_b.clone(), 1).await?;
    let linked_b_peer_window = raw_window(harness.socket_path(), linked_b_peer.clone(), 1).await?;
    assert_eq!(linked_a_window.window_id, window_id.to_string());
    assert_eq!(linked_b_window.window_id, window_id.to_string());
    assert_eq!(linked_b_peer_window.window_id, window_id.to_string());
    assert_eq!(linked_a_window.pane_count, 2);
    assert_eq!(linked_b_window.pane_count, 2);
    assert_eq!(linked_b_peer_window.pane_count, 2);

    let info = linked_target.info().await?;
    assert_eq!(info.sessions.len(), 1);
    assert_eq!(info.windows.len(), 1);
    assert_eq!(info.windows[0].id, window_id);
    assert_eq!(info.windows[0].index, 1);
    assert_eq!(info.panes.len(), 2);

    harness.finish().await
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn raw_new_window(socket_path: &Path, target: SessionName, index: u32) -> TestResult {
    match framed_request(
        socket_path,
        Request::NewWindow(NewWindowRequest {
            target: target.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: Some(index),
            insert_at_target: false,
        }),
    )
    .await?
    {
        Response::NewWindow(response) => {
            assert_eq!(response.target, WindowTarget::with_window(target, index));
            Ok(())
        }
        response => Err(format!("unexpected new-window response: {response:?}").into()),
    }
}

async fn raw_link_window(
    socket_path: &Path,
    source_session: SessionName,
    source_index: u32,
    target_session: SessionName,
    target_index: u32,
) -> TestResult {
    match framed_request(
        socket_path,
        Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source_session, source_index),
            target: WindowTarget::with_window(target_session.clone(), target_index),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }),
    )
    .await?
    {
        Response::LinkWindow(response) => {
            assert_eq!(
                response.target,
                WindowTarget::with_window(target_session, target_index)
            );
            Ok(())
        }
        response => Err(format!("unexpected link-window response: {response:?}").into()),
    }
}

async fn raw_window(
    socket_path: &Path,
    target: SessionName,
    index: u32,
) -> TestResult<WindowListEntry> {
    raw_list_windows(socket_path, target)
        .await?
        .into_iter()
        .find(|entry| entry.target.window_index() == index)
        .ok_or_else(|| format!("window {index} was not listed").into())
}

async fn raw_list_window_indices(socket_path: &Path, target: SessionName) -> TestResult<Vec<u32>> {
    Ok(raw_list_windows(socket_path, target)
        .await?
        .into_iter()
        .map(|entry| entry.target.window_index())
        .collect())
}

async fn raw_list_windows(
    socket_path: &Path,
    target: SessionName,
) -> TestResult<Vec<WindowListEntry>> {
    match framed_request(
        socket_path,
        Request::ListWindows(ListWindowsRequest {
            target,
            format: Some("#{window_index}:#{window_id}:#{window_panes}".to_owned()),
        }),
    )
    .await?
    {
        Response::ListWindows(response) => Ok(response.windows),
        response => Err(format!("unexpected list-windows response: {response:?}").into()),
    }
}

async fn raw_list_pane_ids(
    socket_path: &Path,
    target: SessionName,
    target_window_index: Option<u32>,
) -> TestResult<Vec<String>> {
    match framed_request(
        socket_path,
        Request::ListPanes(ListPanesRequest {
            target,
            target_window_index,
            format: Some("#{pane_id}".to_owned()),
        }),
    )
    .await?
    {
        Response::ListPanes(response) => Ok(String::from_utf8_lossy(response.output.stdout())
            .lines()
            .map(str::to_owned)
            .collect()),
        response => Err(format!("unexpected list-panes response: {response:?}").into()),
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
            "rmux-sdk-window-{}-{}-{unique_id}",
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
