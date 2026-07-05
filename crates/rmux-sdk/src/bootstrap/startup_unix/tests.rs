use super::filesystem::{has_mode_bit, prepare_socket_parent, startup_lock_path};
use super::lock::StartupLock;
use super::*;
use std::fs::{self, OpenOptions};
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rustix::fs::{flock, FlockOperation};

static NEXT_TEST_DIR_ID: AtomicUsize = AtomicUsize::new(0);
static TMPDIR_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn unique_dir(label: &str) -> PathBuf {
    let id = NEXT_TEST_DIR_ID.fetch_add(1, Ordering::SeqCst);
    let label: String = label
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(8)
        .collect();
    let label = if label.is_empty() { "case" } else { &label };

    // macOS Unix sockets have a short sockaddr_un path budget, while TMPDIR is
    // often a long /var/folders/... path. Keep test sockets under /tmp.
    PathBuf::from("/tmp").join(format!("rmux-su-{label}-{}-{id}", std::process::id()))
}

fn create_test_dir(path: &std::path::Path) {
    fs::create_dir_all(path).expect("temp dir");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("chmod temp dir");
}

#[tokio::test]
async fn startup_lock_path_uses_sibling_filename() {
    let socket = PathBuf::from("/tmp/rmux-1000/default");
    assert_eq!(
        startup_lock_path(&socket),
        PathBuf::from("/tmp/rmux-1000/default.startup-lock")
    );
}

#[test]
fn bare_relative_socket_path_uses_current_directory_parent() {
    let socket = Path::new("bare.sock");

    assert_eq!(
        socket_parent_or_current(socket).expect("parent"),
        Path::new(".")
    );
    assert_eq!(
        startup_lock_path(socket),
        PathBuf::from("bare.sock.startup-lock")
    );
    assert!(can_probe_existing_socket_before_startup_validation(socket));
}

#[test]
fn shared_sticky_parent_uses_private_lock_directory() {
    let tmp = PathBuf::from("/tmp");
    let metadata = fs::symlink_metadata(&tmp).expect("stat /tmp");
    let owner_uid = real_user_id();
    if metadata.uid() == owner_uid
        || !has_mode_bit(metadata.mode(), libc::S_ISVTX)
        || metadata.mode() & 0o022 == 0
    {
        return;
    }

    let socket = tmp.join(format!(
        "rmux-shared-parent-{}-{}.sock",
        std::process::id(),
        NEXT_TEST_DIR_ID.fetch_add(1, Ordering::SeqCst)
    ));
    let prepared = prepare_socket_parent(&socket, &tmp, owner_uid)
        .expect("sticky shared parent should be accepted");

    let lock_dir = tmp.join(format!("rmux-{owner_uid}")).join("startup-locks");
    assert!(
        prepared.lock_path.starts_with(&lock_dir),
        "lock path should live under private lock dir: {:?}",
        prepared.lock_path
    );

    let lock_dir_metadata = fs::symlink_metadata(&lock_dir).expect("stat lock dir");
    assert_eq!(lock_dir_metadata.uid(), owner_uid);
    assert_eq!(lock_dir_metadata.mode() & 0o777, SOCKET_DIRECTORY_MODE);
}

#[test]
fn shared_sticky_parent_symlink_uses_private_lock_directory() {
    let tmp = PathBuf::from("/tmp");
    let link_metadata = fs::symlink_metadata(&tmp).expect("stat /tmp");
    if !link_metadata.file_type().is_symlink() {
        return;
    }

    let target_metadata = fs::metadata(&tmp).expect("stat resolved /tmp");
    let owner_uid = real_user_id();
    if target_metadata.uid() == owner_uid
        || !has_mode_bit(target_metadata.mode(), libc::S_ISVTX)
        || target_metadata.mode() & 0o022 == 0
    {
        return;
    }

    let socket = tmp.join(format!(
        "rmux-shared-parent-link-{}-{}.sock",
        std::process::id(),
        NEXT_TEST_DIR_ID.fetch_add(1, Ordering::SeqCst)
    ));
    let prepared = prepare_socket_parent(&socket, &tmp, owner_uid)
        .expect("symlink to sticky shared parent should be accepted");

    let lock_dir = tmp.join(format!("rmux-{owner_uid}")).join("startup-locks");
    assert!(
        prepared.lock_path.starts_with(&lock_dir),
        "lock path should live under private lock dir through the symlink: {:?}",
        prepared.lock_path
    );
}

