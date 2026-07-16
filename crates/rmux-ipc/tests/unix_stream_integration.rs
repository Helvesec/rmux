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
use tokio::io::AsyncWriteExt;
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
