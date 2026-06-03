#![cfg(unix)]

mod common;

use std::collections::HashSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_proto::{encode_frame, FrameDecoder, HasSessionRequest, Request, Response};
use rmux_sdk::{EnsureSession, PaneProcessState, RmuxBuilder, SessionName, SplitDirection};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::Instant;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

static LIVE_DAEMON_LOCK: common::unix_smoke::LiveDaemonLock =
    common::unix_smoke::LiveDaemonLock::new();
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn layout_builder_creates_incomplete_grid_with_process_options() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("layout-builder-main").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(session_name("sdklayoutmain"))
        .create_only()
        .ensure(&rmux)
        .await?;
    let root_command = format!(
        "printf 'sdk_layout_alpha\\n'; \
         [ \"$RMUX_LAYOUT_ENV\" = ok ] && printf 'sdk_layout_env_ok\\n'; \
         [ \"$PWD\" = '{}' ] && printf 'sdk_layout_cwd_ok\\n'; \
         sleep 30",
        harness.root_path().display()
    );

    let panes = session
        .layout()
        .grid(3, 2)
        .pane("Alpha")
        .spawn(["sh".to_owned(), "-c".to_owned(), root_command])
        .cwd(harness.root_path())
        .env("RMUX_LAYOUT_ENV", "ok")
        .pane("Beta")
        .shell("printf 'sdk_layout_beta\\n'; exit 4")
        .keep_alive_on_exit(true)
        .pane("Gamma")
        .shell("printf 'sdk_layout_gamma\\n'; sleep 30")
        .pane("Delta")
        .shell("printf 'sdk_layout_delta\\n'; sleep 30")
        .pane("Epsilon")
        .shell("printf 'sdk_layout_epsilon\\n'; sleep 30")
        .apply()
        .await?;

    assert_eq!(panes.len(), 5);
    assert_eq!(session.window(0).panes().await?.len(), 5);

    for (pane, marker) in panes.panes().iter().zip([
        "sdk_layout_alpha",
        "sdk_layout_beta",
        "sdk_layout_gamma",
        "sdk_layout_delta",
        "sdk_layout_epsilon",
    ]) {
        wait_for_visible_text(pane, marker).await?;
    }

    let ids = collect_pane_ids(&panes).await?;
    assert_eq!(ids.len(), 5, "layout panes must have stable unique ids");

    let titles = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"];
    for (pane, expected) in panes.panes().iter().zip(titles) {
        assert_eq!(pane.title().await?.as_deref(), Some(expected));
    }

    wait_for_visible_text(&panes.panes()[0], "sdk_layout_env_ok").await?;
    wait_for_visible_text(&panes.panes()[0], "sdk_layout_cwd_ok").await?;
    let alpha = panes.panes()[0].snapshot().await?.visible_text();
    assert!(
        alpha.contains("sdk_layout_env_ok"),
        "root pane should receive env override: {alpha:?}"
    );
    assert!(
        alpha.contains("sdk_layout_cwd_ok"),
        "root pane should receive cwd override: {alpha:?}"
    );

    let beta_exit = panes.panes()[1]
        .wait_for_exit()
        .await?
        .expect("beta exits but remains visible");
    assert_eq!(beta_exit.code, Some(4));
    assert!(matches!(
        panes.panes()[1].info().await?.panes[0].process,
        PaneProcessState::Exited
    ));
    assert!(panes.panes()[1].exists().await?);

    harness.finish().await
}

#[tokio::test]
async fn layout_builder_rejects_unsafe_or_invalid_requests() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("layout-builder-invalid").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(session_name("sdklayoutinvalid"))
        .create_only()
        .ensure(&rmux)
        .await?;

    let zero_grid = session.layout().grid(0, 1).pane("Alpha").apply().await;
    assert_error_contains(zero_grid, "greater than zero");

    let over_capacity = session
        .layout()
        .grid(1, 1)
        .pane("Alpha")
        .pane("Beta")
        .apply()
        .await;
    assert_error_contains(over_capacity, "grid capacity is 1");

    let root_option_without_spawn = session
        .layout()
        .grid(1, 1)
        .pane("Alpha")
        .cwd(harness.root_path())
        .apply()
        .await;
    assert_error_contains(root_option_without_spawn, "require spawn() or shell()");

    session.pane(0, 0).split(SplitDirection::Right).await?;
    let non_empty_window = session
        .layout()
        .grid(2, 1)
        .pane("Alpha")
        .pane("Beta")
        .apply()
        .await;
    assert_error_contains(non_empty_window, "expects exactly one existing pane");

    let replaced = session
        .layout()
        .grid(2, 1)
        .replace_existing_panes(true)
        .pane("Alpha")
        .shell("printf 'sdk_layout_replaced_alpha\\n'; sleep 30")
        .pane("Beta")
        .shell("printf 'sdk_layout_replaced_beta\\n'; sleep 30")
        .apply()
        .await?;
    assert_eq!(replaced.len(), 2);
    assert_eq!(
        session.window(0).panes().await?.len(),
        2,
        "replace_existing_panes(true) should leave only the requested layout panes"
    );
    wait_for_visible_text(&replaced.panes()[0], "sdk_layout_replaced_alpha").await?;
    wait_for_visible_text(&replaced.panes()[1], "sdk_layout_replaced_beta").await?;

    harness.finish().await
}

async fn collect_pane_ids(panes: &rmux_sdk::PaneSet) -> TestResult<HashSet<rmux_sdk::PaneId>> {
    let mut ids = HashSet::new();
    for pane in panes.panes() {
        ids.insert(pane.id().await?.expect("layout pane has id"));
    }
    Ok(ids)
}

fn assert_error_contains<T>(result: rmux_sdk::Result<T>, expected: &str) {
    let error = match result {
        Ok(_) => panic!("operation should fail"),
        Err(error) => error,
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains(expected),
        "expected error containing {expected:?}, got {rendered:?}"
    );
}

async fn wait_for_visible_text(pane: &rmux_sdk::Pane, marker: &str) -> TestResult {
    if let Err(error) = pane
        .expect_visible_text()
        .to_contain(marker)
        .timeout(Duration::from_secs(5))
        .await
    {
        let visible = pane.snapshot().await?.visible_text();
        return Err(format!(
            "pane {:?} did not render {marker:?}: {error}; visible={visible:?}",
            pane.target()
        )
        .into());
    }
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

struct Harness {
    root: TestRoot,
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
            root,
            socket_path,
            child: Some(child),
        })
    }

    fn rmux(&self) -> rmux_sdk::Rmux {
        RmuxBuilder::new().unix_socket(&self.socket_path).build()
    }

    fn root_path(&self) -> &Path {
        self.root.path()
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
            "rmux-sdk-layout-{}-{}-{unique_id}",
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
