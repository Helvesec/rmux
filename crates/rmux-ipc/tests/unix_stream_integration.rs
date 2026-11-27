#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmux_ipc::{
    connect_blocking, resolve_endpoint, wait_for_peer_close, LocalEndpoint, LocalStream,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn wait_for_peer_close_keeps_observing_after_buffered_bytes() -> std::io::Result<()> {
    let (server, mut client) = LocalStream::pair()?;

    let wait = tokio::spawn(async move {
        timeout(Duration::from_secs(2), wait_for_peer_close(&server))
            .await
            .expect("peer close wait timed out after buffered bytes")
    });

    client.write_all(b"buffered protocol bytes").await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(client);

    wait.await.expect("peer close task")?;
    Ok(())
}

/// An exact-length read leaves Tokio's readiness set although the socket is
/// drained. The peer-close wait must still yield, so the caller's timeout fires
/// on a single-threaded runtime, and it must still observe the later close.
#[test]
fn wait_for_peer_close_yields_after_exact_length_read() {
    let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();
    // A wait that never yields also starves this runtime's timer, so the
    // verdict is taken from a separate OS thread.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        let outcome = runtime.block_on(async {
            let (mut server, mut client) = LocalStream::pair()?;
            client.write_all(b"ping").await?;
            let mut frame = [0_u8; 4];
            server.read_exact(&mut frame).await?;
            assert_eq!(&frame, b"ping");

            let silent_peer =
                timeout(Duration::from_millis(200), wait_for_peer_close(&server)).await;
            assert!(
                silent_peer.is_err(),
                "a connected silent peer must not be reported as closed"
            );

            drop(client);
            timeout(Duration::from_secs(2), wait_for_peer_close(&server))
                .await
                .map_err(|_| std::io::Error::other("peer close was not observed"))?
        });
        let _ = outcome_tx.send(outcome);
    });

    let outcome = outcome_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("wait_for_peer_close never yielded after an exact-length read");
    outcome.expect("peer close after an exact-length read");
}

#[test]
fn managed_socket_parent_rejects_unsafe_permissions_then_accepts_owner_only() -> std::io::Result<()>
{
    let root = unique_root("permissions");
    let managed = managed_parent(&root);
    fs::create_dir_all(&managed)?;
    fs::set_permissions(&managed, fs::Permissions::from_mode(0o755))?;
    let socket_path = managed.join("default");
    let listener = UnixListener::bind(&socket_path)?;
    let endpoint = LocalEndpoint::from_path(socket_path);

    let error = connect_blocking(&endpoint, Duration::from_secs(1))
        .expect_err("group-readable managed parent must be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);

    fs::set_permissions(&managed, fs::Permissions::from_mode(0o700))?;
    let stream = connect_blocking(&endpoint, Duration::from_secs(1))?;
    drop(stream);
    drop(listener);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn managed_socket_parent_rejects_symlink_then_accepts_plain_directory() -> std::io::Result<()> {
    let root = unique_root("symlink");
    let target = root.join("target");
    let managed = managed_parent(&root);
    fs::create_dir_all(&target)?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
    symlink(&target, &managed)?;
    let socket_path = managed.join("default");
    let listener = UnixListener::bind(&socket_path)?;
    let endpoint = LocalEndpoint::from_path(socket_path.clone());

    let error = connect_blocking(&endpoint, Duration::from_secs(1))
        .expect_err("symlinked managed parent must be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);

    drop(listener);
    fs::remove_file(&socket_path)?;
    fs::remove_file(&managed)?;
    fs::create_dir(&managed)?;
    fs::set_permissions(&managed, fs::Permissions::from_mode(0o700))?;
    let listener = UnixListener::bind(&socket_path)?;
    let stream = connect_blocking(&endpoint, Duration::from_secs(1))?;
    drop(stream);
    drop(listener);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn explicit_custom_socket_keeps_permissive_parent_policy() -> std::io::Result<()> {
    let root = unique_root("custom");
    let parent = root.join("shared");
    fs::create_dir_all(&parent)?;
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o775))?;
    let socket_path = parent.join("custom.sock");
    let listener = UnixListener::bind(&socket_path)?;
    let endpoint = resolve_endpoint(None, Some(&socket_path))?;

    let stream = connect_blocking(&endpoint, Duration::from_secs(1))?;

    drop(stream);
    drop(listener);
    fs::remove_dir_all(root)?;
    Ok(())
}

fn managed_parent(root: &Path) -> PathBuf {
    root.join(format!("rmux-{}", rmux_os::identity::real_user_id()))
}

fn unique_root(label: &str) -> PathBuf {
    let unique = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    PathBuf::from("/tmp").join(format!("ri-{label}-{}-{unique}", std::process::id()))
}