#[test]
fn custom_socket_parent_accepts_terminal_symlink_to_shared_sticky_parent() {
    let tmp = PathBuf::from("/tmp");
    let target_metadata = fs::metadata(&tmp).expect("stat /tmp");
    let owner_uid = real_user_id();
    if target_metadata.uid() == owner_uid
        || !has_mode_bit(target_metadata.mode(), libc::S_ISVTX)
        || target_metadata.mode() & 0o022 == 0
    {
        return;
    }

    let link = unique_dir("stickylink");
    let _ = fs::remove_dir_all(&link);
    std::os::unix::fs::symlink(&tmp, &link).expect("create sticky parent symlink");
    let socket = link.join(format!(
        "rmux-sticky-link-{}-{}.sock",
        std::process::id(),
        NEXT_TEST_DIR_ID.fetch_add(1, Ordering::SeqCst)
    ));

    let prepared = prepare_socket_parent(&socket, &link, owner_uid)
        .expect("terminal symlink to sticky shared parent should be accepted");

    let lock_dir = link.join(format!("rmux-{owner_uid}")).join("startup-locks");
    assert!(
        prepared.lock_path.starts_with(&lock_dir),
        "lock path should live under private lock dir through custom symlink: {:?}",
        prepared.lock_path
    );
    let _ = fs::remove_file(&link);
}

#[test]
fn custom_socket_parent_preserves_existing_permissions() {
    let dir = unique_dir("parentmode");
    create_test_dir(&dir);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).expect("chmod temp dir");
    let socket = dir.join("sock");
    let owner_uid = real_user_id();

    prepare_socket_parent(&socket, &dir, owner_uid).expect("custom parent should be accepted");

    let metadata = fs::symlink_metadata(&dir).expect("stat temp dir");
    assert_eq!(metadata.mode() & 0o777, 0o750);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn custom_socket_parent_accepts_other_readable_permissions() {
    let dir = unique_dir("parentother");
    create_test_dir(&dir);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod temp dir");
    let socket = dir.join("sock");
    let owner_uid = real_user_id();

    prepare_socket_parent(&socket, &dir, owner_uid)
        .expect("other-readable custom parent should be accepted");

    let metadata = fs::symlink_metadata(&dir).expect("stat temp dir");
    assert_eq!(metadata.mode() & 0o777, 0o755);
    let _ = fs::remove_dir_all(&dir);
}

fn assert_private_lock_path(
    prepared: &super::filesystem::PreparedSocketParent,
    custom_parent: &Path,
) {
    let owner_uid = real_user_id();
    let root = fs::canonicalize("/tmp").expect("private lock root");
    let lock_dir = root.join(format!("rmux-{owner_uid}")).join("startup-locks");
    assert!(
        prepared.lock_path.starts_with(&lock_dir),
        "group-writable custom parents must use private lock dir: {:?}",
        prepared.lock_path
    );
    assert!(
        !prepared.lock_path.starts_with(custom_parent),
        "private lock path must not live below custom parent: {:?}",
        prepared.lock_path
    );
    let metadata = fs::symlink_metadata(&lock_dir).expect("stat private lock dir");
    assert_eq!(metadata.uid(), owner_uid);
    assert_eq!(metadata.mode() & 0o777, SOCKET_DIRECTORY_MODE);
}

