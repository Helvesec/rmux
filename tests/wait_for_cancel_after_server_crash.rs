#![cfg(unix)]

use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmux_sdk::{EnsureSession, RmuxBuilder, RmuxError, SessionName};
use rmux_server::{DaemonConfig, ServerDaemon, ServerHandle};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);
const WAIT_SETTLE: Duration = Duration::from_millis(100);
const SDK_WAIT_BOUNDARY_TIMEOUT: Duration = Duration::from_secs(3);

struct Harness {
    socket_path: PathBuf,
    handle: Option<ServerHandle>,
}

impl Harness {
    async fn start(label: &str) -> TestResult<Self> {
        let socket_path = unique_socket_path(label);
        let handle = ServerDaemon::new(DaemonConfig::new(socket_path.clone()))
            .bind()
            .await?;
        Ok(Self {
            socket_path,
            handle: Some(handle),
        })
    }

    fn rmux(&self) -> rmux_sdk::Rmux {
        RmuxBuilder::new().unix_socket(&self.socket_path).build()
    }

    async fn shutdown(mut self) -> TestResult {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().await?;
        }
        if let Some(parent) = self.socket_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(parent) = self.socket_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

#[tokio::test]
async fn dropped_sdk_wait_cancels_without_destroying_runtime_state() -> TestResult {
    let harness = Harness::start("wait-cancel-drop").await?;
    let rmux = harness.rmux();
    let session_name = session_name("sdkwaitdrop");
    let session = EnsureSession::named(session_name.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    let pane = session.pane(0, 0);
    let wait_pane = pane.clone();
    let wait_task = tokio::spawn(async move { wait_pane.wait_for(b"never-observed").await });

    tokio::time::sleep(WAIT_SETTLE).await;
    wait_task.abort();
    let _ = wait_task.await;
    tokio::time::sleep(WAIT_SETTLE).await;

    let session_exists = tokio::time::timeout(SDK_WAIT_BOUNDARY_TIMEOUT, session.exists())
        .await
        .expect("dropped SDK wait must not keep the transport blocked")?;
    assert!(session_exists);
    let pane_exists = tokio::time::timeout(SDK_WAIT_BOUNDARY_TIMEOUT, pane.exists())
        .await
        .expect("dropped SDK wait must cancel without killing the pane")?;
    assert!(pane_exists);
    harness.shutdown().await
}

#[tokio::test]
async fn server_reset_during_sdk_wait_returns_typed_disconnect_error() -> TestResult {
    let mut harness = Harness::start("wait-server-reset").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(session_name("sdkwaitreset"))
        .create_only()
        .ensure(&rmux)
        .await?;
    let pane = session.pane(0, 0);
    let armed_wait = pane.wait_for_next(b"never-observed").await?;

    let handle = harness.handle.take().expect("daemon handle present");
    handle.shutdown().await?;

    let result = tokio::time::timeout(SDK_WAIT_BOUNDARY_TIMEOUT, armed_wait)
        .await
        .expect("wait future must resolve after daemon reset");

    match result {
        Err(RmuxError::Transport {
            operation, source, ..
        }) if matches!(
            source.kind(),
            io::ErrorKind::UnexpectedEof
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::BrokenPipe
        ) && operation.contains("sdk-wait-output") => {}
        other => panic!("expected typed SDK transport disconnect, got {other:?}"),
    }

    if let Some(parent) = harness.socket_path.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
    Ok(())
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn unique_socket_path(label: &str) -> PathBuf {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    // Keep daemon socket paths inside macOS' sockaddr_un path budget.
    PathBuf::from("/tmp")
        .join(format!(
            "rmux-wfc-{}-{}-{unique_id}",
            compact_label(label),
            std::process::id()
        ))
        .join("s")
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
