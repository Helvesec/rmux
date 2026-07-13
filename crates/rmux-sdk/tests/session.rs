#![cfg(unix)]

mod common;

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_proto::{
    encode_frame, FrameDecoder, HasSessionRequest, ListSessionsRequest, Request, Response,
};
use rmux_sdk::bootstrap::discovery::{SDK_ENDPOINT_ENV, SDK_TIMEOUT_MS_ENV};
use rmux_sdk::{
    EnsureSession, InfoSnapshot, NewSessionSpec, ProcessSpec, RmuxBuilder, RmuxError, SessionName,
    TerminalSizeSpec,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio::time::Instant;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

static LIVE_DAEMON_LOCK: common::unix_smoke::LiveDaemonLock =
    common::unix_smoke::LiveDaemonLock::new();
static ENV_LOCK: Mutex<()> = Mutex::const_new(());
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn create_only_builds_live_session_and_hides_process_environment() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("create-only").await?;
    let rmux = harness.rmux();
    let name = session_name("sdkcreate");
    let secret = "SDK_SESSION_SECRET=do-not-render";
    let ensure = EnsureSession::named(name.clone())
        .create_only()
        .environment([secret])
        .empty_tags()
        .window_name("main")
        .size(TerminalSizeSpec::new(80, 24));

    assert_eq!(ensure.configured_tags(), Some([].as_slice()));
    assert_no_secret("ensure debug", &format!("{ensure:?}"), secret);

    let session = ensure.ensure(&rmux).await?;
    assert_eq!(session.name(), &name);
    assert!(session.was_created());
    assert_eq!(session.creation_tags(), Some([].as_slice()));
    assert_no_secret("session debug", &format!("{session:?}"), secret);
    assert!(session.exists().await?);
    assert!(session.is_listed().await?);

    let raw_has = framed_request(
        harness.socket_path(),
        Request::HasSession(HasSessionRequest {
            target: name.clone(),
        }),
    )
    .await?;
    assert_eq!(
        raw_has,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );
    assert!(raw_list_session_names(harness.socket_path())
        .await?
        .contains(&name));

    assert!(session.kill().await?);
    harness.finish().await
}

#[tokio::test]
async fn ensure_session_command_preserves_legacy_and_explicit_modes_are_clear() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("ensure-argv-shell").await?;
    let rmux = harness.rmux();
    let legacy_name = session_name("sdkensurelegacy");
    let direct_name = session_name("sdkensureargv");
    let shell_name = session_name("sdkensureshell");
    let executable = write_executable_script(
        harness.root(),
        "ensure direct binary",
        "#!/bin/sh\nprintf 'sdk_ensure_direct_argv\\n'; sleep 2\n",
    )?;

    let legacy = EnsureSession::named(legacy_name.clone())
        .create_only()
        .detached(true)
        .command(["printf 'sdk_ensure_legacy_shell\\n'; sleep 2"])
        .ensure(&rmux)
        .await?;
    legacy
        .pane(0, 0)
        .wait_for_text("sdk_ensure_legacy_shell")
        .await?;

    let direct = EnsureSession::named(direct_name.clone())
        .create_only()
        .detached(true)
        .argv([executable.to_string_lossy().into_owned()])
        .ensure(&rmux)
        .await?;
    direct
        .pane(0, 0)
        .wait_for_text("sdk_ensure_direct_argv")
        .await?;

    let shell = EnsureSession::named(shell_name.clone())
        .create_only()
        .detached(true)
        .shell("printf 'sdk_ensure_shell\\n'; sleep 2")
        .ensure(&rmux)
        .await?;
    shell.pane(0, 0).wait_for_text("sdk_ensure_shell").await?;

    assert!(legacy.kill().await?);
    assert!(direct.kill().await?);
    assert!(shell.kill().await?);
    harness.finish().await
}

