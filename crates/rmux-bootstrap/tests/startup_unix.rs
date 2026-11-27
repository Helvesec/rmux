#![cfg(unix)]

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmux_bootstrap::startup_unix::{connect_or_start_with_timeout, StartupError};
use tokio::net::UnixListener;

struct Endpoint(PathBuf);

impl Endpoint {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "rmux-bo-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(root.join("s"))
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.parent().unwrap()).unwrap();
    }
}

#[tokio::test]
async fn started_cleans_stale_socket_and_join_skips_launcher_even_at_zero_budget() {
    let endpoint = Endpoint::new();
    drop(UnixListener::bind(&endpoint.0).unwrap());
    let mut listener = None;
    let started = connect_or_start_with_timeout(
        &endpoint.0,
        || async {
            listener = Some(UnixListener::bind(&endpoint.0)?);
            Ok(())
        },
        Some(Duration::MAX),
        Duration::ZERO,
    )
    .await
    .unwrap();
    assert!(started.is_owner());
    let joined = connect_or_start_with_timeout(
        &endpoint.0,
        || async { panic!("live endpoint must skip launch") },
        Some(Duration::ZERO),
        Duration::ZERO,
    )
    .await
    .unwrap();
    assert!(!joined.is_owner());
    drop((started, joined));
    // Dropping startup streams releases clients, never the listening daemon.
    let again = connect_or_start_with_timeout(
        &endpoint.0,
        || async { panic!("dropping outcomes must preserve listener") },
        None,
        Duration::ZERO,
    )
    .await
    .unwrap();
    assert!(!again.is_owner());
    drop((again, listener));
}

#[tokio::test]
async fn rejected_socket_target_preserves_file_and_never_launches() {
    let endpoint = Endpoint::new();
    std::fs::write(&endpoint.0, b"keep").unwrap();
    let result = connect_or_start_with_timeout(
        &endpoint.0,
        || async { panic!("invalid target must not launch") },
        Some(Duration::ZERO),
        Duration::ZERO,
    )
    .await;
    assert!(matches!(result, Err(StartupError::Filesystem { .. })));
    assert_eq!(std::fs::read(&endpoint.0).unwrap(), b"keep");
}

#[tokio::test]
async fn launcher_failure_and_readiness_timeout_release_ownership_for_retry() {
    let endpoint = Endpoint::new();
    let failed = connect_or_start_with_timeout(
        &endpoint.0,
        || async { Err(io::Error::other("launcher control")) },
        None,
        Duration::ZERO,
    )
    .await;
    assert!(
        matches!(failed, Err(StartupError::Launcher { source }) if source.to_string() == "launcher control")
    );
    let timed_out = connect_or_start_with_timeout(
        &endpoint.0,
        || async { Ok(()) },
        Some(Duration::ZERO),
        Duration::ZERO,
    )
    .await;
    assert!(
        matches!(timed_out, Err(StartupError::StartupTimeout { socket_path, .. }) if socket_path == endpoint.0)
    );
    let mut listener = None;
    let retried = connect_or_start_with_timeout(
        &endpoint.0,
        || async {
            listener = Some(UnixListener::bind(&endpoint.0)?);
            Ok(())
        },
        Some(Duration::from_secs(2)),
        Duration::ZERO,
    )
    .await
    .unwrap();
    assert!(retried.is_owner());
    drop((retried, listener));
}

#[tokio::test]
async fn cancellation_during_launcher_releases_lock_without_deleting_artifacts() {
    let endpoint = Endpoint::new();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let socket = endpoint.0.clone();
    let task = tokio::spawn(async move {
        connect_or_start_with_timeout(
            &socket,
            || async {
                entered_tx.send(()).unwrap();
                std::future::pending::<io::Result<()>>().await
            },
            None,
            Duration::ZERO,
        )
        .await
    });
    entered_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(endpoint.0.with_file_name("s.startup-lock").is_file());
    let retried = connect_or_start_with_timeout(
        &endpoint.0,
        || async { Err(io::Error::other("retry acquired ownership")) },
        Some(Duration::from_secs(2)),
        Duration::ZERO,
    )
    .await;
    assert!(
        matches!(retried, Err(StartupError::Launcher { source }) if source.to_string() == "retry acquired ownership")
    );
}