#[test]
fn custom_socket_parent_accepts_group_writable_permissions_with_private_lock() {
    let dir = unique_dir("parentgw");
    create_test_dir(&dir);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o775)).expect("chmod temp dir");
    let socket = dir.join("sock");
    let owner_uid = real_user_id();

    let prepared = prepare_socket_parent(&socket, &dir, owner_uid)
        .expect("group-writable owner-owned custom parent should be accepted");

    assert_private_lock_path(&prepared, &dir);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn custom_socket_parent_accepts_setgid_group_writable_permissions_with_private_lock() {
    let dir = unique_dir("parentsgid");
    create_test_dir(&dir);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o2775)).expect("chmod temp dir");
    let socket = dir.join("sock");
    let owner_uid = real_user_id();

    let prepared = prepare_socket_parent(&socket, &dir, owner_uid)
        .expect("setgid group-writable owner-owned custom parent should be accepted");

    assert_private_lock_path(&prepared, &dir);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn custom_socket_parent_rejects_other_writable_permissions() {
    for mode in [0o777, 0o707] {
        let dir = unique_dir(&format!("parent{mode:o}"));
        create_test_dir(&dir);
        fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).expect("chmod temp dir");
        let socket = dir.join("sock");
        let owner_uid = real_user_id();

        let error = prepare_socket_parent(&socket, &dir, owner_uid)
            .expect_err("other-writable custom parent should be rejected");

        assert!(
            matches!(error, StartupError::UnsafePermissions { .. }),
            "unexpected error for mode {mode:o}: {error:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
fn custom_socket_parent_rejects_wrong_owner() {
    let dir = unique_dir("parentowner");
    create_test_dir(&dir);
    let socket = dir.join("sock");
    let unexpected_uid = real_user_id().wrapping_add(1);

    let error = prepare_socket_parent(&socket, &dir, unexpected_uid)
        .expect_err("custom parent with wrong owner should be rejected");

    assert!(
        matches!(error, StartupError::UnsafeOwner { .. }),
        "unexpected error: {error:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn custom_socket_parent_rejects_symlink_parent() {
    let target = unique_dir("parentsymlinktarget");
    let link = unique_dir("parentsymlink");
    create_test_dir(&target);
    std::os::unix::fs::symlink(&target, &link).expect("create parent symlink");
    let socket = link.join("sock");
    let owner_uid = real_user_id();

    let error = prepare_socket_parent(&socket, &link, owner_uid)
        .expect_err("non-sticky parent symlink should be rejected");

    assert!(
        matches!(error, StartupError::SymlinkRejected { .. }),
        "unexpected error: {error:?}"
    );
    let _ = fs::remove_file(&link);
    let _ = fs::remove_dir_all(&target);
}

#[test]
fn custom_socket_parent_rejects_intermediate_symlink_component() {
    let root = unique_dir("parentsymroot");
    let target = unique_dir("parentsymtarget");
    create_test_dir(&root);
    create_test_dir(&target);
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).expect("create intermediate symlink");
    fs::create_dir_all(target.join("child")).expect("create target child");
    let parent = link.join("child");
    let socket = parent.join("sock");
    let owner_uid = real_user_id();

    let error = prepare_socket_parent(&socket, &parent, owner_uid)
        .expect_err("intermediate symlink should be rejected");

    assert!(
        matches!(error, StartupError::SymlinkRejected { .. }),
        "unexpected error: {error:?}"
    );
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&target);
}

#[tokio::test]
async fn launcher_runs_once_when_only_one_caller() {
    let dir = unique_dir("solo");
    create_test_dir(&dir);
    let socket = dir.join("default");
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);

    let result = connect_or_start_with(
        &socket,
        move || async move {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other("no daemon for solo"))
        },
        Duration::from_millis(50),
        Duration::from_millis(10),
    )
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    match result {
        Err(StartupError::Launcher { .. }) => {}
        other => panic!("expected Launcher error, got {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn empty_socket_path_does_not_create_startup_lock_artifact() {
    let _guard = TMPDIR_ENV_LOCK.lock().await;
    let dir = unique_dir("empty");
    create_test_dir(&dir);
    let old_rmux_tmpdir = std::env::var_os("RMUX_TMPDIR");
    let old_tmux_tmpdir = std::env::var_os("TMUX_TMPDIR");
    std::env::set_var("RMUX_TMPDIR", &dir);
    std::env::set_var("TMUX_TMPDIR", &dir);

    let result = connect_or_start_with(
        std::path::Path::new(""),
        || async { Err::<(), io::Error>(io::Error::other("no daemon for empty socket")) },
        Duration::from_millis(20),
        Duration::from_millis(5),
    )
    .await;

    match old_rmux_tmpdir {
        Some(value) => std::env::set_var("RMUX_TMPDIR", value),
        None => std::env::remove_var("RMUX_TMPDIR"),
    }
    match old_tmux_tmpdir {
        Some(value) => std::env::set_var("TMUX_TMPDIR", value),
        None => std::env::remove_var("TMUX_TMPDIR"),
    }

    #[cfg(target_os = "linux")]
    match result {
        Err(StartupError::Launcher { .. }) => {}
        other => panic!("expected Launcher error, got {other:?}"),
    }
    #[cfg(not(target_os = "linux"))]
    match result {
        Err(StartupError::Filesystem {
            operation,
            path,
            source,
        }) => {
            assert_eq!(operation, "connect to daemon socket");
            assert!(path.as_os_str().is_empty());
            assert_eq!(source.kind(), io::ErrorKind::InvalidInput);
        }
        other => panic!("expected InvalidInput filesystem error, got {other:?}"),
    }
    let entries: Vec<_> = fs::read_dir(&dir)
        .expect("read tmpdir")
        .map(|entry| entry.expect("read entry").path())
        .collect();
    assert!(
        entries.is_empty(),
        "empty -S must not create files: {entries:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn invalid_path_when_socket_path_has_no_parent() {
    let socket = PathBuf::from("/");
    let result = connect_or_start_with(
        &socket,
        || async { Err::<(), io::Error>(io::Error::other("never")) },
        Duration::from_millis(10),
        Duration::from_millis(5),
    )
    .await;

    assert!(matches!(result, Err(StartupError::InvalidPath { .. })));
}

#[tokio::test]
async fn lock_acquisition_times_out_when_lock_is_held() {
    let dir = unique_dir("held-lock");
    create_test_dir(&dir);
    let lock_path = dir.join("default.startup-lock");
    let holder = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_CLOEXEC)
        .mode(STARTUP_LOCK_MODE)
        .open(&lock_path)
        .expect("open held lock");
    flock(holder.as_fd(), FlockOperation::LockExclusive).expect("hold startup lock");

    let result = StartupLock::acquire(
        &lock_path,
        real_user_id(),
        StartupDeadline::from_timeout(Some(Duration::from_millis(20))),
        Duration::from_millis(5),
    )
    .await;

    match result {
        Err(StartupError::Lock { path, source }) => {
            assert_eq!(path, lock_path);
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        other => panic!("expected timed-out Lock error, got {other:?}"),
    }

    drop(holder);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn recoverable_matrix_matches_documented_contract() {
    let recoverable = [
        StartupError::Lock {
            path: PathBuf::from("/tmp/lock"),
            source: io::Error::other("lock"),
        },
        StartupError::Launcher {
            source: io::Error::other("launcher"),
        },
        StartupError::StartupTimeout {
            socket_path: PathBuf::from("/tmp/sock"),
            waited: Duration::from_millis(1),
        },
        StartupError::PeerCredentialMismatch {
            expected_uid: 1000,
            actual_uid: 1001,
            socket_path: PathBuf::from("/tmp/sock"),
        },
    ];
    for error in recoverable {
        assert!(
            error.is_recoverable(),
            "expected recoverable, got {error:?}"
        );
    }

    let not_recoverable = [
        StartupError::InvalidPath {
            reason: "no parent".to_owned(),
            path: PathBuf::from("/"),
        },
        StartupError::SymlinkRejected {
            path: PathBuf::from("/tmp/sym"),
        },
        StartupError::Filesystem {
            operation: "stat",
            path: PathBuf::from("/tmp/x"),
            source: io::Error::other("fs"),
        },
        StartupError::UnsafeOwner {
            path: PathBuf::from("/tmp/x"),
            expected_uid: 1000,
            actual_uid: 0,
        },
        StartupError::UnsafePermissions {
            path: PathBuf::from("/tmp/x"),
            mode: 0o644,
        },
    ];
    for error in not_recoverable {
        assert!(
            !error.is_recoverable(),
            "expected non-recoverable, got {error:?}"
        );
    }
}

#[tokio::test]
async fn startup_outcome_is_owner_only_for_started() {
    let dir = unique_dir("outcome-isowner");
    create_test_dir(&dir);
    let socket = dir.join("default");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind helper listener");
    let accept = tokio::spawn(async move { listener.accept().await });

    let stream = UnixStream::connect(&socket).await.expect("connect helper");
    let started = StartupOutcome::Started(stream);
    assert!(started.is_owner());
    let joined = StartupOutcome::JoinedExisting(started.into_stream());
    assert!(!joined.is_owner());
    drop(joined);

    let _ = accept.await;
    let _ = fs::remove_dir_all(&dir);
}