#[tokio::test]
async fn create_only_duplicate_is_error_but_attach_if_exists_reuses() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("reuse-policy").await?;
    let rmux = harness.rmux();
    let name = session_name("sdkreuse");

    let created = EnsureSession::named(name.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    assert!(created.was_created());

    let duplicate_secret = "DUPLICATE_SECRET=hidden";
    let duplicate = EnsureSession::named(name.clone())
        .create_only()
        .environment([duplicate_secret])
        .ensure(&rmux)
        .await
        .expect_err("create-only reports duplicate sessions");
    assert_duplicate_session(&duplicate, "sdkreuse");
    assert_no_secret(
        "duplicate diagnostic",
        &duplicate.to_string(),
        duplicate_secret,
    );

    let reuse_secret = "REUSE_SECRET=hidden";
    let reused = EnsureSession::named(name.clone())
        .create_or_reuse()
        .environment([reuse_secret])
        .tag("kept")
        .ensure(&rmux)
        .await?;
    assert_eq!(reused.name(), &name);
    assert!(!reused.was_created());
    assert_eq!(reused.creation_tags().expect("tag intent"), ["kept"]);
    assert_no_secret("reused session debug", &format!("{reused:?}"), reuse_secret);

    let fresh_name = session_name("sdkreusefresh");
    let fresh = EnsureSession::named(fresh_name.clone())
        .create_or_reuse()
        .ensure(&rmux)
        .await?;
    assert_eq!(fresh.name(), &fresh_name);
    assert!(fresh.was_created());
    assert!(fresh.kill().await?);

    let listed = raw_list_session_names(harness.socket_path()).await?;
    assert_eq!(
        listed
            .iter()
            .filter(|candidate| *candidate == &name)
            .count(),
        1
    );

    rmux.shutdown().await?;
    harness.wait().await
}

#[tokio::test]
async fn concurrent_create_or_reuse_creates_one_named_session() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("concurrent-create-or-reuse").await?;
    let rmux = harness.rmux();
    let name = session_name("sdkconcurrentreuse");

    let first = EnsureSession::named(name.clone()).create_or_reuse();
    let second = EnsureSession::named(name.clone()).create_or_reuse();
    let (first, second) = tokio::join!(rmux.ensure_session(first), rmux.ensure_session(second));
    let first = first?;
    let second = second?;

    assert_eq!(first.name(), &name);
    assert_eq!(second.name(), &name);
    assert_ne!(
        first.was_created(),
        second.was_created(),
        "exactly one concurrent ensure must report creating the session"
    );
    assert_eq!(
        raw_list_session_names(harness.socket_path())
            .await?
            .iter()
            .filter(|candidate| *candidate == &name)
            .count(),
        1
    );

    rmux.shutdown().await?;
    harness.wait().await
}

#[tokio::test]
async fn reuse_only_policy_never_creates_sessions() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("reuse-only").await?;
    let rmux = harness.rmux();
    let name = session_name("sdkreuseonly");
    let missing = session_name("sdkmissing");

    raw_new_session(harness.socket_path(), name.clone()).await?;

    let reused = EnsureSession::named(name.clone())
        .reuse_only()
        .empty_tags()
        .ensure(&rmux)
        .await?;
    assert_eq!(reused.name(), &name);
    assert!(!reused.was_created());
    assert_eq!(reused.creation_tags(), Some([].as_slice()));

    let missing_error = EnsureSession::named(missing.clone())
        .reuse_only()
        .ensure(&rmux)
        .await
        .expect_err("reuse-only must not create a missing session");
    assert_session_not_found(&missing_error, "sdkmissing");
    assert!(!raw_list_session_names(harness.socket_path())
        .await?
        .contains(&missing));

    let automatic_error = EnsureSession::auto_named()
        .reuse_only()
        .ensure(&rmux)
        .await
        .expect_err("reuse-only requires an explicit name");
    assert!(
        automatic_error
            .to_string()
            .contains("requires an explicit session name"),
        "unexpected automatic-name reuse-only diagnostic: {automatic_error}"
    );

    rmux.shutdown().await?;
    harness.wait().await
}

#[tokio::test]
async fn invalid_environment_override_diagnostic_is_redacted() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("invalid-env").await?;
    let rmux = harness.rmux();
    let secret = "SDK_INVALID_ENV_SECRET";
    let error = EnsureSession::named(session_name("sdkinvalidenv"))
        .create_only()
        .environment([secret])
        .ensure(&rmux)
        .await
        .expect_err("daemon rejects invalid environment assignments");
    let rendered = format!("{error:?}\n{error}");

    assert_no_secret("invalid environment diagnostic", &rendered, secret);
    assert!(
        rendered.contains("[redacted process environment]"),
        "diagnostic should identify redaction without exposing the override: {rendered}"
    );

    harness.finish().await
}

