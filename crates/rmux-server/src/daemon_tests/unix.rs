use super::{
    default_socket_path, remove_stale_socket_if_needed, socket_root_from_env, DaemonConfig,
};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream as StdUnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[test]
fn default_socket_path_uses_the_spec_layout() {
    let path = default_socket_path().expect("default socket path");
    let path_string = path.to_string_lossy();

    assert!(path_string.ends_with("/default"));
    assert!(path_string.contains("/rmux-"));
}

#[test]
fn unresolved_rmux_tmpdir_falls_back_to_tmp() {
    assert_eq!(
        socket_root_from_env(Some(OsStr::new(
            "relative-rmux-test-path-that-does-not-exist"
        )))
        .expect("socket root"),
        fs::canonicalize("/tmp").expect("canonical /tmp")
    );
}

#[test]
fn real_user_id_matches_process_identity() {
    assert_eq!(
        super::real_user_id().expect("real uid"),
        rmux_os::identity::real_user_id()
    );
}

#[test]
fn daemon_config_returns_the_configured_path() {
    let path = PathBuf::from("/tmp/rmux-test/default");
    let config = DaemonConfig::new(path.clone());

    assert_eq!(config.socket_path(), path.as_path());
}

#[test]
fn stale_socket_probe_removes_unreachable_socket_files() {
    let socket_path = unique_socket_path("stale-socket");
    let parent = socket_path.parent().expect("socket parent");
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_dir_all(parent);
    fs::create_dir_all(parent).expect("create socket parent");
    let listener = StdUnixListener::bind(&socket_path).expect("bind stale socket");
    drop(listener);
    wait_until_socket_is_stale(&socket_path).expect("dropped listener becomes unreachable");

    remove_stale_socket_if_needed(&socket_path).expect("remove stale socket");

    assert!(!socket_path.exists());
    let _ = fs::remove_dir_all(parent);
}

#[test]
fn unsafe_managed_rmux_socket_directory_is_rejected() {
    let user_id = super::real_user_id().expect("real uid");
    let root = unique_socket_path("permissions")
        .parent()
        .expect("socket parent")
        .to_path_buf();
    let socket_path = root.join(format!("rmux-{user_id}")).join("default");
    let parent = socket_path.parent().expect("socket parent");
    fs::create_dir_all(parent).expect("create managed socket parent");
    fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).expect("set perms");

    let error = super::ensure_parent_directory(parent).expect_err("unsafe parent should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unsafe_managed_rmux_socket_directory_ancestor_is_rejected() {
    let user_id = super::real_user_id().expect("real uid");
    let root = unique_socket_path("nested-permissions")
        .parent()
        .expect("socket parent")
        .to_path_buf();
    let managed = root.join(format!("rmux-{user_id}"));
    let nested_parent = managed.join("nested");
    fs::create_dir_all(&nested_parent).expect("create nested socket parent");
    fs::set_permissions(&managed, fs::Permissions::from_mode(0o755)).expect("set perms");

    let error = super::ensure_parent_directory(&nested_parent)
        .expect_err("unsafe managed ancestor should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let _ = fs::set_permissions(&managed, fs::Permissions::from_mode(0o700));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn stale_socket_removal_leaves_non_socket_paths_untouched() {
    let socket_path = unique_socket_path("not-a-socket");
    let parent = socket_path.parent().expect("socket parent");
    fs::create_dir_all(parent).expect("create socket parent");
    fs::write(&socket_path, "not a socket").expect("write regular file");

    let error = remove_stale_socket_if_needed(&socket_path).expect_err("must fail");

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(fs::symlink_metadata(&socket_path)
        .expect("metadata")
        .file_type()
        .is_file());
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_dir_all(parent);
}

fn unique_socket_path(label: &str) -> PathBuf {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    let process_id = std::process::id();
    std::env::temp_dir()
        .join(format!("rmux-server-{label}-{process_id}-{unique_id}"))
        .join("default.sock")
}

fn wait_until_socket_is_stale(socket_path: &std::path::Path) -> std::io::Result<()> {
    for _ in 0..100 {
        match StdUnixStream::connect(socket_path) {
            Err(error) if super::indicates_stale_socket(&error) => return Ok(()),
            Err(error) => return Err(error),
            Ok(stream) => drop(stream),
        }

        std::thread::sleep(Duration::from_millis(1));
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("socket '{}' stayed reachable", socket_path.display()),
    ))
}