#[tokio::test]
async fn explicit_endpoint_timeout_and_empty_tag_semantics_are_preserved() -> TestResult {
    let _live_lock = LIVE_DAEMON_LOCK.lock().await;
    let _env_lock = ENV_LOCK.lock().await;
    let harness = Harness::start("precedence").await?;
    let wrong_endpoint = harness.root().join("wrong.sock");
    let _endpoint = EnvGuard::set_os(SDK_ENDPOINT_ENV, wrong_endpoint.as_os_str());
    let _timeout = EnvGuard::set(SDK_TIMEOUT_MS_ENV, "1");
    let rmux = RmuxBuilder::new()
        .unix_socket(harness.socket_path())
        .default_timeout(Duration::from_secs(30))
        .build();
    let name = session_name("sdkprecedence");
    let ensure = EnsureSession::named(name.clone())
        .tag("")
        .timeout(Duration::MAX);

    assert_eq!(
        rmux.endpoint(),
        &rmux_sdk::RmuxEndpoint::UnixSocket(harness.socket_path().to_path_buf())
    );
    assert_eq!(ensure.configured_tags().expect("explicit tag"), [""]);
    assert_eq!(ensure.resolved_timeout(&rmux), None);

    let session = rmux.ensure_session(ensure).await?;
    assert_eq!(session.endpoint(), rmux.endpoint());
    assert_eq!(
        session.configured_default_timeout(),
        Some(Duration::from_secs(30))
    );
    assert_eq!(session.creation_tags().expect("explicit tag"), [""]);
    assert!(session.exists().await?);

    harness.finish().await
}

#[test]
fn process_related_debug_output_redacts_environment_values() {
    let secret = "PROCESS_DEBUG_SECRET=hidden";
    let process = ProcessSpec {
        command: Some(vec!["printf".to_owned(), "ok".to_owned()]),
        process_command: None,
        environment: Some(vec![secret.to_owned()]),
    };
    assert_no_secret("process debug", &format!("{process:?}"), secret);

    let spec = NewSessionSpec {
        process: process.clone(),
        ..NewSessionSpec::default()
    };
    assert_no_secret("new-session spec debug", &format!("{spec:?}"), secret);

    let ensure = EnsureSession::named(session_name("debugsecret")).process(process);
    let ensure_debug = format!("{ensure:?}");
    assert!(
        ensure_debug.contains("printf"),
        "ensure debug should keep non-secret process intent: {ensure_debug}"
    );
    assert_no_secret("ensure debug", &ensure_debug, secret);

    let snapshot = InfoSnapshot::default();
    assert_no_secret("info snapshot debug", &format!("{snapshot:?}"), secret);
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn assert_duplicate_session(error: &RmuxError, expected: &str) {
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::DuplicateSession(actual),
            ..
        } => assert_eq!(actual, expected),
        other => panic!("expected duplicate session diagnostic, got {other:?}"),
    }
}

fn assert_session_not_found(error: &RmuxError, expected: &str) {
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::SessionNotFound(actual),
            ..
        } => assert_eq!(actual, expected),
        other => panic!("expected session-not-found diagnostic, got {other:?}"),
    }
}

fn assert_no_secret(label: &str, rendered: &str, secret: &str) {
    assert!(
        !rendered.contains(secret),
        "{label} must not render process environment secrets: {rendered}",
    );
    if let Some((name, _value)) = secret.split_once('=') {
        assert!(
            !rendered.contains(name),
            "{label} must not render process environment names: {rendered}",
        );
    }
    assert!(
        !rendered.contains("SECRET="),
        "{label} must not render process environment bindings: {rendered}",
    );
}

fn write_executable_script(root: &Path, name: &str, body: &str) -> TestResult<PathBuf> {
    let path = root.join(name);
    fs::write(&path, body)?;
    let mut permissions = fs::metadata(&path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions)?;
    Ok(path)
}

async fn raw_list_session_names(socket_path: &Path) -> TestResult<Vec<SessionName>> {
    match framed_request(
        socket_path,
        Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: Some("name".to_owned()),
            reversed: false,
        }),
    )
    .await?
    {
        Response::ListSessions(response) => Ok(String::from_utf8_lossy(response.output.stdout())
            .lines()
            .map(SessionName::new)
            .collect::<Result<Vec<_>, _>>()?),
        response => Err(format!("unexpected list-sessions response: {response:?}").into()),
    }
}

async fn raw_new_session(socket_path: &Path, name: SessionName) -> TestResult {
    let request = NewSessionSpec {
        session_name: Some(name.clone()),
        detached: true,
        ..NewSessionSpec::default()
    };

    match framed_request(
        socket_path,
        Request::NewSessionExt(Box::new(request.into())),
    )
    .await?
    {
        Response::NewSession(response) => {
            assert_eq!(response.session_name, name);
            Ok(())
        }
        response => Err(format!("unexpected new-session response: {response:?}").into()),
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

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn rmux(&self) -> rmux_sdk::Rmux {
        RmuxBuilder::new().unix_socket(&self.socket_path).build()
    }

    async fn wait(self) -> TestResult {
        wait_for_child_exit(self, "server did not exit after shutdown request").await?;
        Ok(())
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
                    || rendered.contains("Connection reset by peer")
                    || rendered.contains("Broken pipe"),
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
            "rmux-sdk-session-{}-{}-{unique_id}",
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

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        Self::set_os(key, OsStr::new(value))
    }

    fn set_os(key: &'static str, value: &OsStr) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
